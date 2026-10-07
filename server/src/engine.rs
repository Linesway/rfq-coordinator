//! All mutations and event publication occur under the API's single state lock.
//! Policy: maker funds reserved per executable RFQ size at placement; taker funds
//! reserved at quote formation. Winners stay firm; every terminal path releases.
//!
//! Additional policy choices, kept next to the code they govern:
//! - No taker hold at submission. At close, omit unaffordable sides; if none
//!   remain, emit NoQuote then Expired. Quote holds cannot be cancelled.
//! - Oversized maker offers reserve only the requested fill, not their excess.
//! - Price ties use server acceptance sequence; simultaneous deadlines process
//!   by deadline then scheduling sequence, so reservation competition is stable.
//! - A client ID stays occupied while its order is open or a selected winner.
//! - Auth failure does not consume a nonce; any authenticated fresh envelope
//!   does, including failed actions/subscriptions. Nonce zero is initially valid.
//! - Decline is only allowed while awaiting acceptance. Repeated terminal
//!   actions return a state error. A missing quote side leaves the RFQ unchanged.
//! - Positive prices may round to zero payment; overflowed notionals are
//!   invalid_size. Overflowing receiver balances fail atomically, release all
//!   holds and return insufficient_balance; balances never wrap or saturate.
//! - Terminal RFQs/orders are retained for snapshots and ownership checks.
//!   State is in memory and resets from the seed on restart.
use crate::error::{Error, Result};
use common::{
    seed::SeedConfig,
    signing::{Envelope, SignablePayload},
    types::{error_codes::*, *},
    Pubkey,
};
use std::collections::{BTreeMap, HashMap};
use tokio::sync::broadcast;
use uuid::Uuid;

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

#[derive(Default, Clone)]
struct Balance {
    total: u64,
    reserved: u64,
}
impl Balance {
    fn available(&self) -> u64 {
        self.total - self.reserved
    }
    fn view(&self) -> BalanceView {
        BalanceView {
            total: self.total,
            reserved: self.reserved,
            available: self.available(),
        }
    }
}
struct Trader {
    name: Option<String>,
    usd: Balance,
    assets: BTreeMap<String, Balance>,
    nonce: Option<u64>,
}
#[derive(Clone)]
struct Order {
    id: Uuid,
    maker: Pubkey,
    rfq: Uuid,
    side: Side,
    price: u64,
    cid: Option<u64>,
    amount: u64,
    sequence: u64,
    active: bool,
}
#[derive(Clone)]
pub struct Rfq {
    pub view: RfqView,
    owner: Pubkey,
    direction: Option<Side>,
    orders: Vec<Uuid>,
    bid: Option<Uuid>,
    ask: Option<Uuid>,
    taker_usd: u64,
    taker_asset: u64,
    no_quote: bool,
    trade: Option<Trade>,
    pub events: broadcast::Sender<TakerEvent>,
}
pub struct Engine {
    assets: BTreeMap<String, u32>,
    traders: BTreeMap<Pubkey, Trader>,
    pub rfqs: HashMap<Uuid, Rfq>,
    orders: HashMap<Uuid, Order>,
    cids: HashMap<(Pubkey, u64), Uuid>,
    deadlines: BTreeMap<(u64, u64), Uuid>,
    sequence: u64,
    accept_ms: u64,
    pub makers: broadcast::Sender<MakerEvent>,
}

impl Engine {
    pub fn new(seed: SeedConfig, accept_secs: u64) -> anyhow::Result<Self> {
        seed.validate()?;
        anyhow::ensure!(
            seed.assets.values().all(|a| a.lot_decimals <= 38),
            "lot_decimals must be <= 38"
        );
        let accept_ms = accept_secs
            .checked_mul(1000)
            .ok_or_else(|| anyhow::anyhow!("accept window overflow"))?;
        anyhow::ensure!(
            accept_ms <= u64::MAX - common::signing::unix_millis() - 29_000,
            "accept deadline overflow"
        );
        let assets: BTreeMap<_, _> = seed
            .assets
            .into_iter()
            .map(|(s, a)| (s, a.lot_decimals))
            .collect();
        let traders = seed
            .traders
            .into_iter()
            .map(|t| {
                let balances = assets
                    .keys()
                    .map(|s| {
                        (
                            s.clone(),
                            Balance {
                                total: t.assets.get(s).copied().unwrap_or(0),
                                reserved: 0,
                            },
                        )
                    })
                    .collect();
                (
                    t.pubkey,
                    Trader {
                        name: t.name,
                        usd: Balance {
                            total: t.usd,
                            reserved: 0,
                        },
                        assets: balances,
                        nonce: None,
                    },
                )
            })
            .collect();
        Ok(Self {
            assets,
            traders,
            rfqs: HashMap::new(),
            orders: HashMap::new(),
            cids: HashMap::new(),
            deadlines: BTreeMap::new(),
            sequence: 0,
            accept_ms,
            makers: broadcast::channel(1024).0,
        })
    }
    // Precedence: decoding (API), signature, registered trader, nonce, action rules.
    // Authenticated fresh nonces are consumed even when action validation fails.
    pub fn authenticate<T: SignablePayload>(&mut self, e: &Envelope<T>) -> Result<Pubkey> {
        e.verify().map_err(|_| Error::new(INVALID_SIGNATURE))?;
        let trader = self
            .traders
            .get_mut(&e.pubkey)
            .ok_or_else(|| Error::new(UNKNOWN_TRADER))?;
        if trader.nonce.is_some_and(|n| e.nonce <= n) {
            return Err(Error::new(STALE_NONCE));
        }
        trader.nonce = Some(e.nonce);
        Ok(e.pubkey)
    }
    pub fn assets(&self) -> AssetsResponse {
        AssetsResponse {
            assets: self
                .assets
                .iter()
                .map(|(s, d)| AssetInfo {
                    symbol: s.clone(),
                    lot_decimals: *d,
                })
                .collect(),
        }
    }
    pub fn balances_of(&self, key: Pubkey) -> Result<TraderBalances> {
        let t = self
            .traders
            .get(&key)
            .ok_or_else(|| Error::new(UNKNOWN_TRADER))?;
        Ok(TraderBalances {
            pubkey: key,
            name: t.name.clone(),
            usd: t.usd.view(),
            assets: t
                .assets
                .iter()
                .map(|(s, b)| (s.clone(), b.view()))
                .collect(),
        })
    }
    pub fn balances(&self) -> BalancesResponse {
        BalancesResponse {
            traders: self
                .traders
                .keys()
                .map(|k| self.balances_of(*k).unwrap())
                .collect(),
        }
    }
    pub fn rfq(&self, id: Uuid) -> Result<&Rfq> {
        self.rfqs.get(&id).ok_or_else(|| Error::new(RFQ_NOT_FOUND))
    }
    fn schedule(&mut self, id: Uuid, at: u64) {
        self.sequence += 1;
        self.deadlines.insert((at, self.sequence), id);
    }
    pub fn submit(
        &mut self,
        owner: Pubkey,
        p: SubmitRfqPayload,
        now: u64,
    ) -> Result<SubmitRfqResponse> {
        if !self.assets.contains_key(&p.asset) {
            return Err(Error::new(UNKNOWN_ASSET));
        }
        if p.size == 0 {
            return Err(Error::new(INVALID_SIZE));
        }
        if !(1..=29).contains(&p.ttl_secs) {
            return Err(Error::new(INVALID_TTL));
        }
        let closes_at = now
            .checked_add(p.ttl_secs * 1000)
            .ok_or_else(|| Error::new(INVALID_TTL))?;
        closes_at
            .checked_add(self.accept_ms)
            .ok_or_else(|| Error::new(INVALID_TTL))?;
        let id = Uuid::new_v4();
        self.rfqs.insert(
            id,
            Rfq {
                view: RfqView {
                    rfq_id: id,
                    asset: p.asset.clone(),
                    size: p.size,
                    state: RfqState::Open,
                    created_at: now,
                    closes_at,
                    accept_deadline: None,
                },
                owner,
                direction: p.direction,
                orders: vec![],
                bid: None,
                ask: None,
                taker_usd: 0,
                taker_asset: 0,
                no_quote: false,
                trade: None,
                events: broadcast::channel(32).0,
            },
        );
        self.schedule(id, closes_at);
        let _ = self.makers.send(MakerEvent::RfqOpened {
            rfq_id: id,
            asset: p.asset,
            size: p.size,
            direction: p.direction,
            created_at: now,
            closes_at,
        });
        Ok(SubmitRfqResponse {
            rfq_id: id,
            created_at: now,
            closes_at,
        })
    }
    fn notional(&self, r: &Rfq, price: u64) -> Result<u64> {
        let amount = (r.view.size as u128 * price as u128) / 10u128.pow(self.assets[&r.view.asset]);
        u64::try_from(amount).map_err(|_| Error::new(INVALID_SIZE))
    }
    pub fn place(&mut self, maker: Pubkey, p: PlaceOrderPayload) -> Result<PlaceOrderResponse> {
        let r = self.rfq(p.rfq_id)?;
        if r.view.state != RfqState::Open {
            return Err(Error::new(RFQ_NOT_OPEN));
        }
        if r.owner == maker {
            return Err(Error::new(SELF_QUOTE_NOT_ALLOWED));
        }
        if p.size == 0 {
            return Err(Error::new(INVALID_SIZE));
        }
        if p.size < r.view.size {
            return Err(Error::new(ORDER_TOO_SMALL));
        }
        if p.price == 0 {
            return Err(Error::malformed("price must be positive"));
        }
        if p.client_order_id
            .is_some_and(|cid| self.cids.contains_key(&(maker, cid)))
        {
            return Err(Error::new(DUPLICATE_CLIENT_ORDER_ID));
        }
        let cash = self.notional(r, p.price)?;
        let amount = if p.side == Side::Buy {
            cash
        } else {
            r.view.size
        };
        let asset = r.view.asset.clone();
        let t = self.traders.get_mut(&maker).unwrap();
        let b = if p.side == Side::Buy {
            &mut t.usd
        } else {
            t.assets.get_mut(&asset).unwrap()
        };
        if b.available() < amount {
            return Err(Error::new(INSUFFICIENT_BALANCE));
        }
        b.reserved += amount;
        self.sequence += 1;
        let id = Uuid::new_v4();
        self.orders.insert(
            id,
            Order {
                id,
                maker,
                rfq: p.rfq_id,
                side: p.side,
                price: p.price,
                cid: p.client_order_id,
                amount,
                sequence: self.sequence,
                active: true,
            },
        );
        if let Some(cid) = p.client_order_id {
            self.cids.insert((maker, cid), id);
        }
        self.rfqs.get_mut(&p.rfq_id).unwrap().orders.push(id);
        Ok(PlaceOrderResponse { order_id: id })
    }
    fn release_order(&mut self, id: Uuid, asset: &str) {
        let o = self.orders.get_mut(&id).unwrap();
        if !o.active {
            return;
        }
        o.active = false;
        let t = self.traders.get_mut(&o.maker).unwrap();
        let b = if o.side == Side::Buy {
            &mut t.usd
        } else {
            t.assets.get_mut(asset).unwrap()
        };
        b.reserved -= o.amount;
        if let Some(cid) = o.cid {
            self.cids.remove(&(o.maker, cid));
        }
    }
    pub fn cancel(&mut self, maker: Pubkey, p: CancelOrderPayload) -> Result<CancelOrderResponse> {
        if p.order_id.is_some() == p.client_order_id.is_some() {
            return Err(Error::malformed("specify exactly one order identifier"));
        }
        let r = self.rfq(p.rfq_id)?;
        if r.view.state != RfqState::Open {
            return Err(Error::new(RFQ_NOT_OPEN));
        }
        let asset = r.view.asset.clone();
        let id = p
            .order_id
            .or_else(|| self.cids.get(&(maker, p.client_order_id.unwrap())).copied())
            .ok_or_else(|| Error::new(ORDER_NOT_FOUND))?;
        let o = self
            .orders
            .get(&id)
            .filter(|o| o.rfq == p.rfq_id)
            .ok_or_else(|| Error::new(ORDER_NOT_FOUND))?;
        if o.maker != maker {
            return Err(Error::new(NOT_YOUR_ORDER));
        }
        if !o.active {
            return Err(Error::new(ORDER_NOT_FOUND));
        }
        self.release_order(id, &asset);
        Ok(CancelOrderResponse { order_id: id })
    }
    pub fn advance(&mut self, now: u64) {
        while let Some((&key, &id)) = self.deadlines.first_key_value() {
            if key.0 > now {
                break;
            }
            self.deadlines.remove(&key);
            match self.rfqs[&id].view.state {
                RfqState::Open => self.close(id),
                RfqState::AwaitingAccept => self.finish(id, RfqState::Expired, None),
                _ => {}
            }
        }
    }
    fn close(&mut self, id: Uuid) {
        let r = self.rfqs[&id].clone();
        let _ = self.makers.send(MakerEvent::RfqWindowClosed { rfq_id: id });
        let mut bid: Option<Uuid> = None;
        let mut ask: Option<Uuid> = None;
        for oid in &r.orders {
            let o = &self.orders[oid];
            if !o.active || r.direction.is_some_and(|d| d == o.side) {
                continue;
            }
            let target = if o.side == Side::Buy {
                &mut bid
            } else {
                &mut ask
            };
            let better = target.is_none_or(|old| {
                let old = &self.orders[&old];
                (if o.side == Side::Buy {
                    o.price > old.price
                } else {
                    o.price < old.price
                }) || (o.price == old.price && o.sequence < old.sequence)
            });
            if better {
                *target = Some(*oid);
            }
        }
        // Reserve both possible taker debits independently. Unaffordable sides
        // are omitted; maker reservations for every omitted/losing side release.
        let cash = ask
            .map(|oid| self.notional(&r, self.orders[&oid].price).unwrap())
            .unwrap_or(0);
        let t = self.traders.get_mut(&r.owner).unwrap();
        if t.usd.available() < cash {
            ask = None;
        }
        if t.assets[&r.view.asset].available() < r.view.size {
            bid = None;
        }
        let taker_usd = if ask.is_some() { cash } else { 0 };
        let taker_asset = if bid.is_some() { r.view.size } else { 0 };
        t.usd.reserved += taker_usd;
        t.assets.get_mut(&r.view.asset).unwrap().reserved += taker_asset;
        for oid in &r.orders {
            if Some(*oid) != bid && Some(*oid) != ask {
                self.release_order(*oid, &r.view.asset);
            }
        }
        let live = self.rfqs.get_mut(&id).unwrap();
        live.bid = bid;
        live.ask = ask;
        live.taker_usd = taker_usd;
        live.taker_asset = taker_asset;
        if bid.is_none() && ask.is_none() {
            live.no_quote = true;
            let _ = live.events.send(TakerEvent::NoQuote { rfq_id: id });
            self.finish(id, RfqState::Expired, None);
        } else {
            live.view.state = RfqState::AwaitingAccept;
            let deadline = r.view.closes_at + self.accept_ms;
            live.view.accept_deadline = Some(deadline);
            let event = self.quote(id);
            let _ = self.rfqs[&id].events.send(event);
            self.schedule(id, deadline);
        }
    }
    fn quote(&self, id: Uuid) -> TakerEvent {
        let r = &self.rfqs[&id];
        TakerEvent::Quote {
            rfq_id: id,
            bid: r.bid.map(|o| QuoteLevel {
                price: self.orders[&o].price,
            }),
            ask: r.ask.map(|o| QuoteLevel {
                price: self.orders[&o].price,
            }),
            size: r.view.size,
            accept_deadline: r.view.accept_deadline.unwrap(),
        }
    }
    fn owned_awaiting(&self, owner: Pubkey, id: Uuid) -> Result<&Rfq> {
        let r = self.rfq(id)?;
        if r.owner != owner {
            return Err(Error::new(NOT_YOUR_RFQ));
        }
        if r.view.state != RfqState::AwaitingAccept {
            return Err(Error::new(NOT_AWAITING_ACCEPT));
        }
        Ok(r)
    }
    pub fn accept(&mut self, owner: Pubkey, p: AcceptPayload, now: u64) -> Result<AcceptResponse> {
        let r = self.owned_awaiting(owner, p.rfq_id)?.clone();
        let oid = if p.side == Side::Buy { r.ask } else { r.bid }
            .ok_or_else(|| Error::new(SIDE_UNAVAILABLE))?;
        let o = self.orders[&oid].clone();
        let cash = self.notional(&r, o.price)?;
        let (buyer, seller) = if p.side == Side::Buy {
            (owner, o.maker)
        } else {
            (o.maker, owner)
        };
        // Validate every arithmetic operation before changing any balance.
        if self.traders[&buyer].assets[&r.view.asset]
            .total
            .checked_add(r.view.size)
            .is_none()
            || self.traders[&seller].usd.total.checked_add(cash).is_none()
        {
            self.finish(p.rfq_id, RfqState::Failed, None);
            return Err(Error::new(INSUFFICIENT_BALANCE));
        }
        let trade = Trade {
            trade_id: Uuid::new_v4(),
            rfq_id: p.rfq_id,
            asset: r.view.asset.clone(),
            taker_side: p.side,
            price: o.price,
            size: r.view.size,
            notional_usd: cash,
            executed_at: now,
        };
        // Release holds and apply transfers within this same critical section.
        self.release_rfq(&r);
        self.traders.get_mut(&buyer).unwrap().usd.total -= cash;
        self.traders.get_mut(&seller).unwrap().usd.total += cash;
        self.traders
            .get_mut(&seller)
            .unwrap()
            .assets
            .get_mut(&r.view.asset)
            .unwrap()
            .total -= r.view.size;
        self.traders
            .get_mut(&buyer)
            .unwrap()
            .assets
            .get_mut(&r.view.asset)
            .unwrap()
            .total += r.view.size;
        self.finish(p.rfq_id, RfqState::Settled, Some(trade.clone()));
        Ok(AcceptResponse { trade })
    }
    pub fn decline(&mut self, owner: Pubkey, p: DeclinePayload) -> Result<DeclineResponse> {
        self.owned_awaiting(owner, p.rfq_id)?;
        self.finish(p.rfq_id, RfqState::Declined, None);
        Ok(DeclineResponse {
            rfq_id: p.rfq_id,
            state: RfqState::Declined,
        })
    }
    fn release_rfq(&mut self, r: &Rfq) {
        for oid in &r.orders {
            self.release_order(*oid, &r.view.asset);
        }
        let t = self.traders.get_mut(&r.owner).unwrap();
        t.usd.reserved -= r.taker_usd;
        t.assets.get_mut(&r.view.asset).unwrap().reserved -= r.taker_asset;
        let live = self.rfqs.get_mut(&r.view.rfq_id).unwrap();
        live.taker_usd = 0;
        live.taker_asset = 0;
    }
    fn finish(&mut self, id: Uuid, state: RfqState, trade: Option<Trade>) {
        let r = self.rfqs[&id].clone();
        if r.view.state.is_terminal() {
            return;
        }
        self.release_rfq(&r);
        let (outcome, event) = match state {
            RfqState::Settled => (
                RfqOutcome::Settled,
                TakerEvent::Settled {
                    rfq_id: id,
                    trade: trade.clone().unwrap(),
                },
            ),
            RfqState::Declined => (RfqOutcome::Declined, TakerEvent::Declined { rfq_id: id }),
            RfqState::Failed => (RfqOutcome::Failed, TakerEvent::Failed { rfq_id: id }),
            _ => (RfqOutcome::Expired, TakerEvent::Expired { rfq_id: id }),
        };
        let fills = trade
            .as_ref()
            .map(|t| {
                let oid = if t.taker_side == Side::Buy {
                    r.ask
                } else {
                    r.bid
                }
                .unwrap();
                let o = &self.orders[&oid];
                vec![Fill {
                    order_id: o.id,
                    maker_side: o.side,
                    price: o.price,
                    size: t.size,
                }]
            })
            .unwrap_or_default();
        let live = self.rfqs.get_mut(&id).unwrap();
        live.view.state = state;
        live.trade = trade;
        let _ = live.events.send(event);
        let _ = self.makers.send(MakerEvent::RfqDone {
            rfq_id: id,
            outcome,
            fills,
        });
    }
    // Snapshot and receiver creation are performed while holding the same lock
    // as transitions, preventing subscribe/quote event gaps.
    pub fn subscribe(
        &self,
        owner: Pubkey,
        id: Uuid,
    ) -> Result<(Vec<TakerEvent>, broadcast::Receiver<TakerEvent>)> {
        let r = self.rfq(id)?;
        if r.owner != owner {
            return Err(Error::new(NOT_YOUR_RFQ));
        }
        let mut events = vec![TakerEvent::Subscribed {
            rfq_id: id,
            state: r.view.state,
            closes_at: r.view.closes_at,
        }];
        if r.no_quote {
            events.push(TakerEvent::NoQuote { rfq_id: id });
        }
        match r.view.state {
            RfqState::Open => {}
            RfqState::AwaitingAccept => events.push(self.quote(id)),
            RfqState::Settled => events.push(TakerEvent::Settled {
                rfq_id: id,
                trade: r.trade.clone().unwrap(),
            }),
            RfqState::Expired => events.push(TakerEvent::Expired { rfq_id: id }),
            RfqState::Declined => events.push(TakerEvent::Declined { rfq_id: id }),
            RfqState::Failed => events.push(TakerEvent::Failed { rfq_id: id }),
        }
        Ok((events, r.events.subscribe()))
    }
}

//! Trading state and account queries. Call engine methods while holding the App lock.
//! See the child modules for offers, deadlines, settlement, and feeds.
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

mod feeds;
mod orders;
mod settlement;
mod windows;

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
    client_order_id: Option<u64>,
    // Cash for a maker buy; asset units for a maker sell.
    reserved_amount: u64,
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
    taker_reserved_usd: u64,
    taker_reserved_asset: u64,
    no_quote: bool,
    trade: Option<Trade>,
    pub events: broadcast::Sender<TakerEvent>,
}
pub struct Engine {
    assets: BTreeMap<String, u32>,
    traders: BTreeMap<Pubkey, Trader>,
    pub rfqs: HashMap<Uuid, Rfq>,
    orders: HashMap<Uuid, Order>,
    client_order_ids: HashMap<(Pubkey, u64), Uuid>,
    // (deadline in milliseconds, insertion sequence) gives stable deadline ordering.
    deadlines: BTreeMap<(u64, u64), Uuid>,
    sequence: u64,
    accept_window_ms: u64,
    pub makers: broadcast::Sender<MakerEvent>,
}

impl Engine {
    pub fn new(seed: SeedConfig, accept_secs: u64) -> anyhow::Result<Self> {
        seed.validate()?;
        anyhow::ensure!(
            seed.assets.values().all(|asset| asset.lot_decimals <= 38),
            "lot_decimals must be <= 38"
        );
        let accept_window_ms = accept_secs
            .checked_mul(1000)
            .ok_or_else(|| anyhow::anyhow!("accept window overflow"))?;
        anyhow::ensure!(
            accept_window_ms <= u64::MAX - common::signing::unix_millis() - 29_000,
            "accept deadline overflow"
        );
        let assets: BTreeMap<_, _> = seed
            .assets
            .into_iter()
            .map(|(symbol, asset)| (symbol, asset.lot_decimals))
            .collect();
        let traders = seed
            .traders
            .into_iter()
            .map(|trader| {
                let balances = assets
                    .keys()
                    .map(|symbol| {
                        (
                            symbol.clone(),
                            Balance {
                                total: trader.assets.get(symbol).copied().unwrap_or(0),
                                reserved: 0,
                            },
                        )
                    })
                    .collect();
                (
                    trader.pubkey,
                    Trader {
                        name: trader.name,
                        usd: Balance {
                            total: trader.usd,
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
            client_order_ids: HashMap::new(),
            deadlines: BTreeMap::new(),
            sequence: 0,
            accept_window_ms,
            makers: broadcast::channel(1024).0,
        })
    }

    /// A verified request consumes its nonce even if its action later fails.
    pub fn authenticate<T: SignablePayload>(&mut self, envelope: &Envelope<T>) -> Result<Pubkey> {
        envelope
            .verify()
            .map_err(|_| Error::new(INVALID_SIGNATURE))?;
        let trader = self
            .traders
            .get_mut(&envelope.pubkey)
            .ok_or_else(|| Error::new(UNKNOWN_TRADER))?;
        if trader
            .nonce
            .is_some_and(|last_nonce| envelope.nonce <= last_nonce)
        {
            return Err(Error::new(STALE_NONCE));
        }
        trader.nonce = Some(envelope.nonce);
        Ok(envelope.pubkey)
    }

    pub fn assets(&self) -> AssetsResponse {
        AssetsResponse {
            assets: self
                .assets
                .iter()
                .map(|(symbol, lot_decimals)| AssetInfo {
                    symbol: symbol.clone(),
                    lot_decimals: *lot_decimals,
                })
                .collect(),
        }
    }

    pub fn balances(&self) -> BalancesResponse {
        BalancesResponse {
            traders: self
                .traders
                .keys()
                .map(|pubkey| self.balances_of(*pubkey).unwrap())
                .collect(),
        }
    }

    pub fn balances_of(&self, key: Pubkey) -> Result<TraderBalances> {
        let trader = self
            .traders
            .get(&key)
            .ok_or_else(|| Error::new(UNKNOWN_TRADER))?;
        Ok(TraderBalances {
            pubkey: key,
            name: trader.name.clone(),
            usd: trader.usd.view(),
            assets: trader
                .assets
                .iter()
                .map(|(symbol, balance)| (symbol.clone(), balance.view()))
                .collect(),
        })
    }

    pub fn rfq(&self, id: Uuid) -> Result<&Rfq> {
        self.rfqs.get(&id).ok_or_else(|| Error::new(RFQ_NOT_FOUND))
    }

    /// Convert the requested asset quantity to cash, rounding down in integer units.
    pub(super) fn notional(&self, rfq: &Rfq, price: u64) -> Result<u64> {
        let cash =
            (rfq.view.size as u128 * price as u128) / 10u128.pow(self.assets[&rfq.view.asset]);
        u64::try_from(cash).map_err(|_| Error::new(INVALID_SIZE))
    }
}

#[cfg(test)]
#[path = "../tests/helpers/engine.rs"]
mod tests;

use super::*;
use std::ops::Range;

pub struct RandomHistory {
    engine: Engine,
    keys: Vec<Pubkey>,
    random: u64,
    seed: u64,
    now: u64,
    requests: Vec<Uuid>,
    initial: BTreeMap<String, u128>,
    initial_totals: (u128, u128),
    prices: Range<u64>,
    sizes: Range<u64>,
    request_lots: u64,
    offer_lots: u64,
    quote_secs: u64,
}

impl RandomHistory {
    pub fn single_asset(seed: u64, cash_each: u64, sol_lots_each: u64) -> Self {
        let (engine, keys) = world_with_funds(cash_each, sol_lots_each);
        Self::from_engine(engine, keys.to_vec(), seed, seed)
    }
    pub fn multiple_assets(seed: u64, cash_each: u64, assets: &[(&str, u32, u64)]) -> Self {
        let keys: Vec<_> = (1..=4)
            .map(|key| Keypair::from_seed([key; 32]).pubkey())
            .collect();
        let config = SeedConfig {
            assets: assets
                .iter()
                .map(|(symbol, decimals, _)| {
                    (
                        (*symbol).into(),
                        AssetDef {
                            lot_decimals: *decimals,
                        },
                    )
                })
                .collect(),
            traders: keys
                .iter()
                .map(|key| TraderSeed {
                    pubkey: *key,
                    name: None,
                    usd: cash_each,
                    assets: assets
                        .iter()
                        .map(|(symbol, _, lots)| ((*symbol).into(), *lots))
                        .collect(),
                })
                .collect(),
        };
        let engine = Engine::new(config, 1).expect("history seed should load");
        Self::from_engine(engine, keys, seed, seed + 1)
    }
    fn from_engine(engine: Engine, keys: Vec<Pubkey>, seed: u64, random: u64) -> Self {
        let initial = ledger(&engine);
        let initial_totals = totals(&engine);
        Self {
            engine,
            keys,
            seed,
            random,
            initial,
            initial_totals,
            now: 0,
            requests: vec![],
            prices: 900..1200,
            sizes: 1..301,
            request_lots: 100,
            offer_lots: 300,
            quote_secs: 1,
        }
    }
    pub fn trade_sizes(&mut self, request_lots: u64, offer_lots: u64) {
        self.request_lots = request_lots;
        self.offer_lots = offer_lots;
    }
    pub fn prices(&mut self, prices: Range<u64>) {
        self.prices = prices;
    }
    pub fn sizes(&mut self, sizes: Range<u64>) {
        self.sizes = sizes;
    }
    pub fn next_single_asset_move(&mut self) {
        self.random = self
            .random
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        let n = self.random >> 32;
        let key = self.keys[(n as usize / 7) % 3];
        self.now += n % 17;
        self.engine.advance(self.now);
        if self.requests.is_empty() || n.is_multiple_of(7) {
            self.requests.push(submit_sized(
                &mut self.engine,
                key,
                if n.is_multiple_of(2) {
                    None
                } else {
                    Some(Side::Buy)
                },
                self.now,
                self.request_lots,
                self.quote_secs,
            ));
        } else {
            let id = self.requests[(n as usize / 13) % self.requests.len()];
            match n % 7 {
                1..=3 => {
                    let _ = order_sized(
                        &mut self.engine,
                        key,
                        id,
                        if n.is_multiple_of(2) {
                            Side::Buy
                        } else {
                            Side::Sell
                        },
                        (self.prices).start + n % ((self.prices).end - (self.prices).start),
                        Some(n % 19),
                        self.offer_lots,
                    );
                }
                4 => {
                    let _ = self.engine.accept(
                        key,
                        AcceptPayload {
                            rfq_id: id,
                            side: if n.is_multiple_of(2) {
                                Side::Buy
                            } else {
                                Side::Sell
                            },
                        },
                        self.now,
                    );
                }
                5 => {
                    let _ = self.engine.decline(key, DeclinePayload { rfq_id: id });
                }
                _ => {
                    let _ = self.engine.cancel(
                        key,
                        CancelOrderPayload {
                            rfq_id: id,
                            order_id: None,
                            client_order_id: Some(n % 19),
                        },
                    );
                }
            }
        }
        invariants(&self.engine, self.initial_totals);
    }
    pub fn next_multi_asset_move(&mut self, step: usize) {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        let n = self.random;
        let owner = self.keys[(n >> 8) as usize % self.keys.len()];
        self.now += n % 31;
        self.engine.advance(self.now);
        if self.requests.is_empty() || n.is_multiple_of(8) {
            let direction = match n % 3 {
                0 => None,
                1 => Some(Side::Buy),
                _ => Some(Side::Sell),
            };
            let id = self
                .engine
                .submit(
                    owner,
                    SubmitRfqPayload {
                        asset: ["SOL", "BTC", "DOGE"][(n >> 16) as usize % 3].into(),
                        size: (self.sizes).start + n % ((self.sizes).end - (self.sizes).start),
                        ttl_secs: self.quote_secs,
                        direction,
                    },
                    self.now,
                )
                .unwrap()
                .rfq_id;
            self.requests.push(id);
        } else {
            let id = self.requests[(n >> 20) as usize % self.requests.len()];
            let side = if n & 256 == 0 { Side::Buy } else { Side::Sell };
            match n % 8 {
                1..=3 => {
                    let _ = self.engine.place(
                        owner,
                        PlaceOrderPayload {
                            rfq_id: id,
                            side,
                            price: (self.prices).start
                                + n % ((self.prices).end - (self.prices).start),
                            size: self.engine.rfqs[&id].view.size + n % 20,
                            client_order_id: Some(n % 13),
                        },
                    );
                }
                4 => {
                    let _ = self
                        .engine
                        .accept(owner, AcceptPayload { rfq_id: id, side }, self.now);
                }
                5 => {
                    let _ = self.engine.decline(owner, DeclinePayload { rfq_id: id });
                }
                6 => {
                    let _ = self.engine.cancel(
                        owner,
                        CancelOrderPayload {
                            rfq_id: id,
                            order_id: None,
                            client_order_id: Some(n % 13),
                        },
                    );
                }
                _ => {
                    let _ = self.engine.subscribe(owner, id);
                }
            }
        }
        assert_eq!(
            ledger(&self.engine),
            self.initial,
            "seed {seed}, step {step}",
            seed = self.seed
        );
        audit(&self.engine, &self.initial);
    }
    pub fn expect_money_and_reservations_correct(&self) {
        audit(&self.engine, &self.initial);
    }
    pub fn expire_every_request(&mut self) {
        self.engine.advance(self.now + 10_000);
    }
    pub fn expect_no_reserved_funds(&self) {
        self.expect_money_and_reservations_correct();
        assert!(self
            .engine
            .traders
            .values()
            .all(|trader| trader.usd.reserved == 0
                && trader.assets.values().all(|asset| asset.reserved == 0)));
    }
}

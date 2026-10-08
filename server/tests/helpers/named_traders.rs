use super::*;
use std::{cell::RefCell, rc::Rc};

pub trait ExpectedResult<T> {
    fn expect_accepted(self) -> T;
    fn expect_rejected(self, code: &str);
}
impl<T> ExpectedResult<T> for Result<T> {
    #[track_caller]
    fn expect_accepted(self) -> T {
        self.expect("action should be accepted")
    }
    #[track_caller]
    fn expect_rejected(self, code: &str) {
        match self {
            Ok(_) => panic!("action should be rejected with {code}"),
            Err(error) => assert_eq!(error.code, code),
        }
    }
}

struct TestState {
    engine: Engine,
    seed: SeedConfig,
    now: u64,
    initial: BTreeMap<String, u128>,
    accept_secs: u64,
}

pub struct TestExchange {
    state: Rc<RefCell<TestState>>,
}
#[derive(Clone)]
pub struct TestTrader {
    state: Rc<RefCell<TestState>>,
    key: Rc<Keypair>,
}

impl TestExchange {
    pub fn new() -> Self {
        Self::with_sol_decimals(0)
    }
    pub fn with_sol_decimals(decimals: u32) -> Self {
        let seed = SeedConfig {
            assets: BTreeMap::from([(
                "SOL".into(),
                AssetDef {
                    lot_decimals: decimals,
                },
            )]),
            traders: vec![],
        };
        let engine = Engine::new(seed.clone(), 1).expect("test seed should load");
        let initial = ledger(&engine);
        Self {
            state: Rc::new(RefCell::new(TestState {
                engine,
                seed,
                now: 0,
                initial,
                accept_secs: 1,
            })),
        }
    }
    pub fn add_trader(&self, name: &str, cash: u64, coins: Coins) -> TestTrader {
        let mut state = self.state.borrow_mut();
        assert!(state.engine.rfqs.is_empty(), "add traders before trading");
        let key = Rc::new(Keypair::from_seed([state.seed.traders.len() as u8 + 1; 32]));
        let amount = state.lots(coins);
        state.seed.traders.push(TraderSeed {
            name: Some(name.into()),
            pubkey: key.pubkey(),
            usd: cash,
            assets: BTreeMap::from([(coins.asset.into(), amount)]),
        });
        state.engine =
            Engine::new(state.seed.clone(), state.accept_secs).expect("trader should load");
        state.initial = ledger(&state.engine);
        TestTrader {
            state: self.state.clone(),
            key,
        }
    }
    pub fn unregistered_trader(&self) -> TestTrader {
        TestTrader {
            state: self.state.clone(),
            key: Rc::new(Keypair::from_seed([99; 32])),
        }
    }
    pub fn advance_to(&self, millisecond: u64) {
        let mut state = self.state.borrow_mut();
        assert!(millisecond >= state.now);
        state.now = millisecond;
        state.engine.advance(millisecond);
        audit(&state.engine, &state.initial);
    }
    pub fn close_quoting(&self, request: Uuid) {
        let deadline = self.state.borrow().engine.rfqs[&request].view.closes_at;
        self.advance_to(deadline);
    }
    pub fn end_acceptance(&self, request: Uuid) {
        let deadline = self.state.borrow().engine.rfqs[&request]
            .view
            .accept_deadline
            .expect("quote should have a deadline");
        self.advance_to(deadline);
    }
    pub fn acceptance_window(&self, seconds: u64) {
        let mut state = self.state.borrow_mut();
        assert!(state.engine.rfqs.is_empty());
        state.accept_secs = seconds;
        state.engine.accept_window_ms = seconds * 1000;
    }
    pub fn expect_state(&self, request: Uuid, expected: RfqState) {
        assert_eq!(
            self.state.borrow().engine.rfqs[&request].view.state,
            expected
        );
    }
    pub fn expect_deadline(&self, request: Uuid, expected: u64) {
        assert_eq!(
            self.state.borrow().engine.rfqs[&request]
                .view
                .accept_deadline,
            Some(expected)
        );
    }
    pub fn expect_winning_sell_offer(&self, request: Uuid, offer: &PlaceOrderResponse) {
        assert_eq!(
            self.state.borrow().engine.rfqs[&request].ask,
            Some(offer.order_id)
        );
    }
    pub fn expect_winning_buy_offer(&self, request: Uuid, offer: &PlaceOrderResponse) {
        assert_eq!(
            self.state.borrow().engine.rfqs[&request].bid,
            Some(offer.order_id)
        );
    }
    pub fn expect_no_sell_quote(&self, request: Uuid) {
        assert!(self.state.borrow().engine.rfqs[&request].bid.is_none());
    }
    pub fn expect_no_offer_numbers_in_use(&self) {
        assert!(self.state.borrow().engine.client_order_ids.is_empty());
    }
    pub fn expect_funds_released(&self) {
        let state = self.state.borrow();
        for trader in state.engine.traders.values() {
            assert_eq!(trader.usd.reserved, 0);
            for asset in trader.assets.values() {
                assert_eq!(asset.reserved, 0);
            }
        }
        assert!(state.engine.orders.values().all(|order| !order.active));
        audit(&state.engine, &state.initial);
    }
    pub fn expect_money_conserved(&self) {
        let state = self.state.borrow();
        audit(&state.engine, &state.initial);
    }
    pub fn balances(&self) -> serde_json::Value {
        snapshot(&self.state.borrow().engine)
    }
    pub fn expect_balances(&self, expected: &serde_json::Value) {
        assert_eq!(&self.balances(), expected);
    }
    pub fn expect_no_requests(&self) {
        let state = self.state.borrow();
        assert!(state.engine.rfqs.is_empty());
        assert!(state.engine.deadlines.is_empty());
    }
    pub fn expect_no_offers(&self) {
        let state = self.state.borrow();
        assert!(state.engine.orders.is_empty());
        assert!(state.engine.client_order_ids.is_empty());
    }
    pub fn authenticate<T: SignablePayload>(&self, request: &Envelope<T>) -> Result<Pubkey> {
        self.state.borrow_mut().engine.authenticate(request)
    }
    pub fn payment(&self, request: Uuid, price: u64) -> Result<u64> {
        let state = self.state.borrow();
        state.engine.notional(&state.engine.rfqs[&request], price)
    }
    pub fn watch_makers(&self) -> broadcast::Receiver<MakerEvent> {
        self.state.borrow().engine.makers.subscribe()
    }
}

impl TestState {
    fn lots(&self, coins: Coins) -> u64 {
        if coins.whole_coins {
            coins
                .amount
                .checked_mul(10u64.pow(self.seed.assets[coins.asset].lot_decimals))
                .expect("test amount must fit")
        } else {
            coins.amount
        }
    }
    fn payload(&self, request: PriceRequest) -> SubmitRfqPayload {
        SubmitRfqPayload {
            asset: request.coins.asset.into(),
            size: self.lots(request.coins),
            direction: request.direction,
            ttl_secs: request.ttl_secs,
        }
    }
}

impl TestTrader {
    pub fn set_sol_balance_for_test(&self, coins: Coins) {
        let mut state = self.state.borrow_mut();
        let amount = state.lots(coins);
        state
            .engine
            .traders
            .get_mut(&self.key.pubkey())
            .unwrap()
            .assets
            .get_mut("SOL")
            .unwrap()
            .total = amount;
        state.initial = ledger(&state.engine);
    }
    pub fn expect_updates(&self, request: Uuid, expected: &[&str]) {
        let (events, _) = self.watch(request);
        let actual: Vec<_> = events
            .iter()
            .map(|event| match event {
                TakerEvent::Subscribed { .. } => "subscribed",
                TakerEvent::NoQuote { .. } => "no_quote",
                TakerEvent::Quote { .. } => "quote",
                TakerEvent::Settled { .. } => "settled",
                TakerEvent::Expired { .. } => "expired",
                TakerEvent::Declined { .. } => "declined",
                TakerEvent::Failed { .. } => "failed",
                TakerEvent::Error { .. } => "error",
            })
            .collect();
        assert_eq!(actual, expected);
    }
    pub fn ask_to_buy(&self, coins: Coins) -> Uuid {
        self.submit(buy(coins)).expect_accepted().rfq_id
    }
    pub fn ask_to_sell(&self, coins: Coins) -> Uuid {
        self.submit(sell(coins)).expect_accepted().rfq_id
    }
    pub fn ask_for_both_prices(&self, coins: Coins) -> Uuid {
        self.submit(both_prices(coins)).expect_accepted().rfq_id
    }
    pub fn submit(&self, request: PriceRequest) -> Result<SubmitRfqResponse> {
        let mut state = self.state.borrow_mut();
        let now = state.now;
        let payload = state.payload(request);
        let result = state.engine.submit(self.key.pubkey(), payload, now);
        audit(&state.engine, &state.initial);
        result
    }
    pub fn offer_to_sell(
        &self,
        request: Uuid,
        coins: Coins,
        price: u64,
    ) -> Result<PlaceOrderResponse> {
        self.offer(request, coins, price, Side::Sell, None)
    }
    pub fn offer_to_buy(
        &self,
        request: Uuid,
        coins: Coins,
        price: u64,
    ) -> Result<PlaceOrderResponse> {
        self.offer(request, coins, price, Side::Buy, None)
    }
    pub fn numbered_sell_offer(
        &self,
        request: Uuid,
        coins: Coins,
        price: u64,
        number: u64,
    ) -> Result<PlaceOrderResponse> {
        self.offer(request, coins, price, Side::Sell, Some(number))
    }
    fn offer(
        &self,
        request: Uuid,
        coins: Coins,
        price: u64,
        side: Side,
        number: Option<u64>,
    ) -> Result<PlaceOrderResponse> {
        let mut state = self.state.borrow_mut();
        let size = state.lots(coins);
        let result = state.engine.place(
            self.key.pubkey(),
            PlaceOrderPayload {
                rfq_id: request,
                side,
                price,
                size,
                client_order_id: number,
            },
        );
        audit(&state.engine, &state.initial);
        result
    }
    pub fn cancel(&self, request: Uuid, offer: &PlaceOrderResponse) -> Result<CancelOrderResponse> {
        self.cancel_identifiers(request, Some(offer.order_id), None)
    }
    pub fn cancel_number(&self, request: Uuid, number: u64) -> Result<CancelOrderResponse> {
        self.cancel_identifiers(request, None, Some(number))
    }
    pub fn cancel_identifiers(
        &self,
        request: Uuid,
        order: Option<Uuid>,
        number: Option<u64>,
    ) -> Result<CancelOrderResponse> {
        let mut state = self.state.borrow_mut();
        let result = state.engine.cancel(
            self.key.pubkey(),
            CancelOrderPayload {
                rfq_id: request,
                order_id: order,
                client_order_id: number,
            },
        );
        audit(&state.engine, &state.initial);
        result
    }
    pub fn accept_buy(&self, request: Uuid) -> Result<AcceptResponse> {
        self.accept(request, Side::Buy)
    }
    pub fn accept_sell(&self, request: Uuid) -> Result<AcceptResponse> {
        self.accept(request, Side::Sell)
    }
    fn accept(&self, request: Uuid, side: Side) -> Result<AcceptResponse> {
        let mut state = self.state.borrow_mut();
        let now = state.now;
        let result = state.engine.accept(
            self.key.pubkey(),
            AcceptPayload {
                rfq_id: request,
                side,
            },
            now,
        );
        audit(&state.engine, &state.initial);
        result
    }
    pub fn decline(&self, request: Uuid) -> Result<DeclineResponse> {
        let mut state = self.state.borrow_mut();
        let result = state
            .engine
            .decline(self.key.pubkey(), DeclinePayload { rfq_id: request });
        audit(&state.engine, &state.initial);
        result
    }
    pub fn sign_request(&self, request: PriceRequest, nonce: u64) -> Envelope<SubmitRfqPayload> {
        self.sign(self.state.borrow().payload(request), nonce)
    }
    pub fn sign<T: SignablePayload>(&self, payload: T, nonce: u64) -> Envelope<T> {
        Envelope::sign(&self.key, nonce, payload)
    }
    pub fn expect_nonce(&self, nonce: Option<u64>) {
        assert_eq!(
            self.state.borrow().engine.traders[&self.key.pubkey()].nonce,
            nonce
        );
    }
    pub fn expect_cash(&self, expected: u64) {
        assert_eq!(self.balance().usd.total, expected);
    }
    pub fn expect_reserved_cash(&self, expected: u64) {
        assert_eq!(self.balance().usd.reserved, expected);
    }
    pub fn expect_total_sol(&self, expected: u64) {
        self.expect_total(lots("SOL", self.state.borrow().lots(sol(expected))));
    }
    pub fn expect_total(&self, coins: Coins) {
        assert_eq!(
            self.balance().assets[coins.asset].total,
            self.state.borrow().lots(coins)
        );
    }
    pub fn expect_reserved_sol(&self, expected: u64) {
        assert_eq!(
            self.balance().assets["SOL"].reserved,
            self.state.borrow().lots(sol(expected))
        );
    }
    pub fn expect_available_sol(&self, expected: u64) {
        assert_eq!(
            self.balance().assets["SOL"].available,
            self.state.borrow().lots(sol(expected))
        );
    }
    fn balance(&self) -> TraderBalances {
        self.state
            .borrow()
            .engine
            .balances_of(self.key.pubkey())
            .expect_accepted()
    }
    pub fn watch(&self, request: Uuid) -> (Vec<TakerEvent>, broadcast::Receiver<TakerEvent>) {
        self.state
            .borrow_mut()
            .engine
            .subscribe(self.key.pubkey(), request)
            .expect_accepted()
    }
}

#[derive(Clone)]
pub struct TestSeed {
    config: SeedConfig,
}
impl TestSeed {
    pub fn empty() -> Self {
        Self {
            config: SeedConfig {
                assets: BTreeMap::new(),
                traders: vec![],
            },
        }
    }
    pub fn asset(mut self, symbol: &str, decimals: u32) -> Self {
        self.config.assets.insert(
            symbol.into(),
            AssetDef {
                lot_decimals: decimals,
            },
        );
        self
    }
    pub fn trader(mut self, name: &str, key_number: u8, cash: u64, assets: &[(&str, u64)]) -> Self {
        self.config.traders.push(TraderSeed {
            name: Some(name.into()),
            pubkey: Keypair::from_seed([key_number; 32]).pubkey(),
            usd: cash,
            assets: assets
                .iter()
                .map(|(symbol, amount)| ((*symbol).into(), *amount))
                .collect(),
        });
        self
    }
    pub fn holding(mut self, key_number: u8, asset: &str, amount: u64) -> Self {
        let key = Keypair::from_seed([key_number; 32]).pubkey();
        self.config
            .traders
            .iter_mut()
            .find(|trader| trader.pubkey == key)
            .expect("trader exists")
            .assets
            .insert(asset.into(), amount);
        self
    }
    pub fn start(&self, accept_secs: u64) -> anyhow::Result<Engine> {
        Engine::new(self.config.clone(), accept_secs)
    }
    pub fn expect_sol_balance(&self, key_number: u8, expected: u64) {
        let engine = self.start(0).expect("seed should load");
        let balance = engine
            .balances_of(Keypair::from_seed([key_number; 32]).pubkey())
            .expect_accepted();
        assert_eq!(balance.assets["SOL"].total, expected);
    }
}

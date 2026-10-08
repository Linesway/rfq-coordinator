use super::*;
use common::{client::MakerFeed, signing::SignablePayload};
use uuid::Uuid;

pub struct ServerSetup {
    traders: Vec<(&'static str, u64, Coins)>,
    accept_secs: u64,
}
pub struct TestServer {
    pub world: World,
    names: Vec<&'static str>,
    initial: BalancesResponse,
}
#[derive(Clone)]
pub struct LiveTrader {
    pub client: Arc<RfqClient>,
    key: Arc<Keypair>,
}

pub trait ExpectedResult<T> {
    fn expect_accepted(self) -> T;
    fn expect_rejected(self, expected: &str);
}
impl<T> ExpectedResult<T> for anyhow::Result<T> {
    #[track_caller]
    fn expect_accepted(self) -> T {
        self.expect("action should be accepted")
    }
    #[track_caller]
    fn expect_rejected(self, expected: &str) {
        match self {
            Ok(_) => panic!("expected rejection: {expected}"),
            Err(error) => assert_eq!(code(error), expected),
        }
    }
}

impl TestServer {
    pub fn setup() -> ServerSetup {
        ServerSetup {
            traders: vec![],
            accept_secs: 2,
        }
    }
    pub fn trader(&self, name: &str) -> LiveTrader {
        let index = self
            .names
            .iter()
            .position(|candidate| *candidate == name)
            .expect("named trader exists");
        LiveTrader {
            client: self.world.clients[index].clone(),
            key: Arc::new(Keypair::from_seed([index as u8 + 1; 32])),
        }
    }
    pub fn unknown_trader(&self) -> LiveTrader {
        LiveTrader {
            client: Arc::new(RfqClient::new(
                &self.world.url,
                Keypair::from_seed([99; 32]),
            )),
            key: Arc::new(Keypair::from_seed([99; 32])),
        }
    }
    pub async fn balances(&self) -> BalancesResponse {
        self.world.clients[0].balances().await.expect_accepted()
    }
    pub async fn expect_money_conserved(&self) {
        expect_same_money(&self.initial, &self.balances().await);
    }
    pub async fn expect_nobody_traded(&self) {
        expect_nobody_traded(&self.initial, &self.balances().await);
    }
    pub async fn expect_funds_released(&self) {
        no_holds(&self.balances().await);
    }
    pub async fn expect_balances(&self, expected: &BalancesResponse) {
        assert_eq!(
            serde_json::to_value(self.balances().await).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
    pub async fn expect_healthy(&self) {
        self.world.clients[0].health().await.expect_accepted();
    }
    pub async fn public_request(&self, request: Uuid) -> Value {
        self.world.public_request(request).await
    }
    pub async fn wait_until_quoted(&self, request: Uuid) {
        timeout(Duration::from_secs(5), async {
            loop {
                let view = self.public_request(request).await;
                if view["state"] == "awaiting_accept" {
                    break;
                }
                assert_eq!(view["state"], "open");
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("quote should arrive");
    }
    pub async fn send(&self, path: &str, signed: &Value) -> HttpReply {
        self.http("POST", path, Some(signed.to_string()), "application/json")
            .await
    }
    pub async fn http(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
        content_type: &str,
    ) -> HttpReply {
        let mut request = reqwest::Client::new()
            .request(method.parse().unwrap(), format!("{}{path}", self.world.url));
        if let Some(body) = body {
            request = request.header("content-type", content_type).body(body);
        }
        let response = request.send().await.expect("HTTP response should arrive");
        HttpReply {
            status: response.status().as_u16(),
            body: response.json().await.expect("response should be JSON"),
        }
    }
}
impl ServerSetup {
    pub fn trader(mut self, name: &'static str, cash: u64, coins: Coins) -> Self {
        self.traders.push((name, cash, coins));
        self
    }
    pub fn acceptance_window(mut self, seconds: u64) -> Self {
        self.accept_secs = seconds;
        self
    }
    pub async fn start(self) -> TestServer {
        let names: Vec<_> = self.traders.iter().map(|(name, _, _)| *name).collect();
        let keys: Vec<_> = (1..=self.traders.len())
            .map(|index| Keypair::from_seed([index as u8; 32]))
            .collect();
        let seed = SeedConfig {
            assets: BTreeMap::from([("SOL".into(), AssetDef { lot_decimals: 2 })]),
            traders: self
                .traders
                .iter()
                .zip(&keys)
                .map(|((name, cash, coins), key)| TraderSeed {
                    pubkey: key.pubkey(),
                    name: Some((*name).into()),
                    usd: *cash,
                    assets: BTreeMap::from([(coins.asset.into(), coin_lots(*coins))]),
                })
                .collect(),
        };
        let app = App::new(seed, self.accept_secs).expect("test server should initialize");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes = router(app.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, routes).await.unwrap();
        });
        let clients = keys
            .into_iter()
            .map(|key| Arc::new(RfqClient::new(&url, key)))
            .collect();
        let world = World {
            url,
            clients,
            task,
            app,
        };
        let initial = world.clients[0].balances().await.expect_accepted();
        TestServer {
            world,
            names,
            initial,
        }
    }
}
pub fn coin_lots(coins: Coins) -> u64 {
    if coins.whole_coins {
        coins.amount.checked_mul(100).unwrap()
    } else {
        coins.amount
    }
}
pub fn request_payload(request: PriceRequest) -> SubmitRfqPayload {
    SubmitRfqPayload {
        asset: request.coins.asset.into(),
        direction: request.direction,
        size: coin_lots(request.coins),
        ttl_secs: request.ttl_secs,
    }
}
pub fn sell_payload(
    request: Uuid,
    coins: Coins,
    price: u64,
    number: Option<u64>,
) -> PlaceOrderPayload {
    PlaceOrderPayload {
        rfq_id: request,
        side: Side::Sell,
        price,
        size: coin_lots(coins),
        client_order_id: number,
    }
}
pub fn offer_payload(
    request: Uuid,
    side: Side,
    coins: Coins,
    price: u64,
    number: Option<u64>,
) -> PlaceOrderPayload {
    let mut offer = sell_payload(request, coins, price, number);
    offer.side = side;
    offer
}
pub fn cancel_message(
    request: Uuid,
    order: Option<Uuid>,
    number: Option<u64>,
) -> CancelOrderPayload {
    CancelOrderPayload {
        rfq_id: request,
        order_id: order,
        client_order_id: number,
    }
}
pub fn accept_message(request: Uuid, side: Side) -> AcceptPayload {
    AcceptPayload {
        rfq_id: request,
        side,
    }
}
pub fn decline_message(request: Uuid) -> DeclinePayload {
    DeclinePayload { rfq_id: request }
}
pub fn subscribe_message(request: Uuid) -> SubscribePayload {
    SubscribePayload { rfq_id: request }
}

impl LiveTrader {
    pub fn connect(url: &str, key_number: u8) -> Self {
        Self {
            client: Arc::new(RfqClient::new(url, Keypair::from_seed([key_number; 32]))),
            key: Arc::new(Keypair::from_seed([key_number; 32])),
        }
    }

    pub fn independently_sign(
        &self,
        action: &str,
        fields: &str,
        payload: Value,
        nonce: u64,
    ) -> Value {
        let pubkey = self.key.pubkey().to_hex();
        let canonical = format!("rfq-v1:{action}:{pubkey}:{nonce}:{fields}");
        let signature: String = self
            .key
            .sign(canonical.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        json!({"pubkey": pubkey, "nonce": nonce, "signature": signature, "payload": payload})
    }
    pub fn raw_price_request(&self, request: PriceRequest, nonce: u64) -> Value {
        let side = match request.direction {
            Some(Side::Buy) => "buy",
            Some(Side::Sell) => "sell",
            None => "-",
        };
        let size = coin_lots(request.coins);
        let fields = format!("{}:{side}:{size}:{}", request.coins.asset, request.ttl_secs);
        let mut payload =
            json!({"asset": request.coins.asset, "size": size, "ttl_secs": request.ttl_secs});
        if request.direction.is_some() {
            payload["direction"] = json!(side);
        }
        self.independently_sign("submit_rfq", &fields, payload, nonce)
    }
    pub fn raw_offer(
        &self,
        request: Uuid,
        side: Side,
        coins: Coins,
        price: u64,
        nonce: u64,
    ) -> Value {
        let side = if side == Side::Buy { "buy" } else { "sell" };
        let size = coin_lots(coins);
        self.independently_sign(
            "place_order",
            &format!("{request}:{side}:{price}:{size}:-"),
            json!({"rfq_id": request, "side": side, "price": price, "size": size}),
            nonce,
        )
    }
    pub fn raw_accept(&self, request: Uuid, side: Side, nonce: u64) -> Value {
        let side = if side == Side::Buy { "buy" } else { "sell" };
        self.independently_sign(
            "accept",
            &format!("{request}:{side}"),
            json!({"rfq_id": request, "side": side}),
            nonce,
        )
    }
    pub async fn expect_unreserved_balance(&self, cash: u64, coins: Coins) {
        let balance = self.balance().await;
        assert_eq!(
            (
                balance.usd.total,
                balance.usd.available,
                balance.usd.reserved
            ),
            (cash, cash, 0)
        );
        let asset = &balance.assets[coins.asset];
        let amount = coin_lots(coins);
        assert_eq!(
            (asset.total, asset.available, asset.reserved),
            (amount, amount, 0)
        );
    }
    pub async fn submit(&self, request: PriceRequest) -> anyhow::Result<SubmitRfqResponse> {
        self.client.submit_rfq(request_payload(request)).await
    }
    pub async fn ask_to_buy(&self, coins: Coins) -> Uuid {
        self.submit(buy(coins)).await.expect_accepted().rfq_id
    }
    pub async fn ask_for_both_prices(&self, coins: Coins) -> Uuid {
        self.submit(both_prices(coins))
            .await
            .expect_accepted()
            .rfq_id
    }
    pub async fn offer_to_sell(
        &self,
        request: Uuid,
        coins: Coins,
        price: u64,
    ) -> anyhow::Result<PlaceOrderResponse> {
        self.numbered_sell_offer(request, coins, price, None).await
    }
    pub async fn numbered_sell_offer(
        &self,
        request: Uuid,
        coins: Coins,
        price: u64,
        number: Option<u64>,
    ) -> anyhow::Result<PlaceOrderResponse> {
        self.client
            .place_order(sell_payload(request, coins, price, number))
            .await
    }
    pub async fn offer_to_buy(
        &self,
        request: Uuid,
        coins: Coins,
        price: u64,
    ) -> anyhow::Result<PlaceOrderResponse> {
        let mut offer = sell_payload(request, coins, price, None);
        offer.side = Side::Buy;
        self.client.place_order(offer).await
    }
    pub async fn cancel(
        &self,
        request: Uuid,
        offer: &PlaceOrderResponse,
    ) -> anyhow::Result<CancelOrderResponse> {
        self.cancel_identifiers(request, Some(offer.order_id), None)
            .await
    }
    pub async fn cancel_number(
        &self,
        request: Uuid,
        number: u64,
    ) -> anyhow::Result<CancelOrderResponse> {
        self.cancel_identifiers(request, None, Some(number)).await
    }
    pub async fn cancel_identifiers(
        &self,
        request: Uuid,
        order: Option<Uuid>,
        number: Option<u64>,
    ) -> anyhow::Result<CancelOrderResponse> {
        self.client
            .cancel_order(CancelOrderPayload {
                rfq_id: request,
                order_id: order,
                client_order_id: number,
            })
            .await
    }
    pub async fn accept_buy(&self, request: Uuid) -> anyhow::Result<AcceptResponse> {
        self.client.accept(request, Side::Buy).await
    }
    pub async fn accept_sell(&self, request: Uuid) -> anyhow::Result<AcceptResponse> {
        self.client.accept(request, Side::Sell).await
    }
    pub async fn decline(&self, request: Uuid) -> anyhow::Result<DeclineResponse> {
        self.client.decline(request).await
    }
    pub async fn watch(&self, request: Uuid, state: RfqState) -> TakerUpdates {
        self.client.watch_request(request, state).await
    }
    pub async fn maker_feed(&self) -> MakerFeed {
        self.client.maker_feed().await.expect_accepted()
    }
    pub async fn expect_cash(&self, expected: u64) {
        self.client.expect_cash(expected).await;
    }
    pub async fn expect_total_sol(&self, expected: u64) {
        self.client.expect_sol(expected).await;
    }
    pub async fn expect_reserved_sol(&self, expected: u64) {
        assert_eq!(self.balance().await.assets["SOL"].reserved, expected * 100);
    }
    pub async fn expect_reserved_cash(&self, expected: u64) {
        assert_eq!(self.balance().await.usd.reserved, expected);
    }
    pub async fn balance(&self) -> TraderBalances {
        self.client
            .balances_of(&self.client.pubkey())
            .await
            .expect_accepted()
    }
    pub fn sign_request(&self, request: PriceRequest, nonce: u64) -> Value {
        self.sign(request_payload(request), nonce)
    }
    pub fn sign<T: SignablePayload + serde::Serialize>(&self, payload: T, nonce: u64) -> Value {
        serde_json::to_value(Envelope::sign(&self.key, nonce, payload)).unwrap()
    }
    pub fn auto_sign<T: SignablePayload + serde::Serialize>(&self, payload: T) -> Value {
        serde_json::to_value(self.client.envelope(payload)).unwrap()
    }
}

pub struct HttpReply {
    pub status: u16,
    pub body: Value,
}
impl HttpReply {
    pub fn expect_error(&self, status: u16, code: &str) {
        assert_eq!(self.status, status, "{}", self.body);
        assert_eq!(self.body["error"]["code"], code);
        assert!(self.body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()));
    }
    pub fn expect_quote_window(&self, milliseconds: u64) {
        assert_eq!(
            self.body["closes_at"].as_u64().unwrap() - self.body["created_at"].as_u64().unwrap(),
            milliseconds
        );
    }
    pub fn expect_success(&self) {
        assert_eq!(self.status, 200, "{}", self.body);
    }
    pub fn request_id(&self) -> Uuid {
        self.expect_success();
        Uuid::parse_str(self.body["rfq_id"].as_str().unwrap()).unwrap()
    }
}

pub async fn expect_maker_opened(feed: &mut MakerFeed, request: Uuid, direction: Option<Side>) {
    match next_maker_update(feed).await {
        MakerEvent::RfqOpened {
            rfq_id,
            direction: actual,
            ..
        } => {
            assert_eq!(rfq_id, request);
            assert_eq!(actual, direction);
        }
        event => panic!("expected new request, received {event:?}"),
    }
}
pub async fn expect_maker_closed(feed: &mut MakerFeed, request: Uuid) {
    assert!(
        matches!(next_maker_update(feed).await, MakerEvent::RfqWindowClosed { rfq_id } if rfq_id == request)
    );
}
pub async fn expect_maker_filled(
    feed: &mut MakerFeed,
    request: Uuid,
    offer: &PlaceOrderResponse,
    lots: u64,
) {
    match next_maker_update(feed).await {
        MakerEvent::RfqDone {
            rfq_id,
            outcome: RfqOutcome::Settled,
            fills,
        } => {
            assert_eq!(rfq_id, request);
            assert_eq!(fills.len(), 1);
            assert_eq!(fills[0].order_id, offer.order_id);
            assert_eq!(fills[0].size, lots);
        }
        event => panic!("expected settlement, received {event:?}"),
    }
}
pub async fn expect_maker_failed(feed: &mut MakerFeed) {
    loop {
        if let MakerEvent::RfqDone { outcome, fills, .. } = next_maker_update(feed).await {
            assert_eq!(outcome, RfqOutcome::Failed);
            assert!(fills.is_empty());
            break;
        }
    }
}

pub fn expect_json_fields(value: &Value, expected: &[&str]) {
    let mut actual: Vec<_> = value
        .as_object()
        .expect("expected JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

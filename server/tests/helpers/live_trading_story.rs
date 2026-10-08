use super::*;
use common::client::MakerFeed;
use uuid::Uuid;

impl World {
    pub async fn public_request(&self, request: Uuid) -> Value {
        let response = reqwest::get(format!("{}/v1/rfqs/{request}", self.url))
            .await
            .expect("request should be readable");
        assert_eq!(response.status(), 200);
        response.json().await.expect("request should be JSON")
    }
}

pub trait TraderActions {
    async fn watch_request(&self, request: Uuid, expected_state: RfqState) -> TakerUpdates;
    async fn expect_cash(&self, cash: u64);
    async fn expect_sol(&self, sol: u64);
}
impl TraderActions for RfqClient {
    async fn watch_request(&self, request: Uuid, expected_state: RfqState) -> TakerUpdates {
        let (feed, acknowledgement) = self.subscribe_rfq(request).await.expect_accepted();
        match acknowledgement {
            TakerEvent::Subscribed { rfq_id, state, .. } => {
                assert_eq!(rfq_id, request);
                assert_eq!(
                    state, expected_state,
                    "subscription must report the current state"
                );
            }
            other => panic!("expected subscription acknowledgement, received {other:?}"),
        }
        TakerUpdates { feed, request }
    }
    async fn expect_cash(&self, cash: u64) {
        assert_eq!(
            self.balances_of(&self.pubkey())
                .await
                .expect_accepted()
                .usd
                .total,
            cash,
            "trader has the wrong cash balance"
        );
    }
    async fn expect_sol(&self, sol: u64) {
        assert_eq!(
            self.balances_of(&self.pubkey())
                .await
                .expect_accepted()
                .assets["SOL"]
                .total,
            sol * 100,
            "trader owns the wrong amount of SOL"
        );
    }
}

#[derive(Debug)]
pub enum Notice {
    Settled,
    Declined,
    Expired,
    NoQuote,
    Failed,
}

pub struct TakerUpdates {
    feed: TakerFeed,
    request: Uuid,
}
impl TakerUpdates {
    pub async fn expect_acceptance_window(&mut self, closes_at: u64, milliseconds: u64) {
        match event(&mut self.feed).await {
            TakerEvent::Quote {
                rfq_id,
                accept_deadline,
                ..
            } => {
                assert_eq!(rfq_id, self.request);
                assert_eq!(accept_deadline - closes_at, milliseconds);
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    pub async fn expect_buy_quote(&mut self, price_each: u64) {
        self.expect_prices(Some(price_each), None).await;
    }
    pub async fn expect_two_way_quote(&mut self, buy_price: u64, sell_price: u64) {
        self.expect_prices(Some(buy_price), Some(sell_price)).await;
    }
    async fn expect_prices(&mut self, buy_price: Option<u64>, sell_price: Option<u64>) {
        match event(&mut self.feed).await {
            TakerEvent::Quote {
                rfq_id, ask, bid, ..
            } => {
                assert_eq!(rfq_id, self.request);
                assert_eq!(
                    ask.map(|quote| quote.price),
                    buy_price,
                    "wrong price for taker to buy"
                );
                assert_eq!(
                    bid.map(|quote| quote.price),
                    sell_price,
                    "wrong price for taker to sell"
                );
            }
            other => panic!("expected a quote, received {other:?}"),
        }
    }
    pub async fn expect(&mut self, expected: Notice) {
        let received = event(&mut self.feed).await;
        let (notice, request) = match &received {
            TakerEvent::Settled { rfq_id, .. } => (Notice::Settled, *rfq_id),
            TakerEvent::Declined { rfq_id } => (Notice::Declined, *rfq_id),
            TakerEvent::Expired { rfq_id } => (Notice::Expired, *rfq_id),
            TakerEvent::NoQuote { rfq_id } => (Notice::NoQuote, *rfq_id),
            TakerEvent::Failed { rfq_id } => (Notice::Failed, *rfq_id),
            _ => panic!("expected {expected:?}, received {received:?}"),
        };
        assert_eq!(
            std::mem::discriminant(&notice),
            std::mem::discriminant(&expected),
            "expected {expected:?}, received {received:?}"
        );
        assert_eq!(request, self.request);
    }
    pub async fn expect_clean_disconnect(&mut self) {
        assert!(
            timeout(Duration::from_secs(2), self.feed.next_event())
                .await
                .expect("socket should close promptly")
                .expect_accepted()
                .is_none(),
            "socket should close after the terminal event"
        );
    }
}

pub async fn next_maker_update(feed: &mut MakerFeed) -> MakerEvent {
    timeout(Duration::from_secs(2), feed.next_event())
        .await
        .expect("maker update should arrive promptly")
        .expect_accepted()
        .expect("maker feed should stay open")
}

pub fn expect_same_money(before: &BalancesResponse, after: &BalancesResponse) {
    assert_eq!(
        totals(before),
        totals(after),
        "cash and SOL must not appear or disappear"
    );
}

pub fn expect_nobody_traded(before: &BalancesResponse, after: &BalancesResponse) {
    assert_eq!(before.traders.len(), after.traders.len());
    for original in &before.traders {
        let current = after
            .traders
            .iter()
            .find(|trader| trader.pubkey == original.pubkey)
            .expect("trader disappeared");
        assert_eq!(
            original.usd.total, current.usd.total,
            "cash moved without a trade"
        );
        assert_eq!(
            original.assets["SOL"].total, current.assets["SOL"].total,
            "SOL moved without a trade"
        );
    }
}

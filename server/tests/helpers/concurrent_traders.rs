use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

impl LiveTrader {
    pub async fn accept_together(&self, request: Uuid, count: usize) -> usize {
        let mut tasks = vec![];
        for _ in 0..count {
            let trader = self.clone();
            tasks.push(tokio::spawn(
                async move { trader.accept_buy(request).await },
            ));
        }
        count_accepted(tasks).await
    }
    pub async fn sell_together(
        &self,
        request: Uuid,
        count: usize,
        coins: Coins,
        price: u64,
    ) -> usize {
        let mut tasks = vec![];
        for _ in 0..count {
            let trader = self.clone();
            tasks.push(tokio::spawn(async move {
                trader.offer_to_sell(request, coins, price).await
            }));
        }
        count_accepted(tasks).await
    }
}
async fn count_accepted<T>(tasks: Vec<JoinHandle<anyhow::Result<T>>>) -> usize {
    let mut accepted = 0;
    for task in tasks {
        if task.await.expect("request task should not panic").is_ok() {
            accepted += 1;
        }
    }
    accepted
}
impl TestServer {
    pub async fn send_together(&self, path: &str, signed: &Value, count: usize) -> Vec<HttpReply> {
        let gate = Arc::new(tokio::sync::Barrier::new(count + 1));
        let mut tasks = vec![];
        for _ in 0..count {
            let gate = gate.clone();
            let url = format!("{}{path}", self.world.url);
            let message = signed.clone();
            tasks.push(tokio::spawn(async move {
                gate.wait().await;
                let response = reqwest::Client::new()
                    .post(url)
                    .json(&message)
                    .send()
                    .await
                    .unwrap();
                HttpReply {
                    status: response.status().as_u16(),
                    body: response.json().await.unwrap(),
                }
            }));
        }
        gate.wait().await;
        let mut replies = vec![];
        for task in tasks {
            replies.push(task.await.expect("request task should not panic"));
        }
        replies
    }
    pub async fn wait_until_before_close(&self, request: Uuid, milliseconds: u64) {
        let closes_at = self.public_request(request).await["closes_at"]
            .as_u64()
            .unwrap();
        let remaining = closes_at.saturating_sub(common::signing::unix_millis());
        sleep(Duration::from_millis(
            remaining.saturating_sub(milliseconds),
        ))
        .await;
    }
}

pub struct SettlementRace {
    gate: Arc<tokio::sync::Barrier>,
    stop: Arc<AtomicBool>,
    readers: Vec<JoinHandle<usize>>,
    accepts: Vec<JoinHandle<Trade>>,
}
impl SettlementRace {
    pub async fn prepare(
        taker: &LiveTrader,
        requests: &[Uuid],
        observer: &LiveTrader,
        reader_count: usize,
    ) -> Self {
        let initial = totals(&observer.client.balances().await.expect_accepted());
        let gate = Arc::new(tokio::sync::Barrier::new(requests.len() + reader_count + 1));
        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = vec![];
        for _ in 0..reader_count {
            let client = observer.client.clone();
            let gate = gate.clone();
            let stop = stop.clone();
            readers.push(tokio::spawn(async move {
                gate.wait().await;
                let mut samples = 0;
                loop {
                    let balances = client.balances().await.expect_accepted();
                    assert_eq!(
                        totals(&balances),
                        initial,
                        "a balance reader saw money appear or disappear"
                    );
                    for trader in &balances.traders {
                        for balance in std::iter::once(&trader.usd).chain(trader.assets.values()) {
                            assert!(balance.reserved <= balance.total);
                            assert_eq!(balance.available + balance.reserved, balance.total);
                        }
                    }
                    samples += 1;
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                }
                samples
            }));
        }
        let mut accepts = vec![];
        for &request in requests {
            let trader = taker.clone();
            let gate = gate.clone();
            accepts.push(tokio::spawn(async move {
                gate.wait().await;
                timeout(Duration::from_secs(4), async {
                    loop {
                        match trader.accept_buy(request).await {
                            Ok(result) => break result.trade,
                            Err(error) => assert_eq!(code(error), "stale_nonce"),
                        }
                    }
                })
                .await
                .expect("acceptance should succeed within four seconds")
            }));
        }
        Self {
            gate,
            stop,
            readers,
            accepts,
        }
    }
    pub async fn start_together(&self) {
        self.gate.wait().await;
    }
    pub async fn collect_trades(&mut self) -> Vec<Trade> {
        let mut trades = vec![];
        for task in self.accepts.drain(..) {
            trades.push(task.await.expect("accept task should not panic"));
        }
        trades
    }
    pub async fn expect_every_reader_checked_balances(self) {
        self.stop.store(true, Ordering::SeqCst);
        for reader in self.readers {
            assert!(reader.await.expect("reader should not panic") > 0);
        }
    }
}

//! Black-box checks over actual TCP HTTP and WebSocket connections.
use common::{
    client::{ApiFailure, RfqClient, TakerFeed},
    seed::{AssetDef, SeedConfig, TraderSeed},
    signing::Envelope,
    types::*,
    Keypair,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use server::api::{router, App};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct World {
    url: String,
    clients: Vec<Arc<RfqClient>>,
    task: JoinHandle<()>,
}
impl Drop for World {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn world() -> World {
    let keys: Vec<_> = (1..=4).map(|i| Keypair::from_seed([i; 32])).collect();
    let seed = SeedConfig {
        assets: BTreeMap::from([("SOL".into(), AssetDef { lot_decimals: 2 })]),
        traders: keys
            .iter()
            .map(|k| TraderSeed {
                name: None,
                pubkey: k.pubkey(),
                usd: 1_000_000,
                assets: BTreeMap::from([("SOL".into(), 1000)]),
            })
            .collect(),
    };
    let app = router(App::new(seed, 2).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let clients = keys
        .into_iter()
        .map(|k| Arc::new(RfqClient::new(&url, k)))
        .collect();
    World { url, clients, task }
}
async fn submit(w: &World, direction: Option<Side>) -> SubmitRfqResponse {
    w.clients[0]
        .submit_rfq(SubmitRfqPayload {
            asset: "SOL".into(),
            direction,
            size: 100,
            ttl_secs: 1,
        })
        .await
        .unwrap()
}
async fn place(
    w: &World,
    maker: usize,
    id: uuid::Uuid,
    side: Side,
    price: u64,
) -> PlaceOrderResponse {
    w.clients[maker]
        .place_order(PlaceOrderPayload {
            rfq_id: id,
            side,
            price,
            size: 300,
            client_order_id: None,
        })
        .await
        .unwrap()
}
async fn event(feed: &mut TakerFeed) -> TakerEvent {
    timeout(Duration::from_secs(5), feed.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
fn code(error: anyhow::Error) -> String {
    error.downcast_ref::<ApiFailure>().unwrap().code.clone()
}
fn totals(b: &BalancesResponse) -> (u128, u128) {
    b.traders.iter().fold((0, 0), |(u, a), t| {
        (u + t.usd.total as u128, a + t.assets["SOL"].total as u128)
    })
}
fn no_holds(b: &BalancesResponse) {
    assert!(b
        .traders
        .iter()
        .all(|t| t.usd.reserved == 0 && t.assets["SOL"].reserved == 0));
}

#[tokio::test]
async fn full_directed_trade_stock_client_and_maker_events() {
    let w = world().await;
    w.clients[0].health().await.unwrap();
    assert_eq!(
        w.clients[0].assets().await.unwrap().assets[0].lot_decimals,
        2
    );
    let before = w.clients[0].balances().await.unwrap();
    let mut makers = w.clients[1].maker_feed().await.unwrap();
    let r = submit(&w, Some(Side::Buy)).await;
    assert!(matches!(
        makers.next_event().await.unwrap().unwrap(),
        MakerEvent::RfqOpened {
            direction: Some(Side::Buy),
            ..
        }
    ));
    let (mut feed, ack) = w.clients[0].subscribe_rfq(r.rfq_id).await.unwrap();
    assert!(matches!(
        ack,
        TakerEvent::Subscribed {
            state: RfqState::Open,
            ..
        }
    ));
    let winning = place(&w, 1, r.rfq_id, Side::Sell, 900).await;
    place(&w, 2, r.rfq_id, Side::Sell, 1000).await;
    place(&w, 1, r.rfq_id, Side::Buy, 800).await; // off-direction orders are allowed
    let view: Value = reqwest::get(format!("{}/v1/rfqs/{}", w.url, r.rfq_id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(view.get("direction").is_none());
    assert!(matches!(
        event(&mut feed).await,
        TakerEvent::Quote {
            bid: None,
            ask: Some(QuoteLevel { price: 900 }),
            ..
        }
    ));
    let trade = w.clients[0]
        .accept(r.rfq_id, Side::Buy)
        .await
        .unwrap()
        .trade;
    assert_eq!((trade.size, trade.notional_usd), (100, 900));
    assert!(matches!(event(&mut feed).await, TakerEvent::Settled { .. }));
    assert!(matches!(
        makers.next_event().await.unwrap().unwrap(),
        MakerEvent::RfqWindowClosed { .. }
    ));
    match makers.next_event().await.unwrap().unwrap() {
        MakerEvent::RfqDone {
            outcome: RfqOutcome::Settled,
            fills,
            ..
        } => {
            assert_eq!(fills.len(), 1);
            assert_eq!(fills[0].order_id, winning.order_id);
            assert_eq!(fills[0].size, 100);
        }
        other => panic!("unexpected {other:?}"),
    }
    let after = w.clients[0].balances().await.unwrap();
    assert_eq!(totals(&before), totals(&after));
    no_holds(&after);
    assert_eq!(
        code(w.clients[0].accept(r.rfq_id, Side::Buy).await.unwrap_err()),
        "not_awaiting_accept"
    );
}

#[tokio::test]
async fn undirected_sell_and_late_subscription_snapshot() {
    let w = world().await;
    let r = submit(&w, None).await;
    place(&w, 1, r.rfq_id, Side::Buy, 900).await;
    place(&w, 2, r.rfq_id, Side::Buy, 1100).await;
    place(&w, 1, r.rfq_id, Side::Sell, 1200).await;
    sleep(Duration::from_millis(1100)).await;
    let (mut feed, ack) = w.clients[0].subscribe_rfq(r.rfq_id).await.unwrap();
    assert!(matches!(
        ack,
        TakerEvent::Subscribed {
            state: RfqState::AwaitingAccept,
            ..
        }
    ));
    assert!(matches!(
        event(&mut feed).await,
        TakerEvent::Quote {
            bid: Some(QuoteLevel { price: 1100 }),
            ask: Some(QuoteLevel { price: 1200 }),
            ..
        }
    ));
    let trade = w.clients[0]
        .accept(r.rfq_id, Side::Sell)
        .await
        .unwrap()
        .trade;
    assert_eq!(trade.notional_usd, 1100);
    let (mut reconnect, _) = w.clients[0].subscribe_rfq(r.rfq_id).await.unwrap();
    assert!(matches!(
        event(&mut reconnect).await,
        TakerEvent::Settled { .. }
    ));
    no_holds(&w.clients[0].balances().await.unwrap());
}

#[tokio::test]
async fn empty_rfq_and_decline_emit_terminal_events() {
    let w = world().await;
    let r = submit(&w, None).await;
    let (mut feed, _) = w.clients[0].subscribe_rfq(r.rfq_id).await.unwrap();
    assert!(matches!(event(&mut feed).await, TakerEvent::NoQuote { .. }));
    assert!(matches!(event(&mut feed).await, TakerEvent::Expired { .. }));
    let r = submit(&w, None).await;
    place(&w, 1, r.rfq_id, Side::Sell, 1000).await;
    let (mut feed, _) = w.clients[0].subscribe_rfq(r.rfq_id).await.unwrap();
    assert!(matches!(event(&mut feed).await, TakerEvent::Quote { .. }));
    w.clients[0].decline(r.rfq_id).await.unwrap();
    assert!(matches!(
        event(&mut feed).await,
        TakerEvent::Declined { .. }
    ));
    no_holds(&w.clients[0].balances().await.unwrap());
}

#[tokio::test]
async fn expiration_without_requests_releases_holds() {
    let w = world().await;
    let r = submit(&w, Some(Side::Buy)).await;
    place(&w, 1, r.rfq_id, Side::Sell, 1000).await;
    let (mut feed, _) = w.clients[0].subscribe_rfq(r.rfq_id).await.unwrap();
    assert!(matches!(event(&mut feed).await, TakerEvent::Quote { .. }));
    assert!(matches!(event(&mut feed).await, TakerEvent::Expired { .. }));
    assert_eq!(
        code(w.clients[0].accept(r.rfq_id, Side::Buy).await.unwrap_err()),
        "not_awaiting_accept"
    );
    no_holds(&w.clients[0].balances().await.unwrap());
}

#[tokio::test]
async fn malformed_and_signed_error_contract() {
    let w = world().await;
    let http = reqwest::Client::new();
    for (method, path, body) in [
        ("POST", "/v1/rfqs", Some("{")),
        ("POST", "/v1/orders", Some("{}")),
        ("GET", "/v1/rfqs/not-a-uuid", None),
        ("GET", "/v1/rfqs/%FF", None),
        ("GET", "/ws/rfq/%FF", None),
        ("GET", "/v1/balances/not-a-key", None),
        ("GET", "/unknown", None),
        ("DELETE", "/v1/rfqs", None),
    ] {
        let mut request = http.request(method.parse().unwrap(), format!("{}{path}", w.url));
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<ApiError>().await.unwrap().error.code,
            "malformed_request"
        );
    }
    let p = SubmitRfqPayload {
        asset: "SOL".into(),
        direction: None,
        size: 100,
        ttl_secs: 1,
    };
    let env = w.clients[0].envelope(p.clone());
    let url = format!("{}/v1/rfqs", w.url);
    assert!(http
        .post(&url)
        .json(&env)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let replay = http.post(&url).json(&env).send().await.unwrap();
    assert_eq!(replay.status(), 401);
    assert_eq!(
        replay.json::<ApiError>().await.unwrap().error.code,
        "stale_nonce"
    );
    let mut tampered = w.clients[0].envelope(p.clone());
    tampered.payload.size += 1;
    assert_eq!(
        http.post(&url)
            .json(&tampered)
            .send()
            .await
            .unwrap()
            .json::<ApiError>()
            .await
            .unwrap()
            .error
            .code,
        "invalid_signature"
    );
    let stranger = Envelope::sign(&Keypair::from_seed([99; 32]), 1, p.clone());
    assert_eq!(
        http.post(&url)
            .json(&stranger)
            .send()
            .await
            .unwrap()
            .json::<ApiError>()
            .await
            .unwrap()
            .error
            .code,
        "unknown_trader"
    );
    assert_eq!(
        code(
            w.clients[0]
                .submit_rfq(SubmitRfqPayload {
                    asset: "BAD".into(),
                    size: 0,
                    ttl_secs: 0,
                    ..p
                })
                .await
                .unwrap_err()
        ),
        "unknown_asset"
    );
}

#[tokio::test]
async fn websocket_rejects_wrong_owner_path_and_replay() {
    let w = world().await;
    let r = submit(&w, None).await;
    assert!(w.clients[1].subscribe_rfq(r.rfq_id).await.is_err());
    let ws_url = format!("{}/ws/rfq/{}", w.url.replace("http://", "ws://"), r.rfq_id);
    let env = w.clients[0].envelope(SubscribePayload { rfq_id: r.rfq_id });
    for expected_error in [false, true] {
        let (mut socket, _) = connect_async(&ws_url).await.unwrap();
        socket
            .send(Message::Text(serde_json::to_string(&env).unwrap()))
            .await
            .unwrap();
        let frame = timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let event: TakerEvent = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(matches!(event, TakerEvent::Error { .. }), expected_error);
    }
    let (mut socket, _) = connect_async(&ws_url).await.unwrap();
    let env = w.clients[0].envelope(SubscribePayload {
        rfq_id: uuid::Uuid::new_v4(),
    });
    socket
        .send(Message::Text(serde_json::to_string(&env).unwrap()))
        .await
        .unwrap();
    let frame = socket.next().await.unwrap().unwrap();
    let event: TakerEvent = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert!(matches!(event, TakerEvent::Error { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_accepts_settle_once_and_competing_orders_cannot_overspend() {
    let w = world().await;
    let before = w.clients[0].balances().await.unwrap();
    let r = submit(&w, Some(Side::Buy)).await;
    place(&w, 1, r.rfq_id, Side::Sell, 1000).await;
    let (mut feed, _) = w.clients[0].subscribe_rfq(r.rfq_id).await.unwrap();
    event(&mut feed).await;
    let mut handles = vec![];
    for _ in 0..20 {
        let client = w.clients[0].clone();
        handles.push(tokio::spawn(async move {
            client.accept(r.rfq_id, Side::Buy).await
        }));
    }
    let mut succeeded = 0;
    for h in handles {
        if h.await.unwrap().is_ok() {
            succeeded += 1;
        }
    }
    assert_eq!(succeeded, 1);
    assert_eq!(
        totals(&before),
        totals(&w.clients[0].balances().await.unwrap())
    );
    no_holds(&w.clients[0].balances().await.unwrap());

    let r = w.clients[0]
        .submit_rfq(SubmitRfqPayload {
            asset: "SOL".into(),
            direction: None,
            size: 600,
            ttl_secs: 2,
        })
        .await
        .unwrap();
    let mut handles = vec![];
    for _ in 0..20 {
        let maker = w.clients[2].clone();
        handles.push(tokio::spawn(async move {
            maker
                .place_order(PlaceOrderPayload {
                    rfq_id: r.rfq_id,
                    side: Side::Sell,
                    price: 1000,
                    size: 600,
                    client_order_id: None,
                })
                .await
        }));
    }
    let mut succeeded = 0;
    for h in handles {
        if h.await.unwrap().is_ok() {
            succeeded += 1;
        }
    }
    assert_eq!(succeeded, 1);
    assert_eq!(
        w.clients[2]
            .balances_of(&w.clients[2].pubkey())
            .await
            .unwrap()
            .assets["SOL"]
            .reserved,
        600
    );
}

#[tokio::test]
async fn ownership_validation_and_cancel_by_client_id() {
    let w = world().await;
    let r = submit(&w, None).await;
    let payload = PlaceOrderPayload {
        rfq_id: r.rfq_id,
        side: Side::Sell,
        price: 1000,
        size: 100,
        client_order_id: Some(42),
    };
    assert_eq!(
        code(w.clients[0].place_order(payload.clone()).await.unwrap_err()),
        "self_quote_not_allowed"
    );
    let o = w.clients[1].place_order(payload.clone()).await.unwrap();
    assert_eq!(
        code(w.clients[1].place_order(payload).await.unwrap_err()),
        "duplicate_client_order_id"
    );
    assert_eq!(
        code(
            w.clients[2]
                .cancel_order(CancelOrderPayload {
                    rfq_id: r.rfq_id,
                    order_id: Some(o.order_id),
                    client_order_id: None
                })
                .await
                .unwrap_err()
        ),
        "not_your_order"
    );
    w.clients[1]
        .cancel_order(CancelOrderPayload {
            rfq_id: r.rfq_id,
            order_id: None,
            client_order_id: Some(42),
        })
        .await
        .unwrap();
    no_holds(&w.clients[0].balances().await.unwrap());
    assert_eq!(
        code(w.clients[2].accept(r.rfq_id, Side::Buy).await.unwrap_err()),
        "not_your_rfq"
    );
    let response = reqwest::Client::new()
        .post(format!("{}/v1/orders", w.url))
        .json(&json!({"pubkey": "oops"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

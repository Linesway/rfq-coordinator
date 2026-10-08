use super::*;

engine_tests! {
    #[test]
    fn no_quote_reconnect_and_decline_cleanup() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(1_000_000), sol(10));
        let empty_request = taker.ask_for_both_prices(sol(1));
        exchange.close_quoting(empty_request);
        taker.expect_updates(empty_request, &["subscribed", "no_quote", "expired"]);

        let request = taker.ask_for_both_prices(sol(1));
        maker
            .offer_to_buy(request, sol(3), micro_usd(1000))
            .expect_accepted();
        maker
            .offer_to_sell(request, sol(3), micro_usd(1100))
            .expect_accepted();
        exchange.close_quoting(request);
        taker.expect_updates(request, &["subscribed", "quote"]);

        taker.decline(request).expect_accepted();
        exchange.advance_to(3000);

        exchange.expect_state(request, RfqState::Declined);
        exchange.expect_money_conserved();
    }

    #[test]
    fn lagging_broadcast_does_not_block_state_or_settlement() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(1_000_000), sol(10));
        let mut slow_feed = exchange.watch_makers();
        for _ in 0..1100 {
            taker.ask_for_both_prices(sol(1));
        }

        let request = taker.ask_to_buy(sol(1));
        maker
            .offer_to_sell(request, sol(3), micro_usd(1000))
            .expect_accepted();
        exchange.close_quoting(request);
        taker.accept_buy(request).expect_accepted();

        assert!(matches!(
            slow_feed.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
        exchange.expect_money_conserved();
    }
}

client_tests! {
    #[tokio::test]
    async fn websocket_rejects_wrong_owner_path_and_replay() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let request = alice.ask_for_both_prices(sol(1)).await;

        assert!(bob.client.subscribe_rfq(request).await.is_err());
        let subscription = alice.auto_sign(subscribe_message(request));
        let mut first = exchange.taker_socket(request).await;
        first.send_json(&subscription).await;
        first.expect_subscribed(request).await;
        let mut replay = exchange.taker_socket(request).await;
        replay.send_json(&subscription).await;
        replay.expect_error("stale_nonce", 2).await;

        let wrong_request = alice.auto_sign(subscribe_message(uuid::Uuid::new_v4()));
        let mut socket = exchange.taker_socket(request).await;
        socket.send_json(&wrong_request).await;
        socket.expect_error("malformed_request", 2).await;
    }

    #[tokio::test]
    async fn malformed_first_frames_and_unknown_rfqs_return_errors_without_harming_server() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let request = alice.ask_for_both_prices(sol(1)).await;
        for frame in [
            Message::Text("{".into()),
            Message::Text("{}".into()),
            Message::Binary(vec![1, 2, 3]),
        ] {
            let mut socket = exchange.taker_socket(request).await;
            socket.send(frame).await;
            socket.expect_error("malformed_request", 2).await;
        }
        let missing_request = uuid::Uuid::new_v4();
        let mut socket = exchange.taker_socket(missing_request).await;
        socket
            .send_json(&alice.auto_sign(subscribe_message(missing_request)))
            .await;
        socket.expect_error("rfq_not_found", 2).await;
        exchange.expect_healthy().await;
    }

    #[tokio::test]
    async fn reconnect_after_decline_returns_snapshot_and_terminal_event() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let request = alice.ask_for_both_prices(sol(1)).await;
        bob.offer_to_sell(request, sol(3), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;

        alice.decline(request).await.expect_accepted();
        let mut reconnected = alice.watch(request, RfqState::Declined).await;
        reconnected.expect(Notice::Declined).await;
        reconnected.expect_clean_disconnect().await;
    }

    #[tokio::test]
    async fn maker_feed_is_live_only_and_undirected_rfq_omits_direction() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let old_request = alice.ask_to_buy(sol(1)).await;
        let mut maker = exchange.maker_socket().await;
        let new_request = alice.ask_for_both_prices(sol(1)).await;

        let announcement = maker.next_json().await;
        assert_eq!(announcement["rfq_id"], new_request.to_string());
        assert_ne!(announcement["rfq_id"], old_request.to_string());
        assert!(announcement.get("direction").is_none());
        maker.send(Message::Ping(vec![1, 2, 3])).await;
        maker.expect_pong(&[1, 2, 3]).await;
    }

    #[tokio::test]
    async fn silent_subscriber_times_out_and_closes_cleanly() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let request = exchange.trader("Alice").ask_for_both_prices(sol(1)).await;
        let mut silent_client = exchange.taker_socket(request).await;

        silent_client.expect_error("malformed_request", 7).await;
        silent_client.expect_closed().await;
        exchange.expect_healthy().await;
    }
}

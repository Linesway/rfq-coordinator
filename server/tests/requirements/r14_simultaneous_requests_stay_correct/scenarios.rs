use super::*;

client_tests! {
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_accepts_settle_once_and_competing_orders_cannot_overspend() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .trader("Carol", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let carol = exchange.trader("Carol");
        let request = alice.ask_to_buy(sol(1)).await;
        bob.offer_to_sell(request, sol(3), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;

        assert_eq!(alice.accept_together(request, 20).await, 1);
        exchange.expect_money_conserved().await;
        exchange.expect_funds_released().await;

        let next_request = alice
            .submit(both_prices(sol(6)).quoting_for(2))
            .await
            .expect_accepted()
            .rfq_id;
        assert_eq!(
            carol
                .sell_together(next_request, 20, sol(6), micro_usd(1000))
                .await,
            1
        );
        carol.expect_reserved_sol(6).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn identical_signed_requests_racing_create_exactly_one_rfq() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let signed = alice.sign_request(both_prices(sol(1)), 0);

        let replies = exchange.send_together("/v1/rfqs", &signed, 24).await;

        assert_eq!(
            replies.iter().filter(|reply| reply.status == 200).count(),
            1
        );
        for rejected in replies.iter().filter(|reply| reply.status != 200) {
            rejected.expect_error(401, "stale_nonce");
        }
        exchange.expect_funds_released().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn accept_and_decline_race_has_one_terminal_outcome() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let request = alice.ask_to_buy(sol(1)).await;
        bob.offer_to_sell(request, sol(3), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;
        let accept = alice.auto_sign(accept_message(request, Side::Buy));
        let decline = alice.auto_sign(decline_message(request));

        let (accepted, declined) = tokio::join!(
            exchange.send("/v1/accept", &accept),
            exchange.send("/v1/decline", &decline)
        );

        assert_ne!(accepted.status == 200, declined.status == 200);
        if accepted.status == 200 {
            updates.expect(Notice::Settled).await;
            alice
                .expect_unreserved_balance(micro_usd(999_000), sol(11))
                .await;
        } else {
            updates.expect(Notice::Declined).await;
            alice.expect_unreserved_balance(dollars(1), sol(10)).await;
        }
        exchange.expect_money_conserved().await;
        exchange.expect_funds_released().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancellation_racing_close_cannot_release_a_winning_quote() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let request = alice.ask_to_buy(sol(1)).await;
        let offer = bob
            .offer_to_sell(request, sol(3), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;

        exchange.wait_until_before_close(request, 1).await;
        let cancellation = bob.cancel(request, &offer).await;

        if cancellation.is_ok() {
            updates.expect(Notice::NoQuote).await;
            updates.expect(Notice::Expired).await;
        } else {
            cancellation.expect_rejected("rfq_not_open");
            updates.expect_buy_quote(micro_usd(1000)).await;
            alice.accept_buy(request).await.expect_accepted();
            updates.expect(Notice::Settled).await;
        }
        exchange.expect_funds_released().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn balance_readers_never_observe_partial_settlement_during_many_accepts() {
        let exchange = TestServer::setup()
            .acceptance_window(5)
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .trader("Carol", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let observer = exchange.trader("Carol");
        let mut requests = vec![];
        let mut subscriptions = vec![];
        for _ in 0..24 {
            let request = alice
                .submit(buy(lots("SOL", 10)).quoting_for(3))
                .await
                .expect_accepted()
                .rfq_id;
            bob.offer_to_sell(request, lots("SOL", 10), micro_usd(1000))
                .await
                .expect_accepted();
            subscriptions.push(alice.watch(request, RfqState::Open).await);
            requests.push(request);
        }
        for updates in &mut subscriptions {
            updates.expect_buy_quote(micro_usd(1000)).await;
        }

        let mut race = SettlementRace::prepare(&alice, &requests, &observer, 3).await;
        race.start_together().await;
        let trades = race.collect_trades().await;
        race.expect_every_reader_checked_balances().await;

        let unique_ids: std::collections::HashSet<_> =
            trades.iter().map(|trade| trade.trade_id).collect();
        assert_eq!(unique_ids.len(), 24);
        for trade in trades {
            assert_eq!((trade.size, trade.notional_usd), (10, 100));
        }
        alice
            .expect_unreserved_balance(micro_usd(997_600), lots("SOL", 1240))
            .await;
        exchange.expect_money_conserved().await;
        exchange.expect_funds_released().await;
    }

    #[tokio::test]
    async fn disconnecting_clients_does_not_cancel_reserved_offers_or_quotes() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let maker_feed = bob.maker_feed().await;
        let request = alice.ask_to_buy(sol(1)).await;
        bob.offer_to_sell(request, sol(3), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;

        drop(maker_feed);
        drop(updates);
        alice.expect_reserved_cash(micro_usd(1000)).await;
        let mut reconnected = alice.watch(request, RfqState::AwaitingAccept).await;
        reconnected.expect_buy_quote(micro_usd(1000)).await;
        alice.accept_buy(request).await.expect_accepted();

        reconnected.expect(Notice::Settled).await;
        exchange.expect_funds_released().await;
    }

    #[tokio::test]
    async fn rejected_owner_actions_leave_balances_orders_and_quote_unchanged() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .trader("Carol", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let carol = exchange.trader("Carol");
        let request = alice.ask_to_buy(sol(1)).await;
        let offer = bob
            .offer_to_sell(request, sol(3), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;
        let before = exchange.balances().await;

        carol
            .accept_buy(request)
            .await
            .expect_rejected("not_your_rfq");
        carol.decline(request).await.expect_rejected("not_your_rfq");
        bob.cancel(request, &offer)
            .await
            .expect_rejected("rfq_not_open");
        alice
            .accept_sell(request)
            .await
            .expect_rejected("side_unavailable");

        exchange.expect_balances(&before).await;
        let mut reconnected = alice.watch(request, RfqState::AwaitingAccept).await;
        reconnected.expect_buy_quote(micro_usd(1000)).await;
        alice.accept_buy(request).await.expect_accepted();
        exchange.expect_funds_released().await;
    }
}

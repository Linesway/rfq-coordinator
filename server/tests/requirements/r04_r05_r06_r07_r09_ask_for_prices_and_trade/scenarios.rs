use super::*;

engine_tests! {
    #[test]
    fn cheapest_seller_wins_and_equal_prices_use_the_first_offer() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(1000), sol(0));
        let first_maker = exchange.add_trader("Bob", dollars(1000), sol(10));
        let second_maker = exchange.add_trader("Carol", dollars(0), sol(10));
        let request = taker.ask_to_buy(sol(2));

        let first_offer = first_maker
            .offer_to_sell(request, sol(6), dollars_per_sol(150))
            .expect_accepted();
        second_maker
            .offer_to_sell(request, sol(6), dollars_per_sol(150))
            .expect_accepted();
        second_maker
            .offer_to_sell(request, sol(6), dollars_per_sol(160))
            .expect_accepted();
        first_maker
            .offer_to_buy(request, sol(6), dollars_per_sol(200))
            .expect_accepted();
        exchange.close_quoting(request);

        exchange.expect_winning_sell_offer(request, &first_offer);
        exchange.expect_no_sell_quote(request);
        second_maker.expect_reserved_sol(0);
        first_maker.expect_reserved_sol(2);
        taker.expect_reserved_cash(dollars(300));

        let purchase = taker.accept_buy(request).expect_accepted();
        assert_eq!(purchase.trade.size, 2);
        assert_eq!(purchase.trade.notional_usd, dollars(300));
        taker.expect_total_sol(2);
        taker.expect_cash(dollars(700));
        first_maker.expect_total_sol(8);
        first_maker.expect_cash(dollars(1300));
        taker
            .accept_buy(request)
            .expect_rejected("not_awaiting_accept");
        exchange.expect_funds_released();
    }

    #[test]
    fn highest_buyer_wins_and_the_unused_buy_option_is_released() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(1000), sol(2));
        let first_maker = exchange.add_trader("Bob", dollars(1000), sol(2));
        let second_maker = exchange.add_trader("Carol", dollars(1000), sol(0));
        let request = taker.ask_for_both_prices(sol(2));

        first_maker
            .offer_to_buy(request, sol(2), dollars_per_sol(140))
            .expect_accepted();
        let best_offer = second_maker
            .offer_to_buy(request, sol(2), dollars_per_sol(150))
            .expect_accepted();
        first_maker
            .offer_to_sell(request, sol(2), dollars_per_sol(160))
            .expect_accepted();
        exchange.close_quoting(request);

        exchange.expect_winning_buy_offer(request, &best_offer);
        taker.expect_reserved_cash(dollars(320));
        taker.expect_reserved_sol(2);
        taker.accept_sell(request).expect_accepted();
        taker.expect_cash(dollars(1300));
        taker.expect_total_sol(0);
        second_maker.expect_cash(dollars(700));
        second_maker.expect_total_sol(2);
        exchange.expect_funds_released();
    }
}

client_tests! {
    #[tokio::test]
    async fn taker_buys_two_sol_and_both_clients_hear_the_result() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1000), sol(10))
            .trader("Bob", dollars(1000), sol(10))
            .trader("Carol", dollars(1000), sol(10))
            .start()
            .await;
        let taker = exchange.trader("Alice");
        let maker = exchange.trader("Bob");
        let competitor = exchange.trader("Carol");
        exchange.expect_healthy().await;
        assert_eq!(
            taker.client.assets().await.expect_accepted().assets[0].lot_decimals,
            2
        );
        let mut maker_updates = maker.maker_feed().await;

        let request = taker.ask_to_buy(sol(2)).await;
        expect_maker_opened(&mut maker_updates, request, Some(Side::Buy)).await;
        let mut updates = taker.watch(request, RfqState::Open).await;
        let winning_offer = maker
            .offer_to_sell(request, sol(6), dollars_per_sol(150))
            .await
            .expect_accepted();
        competitor
            .offer_to_sell(request, sol(6), dollars_per_sol(160))
            .await
            .expect_accepted();
        maker
            .offer_to_buy(request, sol(6), dollars_per_sol(140))
            .await
            .expect_accepted();
        assert!(exchange
            .public_request(request)
            .await
            .get("direction")
            .is_none());
        updates.expect_buy_quote(dollars_per_sol(150)).await;

        let purchase = taker.accept_buy(request).await.expect_accepted();

        assert_eq!(purchase.trade.size, 200);
        assert_eq!(purchase.trade.notional_usd, dollars(300));
        updates.expect(Notice::Settled).await;
        expect_maker_closed(&mut maker_updates, request).await;
        expect_maker_filled(&mut maker_updates, request, &winning_offer, 200).await;
        taker.expect_cash(dollars(700)).await;
        taker.expect_total_sol(12).await;
        maker.expect_cash(dollars(1300)).await;
        maker.expect_total_sol(8).await;
        exchange.expect_money_conserved().await;
        exchange.expect_funds_released().await;
        taker
            .accept_buy(request)
            .await
            .expect_rejected("not_awaiting_accept");
    }

    #[tokio::test]
    async fn taker_can_join_late_sell_at_the_best_price_and_reconnect_afterward() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1000), sol(10))
            .trader("Bob", dollars(1000), sol(10))
            .trader("Carol", dollars(1000), sol(10))
            .start()
            .await;
        let taker = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let carol = exchange.trader("Carol");
        let request = taker.ask_for_both_prices(sol(2)).await;
        bob.offer_to_buy(request, sol(2), dollars_per_sol(140))
            .await
            .expect_accepted();
        carol
            .offer_to_buy(request, sol(2), dollars_per_sol(150))
            .await
            .expect_accepted();
        bob.offer_to_sell(request, sol(2), dollars_per_sol(160))
            .await
            .expect_accepted();

        exchange.wait_until_quoted(request).await;
        let mut updates = taker.watch(request, RfqState::AwaitingAccept).await;
        updates
            .expect_two_way_quote(dollars_per_sol(160), dollars_per_sol(150))
            .await;
        let sale = taker.accept_sell(request).await.expect_accepted();

        assert_eq!(sale.trade.notional_usd, dollars(300));
        taker.expect_cash(dollars(1300)).await;
        taker.expect_total_sol(8).await;
        let mut reconnected = taker.watch(request, RfqState::Settled).await;
        reconnected.expect(Notice::Settled).await;
        exchange.expect_funds_released().await;
    }

    #[tokio::test]
    async fn no_offers_means_no_trade_and_declining_also_means_no_trade() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1000), sol(10))
            .trader("Bob", dollars(1000), sol(10))
            .start()
            .await;
        let taker = exchange.trader("Alice");
        let maker = exchange.trader("Bob");
        let empty_request = taker.ask_for_both_prices(sol(2)).await;
        let mut updates = taker.watch(empty_request, RfqState::Open).await;
        updates.expect(Notice::NoQuote).await;
        updates.expect(Notice::Expired).await;

        let request = taker.ask_for_both_prices(sol(2)).await;
        maker
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .await
            .expect_accepted();
        let mut updates = taker.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(dollars_per_sol(150)).await;
        taker.decline(request).await.expect_accepted();

        updates.expect(Notice::Declined).await;
        exchange.expect_nobody_traded().await;
        exchange.expect_funds_released().await;
    }

    #[tokio::test]
    async fn doing_nothing_until_the_quote_expires_frees_everyones_funds() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1000), sol(10))
            .trader("Bob", dollars(1000), sol(10))
            .start()
            .await;
        let taker = exchange.trader("Alice");
        let maker = exchange.trader("Bob");
        let request = taker.ask_to_buy(sol(2)).await;
        maker
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .await
            .expect_accepted();
        let mut updates = taker.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(dollars_per_sol(150)).await;

        updates.expect(Notice::Expired).await;

        taker
            .accept_buy(request)
            .await
            .expect_rejected("not_awaiting_accept");
        exchange.expect_nobody_traded().await;
        exchange.expect_funds_released().await;
    }

    #[tokio::test]
    async fn seller_with_a_full_cash_balance_cannot_receive_more_and_nobody_loses_money() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1000), sol(10))
            .trader("Bob", micro_usd(u64::MAX), sol(10))
            .start()
            .await;
        let taker = exchange.trader("Alice");
        let maker = exchange.trader("Bob");
        let mut maker_updates = maker.maker_feed().await;
        let request = taker.ask_to_buy(sol(2)).await;
        maker
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .await
            .expect_accepted();
        let mut updates = taker.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(dollars_per_sol(150)).await;

        taker
            .accept_buy(request)
            .await
            .expect_rejected("insufficient_balance");

        updates.expect(Notice::Failed).await;
        updates.expect_clean_disconnect().await;
        expect_maker_failed(&mut maker_updates).await;
        exchange.expect_money_conserved().await;
        exchange.expect_nobody_traded().await;
        exchange.expect_funds_released().await;
    }

    #[tokio::test]
    async fn taker_with_no_cash_or_sol_gets_no_quote_even_when_makers_offer() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(0), sol(0))
            .trader("Bob", dollars(1000), sol(10))
            .trader("Carol", dollars(1000), sol(10))
            .start()
            .await;
        let taker = exchange.trader("Alice");
        let request = taker.ask_for_both_prices(sol(2)).await;
        exchange
            .trader("Bob")
            .offer_to_buy(request, sol(2), dollars_per_sol(140))
            .await
            .expect_accepted();
        exchange
            .trader("Carol")
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .await
            .expect_accepted();
        let mut updates = taker.watch(request, RfqState::Open).await;

        updates.expect(Notice::NoQuote).await;
        updates.expect(Notice::Expired).await;
        exchange.expect_funds_released().await;
    }
}

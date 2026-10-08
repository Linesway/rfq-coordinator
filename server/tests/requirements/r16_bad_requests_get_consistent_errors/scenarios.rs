use super::*;

engine_tests! {
    #[test]
    fn submit_validation_order_and_boundaries_leave_no_state_on_failure() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let before = exchange.balances();

        for (asset, size, ttl, error) in [
            ("BAD", 0, 0, "unknown_asset"),
            ("SOL", 0, 0, "invalid_size"),
            ("SOL", 1, 0, "invalid_ttl"),
            ("SOL", 1, 30, "invalid_ttl"),
            ("SOL", 1, u64::MAX, "invalid_ttl"),
        ] {
            taker
                .submit(both_prices(lots(asset, size)).quoting_for(ttl))
                .expect_rejected(error);
            exchange.expect_no_requests();
            exchange.expect_balances(&before);
        }
        for ttl in [1, 29] {
            let request = taker
                .submit(both_prices(lots("SOL", 1)).quoting_for(ttl))
                .expect_accepted();
            assert_eq!(request.closes_at, ttl * 1000);
        }

        exchange.advance_to(u64::MAX - 500);
        taker
            .submit(both_prices(lots("SOL", 1)).quoting_for(1))
            .expect_rejected("invalid_ttl");
    }

    #[test]
    fn order_validation_precedence_and_rejected_orders_do_not_reserve() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(1_000_000), sol(10));
        let request = taker.ask_for_both_prices(sol(1));
        let before = exchange.balances();

        for (sender, rfq, size, price, error) in [
            (&maker, Uuid::nil(), 0, 1000, "rfq_not_found"),
            (&taker, request, 0, 1000, "self_quote_not_allowed"),
            (&maker, request, 0, 0, "invalid_size"),
            (&maker, request, 99, 0, "order_too_small"),
            (&maker, request, 100, 0, "malformed_request"),
        ] {
            sender
                .offer_to_sell(rfq, lots("SOL", size), micro_usd(price))
                .expect_rejected(error);
            exchange.expect_balances(&before);
            exchange.expect_no_offers();
        }
        exchange.close_quoting(request);
        taker
            .offer_to_sell(request, lots("SOL", 0), micro_usd(1000))
            .expect_rejected("rfq_not_open");
    }

    #[test]
    fn cancel_validation_does_not_release_someone_elses_funds() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(1_000_000), sol(10));
        let stranger = exchange.add_trader("Carol", micro_usd(1_000_000), sol(10));
        let request = taker.ask_for_both_prices(sol(1));
        let other_request = taker.ask_for_both_prices(sol(1));
        let offer = maker
            .numbered_sell_offer(request, sol(3), micro_usd(1000), 1)
            .expect_accepted();
        let before = exchange.balances();

        maker
            .cancel_identifiers(request, None, None)
            .expect_rejected("malformed_request");
        exchange.expect_balances(&before);
        maker
            .cancel_identifiers(request, Some(offer.order_id), Some(1))
            .expect_rejected("malformed_request");
        exchange.expect_balances(&before);
        maker
            .cancel(other_request, &offer)
            .expect_rejected("order_not_found");
        exchange.expect_balances(&before);
        stranger
            .cancel(request, &offer)
            .expect_rejected("not_your_order");
        exchange.expect_balances(&before);
        stranger
            .cancel_number(request, 1)
            .expect_rejected("order_not_found");
        exchange.expect_balances(&before);

        exchange.close_quoting(request);
        maker
            .cancel(request, &offer)
            .expect_rejected("rfq_not_open");
        exchange.expect_winning_sell_offer(request, &offer);
        maker.expect_reserved_sol(1);
    }

    #[test]
    fn unavailable_side_and_wrong_owner_leave_firm_quote_untouched() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(1_000_000), sol(10));
        let stranger = exchange.add_trader("Carol", micro_usd(1_000_000), sol(10));
        let request = taker.ask_to_buy(sol(1));
        maker
            .offer_to_sell(request, sol(3), micro_usd(1000))
            .expect_accepted();
        taker
            .accept_buy(request)
            .expect_rejected("not_awaiting_accept");
        exchange.close_quoting(request);
        let before = exchange.balances();

        taker
            .accept_sell(request)
            .expect_rejected("side_unavailable");
        stranger.decline(request).expect_rejected("not_your_rfq");

        exchange.expect_balances(&before);
        exchange.expect_state(request, RfqState::AwaitingAccept);
        taker.accept_buy(request).expect_accepted();
    }

    #[test]
    fn seed_validation_missing_assets_and_accept_window_overflow() {
        let seed = TestSeed::empty()
            .asset("SOL", 2)
            .trader("Alice", 1, micro_usd(7), &[]);
        seed.expect_sol_balance(1, 0);

        let duplicate_trader = seed.clone().trader("Alice again", 1, micro_usd(7), &[]);
        assert!(duplicate_trader.start(1).is_err());
        let unknown_asset = seed.clone().holding(1, "UNKNOWN", 1);
        assert!(unknown_asset.start(1).is_err());
        for seconds in [u64::MAX, u64::MAX / 1000] {
            assert!(seed.start(seconds).is_err());
        }
    }
}

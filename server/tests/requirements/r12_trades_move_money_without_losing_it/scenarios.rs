use super::*;

engine_tests! {
    #[test]
    fn rounding_and_overflows_do_not_destroy_balances() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(1_000_000), sol(10));
        let request = taker.ask_for_both_prices(lots("SOL", 3));
        maker
            .offer_to_sell(request, sol(3), micro_usd(101))
            .expect_accepted();
        exchange.close_quoting(request);
        exchange.advance_to(1100);

        let purchase = taker.accept_buy(request).expect_accepted();
        assert_eq!(purchase.trade.notional_usd, micro_usd(3));

        exchange.advance_to(2000);
        let huge_request = taker.ask_for_both_prices(lots("SOL", u64::MAX));
        maker
            .offer_to_buy(huge_request, lots("SOL", u64::MAX), micro_usd(u64::MAX))
            .expect_rejected("invalid_size");

        taker.set_sol_balance_for_test(lots("SOL", u64::MAX));
        let overflow_request = taker.ask_to_buy(sol(1));
        maker
            .offer_to_sell(overflow_request, sol(3), micro_usd(1000))
            .expect_accepted();
        exchange.close_quoting(overflow_request);
        exchange.advance_to(3100);
        taker
            .accept_buy(overflow_request)
            .expect_rejected("insufficient_balance");

        exchange.expect_state(overflow_request, RfqState::Failed);
        exchange.expect_money_conserved();
    }

    #[test]
    fn malformed_seed_precision_is_rejected() {
        let seed = TestSeed::empty().asset("X", 39);
        assert!(seed.start(1).is_err());
    }

    #[test]
    fn notional_boundaries_match_exact_integer_examples() {
        for (decimals, size, price, expected_payment) in [
            (0, 1, 1, Some(1)),
            (2, 3, 101, Some(3)),
            (2, 1, 1, Some(0)),
            (0, u64::MAX, 1, Some(u64::MAX)),
            (0, u64::MAX, 2, None),
            (19, u64::MAX, u64::MAX, None),
            (38, u64::MAX, u64::MAX, Some(3)),
            (38, 1, u64::MAX, Some(0)),
        ] {
            let exchange = TestExchange::with_sol_decimals(decimals);
            let taker = exchange.add_trader("Alice", micro_usd(1_000_000), lots("SOL", 1000));
            let request = taker.ask_for_both_prices(lots("SOL", size));
            let payment = exchange.payment(request, micro_usd(price));

            match expected_payment {
                Some(expected) => assert_eq!(payment.expect_accepted(), expected),
                None => payment.expect_rejected("invalid_size"),
            }
        }
    }

    #[test]
    fn seller_cash_overflow_fails_without_partial_transfer_and_releases_holds() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(u64::MAX), sol(10));
        let request = taker.ask_to_buy(sol(1));
        maker
            .numbered_sell_offer(request, sol(3), micro_usd(1000), 8)
            .expect_accepted();
        exchange.close_quoting(request);

        taker
            .accept_buy(request)
            .expect_rejected("insufficient_balance");

        exchange.expect_state(request, RfqState::Failed);
        exchange.expect_funds_released();
        exchange.expect_no_offer_numbers_in_use();
        taker.expect_updates(request, &["subscribed", "failed"]);
    }

    #[test]
    fn zero_payment_trade_moves_only_assets_and_cleans_up() {
        let exchange = TestExchange::with_sol_decimals(2);
        let taker = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let maker = exchange.add_trader("Bob", micro_usd(1_000_000), sol(10));
        let request = taker.ask_to_buy(lots("SOL", 1));
        maker
            .offer_to_sell(request, sol(3), micro_usd(1))
            .expect_accepted();
        exchange.close_quoting(request);

        let purchase = taker.accept_buy(request).expect_accepted();

        assert_eq!(purchase.trade.notional_usd, micro_usd(0));
        taker.expect_cash(micro_usd(1_000_000));
        taker.expect_total(lots("SOL", 1001));
        exchange.expect_money_conserved();
        exchange.expect_funds_released();
    }

    #[test]
    fn seeded_randomized_operations_preserve_money_and_exact_holds() {
        let mut history = RandomHistory::single_asset(0x726671, micro_usd(1_000_000), 1000);
        history.trade_sizes(100, 300);
        history.prices(900..1200);

        for _ in 0..1500 {
            history.next_single_asset_move();
            history.expect_money_and_reservations_correct();
        }
        history.expire_every_request();
        history.expect_no_reserved_funds();
    }

    #[test]
    fn exhaustive_small_books_match_an_independent_price_and_transfer_model() {
        let books = all_three_offer_books([900, 1000, 1100]);
        assert_eq!(books.len(), 1728);
        for direction in [None, Some(Side::Buy), Some(Side::Sell)] {
            for book in &books {
                let mut exchange = BookTrial::new(direction, 100, micro_usd(1_000_000), 1000);
                exchange.place_offers_and_cancel_selected(book, 300);
                let expected_quote = exchange.independently_choose_best_prices();

                exchange.close_quoting(1000);

                exchange.expect_quote(&expected_quote);
                exchange.accept_and_expect_exact_balances(&expected_quote, book.preferred_side);
                exchange.expire_every_request(5000);
                exchange.expect_no_reserved_funds();
            }
        }
    }

    #[test]
    fn many_seeded_multi_asset_histories_preserve_exact_reservations_and_totals() {
        for seed in 0..32 {
            let mut history = RandomHistory::multiple_assets(
                seed,
                micro_usd(100_000),
                &[("SOL", 2, 1000), ("BTC", 8, 1000), ("DOGE", 0, 1000)],
            );
            history.sizes(1..301);
            history.prices(1..10001);

            for step in 0..500 {
                history.next_multi_asset_move(step);
                history.expect_money_and_reservations_correct();
            }
            history.expire_every_request();
            history.expect_no_reserved_funds();
        }
    }
}

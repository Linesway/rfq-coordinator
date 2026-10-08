use super::*;

engine_tests! {
    #[test]
    fn an_action_exactly_at_its_deadline_is_too_late() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(300), sol(0));
        let maker = exchange.add_trader("Bob", dollars(0), sol(2));
        let request = taker.ask_to_buy(sol(2));
        let offer = maker
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .expect_accepted();

        exchange.advance_to(1000);
        maker
            .offer_to_sell(request, sol(2), dollars_per_sol(140))
            .expect_rejected("rfq_not_open");
        maker
            .cancel(request, &offer)
            .expect_rejected("rfq_not_open");
        exchange.advance_to(2000);
        taker
            .accept_buy(request)
            .expect_rejected("not_awaiting_accept");

        exchange.expect_state(request, RfqState::Expired);
        exchange.expect_funds_released();
    }

    #[test]
    fn a_late_timer_does_not_give_the_taker_extra_time() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(300), sol(0));
        let maker = exchange.add_trader("Bob", dollars(0), sol(2));
        let request = taker.ask_to_buy(sol(2));
        maker
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .expect_accepted();

        exchange.advance_to(9000);

        exchange.expect_deadline(request, 2000);
        exchange.expect_state(request, RfqState::Expired);
        exchange.expect_funds_released();
    }

    #[test]
    fn maker_can_cancel_before_quoting_closes_but_not_at_or_after() {
        for (time, can_cancel) in [(999, true), (1000, false), (1001, false)] {
            let exchange = TestExchange::new();
            let taker = exchange.add_trader("Alice", dollars(300), sol(0));
            let maker = exchange.add_trader("Bob", dollars(0), sol(2));
            let request = taker.ask_to_buy(sol(2));
            let offer = maker
                .offer_to_sell(request, sol(2), dollars_per_sol(150))
                .expect_accepted();

            exchange.advance_to(time);
            let cancellation = maker.cancel(request, &offer);

            if can_cancel {
                cancellation.expect_accepted();
            } else {
                cancellation.expect_rejected("rfq_not_open");
            }
            exchange.advance_to(1001);
            exchange.expect_state(
                request,
                if can_cancel {
                    RfqState::Expired
                } else {
                    RfqState::AwaitingAccept
                },
            );
            exchange.advance_to(2000);
            exchange.expect_funds_released();
        }
    }

    #[test]
    fn taker_can_accept_only_between_the_two_deadlines() {
        for (time, can_accept) in [
            (999, false),
            (1000, true),
            (1001, true),
            (1999, true),
            (2000, false),
            (2001, false),
        ] {
            let exchange = TestExchange::new();
            let taker = exchange.add_trader("Alice", dollars(300), sol(0));
            let maker = exchange.add_trader("Bob", dollars(0), sol(2));
            let request = taker.ask_to_buy(sol(2));
            maker
                .offer_to_sell(request, sol(2), dollars_per_sol(150))
                .expect_accepted();

            exchange.advance_to(time);
            let acceptance = taker.accept_buy(request);

            if can_accept {
                acceptance.expect_accepted();
                taker.expect_cash(dollars(0));
                taker.expect_total_sol(2);
            } else {
                acceptance.expect_rejected("not_awaiting_accept");
                taker.expect_cash(dollars(300));
                taker.expect_total_sol(0);
            }
            exchange.advance_to(9000);
            exchange.expect_funds_released();
        }
    }

    #[test]
    fn zero_time_to_accept_means_immediate_expiry_and_no_reserved_funds() {
        let exchange = TestExchange::new();
        exchange.acceptance_window(0);
        let taker = exchange.add_trader("Alice", dollars(300), sol(0));
        let maker = exchange.add_trader("Bob", dollars(0), sol(2));
        let request = taker.ask_to_buy(sol(2));
        maker
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .expect_accepted();

        exchange.close_quoting(request);

        exchange.expect_state(request, RfqState::Expired);
        exchange.expect_funds_released();
    }
}

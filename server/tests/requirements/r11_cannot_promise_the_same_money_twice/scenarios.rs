use super::*;

engine_tests! {
    #[test]
    fn maker_cannot_offer_the_same_sol_twice() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(600), sol(0));
        let maker = exchange.add_trader("Bob", dollars(0), sol(3));

        let first_request = taker.ask_to_buy(sol(2));
        let offer = maker
            .offer_to_sell(first_request, sol(2), dollars_per_sol(150))
            .expect_accepted();
        let second_request = taker.ask_to_buy(sol(2));
        maker
            .offer_to_sell(second_request, sol(2), dollars_per_sol(150))
            .expect_rejected("insufficient_balance");

        maker.expect_total_sol(3);
        maker.expect_reserved_sol(2);
        maker.expect_available_sol(1);

        maker.cancel(first_request, &offer).expect_accepted();
        maker.expect_available_sol(3);
        maker
            .offer_to_sell(second_request, sol(2), dollars_per_sol(150))
            .expect_accepted();
        maker.expect_reserved_sol(2);
    }

    #[test]
    fn only_the_maker_can_cancel_and_then_the_offer_number_can_be_reused() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(0), sol(0));
        let maker = exchange.add_trader("Bob", dollars(0), sol(2));
        let stranger = exchange.add_trader("Carol", dollars(0), sol(0));
        let first_request = taker.ask_for_both_prices(sol(2));
        let second_request = taker.ask_for_both_prices(sol(2));

        let offer = maker
            .numbered_sell_offer(first_request, sol(2), dollars_per_sol(150), 7)
            .expect_accepted();
        maker
            .numbered_sell_offer(second_request, sol(2), dollars_per_sol(150), 7)
            .expect_rejected("duplicate_client_order_id");
        stranger
            .cancel(first_request, &offer)
            .expect_rejected("not_your_order");
        maker.cancel(first_request, &offer).expect_accepted();
        maker
            .cancel(first_request, &offer)
            .expect_rejected("order_not_found");
        maker.expect_reserved_sol(0);

        maker
            .numbered_sell_offer(second_request, sol(2), dollars_per_sol(150), 7)
            .expect_accepted();
        maker.cancel_number(second_request, 7).expect_accepted();
        exchange.expect_funds_released();
        exchange.expect_no_offer_numbers_in_use();
    }

    #[test]
    fn taker_with_300_dollars_can_hold_only_one_300_dollar_quote() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(300), sol(0));
        let maker = exchange.add_trader("Bob", dollars(1000), sol(4));
        let first_request = taker.ask_for_both_prices(sol(2));
        let second_request = taker.ask_for_both_prices(sol(2));

        for request in [first_request, second_request] {
            maker
                .offer_to_buy(request, sol(2), dollars_per_sol(140))
                .expect_accepted();
            maker
                .offer_to_sell(request, sol(2), dollars_per_sol(150))
                .expect_accepted();
        }
        exchange.close_quoting(first_request);

        exchange.expect_state(first_request, RfqState::AwaitingAccept);
        exchange.expect_no_sell_quote(first_request);
        exchange.expect_state(second_request, RfqState::Expired);
        taker.expect_reserved_cash(dollars(300));
        maker.expect_reserved_sol(2);
    }

    #[test]
    fn a_winning_offer_keeps_its_number_until_the_request_finishes() {
        for ending in [
            RfqState::Settled,
            RfqState::Declined,
            RfqState::Expired,
            RfqState::Failed,
        ] {
            let exchange = TestExchange::new();
            let taker = exchange.add_trader("Alice", dollars(1000), sol(0));
            let cash = if ending == RfqState::Failed {
                u64::MAX
            } else {
                dollars(1000)
            };
            let maker = exchange.add_trader("Bob", cash, sol(10));
            let request = taker.ask_to_buy(sol(2));
            maker
                .numbered_sell_offer(request, sol(2), dollars_per_sol(150), 7)
                .expect_accepted();
            exchange.close_quoting(request);

            let next_request = taker.ask_to_buy(sol(2));
            maker
                .numbered_sell_offer(next_request, sol(2), dollars_per_sol(150), 7)
                .expect_rejected("duplicate_client_order_id");
            match ending {
                RfqState::Settled => {
                    taker.accept_buy(request).expect_accepted();
                }
                RfqState::Declined => {
                    taker.decline(request).expect_accepted();
                }
                RfqState::Expired => exchange.end_acceptance(request),
                RfqState::Failed => taker
                    .accept_buy(request)
                    .expect_rejected("insufficient_balance"),
                _ => unreachable!(),
            }
            exchange.expect_state(request, ending);

            exchange.advance_to(2000);
            let fresh_request = taker.ask_to_buy(sol(2));
            maker
                .numbered_sell_offer(fresh_request, sol(2), dollars_per_sol(150), 7)
                .expect_accepted();
            exchange.advance_to(10000);
            exchange.expect_funds_released();
            exchange.expect_no_offer_numbers_in_use();
        }
    }

    #[test]
    fn changing_roles_does_not_give_a_trader_more_money() {
        let exchange = TestExchange::new();
        let alice = exchange.add_trader("Alice", dollars(300), sol(0));
        let bob = exchange.add_trader("Bob", dollars(0), sol(2));
        let carol = exchange.add_trader("Carol", dollars(0), sol(2));
        let alice_buying = alice.ask_to_buy(sol(2));
        let carol_selling = carol.ask_to_sell(sol(2));

        bob.offer_to_sell(alice_buying, sol(2), dollars_per_sol(150))
            .expect_accepted();
        alice
            .offer_to_buy(carol_selling, sol(2), dollars_per_sol(150))
            .expect_accepted();
        exchange.close_quoting(alice_buying);
        carol.accept_sell(carol_selling).expect_accepted();

        exchange.expect_state(alice_buying, RfqState::Expired);
        alice.expect_cash(dollars(0));
        alice.expect_total_sol(2);
        carol.expect_cash(dollars(300));
        exchange.expect_funds_released();
    }

    #[test]
    fn declining_a_quote_gives_both_people_their_funds_back() {
        let exchange = TestExchange::new();
        let taker = exchange.add_trader("Alice", dollars(300), sol(0));
        let maker = exchange.add_trader("Bob", dollars(0), sol(2));
        let request = taker.ask_to_buy(sol(2));
        maker
            .offer_to_sell(request, sol(2), dollars_per_sol(150))
            .expect_accepted();
        exchange.close_quoting(request);
        taker.expect_reserved_cash(dollars(300));
        maker.expect_reserved_sol(2);

        taker.decline(request).expect_accepted();

        taker.expect_cash(dollars(300));
        taker.expect_total_sol(0);
        maker.expect_cash(dollars(0));
        maker.expect_total_sol(2);
        exchange.expect_funds_released();
    }
}

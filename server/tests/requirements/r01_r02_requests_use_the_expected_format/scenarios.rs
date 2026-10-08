use super::*;

client_tests! {
    #[tokio::test]
    async fn malformed_and_signed_error_contract() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
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
            exchange
                .http(method, path, body.map(str::to_string), "application/json")
                .await
                .expect_error(400, "malformed_request");
        }

        let signed = alice.sign_request(both_prices(sol(1)), 1);
        exchange.send("/v1/rfqs", &signed).await.expect_success();
        exchange
            .send("/v1/rfqs", &signed)
            .await
            .expect_error(401, "stale_nonce");
        let mut tampered = alice.sign_request(both_prices(sol(1)), 2);
        tampered["payload"]["size"] = json!(101);
        exchange
            .send("/v1/rfqs", &tampered)
            .await
            .expect_error(401, "invalid_signature");
        let stranger = exchange
            .unknown_trader()
            .sign_request(both_prices(sol(1)), 1);
        exchange
            .send("/v1/rfqs", &stranger)
            .await
            .expect_error(401, "unknown_trader");
        alice
            .submit(both_prices(lots("BAD", 0)).quoting_for(0))
            .await
            .expect_rejected("unknown_asset");
    }

    #[tokio::test]
    async fn ownership_validation_and_cancel_by_client_id() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .trader("Carol", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let carol = exchange.trader("Carol");
        let request = alice.ask_for_both_prices(sol(1)).await;

        alice
            .numbered_sell_offer(request, sol(1), micro_usd(1000), Some(42))
            .await
            .expect_rejected("self_quote_not_allowed");
        let offer = bob
            .numbered_sell_offer(request, sol(1), micro_usd(1000), Some(42))
            .await
            .expect_accepted();
        bob.numbered_sell_offer(request, sol(1), micro_usd(1000), Some(42))
            .await
            .expect_rejected("duplicate_client_order_id");
        carol
            .cancel(request, &offer)
            .await
            .expect_rejected("not_your_order");
        bob.cancel_number(request, 42).await.expect_accepted();

        exchange.expect_funds_released().await;
        carol
            .accept_buy(request)
            .await
            .expect_rejected("not_your_rfq");
        exchange
            .send("/v1/orders", &json!({"pubkey": "oops"}))
            .await
            .expect_error(400, "malformed_request");
    }

    #[tokio::test]
    async fn action_validation_codes_and_statuses_follow_the_wire_contract() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .trader("Carol", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let carol = exchange.trader("Carol");
        for (asset, size, ttl, error) in [
            ("BAD", 0, 0, "unknown_asset"),
            ("SOL", 0, 0, "invalid_size"),
            ("SOL", 100, 0, "invalid_ttl"),
            ("SOL", 100, 30, "invalid_ttl"),
        ] {
            let message = alice.auto_sign(request_payload(
                both_prices(lots(asset, size)).quoting_for(ttl),
            ));
            exchange
                .send("/v1/rfqs", &message)
                .await
                .expect_error(422, error);
        }
        let request = alice.ask_for_both_prices(sol(1)).await;
        for (sender, rfq, side, size, price, status, error) in [
            (
                &alice,
                request,
                Side::Sell,
                100,
                1000,
                422,
                "self_quote_not_allowed",
            ),
            (
                &bob,
                uuid::Uuid::nil(),
                Side::Sell,
                100,
                1000,
                404,
                "rfq_not_found",
            ),
            (&bob, request, Side::Sell, 0, 1000, 422, "invalid_size"),
            (&bob, request, Side::Sell, 99, 1000, 422, "order_too_small"),
            (&bob, request, Side::Sell, 100, 0, 400, "malformed_request"),
            (
                &bob,
                request,
                Side::Buy,
                100,
                2_000_000,
                422,
                "insufficient_balance",
            ),
        ] {
            let message = sender.auto_sign(offer_payload(
                rfq,
                side,
                lots("SOL", size),
                micro_usd(price),
                Some(1),
            ));
            exchange
                .send("/v1/orders", &message)
                .await
                .expect_error(status, error);
            exchange.expect_funds_released().await;
        }
        let offer = bob
            .numbered_sell_offer(request, sol(1), micro_usd(1000), Some(1))
            .await
            .expect_accepted();
        let duplicate = bob.auto_sign(sell_payload(request, sol(1), micro_usd(1000), Some(1)));
        exchange
            .send("/v1/orders", &duplicate)
            .await
            .expect_error(422, "duplicate_client_order_id");
        for (sender, order, number, status, error) in [
            (&bob, None, None, 400, "malformed_request"),
            (
                &bob,
                Some(offer.order_id),
                Some(1),
                400,
                "malformed_request",
            ),
            (&carol, Some(offer.order_id), None, 403, "not_your_order"),
            (&bob, Some(uuid::Uuid::nil()), None, 404, "order_not_found"),
        ] {
            let message = sender.auto_sign(cancel_message(request, order, number));
            exchange
                .send("/v1/orders/cancel", &message)
                .await
                .expect_error(status, error);
        }
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;
        let missing_side = alice.auto_sign(accept_message(request, Side::Sell));
        exchange
            .send("/v1/accept", &missing_side)
            .await
            .expect_error(422, "side_unavailable");
        alice.decline(request).await.expect_accepted();
        exchange.expect_funds_released().await;
    }

    #[tokio::test]
    async fn public_views_have_exact_shapes_and_never_expose_direction_or_keys() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let request = alice.ask_to_buy(sol(1)).await;
        let view = exchange.public_request(request).await;

        expect_json_fields(
            &view,
            &[
                "asset",
                "closes_at",
                "created_at",
                "rfq_id",
                "size",
                "state",
            ],
        );
        assert_eq!(view["state"], "open");
        assert_eq!(request.get_version_num(), 4);
        let balance = alice.balance().await;
        assert_eq!(
            balance.usd.available + balance.usd.reserved,
            balance.usd.total
        );
        for asset in balance.assets.values() {
            assert_eq!(asset.available + asset.reserved, asset.total);
        }
        let unknown_key = exchange.unknown_trader().client.pubkey().to_hex();
        exchange
            .http(
                "GET",
                &format!("/v1/balances/{unknown_key}"),
                None,
                "application/json",
            )
            .await
            .expect_error(401, "unknown_trader");
        exchange
            .http(
                "GET",
                &format!("/v1/rfqs/{}", uuid::Uuid::nil()),
                None,
                "application/json",
            )
            .await
            .expect_error(404, "rfq_not_found");
    }

    #[tokio::test]
    async fn raw_json_directed_and_two_way_trades_choose_price_then_arrival_and_transfer_exact_balances(
    ) {
        for (price_request, accepted_side) in [
            (buy(sol(2)), Side::Buy),
            (sell(sol(2)), Side::Sell),
            (both_prices(sol(2)), Side::Buy),
            (both_prices(sol(2)), Side::Sell),
        ] {
            let exchange = TestServer::setup()
                .acceptance_window(5)
                .trader("Alice", dollars(1), sol(10))
                .trader("Bob", dollars(1), sol(10))
                .trader("Carol", dollars(1), sol(10))
                .trader("Dan", dollars(1), sol(10))
                .start()
                .await;
            let alice = exchange.trader("Alice");
            let signed = alice.raw_price_request(price_request.quoting_for(2), 0);
            let reply = exchange.send("/v1/rfqs", &signed).await;
            let request = reply.request_id();
            reply.expect_quote_window(2000);

            for (name, bid, ask) in [("Bob", 800, 1100), ("Carol", 1000, 900), ("Dan", 1000, 900)] {
                let maker = exchange.trader(name);
                exchange
                    .send(
                        "/v1/orders",
                        &maker.raw_offer(request, Side::Buy, sol(6), bid, 0),
                    )
                    .await
                    .expect_success();
                exchange
                    .send(
                        "/v1/orders",
                        &maker.raw_offer(request, Side::Sell, sol(6), ask, 1),
                    )
                    .await
                    .expect_success();
            }
            exchange.wait_until_quoted(request).await;
            let acceptance = alice.raw_accept(request, accepted_side, 1);
            let result = exchange.send("/v1/accept", &acceptance).await;
            result.expect_success();

            let buying = accepted_side == Side::Buy;
            let payment = if buying { 1800 } else { 2000 };
            assert_eq!(result.body["trade"]["size"], 200);
            assert_eq!(result.body["trade"]["notional_usd"], payment);
            alice
                .expect_unreserved_balance(
                    if buying {
                        1_000_000 - payment
                    } else {
                        1_000_000 + payment
                    },
                    sol(if buying { 12 } else { 8 }),
                )
                .await;
            exchange
                .trader("Carol")
                .expect_unreserved_balance(
                    if buying {
                        1_000_000 + payment
                    } else {
                        1_000_000 - payment
                    },
                    sol(if buying { 8 } else { 12 }),
                )
                .await;
            for name in ["Bob", "Dan"] {
                exchange
                    .trader(name)
                    .expect_unreserved_balance(1_000_000, sol(10))
                    .await;
            }
            let before_replay = exchange.balances().await;
            exchange
                .send("/v1/accept", &alice.raw_accept(request, accepted_side, 2))
                .await
                .expect_error(409, "not_awaiting_accept");
            exchange.expect_balances(&before_replay).await;
        }
    }
}

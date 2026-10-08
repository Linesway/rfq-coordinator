use super::*;

engine_tests! {
    #[test]
    fn nonce_is_global_and_consumed_on_validly_signed_business_error() {
        let exchange = TestExchange::with_sol_decimals(2);
        let alice = exchange.add_trader("Alice", micro_usd(1_000_000), sol(10));
        let request = both_prices(lots("MISSING", 100));
        let signed = alice.sign_request(request, 0);

        exchange.authenticate(&signed).expect_accepted();
        alice.submit(request).expect_rejected("unknown_asset");
        let replay = alice.sign(subscription(Uuid::new_v4()), 0);
        exchange
            .authenticate(&replay)
            .expect_rejected("stale_nonce");

        let mut tampered = alice.sign_request(request, 10);
        tampered.payload.size += 1;
        exchange
            .authenticate(&tampered)
            .expect_rejected("invalid_signature");
        exchange
            .authenticate(&alice.sign_request(request, 1))
            .expect_accepted();
        alice.expect_nonce(Some(1));
    }

    #[test]
    fn every_signed_payload_field_is_authenticated() {
        use serde_json::json;
        {
            let exchange = TestExchange::new();
            let alice = exchange.add_trader("Alice", dollars(1), sol(10));
            let signed = alice.sign_request(both_prices(lots("SOL", 100)).quoting_for(1), 7);
            for altered in changed_fields(
                &signed,
                &[
                    ("asset", json!("OTHER")),
                    ("direction", json!("buy")),
                    ("size", json!(101)),
                    ("ttl_secs", json!(2)),
                ],
            ) {
                exchange
                    .authenticate(&altered)
                    .expect_rejected("invalid_signature");
                alice.expect_nonce(None);
            }
            exchange.authenticate(&signed).expect_accepted();
        }
        {
            let exchange = TestExchange::new();
            let alice = exchange.add_trader("Alice", dollars(1), sol(10));
            let request = Uuid::new_v4();
            let signed = alice.sign(sell_offer(request, 100, 1000), 7);
            for altered in changed_fields(
                &signed,
                &[
                    ("rfq_id", json!(Uuid::nil())),
                    ("side", json!("buy")),
                    ("price", json!(1001)),
                    ("size", json!(101)),
                    ("client_order_id", json!(1)),
                ],
            ) {
                exchange
                    .authenticate(&altered)
                    .expect_rejected("invalid_signature");
                alice.expect_nonce(None);
            }
            exchange.authenticate(&signed).expect_accepted();
        }
        {
            let exchange = TestExchange::new();
            let alice = exchange.add_trader("Alice", dollars(1), sol(10));
            let request = Uuid::new_v4();
            let signed = alice.sign(cancellation(request, Some(request), None), 7);
            for altered in changed_fields(
                &signed,
                &[
                    ("rfq_id", json!(Uuid::nil())),
                    ("order_id", json!(Uuid::nil())),
                ],
            ) {
                exchange
                    .authenticate(&altered)
                    .expect_rejected("invalid_signature");
                alice.expect_nonce(None);
            }
            exchange.authenticate(&signed).expect_accepted();
        }
        {
            let exchange = TestExchange::new();
            let alice = exchange.add_trader("Alice", dollars(1), sol(10));
            let request = Uuid::new_v4();
            let signed = alice.sign(cancellation(request, None, Some(7)), 7);
            for altered in changed_fields(&signed, &[("client_order_id", json!(8))]) {
                exchange
                    .authenticate(&altered)
                    .expect_rejected("invalid_signature");
                alice.expect_nonce(None);
            }
            exchange.authenticate(&signed).expect_accepted();
        }
        {
            let exchange = TestExchange::new();
            let alice = exchange.add_trader("Alice", dollars(1), sol(10));
            let request = Uuid::new_v4();
            let signed = alice.sign(acceptance(request, Side::Buy), 7);
            for altered in changed_fields(
                &signed,
                &[("rfq_id", json!(Uuid::nil())), ("side", json!("sell"))],
            ) {
                exchange
                    .authenticate(&altered)
                    .expect_rejected("invalid_signature");
                alice.expect_nonce(None);
            }
            exchange.authenticate(&signed).expect_accepted();
        }
        {
            let exchange = TestExchange::new();
            let alice = exchange.add_trader("Alice", dollars(1), sol(10));
            let request = Uuid::new_v4();
            let signed = alice.sign(decline_request(request), 7);
            for altered in changed_fields(&signed, &[("rfq_id", json!(Uuid::nil()))]) {
                exchange
                    .authenticate(&altered)
                    .expect_rejected("invalid_signature");
                alice.expect_nonce(None);
            }
            exchange.authenticate(&signed).expect_accepted();
        }
        {
            let exchange = TestExchange::new();
            let alice = exchange.add_trader("Alice", dollars(1), sol(10));
            let request = Uuid::new_v4();
            let signed = alice.sign(subscription(request), 7);
            for altered in changed_fields(&signed, &[("rfq_id", json!(Uuid::nil()))]) {
                exchange
                    .authenticate(&altered)
                    .expect_rejected("invalid_signature");
                alice.expect_nonce(None);
            }
            exchange.authenticate(&signed).expect_accepted();
        }
    }

    #[test]
    fn envelope_identity_nonce_and_action_cannot_be_substituted() {
        let exchange = TestExchange::new();
        let alice = exchange.add_trader("Alice", dollars(1), sol(10));
        let bob = exchange.add_trader("Bob", dollars(1), sol(10));
        let request = Uuid::new_v4();
        let signed = alice.sign(decline_request(request), 0);

        let mut changed_nonce = signed.clone();
        changed_nonce.nonce = 1;
        exchange
            .authenticate(&changed_nonce)
            .expect_rejected("invalid_signature");
        let mut changed_sender = signed.clone();
        changed_sender.pubkey = bob.sign(decline_request(request), 0).pubkey;
        exchange
            .authenticate(&changed_sender)
            .expect_rejected("invalid_signature");
        for signature in [
            String::new(),
            "00".repeat(64),
            "zz".repeat(64),
            "00".repeat(63),
        ] {
            let mut forged = signed.clone();
            forged.signature = signature;
            exchange
                .authenticate(&forged)
                .expect_rejected("invalid_signature");
        }
        let mut wrong_action = alice.sign(subscription(request), 0);
        wrong_action.signature = signed.signature.clone();
        exchange
            .authenticate(&wrong_action)
            .expect_rejected("invalid_signature");

        exchange.authenticate(&signed).expect_accepted();
        exchange
            .authenticate(&alice.sign(decline_request(request), u64::MAX))
            .expect_accepted();
        for nonce in [0, 1, u64::MAX] {
            exchange
                .authenticate(&alice.sign(decline_request(request), nonce))
                .expect_rejected("stale_nonce");
        }
    }

    #[test]
    fn nonces_are_shared_by_all_actions_but_not_by_different_traders() {
        let exchange = TestExchange::new();
        let alice = exchange.add_trader("Alice", dollars(1), sol(10));
        let bob = exchange.add_trader("Bob", dollars(1), sol(10));
        let stranger = exchange.unregistered_trader();
        let request = Uuid::new_v4();

        exchange
            .authenticate(&alice.sign(subscription(request), 10))
            .expect_accepted();
        exchange
            .authenticate(&alice.sign(decline_request(request), 9))
            .expect_rejected("stale_nonce");
        exchange
            .authenticate(&bob.sign(subscription(request), 0))
            .expect_accepted();
        exchange
            .authenticate(&stranger.sign(subscription(request), 0))
            .expect_rejected("unknown_trader");
        let mut forged = stranger.sign(subscription(request), 0);
        forged.signature = "00".repeat(64);
        exchange
            .authenticate(&forged)
            .expect_rejected("invalid_signature");
    }
}

client_tests! {
    #[tokio::test]
    async fn independently_signed_canonical_request_and_cross_action_replay() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let signed = alice.raw_price_request(both_prices(sol(1)), 0);
        let reply = exchange.send("/v1/rfqs", &signed).await;
        let request = reply.request_id();
        reply.expect_quote_window(1000);

        let replay = alice.sign(decline_message(request), 0);
        exchange
            .send("/v1/decline", &replay)
            .await
            .expect_error(401, "stale_nonce");
        for wrong_action in ["subscribe", "wrong_action"] {
            let forged = alice.independently_sign(
                wrong_action,
                &request.to_string(),
                json!({"rfq_id": request}),
                1,
            );
            exchange
                .send("/v1/decline", &forged)
                .await
                .expect_error(401, "invalid_signature");
        }

        let decline = alice.sign(decline_message(request), 1);
        exchange
            .send("/v1/decline", &decline)
            .await
            .expect_error(409, "not_awaiting_accept");
        exchange
            .send("/v1/decline", &decline)
            .await
            .expect_error(401, "stale_nonce");
    }

    #[tokio::test]
    async fn malformed_json_and_transport_errors_do_not_consume_nonce() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let signed = alice.sign_request(both_prices(sol(1)), 0);

        for (field, invalid_value) in [
            ("size", json!(-1)),
            ("size", json!(1.5)),
            ("size", json!("100")),
            ("direction", json!("BUY")),
            ("ttl_secs", json!(null)),
        ] {
            let mut malformed = signed.clone();
            malformed["payload"][field] = invalid_value;
            exchange
                .send("/v1/rfqs", &malformed)
                .await
                .expect_error(400, "malformed_request");
        }
        exchange
            .http("POST", "/v1/rfqs", Some(signed.to_string()), "text/plain")
            .await
            .expect_error(400, "malformed_request");
        exchange
            .http(
                "POST",
                "/v1/rfqs",
                Some(" ".repeat(2 * 1024 * 1024 + 1)),
                "application/json",
            )
            .await
            .expect_error(400, "malformed_request");

        exchange.send("/v1/rfqs", &signed).await.expect_success();
        exchange.expect_healthy().await;
    }
}

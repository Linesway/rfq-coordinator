use super::*;

client_tests! {
    #[test]
    fn binary_rejects_missing_flags_missing_seed_and_invalid_window() {
        let missing_seed = missing_seed_path();
        expect_startup_failure(&[]);
        expect_startup_failure(&["--seed", &missing_seed, "--port", "4402"]);
        expect_startup_failure(&[
            "--seed",
            "seed.toml",
            "--port",
            "4402",
            "--accept-window-secs",
            "-1",
        ]);
    }

    #[tokio::test]
    async fn binary_defaults_to_five_second_accept_window() {
        let server = ServerProcess::start(None).await;
        let alice = server.trader("Alice");
        let bob = server.trader("Bob");
        let request = alice.submit(buy(sol(1))).await.expect_accepted();
        bob.offer_to_sell(request.rfq_id, sol(1), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request.rfq_id, RfqState::Open).await;

        updates
            .expect_acceptance_window(request.closes_at, 5000)
            .await;
    }

    #[tokio::test]
    async fn binary_honors_custom_accept_window() {
        let server = ServerProcess::start(Some(1)).await;
        let alice = server.trader("Alice");
        let bob = server.trader("Bob");
        let request = alice.submit(buy(sol(1))).await.expect_accepted();
        bob.offer_to_sell(request.rfq_id, sol(1), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request.rfq_id, RfqState::Open).await;

        updates
            .expect_acceptance_window(request.closes_at, 1000)
            .await;
    }

    #[tokio::test]
    async fn binary_restart_reloads_seed_instead_of_retaining_trades() {
        let first = ServerProcess::start(Some(1)).await;
        let alice = first.trader("Alice");
        let bob = first.trader("Bob");
        let request = alice.ask_for_both_prices(sol(1)).await;
        bob.offer_to_sell(request, sol(1), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;
        alice.accept_buy(request).await.expect_accepted();
        alice.expect_cash(micro_usd(999_000)).await;

        drop(first);
        let restarted = ServerProcess::start(Some(1)).await;

        restarted.expect_request_missing(request).await;
        restarted
            .trader("Alice")
            .expect_unreserved_balance(dollars(1), sol(10))
            .await;
    }
}

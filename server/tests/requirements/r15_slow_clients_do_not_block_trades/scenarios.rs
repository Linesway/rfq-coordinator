use super::*;

client_tests! {
    #[tokio::test]
    async fn overflowing_a_maker_feed_disconnects_it_without_blocking_a_trade() {
        let exchange = TestServer::setup()
            .trader("Alice", dollars(1), sol(10))
            .trader("Bob", dollars(1), sol(10))
            .start()
            .await;
        let alice = exchange.trader("Alice");
        let bob = exchange.trader("Bob");
        let mut slow_maker = exchange.maker_socket().await;

        exchange.burst_without_reading(&alice, 1100, both_prices(lots("SOL", 1)).quoting_for(29));
        slow_maker.expect_closed().await;
        exchange.expect_healthy().await;

        let request = alice.ask_to_buy(sol(1)).await;
        bob.offer_to_sell(request, sol(3), micro_usd(1000))
            .await
            .expect_accepted();
        let mut updates = alice.watch(request, RfqState::Open).await;
        updates.expect_buy_quote(micro_usd(1000)).await;
        alice.accept_buy(request).await.expect_accepted();
        exchange.expect_funds_released().await;
    }
}

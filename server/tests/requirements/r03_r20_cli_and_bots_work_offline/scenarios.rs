use super::*;

client_tests! {
    #[path = "../../helpers/stock_clients.rs"]
    mod stock_clients;
    use stock_clients::StockClients;

    #[tokio::test]
    async fn cli_trades_with_all_four_bots_without_live_prices() {
        let mut game = StockClients::build();
        game.start_server("seed.toml", 3).await;
        game.expect_sol_is_listed().await;
        game.expect_request_without_makers_to_expire(2, 1).await;
        game.start_offline_makers(&["tight", "lazy", "skew", "sniper"], 2026)
            .await;
        game.buy_sol(2).await;
        game.buy_doge_from_two_way_quote(5000).await;
        game.sell_sol(2).await;
        game.sell_doge_from_two_way_quote(5000).await;
        game.expect_balances_command_to_work().await;
        game.expect_cash_and_every_asset_conserved().await;
        game.expect_everyones_funds_released().await;
    }
}

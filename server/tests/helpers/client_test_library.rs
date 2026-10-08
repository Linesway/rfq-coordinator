macro_rules! engine_tests {
    ($($item:item)*) => {};
}
macro_rules! client_tests { ($($item:item)*) => { $($item)* }; }

use common::{
    client::{ApiFailure, RfqClient, TakerFeed},
    seed::{AssetDef, SeedConfig, TraderSeed},
    signing::Envelope,
    types::*,
    Keypair,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use server::{api::router, app::App};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct World {
    url: String,
    clients: Vec<Arc<RfqClient>>,
    task: JoinHandle<()>,
    app: Arc<App>,
}
impl Drop for World {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn event(feed: &mut TakerFeed) -> TakerEvent {
    timeout(Duration::from_secs(5), feed.next_event())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
fn code(error: anyhow::Error) -> String {
    error.downcast_ref::<ApiFailure>().unwrap().code.clone()
}
fn totals(balances: &BalancesResponse) -> (u128, u128) {
    balances
        .traders
        .iter()
        .fold((0, 0), |(usd, asset), trader| {
            (
                usd + trader.usd.total as u128,
                asset + trader.assets["SOL"].total as u128,
            )
        })
}
fn no_holds(balances: &BalancesResponse) {
    assert!(balances
        .traders
        .iter()
        .all(|trader| trader.usd.reserved == 0 && trader.assets["SOL"].reserved == 0));
}

mod trade_inputs;
use trade_inputs::*;
mod server_process;
use server_process::*;
mod named_clients;
mod socket_clients;
use named_clients::*;

#[path = "live_trading_story.rs"]
mod live_trading_story;
use live_trading_story::*;

#[path = "concurrent_traders.rs"]
mod concurrent_traders;
use concurrent_traders::*;

#[path = "../requirements/r04_r05_r06_r07_r09_ask_for_prices_and_trade/scenarios.rs"]
mod r04_r05_r06_r07_r09_ask_for_prices_and_trade;

#[path = "../requirements/r08_clients_receive_updates/scenarios.rs"]
mod r08_clients_receive_updates;

#[path = "../requirements/r10_reject_forged_and_repeated_requests/scenarios.rs"]
mod r10_reject_forged_and_repeated_requests;

#[path = "../requirements/r01_r02_requests_use_the_expected_format/scenarios.rs"]
mod r01_r02_requests_use_the_expected_format;

#[path = "../requirements/r14_simultaneous_requests_stay_correct/scenarios.rs"]
mod r14_simultaneous_requests_stay_correct;

#[path = "../requirements/r15_slow_clients_do_not_block_trades/scenarios.rs"]
mod r15_slow_clients_do_not_block_trades;

#[path = "../requirements/r17_r18_r19_server_builds_and_starts/scenarios.rs"]
mod r17_r18_r19_server_builds_and_starts;

#[path = "../requirements/r03_r20_cli_and_bots_work_offline/scenarios.rs"]
mod r03_r20_cli_and_bots_work_offline;

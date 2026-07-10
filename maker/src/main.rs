//! `rfq-maker` — stock market makers for the RFQ coordinator.
//!
//! Four strategies ship with the repo. They are deliberately different so a
//! server implementation gets exercised from several angles:
//!
//! | strategy | behavior |
//! |----------|----------|
//! | `tight`  | instant two-sided quotes at ±10 bps, exact RFQ size, always tags orders with a `client_order_id`; cancel/replaces on every mark-price tick |
//! | `lazy`   | waits 500 ms, quotes wide (±150 bps) at 3× the RFQ size; quotes only the useful side when the taker's direction is visible; re-quotes when the mark drifts ≥ 15 bps |
//! | `skew`   | prices off its own inventory (via `/v1/balances/{pubkey}`) and occasionally over-commits on purpose to probe the server's inventory policy; re-quotes on ≥ 5 bps moves |
//! | `sniper` | ignores 30% of RFQs; otherwise places a wide throwaway quote, then cancels (by `client_order_id`) and re-quotes at ±2 bps off the freshest mark just before the window closes — sometimes deliberately losing the race |
//!
//! Pricing comes from live Phoenix Perps websocket streams (Rise SDK) with a
//! static `prices.toml` fallback; a symbol's stream only runs while an RFQ in
//! that symbol is active. Pass `--offline` to skip the network entirely.

mod prices;
mod strategy;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use common::client::RfqClient;
use common::types::MakerEvent;
use common::Keypair;

use crate::prices::PriceBook;
use crate::strategy::{handle_rfq, Ctx, OpenRfq, Strategy};

#[derive(Parser)]
#[command(name = "rfq-maker", about = "Stock market maker for the RFQ coordinator")]
struct Args {
    /// Server base URL.
    #[arg(long, default_value = "http://127.0.0.1:4402")]
    server: String,
    /// Path to this maker's keypair file.
    #[arg(long)]
    key: String,
    /// Quoting strategy.
    #[arg(long)]
    strategy: Strategy,
    /// Skip the live Phoenix price feed; use only the static price file.
    #[arg(long)]
    offline: bool,
    /// Static fallback price file (micro-USD per whole unit).
    #[arg(long, default_value = "prices.toml")]
    prices: String,
    /// Seed for the strategy RNG (default: random). Useful for reproducing
    /// sniper/skew behavior.
    #[arg(long)]
    rng_seed: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();

    let keypair = Keypair::load(&args.key)?;
    let client = RfqClient::new(&args.server, keypair);
    tracing::info!(
        strategy = ?args.strategy,
        pubkey = %client.pubkey(),
        server = %args.server,
        "maker starting"
    );

    client
        .health()
        .await
        .context("server not reachable (is it running?)")?;
    let symbols: Vec<String> = client
        .assets()
        .await?
        .assets
        .into_iter()
        .map(|a| a.symbol)
        .collect();

    let prices = PriceBook::load_static(&args.prices, &symbols)?;
    let mut live_feed = if args.offline {
        tracing::info!("offline mode: using static prices from {}", args.prices);
        None
    } else {
        Some(prices::LiveFeed::new(prices.clone()))
    };
    // rfq_id -> asset, so rfq_done can release the right price stream.
    let mut rfq_assets: HashMap<uuid::Uuid, String> = HashMap::new();

    let ctx = Arc::new(Ctx {
        client,
        prices,
        strategy: args.strategy,
        rng_seed: args.rng_seed.unwrap_or_else(rand::random),
        next_cid: AtomicU64::new(1),
        rfq_counter: AtomicU64::new(0),
        my_orders: Mutex::new(HashSet::new()),
    });

    let mut feed = ctx.client.maker_feed().await?;
    tracing::info!("subscribed to /ws/makers; quoting");

    while let Some(event) = feed.next_event().await? {
        match event {
            MakerEvent::RfqOpened {
                rfq_id,
                asset,
                size,
                direction,
                closes_at,
                ..
            } => {
                tracing::info!(%rfq_id, %asset, size, ?direction, "RFQ opened");
                if let Some(feed) = live_feed.as_mut() {
                    feed.acquire(&asset);
                    rfq_assets.insert(rfq_id, asset.clone());
                }
                let rfq = OpenRfq {
                    rfq_id,
                    asset,
                    size,
                    direction,
                    closes_at,
                };
                tokio::spawn(handle_rfq(ctx.clone(), rfq));
            }
            MakerEvent::RfqWindowClosed { rfq_id } => {
                tracing::debug!(%rfq_id, "window closed");
            }
            MakerEvent::RfqDone {
                rfq_id,
                outcome,
                fills,
            } => {
                let mine = ctx.my_orders.lock().unwrap();
                for fill in &fills {
                    if mine.contains(&fill.order_id) {
                        tracing::info!(
                            %rfq_id,
                            side = %fill.maker_side,
                            price = fill.price,
                            size = fill.size,
                            "WON: our order filled"
                        );
                    }
                }
                drop(mine);
                if let (Some(feed), Some(asset)) =
                    (live_feed.as_mut(), rfq_assets.remove(&rfq_id))
                {
                    feed.release(&asset);
                }
                tracing::debug!(%rfq_id, ?outcome, "RFQ done");
            }
        }
    }
    anyhow::bail!("maker feed closed by server")
}

//! The RFQ coordinator server — YOUR implementation goes here.
//!
//! This stub compiles, parses the required CLI flags, loads the seed file,
//! and serves `GET /health`. Everything else is up to you; the wire
//! protocol is defined by the shipped code (`common` and the clients that
//! use it) — recovering it from there is part of the exercise.
//!
//! Contract you must keep (the evaluator relies on it):
//! - Binary named `server`, built by `cargo build --release -p server`.
//! - Accepts `--seed <path>`, `--port <u16>`, `--accept-window-secs <u64>`
//!   (default 5).
//! - Binds 127.0.0.1:<port> and answers `GET /health` with 200 once ready.

use std::sync::Arc;

use anyhow::Context;
use axum::routing::get;
use axum::Router;
use clap::Parser;
use common::seed::SeedConfig;

#[derive(Parser)]
#[command(about = "RFQ coordinator server")]
struct Args {
    /// Path to the TOML seed file with assets and trader balances.
    #[arg(long)]
    seed: String,
    /// Port to listen on (bind 127.0.0.1).
    #[arg(long)]
    port: u16,
    /// Seconds the taker has to accept a quote. Protocol default is 5;
    /// the evaluator passes a smaller value so tests run fast.
    #[arg(long, default_value_t = 5)]
    accept_window_secs: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();

    let seed = SeedConfig::load(&args.seed)?;
    tracing::info!(
        "loaded seed: {} assets, {} traders",
        seed.assets.len(),
        seed.traders.len()
    );

    // TODO: replace this with your exchange state. `common::types`,
    // `common::signing`, and `common::seed` define the wire types, the
    // canonical signing message, and the seed schema — you are encouraged
    // to use them. `common::client` shows every endpoint the shipped
    // clients call; your server has to answer all of them.
    let _state = Arc::new(seed);

    let app = Router::new().route("/health", get(|| async { "ok" }));
    // TODO: the rest of the routes.

    let addr = format!("127.0.0.1:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

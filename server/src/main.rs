//! RFQ coordinator executable. Protocol definitions live in `common`;
//! state transitions and transport handlers live in the server library.
//!
//! Contract you must keep (the evaluator relies on it):
//! - Binary named `server`, built by `cargo build --release -p server`.
//! - Accepts `--seed <path>`, `--port <u16>`, `--accept-window-secs <u64>`
//!   (default 5).
//! - Binds 127.0.0.1:<port> and answers `GET /health` with 200 once ready.

use anyhow::Context;
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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();

    let seed = SeedConfig::load(&args.seed)?;
    tracing::info!(
        "loaded seed: {} assets, {} traders",
        seed.assets.len(),
        seed.traders.len()
    );

    let state = server::api::App::new(seed, args.accept_window_secs)?;
    let app = server::api::router(state);

    let addr = format!("127.0.0.1:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

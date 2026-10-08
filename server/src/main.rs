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

    let state = server::app::App::new(seed, args.accept_window_secs)?;
    let app = server::api::router(state);

    let address = format!("127.0.0.1:{}", args.port);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("binding {address}"))?;
    tracing::info!("server listening on http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}

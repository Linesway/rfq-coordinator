//! `rfq-cli` — the taker client for the RFQ coordinator.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use common::client::RfqClient;
use common::signing::unix_millis;
use common::types::*;
use common::Keypair;

#[derive(Parser)]
#[command(name = "rfq-cli", about = "Taker client for the RFQ coordinator")]
struct Args {
    /// Server base URL.
    #[arg(long, global = true, default_value = "http://127.0.0.1:4402")]
    server: String,
    /// Path to your keypair file (required for signed actions).
    #[arg(long, global = true)]
    key: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Submit an RFQ, wait for the quote, and (interactively) accept it.
    Submit {
        /// Asset symbol, e.g. SOL.
        #[arg(long)]
        asset: String,
        /// Size in whole units of the asset, e.g. `2` or `0.5`. Precision is
        /// capped by the asset's lot size (see `rfq-cli assets`).
        #[arg(long)]
        size: String,
        /// Optional direction. If omitted, makers won't see which way you
        /// are trading and you'll receive both a bid and an ask.
        #[arg(long)]
        direction: Option<Side>,
        /// Quoting window in seconds (1..=29).
        #[arg(long, default_value_t = 5)]
        ttl: u64,
        /// Auto-accept the quote without prompting. For an undirected RFQ
        /// pass a side to --accept instead.
        #[arg(long)]
        yes: bool,
        /// Auto-accept a specific side (buy = lift the ask, sell = hit the bid).
        #[arg(long, conflicts_with = "yes")]
        accept: Option<Side>,
    },
    /// Show balances for all participants, or one (by name or pubkey).
    Balances { who: Option<String> },
    /// List the assets tradeable on this server.
    Assets,
    /// Generate a new keypair file and print its pubkey.
    Keygen {
        #[arg(long)]
        out: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if let Command::Keygen { out } = &args.command {
        let keypair = Keypair::generate();
        keypair.save(out)?;
        println!("wrote {out}");
        println!("pubkey: {}", keypair.pubkey());
        return Ok(());
    }

    // Signed actions need the user's key; views work with a throwaway key.
    let keypair = match &args.key {
        Some(path) => Keypair::load(path)?,
        None => Keypair::generate(),
    };
    let client = RfqClient::new(&args.server, keypair);

    match args.command {
        Command::Keygen { .. } => unreachable!(),
        Command::Assets => {
            let assets = client.assets().await?;
            println!("{:<8} {:>12} {:>16}", "ASSET", "LOT DECIMALS", "1 LOT =");
            for a in assets.assets {
                println!(
                    "{:<8} {:>12} {:>16}",
                    a.symbol,
                    a.lot_decimals,
                    format!("{} {}", format_lots(1, a.lot_decimals), a.symbol)
                );
            }
        }
        Command::Balances { who } => {
            let decimals = asset_decimals(&client).await?;
            let all = client.balances().await?;
            let selected: Vec<_> = match &who {
                None => all.traders,
                Some(who) => {
                    let found: Vec<_> = all
                        .traders
                        .into_iter()
                        .filter(|t| {
                            t.name.as_deref() == Some(who.as_str()) || t.pubkey.to_hex() == *who
                        })
                        .collect();
                    if found.is_empty() {
                        bail!("no trader named {who:?} (try a name from the seed file or a hex pubkey)");
                    }
                    found
                }
            };
            for trader in selected {
                print_balances(&trader, &decimals);
                println!();
            }
        }
        Command::Submit {
            asset,
            size,
            direction,
            ttl,
            yes,
            accept,
        } => {
            if args.key.is_none() {
                bail!("--key is required to submit an RFQ");
            }
            let auto_side = match (yes, accept, direction) {
                (true, _, Some(direction)) => Some(direction),
                (true, _, None) => {
                    bail!("--yes needs a directed RFQ; use --accept <buy|sell> for undirected RFQs")
                }
                (false, accept, _) => accept,
            };
            submit_flow(&client, asset, size, direction, ttl, auto_side, yes || accept.is_some())
                .await?;
        }
    }
    Ok(())
}

async fn submit_flow(
    client: &RfqClient,
    asset: String,
    size: String,
    direction: Option<Side>,
    ttl: u64,
    auto_side: Option<Side>,
    auto: bool,
) -> Result<()> {
    let decimals = asset_decimals(client).await?;
    let lot_decimals = *decimals
        .get(&asset)
        .with_context(|| format!("unknown asset {asset:?} (see `rfq-cli assets`)"))?;
    let size = parse_size(&size, lot_decimals)?;

    let before = client.balances_of(&client.pubkey()).await.ok();

    let rfq = client
        .submit_rfq(SubmitRfqPayload {
            asset: asset.clone(),
            direction,
            size,
            ttl_secs: ttl,
        })
        .await?;
    println!(
        "submitted RFQ {} ({} {} {}, window {}s)",
        rfq.rfq_id,
        direction.map(|d| d.to_string()).unwrap_or_else(|| "two-way".into()),
        format_lots(size, lot_decimals),
        asset,
        ttl
    );

    let (mut feed, _ack) = client.subscribe_rfq(rfq.rfq_id).await?;
    println!("waiting for quotes...");

    loop {
        let event = feed
            .next_event()
            .await?
            .context("server closed the RFQ feed unexpectedly")?;
        match event {
            TakerEvent::Subscribed { .. } => {}
            TakerEvent::NoQuote { .. } => println!("no quotes received before the window closed"),
            TakerEvent::Quote {
                bid,
                ask,
                size,
                accept_deadline,
                ..
            } => {
                println!("quote received (size {}):", format_lots(size, lot_decimals));
                if let Some(ask) = ask {
                    println!("  you BUY  at {} (ask)", format_usd(ask.price));
                }
                if let Some(bid) = bid {
                    println!("  you SELL at {} (bid)", format_usd(bid.price));
                }
                let side = match auto_side {
                    Some(side) => Some(side),
                    None if auto => None,
                    None => prompt_side(bid.is_some(), ask.is_some(), accept_deadline).await?,
                };
                match side {
                    Some(side) => match client.accept(rfq.rfq_id, side).await {
                        Ok(response) => print_trade(&response.trade, lot_decimals),
                        Err(e) => println!("accept failed: {e:#}"),
                    },
                    None => {
                        let _ = client.decline(rfq.rfq_id).await;
                        println!("declined");
                    }
                }
            }
            TakerEvent::Settled { trade, .. } => {
                println!("RFQ settled");
                let _ = trade;
                break;
            }
            TakerEvent::Expired { .. } => {
                println!("RFQ expired");
                break;
            }
            TakerEvent::Declined { .. } => {
                println!("RFQ declined");
                break;
            }
            TakerEvent::Failed { .. } => {
                println!("RFQ failed (insufficient balance at settlement)");
                break;
            }
            TakerEvent::Error { message } => bail!("server error: {message}"),
        }
    }

    if let (Some(before), Ok(after)) = (before, client.balances_of(&client.pubkey()).await) {
        print_deltas(&before, &after, &decimals);
    }
    Ok(())
}

async fn prompt_side(has_bid: bool, has_ask: bool, deadline: u64) -> Result<Option<Side>> {
    let mut options = Vec::new();
    if has_ask {
        options.push("[b]uy");
    }
    if has_bid {
        options.push("[s]ell");
    }
    options.push("[d]ecline");
    println!("{}? ", options.join(" / "));

    let time_left = Duration::from_millis(deadline.saturating_sub(unix_millis()));
    let line = tokio::time::timeout(time_left, async {
        tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).ok();
            line
        })
        .await
        .unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    Ok(match line.trim() {
        "b" | "buy" if has_ask => Some(Side::Buy),
        "s" | "sell" if has_bid => Some(Side::Sell),
        _ => None,
    })
}

/// Parse a human-readable size like `2`, `0.5`, or `12.345` into lots.
fn parse_size(text: &str, lot_decimals: u32) -> Result<u64> {
    let (int, frac) = text.split_once('.').unwrap_or((text, ""));
    if int.is_empty() && frac.is_empty() {
        bail!("invalid size {text:?}");
    }
    if !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        bail!("invalid size {text:?} (expected a decimal number like 2 or 0.5)");
    }
    if frac.len() > lot_decimals as usize {
        bail!(
            "size {text:?} is finer than this asset's lot size \
             ({lot_decimals} decimal place{}; see `rfq-cli assets`)",
            if lot_decimals == 1 { "" } else { "s" }
        );
    }
    let scale = 10u64.pow(lot_decimals);
    let int_lots: u64 = if int.is_empty() { 0 } else { int.parse()? };
    let frac_lots: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse::<u64>()? * 10u64.pow(lot_decimals - frac.len() as u32)
    };
    let lots = int_lots
        .checked_mul(scale)
        .and_then(|v| v.checked_add(frac_lots))
        .with_context(|| format!("size {text:?} is too large"))?;
    if lots == 0 {
        bail!("size must be positive");
    }
    Ok(lots)
}

async fn asset_decimals(client: &RfqClient) -> Result<BTreeMap<String, u32>> {
    Ok(client
        .assets()
        .await?
        .assets
        .into_iter()
        .map(|a| (a.symbol, a.lot_decimals))
        .collect())
}

fn print_trade(trade: &Trade, lot_decimals: u32) {
    println!(
        "TRADE {}: {} {} {} @ {} (notional {})",
        trade.trade_id,
        trade.taker_side,
        format_lots(trade.size, lot_decimals),
        trade.asset,
        format_usd(trade.price),
        format_usd(trade.notional_usd),
    );
}

fn print_balances(trader: &TraderBalances, decimals: &BTreeMap<String, u32>) {
    let name = trader.name.clone().unwrap_or_else(|| "<unnamed>".into());
    println!("{name} ({})", trader.pubkey);
    println!(
        "  {:<8} {:>20} {:>20} {:>20}",
        "", "TOTAL", "RESERVED", "AVAILABLE"
    );
    println!(
        "  {:<8} {:>20} {:>20} {:>20}",
        "USD",
        format_usd(trader.usd.total),
        format_usd(trader.usd.reserved),
        format_usd(trader.usd.available)
    );
    for (symbol, balance) in &trader.assets {
        let d = decimals.get(symbol).copied().unwrap_or(0);
        println!(
            "  {:<8} {:>20} {:>20} {:>20}",
            symbol,
            format_lots(balance.total, d),
            format_lots(balance.reserved, d),
            format_lots(balance.available, d)
        );
    }
}

fn print_deltas(before: &TraderBalances, after: &TraderBalances, decimals: &BTreeMap<String, u32>) {
    let usd_delta = after.usd.total as i128 - before.usd.total as i128;
    if usd_delta != 0 {
        println!(
            "USD: {} -> {} ({}{})",
            format_usd(before.usd.total),
            format_usd(after.usd.total),
            if usd_delta > 0 { "+" } else { "-" },
            format_usd(usd_delta.unsigned_abs() as u64)
        );
    }
    for (symbol, after_bal) in &after.assets {
        let before_total = before.assets.get(symbol).map(|b| b.total).unwrap_or(0);
        let delta = after_bal.total as i128 - before_total as i128;
        if delta != 0 {
            let d = decimals.get(symbol).copied().unwrap_or(0);
            println!(
                "{symbol}: {} -> {} ({}{})",
                format_lots(before_total, d),
                format_lots(after_bal.total, d),
                if delta > 0 { "+" } else { "-" },
                format_lots(delta.unsigned_abs() as u64, d)
            );
        }
    }
}

/// Render micro-USD as dollars.
fn format_usd(micro: u64) -> String {
    format!("${}.{:06}", micro / 1_000_000, micro % 1_000_000)
}

/// Render lots as whole units of the asset.
fn format_lots(lots: u64, lot_decimals: u32) -> String {
    if lot_decimals == 0 {
        return lots.to_string();
    }
    let scale = 10u64.pow(lot_decimals);
    format!(
        "{}.{:0width$}",
        lots / scale,
        lots % scale,
        width = lot_decimals as usize
    )
}

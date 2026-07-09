//! The four stock quoting strategies.
//!
//! Remember the side convention: an order's `side` is the MAKER's side, so a
//! maker `sell` order fills a taker who wants to `buy` (it is the taker's
//! ask), and a maker `buy` order fills a taker who wants to `sell` (the
//! taker's bid).

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use common::client::RfqClient;
use common::signing::unix_millis;
use common::types::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::prices::PriceBook;

/// Don't bother cancel/replacing this close to the window close; the swap
/// would race the cutoff for no benefit. (Sniper races on purpose anyway.)
const REQUOTE_QUIET_MS: u64 = 75;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Strategy {
    Tight,
    Lazy,
    Skew,
    Sniper,
}

/// Shared state for one running maker process.
pub struct Ctx {
    pub client: RfqClient,
    pub prices: Arc<PriceBook>,
    pub strategy: Strategy,
    pub rng_seed: u64,
    /// Monotonic source of client_order_ids (unique among our open orders).
    pub next_cid: AtomicU64,
    /// Counts RFQs seen, used to derive a per-RFQ RNG from `rng_seed`.
    pub rfq_counter: AtomicU64,
    /// Every order id the server has accepted from us (win detection).
    pub my_orders: Mutex<HashSet<Uuid>>,
}

/// The fields of `rfq_opened` a strategy needs.
pub struct OpenRfq {
    pub rfq_id: Uuid,
    pub asset: String,
    pub size: u64,
    /// Present only when the taker chose to reveal a direction.
    pub direction: Option<Side>,
    pub closes_at: u64,
}

impl OpenRfq {
    /// The maker sides worth quoting: the opposite of a revealed taker
    /// direction, or both sides when the direction is hidden.
    fn useful_sides(&self) -> Vec<Side> {
        match self.direction {
            Some(taker_side) => vec![taker_side.opposite()],
            None => vec![Side::Sell, Side::Buy],
        }
    }
}

pub async fn handle_rfq(ctx: Arc<Ctx>, rfq: OpenRfq) {
    let n = ctx.rfq_counter.fetch_add(1, Ordering::SeqCst);
    let rng = StdRng::seed_from_u64(ctx.rng_seed.wrapping_add(n));
    let Some(mark) = ctx.prices.get(&rfq.asset) else {
        tracing::warn!(asset = %rfq.asset, "no mark price; not quoting");
        return;
    };
    let result = match ctx.strategy {
        Strategy::Tight => tight(&ctx, &rfq, mark).await,
        Strategy::Lazy => lazy(&ctx, &rfq, mark).await,
        Strategy::Skew => skew(&ctx, &rfq, mark, rng).await,
        Strategy::Sniper => sniper(&ctx, &rfq, mark, rng).await,
    };
    if let Err(e) = result {
        tracing::warn!(rfq = %rfq.rfq_id, "strategy error: {e:#}");
    }
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Instant two-sided quotes at ±10 bps, exact size, always tagged with a
/// client_order_id. Quotes both sides even for directed RFQs (allowed; the
/// off-direction order just gets released at the end). Re-quotes on every
/// mark-price tick until the window closes.
async fn tight(ctx: &Ctx, rfq: &OpenRfq, mark: u64) -> Result<()> {
    track_quotes(
        ctx,
        rfq,
        mark,
        QuotePlan {
            sides: vec![Side::Sell, Side::Buy],
            mid_shift_bps: 0,
            half_spread_bps: 10,
            size: rfq.size,
            min_move_bps: 0,
            use_cid: true,
        },
    )
    .await
}

/// Thinks about it for 500 ms, then quotes wide (±150 bps) at three times
/// the requested size. Only bothers with the useful side(s), and only
/// re-quotes when the mark has drifted at least 15 bps — it's lazy.
async fn lazy(ctx: &Ctx, rfq: &OpenRfq, mark: u64) -> Result<()> {
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mark = ctx.prices.get(&rfq.asset).unwrap_or(mark);
    track_quotes(
        ctx,
        rfq,
        mark,
        QuotePlan {
            sides: rfq.useful_sides(),
            mid_shift_bps: 0,
            half_spread_bps: 150,
            size: rfq.size.saturating_mul(3),
            min_move_bps: 15,
            use_cid: false,
        },
    )
    .await
}

/// Prices off its own inventory: the more of the asset it holds (measured in
/// multiples of the RFQ size), the lower it shifts both quotes, making it
/// eager to sell and reluctant to buy. Occasionally over-commits on purpose
/// to exercise the server's reservation checks. Re-quotes when the mark
/// drifts 5 bps or more (the inventory skew is computed once per RFQ).
async fn skew(ctx: &Ctx, rfq: &OpenRfq, mark: u64, mut rng: StdRng) -> Result<()> {
    let balances = ctx.client.balances_of(&ctx.client.pubkey()).await?;
    let lots_available = balances
        .assets
        .get(&rfq.asset)
        .map(|b| b.available)
        .unwrap_or(0);

    // Inventory measured in "fills of this RFQ"; 10 is the comfort point.
    let fills_of_inventory = (lots_available / rfq.size.max(1)).min(35) as i64;
    let skew_bps = 10 - fills_of_inventory; // long => negative => shift down

    if rng.gen_bool(0.25) {
        // Deliberately try to sell more than we have; the server must
        // reject this with `insufficient_balance` and reserve nothing.
        let mid = with_bps(mark, skew_bps);
        let too_big = lots_available.saturating_add(rfq.size).saturating_mul(2);
        tracing::info!(
            rfq = %rfq.rfq_id,
            size = too_big,
            "over-committing on purpose (expect insufficient_balance)"
        );
        place(ctx, rfq.rfq_id, Side::Sell, quote_price(mid, Side::Sell, 30), too_big, None).await;
    }

    track_quotes(
        ctx,
        rfq,
        mark,
        QuotePlan {
            sides: rfq.useful_sides(),
            mid_shift_bps: skew_bps,
            half_spread_bps: 30,
            size: rfq.size,
            min_move_bps: 5,
            use_cid: false,
        },
    )
    .await
}

/// Ignores 30% of RFQs. Otherwise places a wide throwaway quote immediately,
/// then just before the window closes cancels it by client_order_id and
/// re-quotes razor-thin (±2 bps) at the freshest mark price it has. A
/// quarter of the time it waits until 20 ms before the close, deliberately
/// risking losing the race — a late cancel or re-quote getting
/// `rfq_not_open` is expected and tolerated.
async fn sniper(ctx: &Ctx, rfq: &OpenRfq, mark: u64, mut rng: StdRng) -> Result<()> {
    if rng.gen_bool(0.3) {
        tracing::info!(rfq = %rfq.rfq_id, "sniper sits this one out");
        return Ok(());
    }

    let mut placed = Vec::new();
    for side in rfq.useful_sides() {
        let cid = ctx.next_cid.fetch_add(1, Ordering::SeqCst);
        if place(ctx, rfq.rfq_id, side, quote_price(mark, side, 500), rfq.size, Some(cid))
            .await
            .is_some()
        {
            placed.push((side, cid));
        }
    }

    let lead_ms: u64 = if rng.gen_bool(0.25) { 20 } else { 200 };
    let wake = rfq.closes_at.saturating_sub(lead_ms);
    let now = unix_millis();
    if wake > now {
        tokio::time::sleep(Duration::from_millis(wake - now)).await;
    }

    // Snipe off the freshest mark: the live feed may have ticked since the
    // RFQ opened, and quoting ±2 bps only makes sense against current fair.
    let mark = ctx.prices.get(&rfq.asset).unwrap_or(mark);
    for (side, cid) in placed {
        match ctx
            .client
            .cancel_order(CancelOrderPayload {
                rfq_id: rfq.rfq_id,
                order_id: None,
                client_order_id: Some(cid),
            })
            .await
        {
            Ok(_) => tracing::debug!(rfq = %rfq.rfq_id, cid, "cancelled throwaway quote"),
            Err(e) => {
                tracing::info!(rfq = %rfq.rfq_id, cid, "late cancel lost the race (fine): {e:#}");
                continue;
            }
        }
        let cid = ctx.next_cid.fetch_add(1, Ordering::SeqCst);
        place(ctx, rfq.rfq_id, side, quote_price(mark, side, 2), rfq.size, Some(cid)).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Live quote tracking
// ---------------------------------------------------------------------------

/// How a strategy wants its resting quotes shaped.
struct QuotePlan {
    /// Maker sides to quote.
    sides: Vec<Side>,
    /// Shift applied to the mark to get the strategy's mid (skew).
    mid_shift_bps: i64,
    /// Half-spread around that mid.
    half_spread_bps: i64,
    /// Order size in lots.
    size: u64,
    /// Ignore mark moves smaller than this (0 = re-quote on every tick).
    min_move_bps: u64,
    /// Tag orders with client_order_ids and cancel by them (else by UUID).
    use_cid: bool,
}

/// One resting order we placed and may need to cancel on a re-quote.
struct OpenQuote {
    order_id: Uuid,
    client_order_id: Option<u64>,
}

/// Place the plan's quotes, then keep them pinned to the fair price: every
/// time the mark moves at least `min_move_bps`, cancel and replace them.
/// Runs until just before the window closes; whatever rests then is the
/// final quote.
async fn track_quotes(ctx: &Ctx, rfq: &OpenRfq, mut mark: u64, plan: QuotePlan) -> Result<()> {
    // Subscribe before the first placement so no tick can slip between.
    let mut changes = ctx.prices.watch();
    let mut open = place_set(ctx, rfq, &plan, mark).await;

    loop {
        let now = unix_millis();
        if rfq.closes_at <= now + REQUOTE_QUIET_MS {
            break;
        }
        let until_close = Duration::from_millis(rfq.closes_at - now);
        let symbol = tokio::select! {
            _ = tokio::time::sleep(until_close) => break,
            changed = changes.recv() => match changed {
                Ok(symbol) => symbol,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
        };
        if symbol != rfq.asset {
            continue;
        }
        let Some(new_mark) = ctx.prices.get(&rfq.asset) else {
            continue;
        };
        if new_mark == mark || move_bps(mark, new_mark) < plan.min_move_bps {
            continue;
        }
        mark = new_mark;
        tracing::debug!(rfq = %rfq.rfq_id, mark, "fair price moved; re-quoting");
        for quote in open.drain(..) {
            cancel_quote(ctx, rfq.rfq_id, quote).await;
        }
        open = place_set(ctx, rfq, &plan, mark).await;
    }
    Ok(())
}

/// Place one order per planned side around `mark`; returns what rested.
async fn place_set(ctx: &Ctx, rfq: &OpenRfq, plan: &QuotePlan, mark: u64) -> Vec<OpenQuote> {
    let mid = with_bps(mark, plan.mid_shift_bps);
    let mut open = Vec::new();
    for &side in &plan.sides {
        let cid = plan
            .use_cid
            .then(|| ctx.next_cid.fetch_add(1, Ordering::SeqCst));
        let price = quote_price(mid, side, plan.half_spread_bps);
        if let Some(order_id) = place(ctx, rfq.rfq_id, side, price, plan.size, cid).await {
            open.push(OpenQuote {
                order_id,
                client_order_id: cid,
            });
        }
    }
    open
}

/// Cancel a resting quote by whichever id we tagged it with. Losing to the
/// window close is fine and expected near the cutoff.
async fn cancel_quote(ctx: &Ctx, rfq_id: Uuid, quote: OpenQuote) {
    let payload = match quote.client_order_id {
        Some(cid) => CancelOrderPayload {
            rfq_id,
            order_id: None,
            client_order_id: Some(cid),
        },
        None => CancelOrderPayload {
            rfq_id,
            order_id: Some(quote.order_id),
            client_order_id: None,
        },
    };
    match ctx.client.cancel_order(payload).await {
        Ok(_) => tracing::debug!(%rfq_id, order = %quote.order_id, "cancelled for re-quote"),
        Err(e) => {
            tracing::info!(%rfq_id, order = %quote.order_id, "re-quote cancel lost a race (fine): {e:#}")
        }
    }
}

/// Absolute move between two prices in basis points of the old price.
fn move_bps(old: u64, new: u64) -> u64 {
    (old.abs_diff(new) as u128 * 10_000 / old.max(1) as u128) as u64
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Place an order, log the outcome, and remember accepted order ids.
/// Rejections are logged but never fatal — several strategies provoke them
/// on purpose.
async fn place(
    ctx: &Ctx,
    rfq_id: Uuid,
    side: Side,
    price: u64,
    size: u64,
    client_order_id: Option<u64>,
) -> Option<Uuid> {
    let payload = PlaceOrderPayload {
        rfq_id,
        side,
        price,
        size,
        client_order_id,
    };
    match ctx.client.place_order(payload).await {
        Ok(resp) => {
            ctx.my_orders.lock().unwrap().insert(resp.order_id);
            tracing::info!(%rfq_id, %side, price, size, order = %resp.order_id, "order placed");
            Some(resp.order_id)
        }
        Err(e) => {
            tracing::info!(%rfq_id, %side, price, size, "order rejected: {e:#}");
            None
        }
    }
}

/// `price` shifted by `delta_bps` basis points (never below 1 micro-USD).
fn with_bps(price: u64, delta_bps: i64) -> u64 {
    let shifted = price as i128 * (10_000 + delta_bps as i128) / 10_000;
    shifted.max(1) as u64
}

/// A maker quote `half_spread_bps` away from `mid`: sells above, buys below.
fn quote_price(mid: u64, maker_side: Side, half_spread_bps: i64) -> u64 {
    match maker_side {
        Side::Sell => with_bps(mid, half_spread_bps),
        Side::Buy => with_bps(mid, -half_spread_bps),
    }
}

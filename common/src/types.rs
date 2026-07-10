//! Wire types for the RFQ coordinator protocol.
//!
//! Units:
//! - USD amounts are integer micro-USD (1_000_000 = $1).
//! - Asset sizes are integer lots; each asset defines `lot_decimals`
//!   (1 whole unit = 10^lot_decimals lots).
//! - Prices are integer micro-USD per whole unit of the asset.
//! - Timestamps are unix milliseconds. IDs are UUID v4 strings.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::keys::Pubkey;
use crate::signing::SignablePayload;

// ---------------------------------------------------------------------------
// Core enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(&self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }

    pub fn opposite(&self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Side {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "buy" => Ok(Side::Buy),
            "sell" => Ok(Side::Sell),
            other => anyhow::bail!("invalid side: {other:?} (expected \"buy\" or \"sell\")"),
        }
    }
}

/// RFQ lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RfqState {
    Open,
    AwaitingAccept,
    Settled,
    Expired,
    Declined,
    Failed,
}

impl RfqState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, RfqState::Open | RfqState::AwaitingAccept)
    }
}

// ---------------------------------------------------------------------------
// Amount math
// ---------------------------------------------------------------------------

/// The exact micro-USD amount that moves at settlement:
/// `floor(size_lots * price_micro_usd / 10^lot_decimals)`.
///
/// The intermediate product is computed in 128 bits. The same single number
/// is debited from the buyer and credited to the seller, so settlement is
/// exactly zero-sum.
pub fn notional_micro_usd(size_lots: u64, price_micro_usd: u64, lot_decimals: u32) -> u64 {
    let product = size_lots as u128 * price_micro_usd as u128;
    (product / 10u128.pow(lot_decimals)) as u64
}

// ---------------------------------------------------------------------------
// Signed request payloads
// ---------------------------------------------------------------------------

fn opt_field<T: ToString>(v: &Option<T>) -> String {
    match v {
        Some(v) => v.to_string(),
        None => "-".to_string(),
    }
}

/// `POST /v1/rfqs` — action `submit_rfq`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRfqPayload {
    pub asset: String,
    /// If present, makers are told the taker's side. If absent, makers do
    /// not learn the direction and the taker receives quotes on both sides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Side>,
    /// Requested size in lots.
    pub size: u64,
    /// Quoting window in seconds. Must be in `1..=29`.
    pub ttl_secs: u64,
}

impl SignablePayload for SubmitRfqPayload {
    fn action(&self) -> &'static str {
        "submit_rfq"
    }
    fn signed_fields(&self) -> Vec<String> {
        vec![
            self.asset.clone(),
            opt_field(&self.direction),
            self.size.to_string(),
            self.ttl_secs.to_string(),
        ]
    }
}

/// `POST /v1/orders` — action `place_order`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderPayload {
    pub rfq_id: Uuid,
    /// The MAKER's side: a maker `sell` order fills a taker `buy`.
    pub side: Side,
    /// Micro-USD per whole unit.
    pub price: u64,
    /// Size in lots. Must be >= the RFQ's requested size.
    pub size: u64,
    /// Optional maker-chosen id, unique among the maker's open orders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<u64>,
}

impl SignablePayload for PlaceOrderPayload {
    fn action(&self) -> &'static str {
        "place_order"
    }
    fn signed_fields(&self) -> Vec<String> {
        vec![
            self.rfq_id.to_string(),
            self.side.to_string(),
            self.price.to_string(),
            self.size.to_string(),
            opt_field(&self.client_order_id),
        ]
    }
}

/// `POST /v1/orders/cancel` — action `cancel_order`.
///
/// Exactly one of `order_id` / `client_order_id` must be set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderPayload {
    pub rfq_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<u64>,
}

impl SignablePayload for CancelOrderPayload {
    fn action(&self) -> &'static str {
        "cancel_order"
    }
    fn signed_fields(&self) -> Vec<String> {
        // order_ref is `id:{uuid}` or `cid:{u64}`.
        let order_ref = match (&self.order_id, &self.client_order_id) {
            (Some(id), _) => format!("id:{id}"),
            (None, Some(cid)) => format!("cid:{cid}"),
            (None, None) => "-".to_string(),
        };
        vec![self.rfq_id.to_string(), order_ref]
    }
}

/// `POST /v1/accept` — action `accept`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptPayload {
    pub rfq_id: Uuid,
    /// The TAKER's side: `buy` lifts the best ask, `sell` hits the best bid.
    /// For a directional RFQ this must equal the RFQ's direction.
    pub side: Side,
}

impl SignablePayload for AcceptPayload {
    fn action(&self) -> &'static str {
        "accept"
    }
    fn signed_fields(&self) -> Vec<String> {
        vec![self.rfq_id.to_string(), self.side.to_string()]
    }
}

/// `POST /v1/decline` — action `decline`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclinePayload {
    pub rfq_id: Uuid,
}

impl SignablePayload for DeclinePayload {
    fn action(&self) -> &'static str {
        "decline"
    }
    fn signed_fields(&self) -> Vec<String> {
        vec![self.rfq_id.to_string()]
    }
}

/// First WS frame on `GET /ws/rfq/{rfq_id}` — action `subscribe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribePayload {
    pub rfq_id: Uuid,
}

impl SignablePayload for SubscribePayload {
    fn action(&self) -> &'static str {
        "subscribe"
    }
    fn signed_fields(&self) -> Vec<String> {
        vec![self.rfq_id.to_string()]
    }
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitRfqResponse {
    pub rfq_id: Uuid,
    pub created_at: u64,
    pub closes_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderResponse {
    pub order_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderResponse {
    pub order_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub trade_id: Uuid,
    pub rfq_id: Uuid,
    pub asset: String,
    pub taker_side: Side,
    pub price: u64,
    pub size: u64,
    pub notional_usd: u64,
    pub executed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptResponse {
    pub trade: Trade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeclineResponse {
    pub rfq_id: Uuid,
    pub state: RfqState,
}

// ---------------------------------------------------------------------------
// Unauthenticated views
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetInfo {
    pub symbol: String,
    pub lot_decimals: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetsResponse {
    pub assets: Vec<AssetInfo>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct BalanceView {
    pub total: u64,
    pub reserved: u64,
    pub available: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderBalances {
    pub pubkey: Pubkey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub usd: BalanceView,
    pub assets: std::collections::BTreeMap<String, BalanceView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalancesResponse {
    pub traders: Vec<TraderBalances>,
}

/// Public RFQ snapshot (`GET /v1/rfqs/{rfq_id}`). Never reveals the
/// direction — same information the maker feed carries once the RFQ opened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqView {
    pub rfq_id: Uuid,
    pub asset: String,
    pub size: u64,
    pub state: RfqState,
    pub created_at: u64,
    pub closes_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_deadline: Option<u64>,
}

// ---------------------------------------------------------------------------
// WebSocket events
// ---------------------------------------------------------------------------

/// One side of a quote as delivered to the taker.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QuoteLevel {
    /// Micro-USD per whole unit.
    pub price: u64,
}

/// Events on the maker broadcast feed `GET /ws/makers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MakerEvent {
    RfqOpened {
        rfq_id: Uuid,
        asset: String,
        size: u64,
        /// Present if and only if the taker specified a direction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<Side>,
        created_at: u64,
        closes_at: u64,
    },
    RfqWindowClosed {
        rfq_id: Uuid,
    },
    RfqDone {
        rfq_id: Uuid,
        outcome: RfqOutcome,
        /// Exactly one entry when `outcome == settled`, else empty.
        fills: Vec<Fill>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RfqOutcome {
    Settled,
    Expired,
    Declined,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: Uuid,
    pub maker_side: Side,
    pub price: u64,
    pub size: u64,
}

/// Events on the taker socket `GET /ws/rfq/{rfq_id}` (server -> client).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TakerEvent {
    /// Ack + state snapshot, sent immediately after a valid subscribe.
    Subscribed {
        rfq_id: Uuid,
        state: RfqState,
        closes_at: u64,
    },
    /// Sent at window end when at least one side has an eligible order.
    /// `ask` is the price at which the taker may BUY; `bid` is the price at
    /// which the taker may SELL. For a directional RFQ only the relevant
    /// side is present.
    Quote {
        rfq_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bid: Option<QuoteLevel>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask: Option<QuoteLevel>,
        size: u64,
        accept_deadline: u64,
    },
    /// Sent at window end when no eligible orders exist on any quotable side.
    NoQuote { rfq_id: Uuid },
    Settled { rfq_id: Uuid, trade: Trade },
    Expired { rfq_id: Uuid },
    Declined { rfq_id: Uuid },
    Failed { rfq_id: Uuid },
    /// Subscribe failure (bad signature, wrong subscriber, unknown RFQ).
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every non-2xx HTTP response body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: ApiErrorBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
}

/// The error codes, grouped by the HTTP status they ride on.
pub mod error_codes {
    // 400
    pub const MALFORMED_REQUEST: &str = "malformed_request";
    // 401
    pub const INVALID_SIGNATURE: &str = "invalid_signature";
    pub const UNKNOWN_TRADER: &str = "unknown_trader";
    pub const STALE_NONCE: &str = "stale_nonce";
    // 403
    pub const NOT_YOUR_ORDER: &str = "not_your_order";
    pub const NOT_YOUR_RFQ: &str = "not_your_rfq";
    // 404
    pub const RFQ_NOT_FOUND: &str = "rfq_not_found";
    pub const ORDER_NOT_FOUND: &str = "order_not_found";
    // 409
    pub const RFQ_NOT_OPEN: &str = "rfq_not_open";
    pub const NOT_AWAITING_ACCEPT: &str = "not_awaiting_accept";
    // 422
    pub const UNKNOWN_ASSET: &str = "unknown_asset";
    pub const INVALID_SIZE: &str = "invalid_size";
    pub const INVALID_TTL: &str = "invalid_ttl";
    pub const ORDER_TOO_SMALL: &str = "order_too_small";
    pub const DUPLICATE_CLIENT_ORDER_ID: &str = "duplicate_client_order_id";
    pub const SELF_QUOTE_NOT_ALLOWED: &str = "self_quote_not_allowed";
    pub const SIDE_UNAVAILABLE: &str = "side_unavailable";
    pub const INSUFFICIENT_BALANCE: &str = "insufficient_balance";
}

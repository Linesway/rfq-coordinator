//! Thin HTTP/WebSocket client for the RFQ coordinator.
//!
//! Used by the CLI and the makers (and by our test harness). Server
//! implementations do not need this module, but it documents exactly how
//! clients drive the protocol.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use crate::keys::{Keypair, Pubkey};
use crate::signing::{unix_millis, Envelope, SignablePayload};
use crate::types::*;

/// A structured error returned by the server (non-2xx response).
#[derive(Debug, Clone)]
pub struct ApiFailure {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.status, self.code, self.message)
    }
}

impl std::error::Error for ApiFailure {}

pub type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A signing client bound to one keypair. Both takers and makers use this.
pub struct RfqClient {
    base_url: String,
    http: reqwest::Client,
    keypair: Keypair,
    nonce: std::sync::atomic::AtomicU64,
}

impl RfqClient {
    /// `base_url` e.g. `http://127.0.0.1:4402` (no trailing slash).
    pub fn new(base_url: impl Into<String>, keypair: Keypair) -> Self {
        RfqClient {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            keypair,
            // Nonces must be strictly increasing per pubkey; unix millis is a
            // convenient starting point that survives restarts.
            nonce: std::sync::atomic::AtomicU64::new(unix_millis()),
        }
    }

    pub fn pubkey(&self) -> Pubkey {
        self.keypair.pubkey()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn ws_url(&self, path: &str) -> String {
        let ws_base = self
            .base_url
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1);
        format!("{ws_base}{path}")
    }

    fn next_nonce(&self) -> u64 {
        self.nonce
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Sign a payload with the next nonce.
    pub fn envelope<T: SignablePayload>(&self, payload: T) -> Envelope<T> {
        Envelope::sign(&self.keypair, self.next_nonce(), payload)
    }

    async fn post<T: SignablePayload + serde::Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        payload: T,
    ) -> Result<R> {
        let envelope = self.envelope(payload);
        let resp = self
            .http
            .post(format!("{}{path}", self.base_url))
            .json(&envelope)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        Self::decode(resp).await
    }

    async fn decode<R: DeserializeOwned>(resp: reqwest::Response) -> Result<R> {
        let status = resp.status();
        let body = resp.bytes().await.context("reading response body")?;
        if status.is_success() {
            serde_json::from_slice(&body).context("decoding response body")
        } else {
            let failure = match serde_json::from_slice::<ApiError>(&body) {
                Ok(err) => ApiFailure {
                    status: status.as_u16(),
                    code: err.error.code,
                    message: err.error.message,
                },
                Err(_) => ApiFailure {
                    status: status.as_u16(),
                    code: "unknown".to_string(),
                    message: String::from_utf8_lossy(&body).into_owned(),
                },
            };
            Err(failure.into())
        }
    }

    // -- signed actions ----------------------------------------------------

    pub async fn submit_rfq(&self, payload: SubmitRfqPayload) -> Result<SubmitRfqResponse> {
        self.post("/v1/rfqs", payload).await
    }

    pub async fn place_order(&self, payload: PlaceOrderPayload) -> Result<PlaceOrderResponse> {
        self.post("/v1/orders", payload).await
    }

    pub async fn cancel_order(&self, payload: CancelOrderPayload) -> Result<CancelOrderResponse> {
        self.post("/v1/orders/cancel", payload).await
    }

    pub async fn accept(&self, rfq_id: Uuid, side: Side) -> Result<AcceptResponse> {
        self.post("/v1/accept", AcceptPayload { rfq_id, side }).await
    }

    pub async fn decline(&self, rfq_id: Uuid) -> Result<DeclineResponse> {
        self.post("/v1/decline", DeclinePayload { rfq_id }).await
    }

    // -- unauthenticated views ----------------------------------------------

    pub async fn health(&self) -> Result<()> {
        let resp = self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await?;
        anyhow::ensure!(resp.status().is_success(), "health check failed");
        Ok(())
    }

    pub async fn assets(&self) -> Result<AssetsResponse> {
        let resp = self
            .http
            .get(format!("{}/v1/assets", self.base_url))
            .send()
            .await?;
        Self::decode(resp).await
    }

    pub async fn balances(&self) -> Result<BalancesResponse> {
        let resp = self
            .http
            .get(format!("{}/v1/balances", self.base_url))
            .send()
            .await?;
        Self::decode(resp).await
    }

    pub async fn balances_of(&self, pubkey: &Pubkey) -> Result<TraderBalances> {
        let resp = self
            .http
            .get(format!("{}/v1/balances/{}", self.base_url, pubkey))
            .send()
            .await?;
        Self::decode(resp).await
    }

    pub async fn rfq_view(&self, rfq_id: Uuid) -> Result<RfqView> {
        let resp = self
            .http
            .get(format!("{}/v1/rfqs/{rfq_id}", self.base_url))
            .send()
            .await?;
        Self::decode(resp).await
    }

    // -- websockets ---------------------------------------------------------

    /// Connect to the maker broadcast feed.
    pub async fn maker_feed(&self) -> Result<MakerFeed> {
        let (stream, _) = connect_async(self.ws_url("/ws/makers"))
            .await
            .context("connecting to /ws/makers")?;
        Ok(MakerFeed { stream })
    }

    /// Connect to the taker socket for `rfq_id` and authenticate with a
    /// signed `subscribe` frame. Returns the stream after the server's
    /// `subscribed` ack (which is also returned, since it carries a state
    /// snapshot).
    pub async fn subscribe_rfq(&self, rfq_id: Uuid) -> Result<(TakerFeed, TakerEvent)> {
        let (mut stream, _) = connect_async(self.ws_url(&format!("/ws/rfq/{rfq_id}")))
            .await
            .context("connecting to /ws/rfq/{rfq_id}")?;
        let envelope = self.envelope(SubscribePayload { rfq_id });
        stream
            .send(Message::Text(serde_json::to_string(&envelope)?))
            .await
            .context("sending subscribe frame")?;
        let mut feed = TakerFeed { stream };
        let first = feed
            .next_event()
            .await?
            .context("server closed the socket before acking the subscribe")?;
        if let TakerEvent::Error { message } = &first {
            anyhow::bail!("subscribe rejected: {message}");
        }
        Ok((feed, first))
    }
}

/// The maker broadcast feed: a stream of [`MakerEvent`]s.
pub struct MakerFeed {
    stream: WsStream,
}

impl MakerFeed {
    /// Next event, skipping non-text frames. `None` means the server closed.
    pub async fn next_event(&mut self) -> Result<Option<MakerEvent>> {
        next_json(&mut self.stream).await
    }
}

/// The per-RFQ taker feed: a stream of [`TakerEvent`]s.
pub struct TakerFeed {
    stream: WsStream,
}

impl TakerFeed {
    /// Next event, skipping non-text frames. `None` means the server closed.
    pub async fn next_event(&mut self) -> Result<Option<TakerEvent>> {
        next_json(&mut self.stream).await
    }
}

async fn next_json<E: DeserializeOwned>(stream: &mut WsStream) -> Result<Option<E>> {
    while let Some(frame) = stream.next().await {
        match frame.context("websocket error")? {
            Message::Text(text) => {
                return serde_json::from_str(&text)
                    .with_context(|| format!("decoding ws event: {text}"))
                    .map(Some);
            }
            Message::Close(_) => return Ok(None),
            _ => continue,
        }
    }
    Ok(None)
}

//! Mark-price cache: live Phoenix Perps prices over a static TOML fallback.
//!
//! The static file is loaded once at startup and is always a complete price
//! book. When live pricing is enabled, [`LiveFeed`] opens a Rise SDK
//! websocket stream per symbol — but only while at least one RFQ in that
//! symbol is active. The last RFQ in a symbol tears its stream down, and
//! when no RFQs are active at all the websocket connection itself is
//! dropped, so an idle maker makes zero API requests.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use phoenix_rise::api::{PhoenixWSClient, SubscriptionHandle};
use tokio::sync::broadcast;

const PHOENIX_WS: &str = "wss://perp-api.phoenix.trade/ws";

pub struct PriceBook {
    /// Symbol -> micro-USD per whole unit.
    prices: RwLock<BTreeMap<String, u64>>,
    /// Notifies watchers with the symbol whenever a price actually changes.
    changed: broadcast::Sender<String>,
}

#[derive(serde::Deserialize)]
struct PricesFile {
    prices: BTreeMap<String, u64>,
}

impl PriceBook {
    /// Load the static fallback file and sanity-check it against the
    /// server's asset list.
    pub fn load_static(path: &str, symbols: &[String]) -> Result<Arc<Self>> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading static price file {path}"))?;
        let file: PricesFile =
            toml::from_str(&text).with_context(|| format!("parsing {path}"))?;
        for symbol in symbols {
            if !file.prices.contains_key(symbol) {
                tracing::warn!(
                    "{path} has no price for {symbol}; it won't be quoted \
                     unless the live feed supplies one"
                );
            }
        }
        Ok(Arc::new(PriceBook {
            prices: RwLock::new(file.prices),
            changed: broadcast::channel(256).0,
        }))
    }

    /// Current mark price for `symbol` in micro-USD, if known.
    pub fn get(&self, symbol: &str) -> Option<u64> {
        self.prices.read().unwrap().get(symbol).copied()
    }

    /// Subscribe to price-change notifications (the changed symbol).
    pub fn watch(&self) -> broadcast::Receiver<String> {
        self.changed.subscribe()
    }

    fn set(&self, symbol: &str, micro: u64) {
        let old = self
            .prices
            .write()
            .unwrap()
            .insert(symbol.to_string(), micro);
        if old != Some(micro) {
            let _ = self.changed.send(symbol.to_string());
        }
    }
}

/// Refcounted live price streams, one per symbol with an active RFQ.
///
/// Owned by the maker's main event loop: `acquire` on `rfq_opened`,
/// `release` on `rfq_done`. Any live-feed failure is warned about once and
/// then quietly falls back to the static book.
pub struct LiveFeed {
    book: Arc<PriceBook>,
    client: Option<PhoenixWSClient>,
    streams: HashMap<String, Stream>,
    warned: bool,
}

struct Stream {
    refcount: usize,
    /// Dropping this unsubscribes from the symbol on the server.
    _handle: SubscriptionHandle,
    drain: tokio::task::JoinHandle<()>,
}

impl LiveFeed {
    pub fn new(book: Arc<PriceBook>) -> Self {
        LiveFeed {
            book,
            client: None,
            streams: HashMap::new(),
            warned: false,
        }
    }

    /// An RFQ in `symbol` opened: start (or share) its price stream.
    pub fn acquire(&mut self, symbol: &str) {
        if let Some(stream) = self.streams.get_mut(symbol) {
            stream.refcount += 1;
            return;
        }
        let Some(client) = self.ensure_client() else {
            return;
        };
        match client.subscribe_to_market(symbol.to_string()) {
            Ok((mut rx, handle)) => {
                tracing::debug!(%symbol, "live price stream started");
                let book = self.book.clone();
                let sym = symbol.to_string();
                let drain = tokio::spawn(async move {
                    while let Some(update) = rx.recv().await {
                        if let Some(micro) = to_micro_usd(update.mark_price) {
                            tracing::debug!(symbol = %sym, micro, "live mark price");
                            book.set(&sym, micro);
                        }
                    }
                });
                self.streams.insert(
                    symbol.to_string(),
                    Stream {
                        refcount: 1,
                        _handle: handle,
                        drain,
                    },
                );
            }
            Err(e) => {
                // The connection task is gone (e.g. the initial connect
                // failed); drop the client so the next RFQ retries fresh.
                self.client = None;
                self.warn_once(&format!("subscribing to {symbol}: {e}"));
            }
        }
    }

    /// An RFQ in `symbol` finished: drop its stream if it was the last one.
    pub fn release(&mut self, symbol: &str) {
        let Some(stream) = self.streams.get_mut(symbol) else {
            return; // acquire failed earlier; nothing to release
        };
        stream.refcount -= 1;
        if stream.refcount > 0 {
            return;
        }
        let stream = self.streams.remove(symbol).unwrap();
        stream.drain.abort();
        drop(stream._handle); // unsubscribe
        tracing::debug!(%symbol, "live price stream stopped");
        if self.streams.is_empty() {
            // No active RFQs anywhere: close the websocket entirely.
            self.client = None;
        }
    }

    fn ensure_client(&mut self) -> Option<&PhoenixWSClient> {
        if self.client.is_none() {
            match PhoenixWSClient::new(PHOENIX_WS) {
                Ok(client) => self.client = Some(client),
                Err(e) => {
                    self.warn_once(&format!("connecting to {PHOENIX_WS}: {e}"));
                    return None;
                }
            }
        }
        self.client.as_ref()
    }

    fn warn_once(&mut self, what: &str) {
        if !self.warned {
            self.warned = true;
            tracing::warn!(
                "live price feed failed ({what}); using static prices \
                 (pass --offline to silence this)"
            );
        }
    }
}

/// USD-per-unit float from the feed -> micro-USD, rejecting garbage.
fn to_micro_usd(price: f64) -> Option<u64> {
    let micro = (price * 1_000_000.0).round();
    if micro.is_finite() && micro >= 1.0 {
        Some(micro as u64)
    } else {
        None
    }
}

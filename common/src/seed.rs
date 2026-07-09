//! The TOML seed file that initializes the server's world.
//!
//! ```toml
//! [assets.SOL]
//! lot_decimals = 3        # 1 lot = 0.001 SOL
//!
//! [[traders]]
//! name = "maker-tight"    # display only, optional
//! pubkey = "<64 hex>"     # ed25519 public key
//! usd = 1_000_000_000_000 # micro-USD
//! [traders.assets]
//! SOL = 5_000_000         # lots
//! ```
//!
//! Traders may omit assets (implicitly zero). Referencing an asset that is
//! not declared under `[assets]` is a startup error.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::keys::Pubkey;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedConfig {
    /// Asset definitions, keyed by symbol (e.g. "SOL").
    pub assets: BTreeMap<String, AssetDef>,
    #[serde(default)]
    pub traders: Vec<TraderSeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDef {
    /// 1 whole unit = 10^lot_decimals lots.
    pub lot_decimals: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderSeed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub pubkey: Pubkey,
    /// Micro-USD.
    pub usd: u64,
    /// Lots per asset symbol.
    #[serde(default)]
    pub assets: BTreeMap<String, u64>,
}

impl SeedConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading seed file {}", path.display()))?;
        let config: SeedConfig = toml::from_str(&raw)
            .with_context(|| format!("parsing seed file {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for trader in &self.traders {
            if !seen.insert(trader.pubkey) {
                bail!("duplicate trader pubkey in seed: {}", trader.pubkey);
            }
            for symbol in trader.assets.keys() {
                if !self.assets.contains_key(symbol) {
                    bail!(
                        "trader {} holds unknown asset {symbol:?} (not declared under [assets])",
                        trader.pubkey
                    );
                }
            }
        }
        Ok(())
    }
}

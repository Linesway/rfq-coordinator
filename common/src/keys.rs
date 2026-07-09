//! Ed25519 keypair handling.
//!
//! Keypairs are stored on disk as JSON:
//! `{"secret_key": "<64 hex chars>", "public_key": "<64 hex chars>"}`
//! where `secret_key` is the 32-byte ed25519 seed and `public_key` is the
//! corresponding 32-byte public key, both lowercase hex.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// A 32-byte ed25519 public key, rendered as 64 lowercase hex chars on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pubkey(pub [u8; 32]);

impl Pubkey {
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn verifying_key(&self) -> Result<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0).context("invalid ed25519 public key")
    }
}

impl fmt::Display for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pubkey({})", self.to_hex())
    }
}

impl FromStr for Pubkey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = hex::decode(s).context("pubkey must be hex")?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("pubkey must be exactly 32 bytes (64 hex chars)"))?;
        Ok(Pubkey(arr))
    }
}

impl Serialize for Pubkey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Pubkey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// An ed25519 keypair loaded from a JSON key file.
pub struct Keypair {
    signing_key: SigningKey,
}

#[derive(Serialize, Deserialize)]
struct KeyFile {
    secret_key: String,
    public_key: String,
}

impl Keypair {
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        Keypair { signing_key }
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Keypair {
            signing_key: SigningKey::from_bytes(&seed),
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading key file {}", path.display()))?;
        let file: KeyFile = serde_json::from_str(&raw)
            .with_context(|| format!("parsing key file {}", path.display()))?;
        let seed_bytes = hex::decode(&file.secret_key).context("secret_key must be hex")?;
        let seed: [u8; 32] = seed_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("secret_key must be exactly 32 bytes (64 hex chars)"))?;
        let kp = Keypair::from_seed(seed);
        if kp.pubkey().to_hex() != file.public_key {
            bail!(
                "key file {} is inconsistent: public_key does not match secret_key",
                path.display()
            );
        }
        Ok(kp)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let file = KeyFile {
            secret_key: hex::encode(self.signing_key.to_bytes()),
            public_key: self.pubkey().to_hex(),
        };
        std::fs::write(path.as_ref(), serde_json::to_string_pretty(&file)?)?;
        Ok(())
    }

    pub fn pubkey(&self) -> Pubkey {
        Pubkey(self.signing_key.verifying_key().to_bytes())
    }

    /// Sign an arbitrary message; returns the 64-byte signature.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }
}

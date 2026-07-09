//! Signature canonicalization and the signed request envelope.
//!
//! Every state-changing request is wrapped in an [`Envelope`]:
//!
//! ```json
//! { "pubkey": "<64 hex>", "nonce": 123, "signature": "<128 hex>", "payload": { ... } }
//! ```
//!
//! The bytes that are signed are NOT the JSON body. They are the UTF-8 bytes
//! of a domain-separated, colon-delimited ASCII string:
//!
//! ```text
//! rfq-v1:{action}:{pubkey_hex}:{nonce}:{field_1}:{field_2}:...
//! ```
//!
//! where the action name and the exact field order for each action are
//! defined in `PROTOCOL.md` (and implemented by [`SignablePayload`] below).
//! Absent optional fields are rendered as the literal `-`.
//!
//! Nonces must be strictly increasing per pubkey across ALL action types.
//! Clients conventionally use the current unix time in milliseconds.

use anyhow::{Context, Result};
use ed25519_dalek::{Signature, Verifier};
use serde::{Deserialize, Serialize};

use crate::keys::{Keypair, Pubkey};

pub const DOMAIN: &str = "rfq-v1";

/// A payload that can be canonicalized for signing.
pub trait SignablePayload {
    /// The action name, e.g. `submit_rfq`.
    fn action(&self) -> &'static str;
    /// The signed fields, in the order given in `PROTOCOL.md`.
    /// Absent optional fields must be rendered as `-`.
    fn signed_fields(&self) -> Vec<String>;
}

/// Build the canonical message string that gets signed.
pub fn canonical_message(action: &str, pubkey: &Pubkey, nonce: u64, fields: &[String]) -> String {
    let mut msg = format!("{DOMAIN}:{action}:{}:{nonce}", pubkey.to_hex());
    for field in fields {
        msg.push(':');
        msg.push_str(field);
    }
    msg
}

/// The signed request envelope used for every state-changing request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub pubkey: Pubkey,
    pub nonce: u64,
    /// 64-byte ed25519 signature, 128 lowercase hex chars.
    pub signature: String,
    pub payload: T,
}

impl<T: SignablePayload> Envelope<T> {
    /// Sign `payload` with `keypair` and produce a complete envelope.
    pub fn sign(keypair: &Keypair, nonce: u64, payload: T) -> Self {
        let pubkey = keypair.pubkey();
        let msg = canonical_message(payload.action(), &pubkey, nonce, &payload.signed_fields());
        let signature = hex::encode(keypair.sign(msg.as_bytes()));
        Envelope {
            pubkey,
            nonce,
            signature,
            payload,
        }
    }

    /// Verify the envelope's signature against its payload.
    ///
    /// This checks only the cryptographic signature; nonce monotonicity and
    /// trader existence are the server's responsibility.
    pub fn verify(&self) -> Result<()> {
        let msg = canonical_message(
            self.payload.action(),
            &self.pubkey,
            self.nonce,
            &self.payload.signed_fields(),
        );
        let sig_bytes: [u8; 64] = hex::decode(&self.signature)
            .context("signature must be hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("signature must be exactly 64 bytes (128 hex chars)"))?;
        let signature = Signature::from_bytes(&sig_bytes);
        self.pubkey
            .verifying_key()?
            .verify(msg.as_bytes(), &signature)
            .context("signature verification failed")
    }
}

/// Current unix time in milliseconds — the conventional nonce source.
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

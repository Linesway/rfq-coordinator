//! Shared protocol definitions for the RFQ coordinator.
//!
//! This crate is the ground truth for the wire protocol: it contains the
//! wire types, the signature canonicalization, keypair handling, the
//! seed-file schema, and thin HTTP/WebSocket clients used by the CLI and
//! the makers.
//!
//! Server implementations may depend on this crate, but are not required
//! to; any server that speaks exactly what these types and clients speak
//! is conformant.

pub mod client;
pub mod keys;
pub mod seed;
pub mod signing;
pub mod types;

pub use keys::{Keypair, Pubkey};

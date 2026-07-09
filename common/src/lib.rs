//! Shared protocol definitions for the RFQ coordinator.
//!
//! This crate is the executable form of `PROTOCOL.md`: it contains the wire
//! types, the signature canonicalization, keypair handling, the seed-file
//! schema, and thin HTTP/WebSocket clients used by the CLI and the makers.
//!
//! Server implementations may depend on this crate, but are not required to;
//! `PROTOCOL.md` specifies the full wire protocol on its own.

pub mod client;
pub mod keys;
pub mod seed;
pub mod signing;
pub mod types;

pub use keys::{Keypair, Pubkey};

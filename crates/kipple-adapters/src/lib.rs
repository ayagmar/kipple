//! kipple's tool integrations and rule-pack loader.
//!
//! This crate depends only on `kipple-core`, never on `kipple-platform` (see
//! `docs/05-architecture.md`).
#![forbid(unsafe_code)]

pub mod builtin;
mod integrations;
pub mod pack;

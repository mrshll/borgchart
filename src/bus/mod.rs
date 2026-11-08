//! The coordination bus: durable records members and outside callers
//! publish to each other, kept for the lifetime of an instance, outside any checkout.
//!
//! This module holds the shared interfaces (configuration, on-disk layout,
//! caller context, record envelope, outcome codes and the no-replace write
//! primitive). See `docs/bus.md` for the contract each piece builds on.

pub mod cli;
pub mod context;
pub mod fsx;
pub mod help;
pub mod layout;
pub mod outcome;
pub mod record;
pub mod store;
pub mod time;

/// The on-disk schema of records and acknowledgements written by this brg.
pub const SCHEMA: u32 = 2;

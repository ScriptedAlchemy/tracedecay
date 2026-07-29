//! Database-independent migration contracts for TraceDecay's upgrade path.
//!
//! This package owns the parts of the migration journey that decide *what*
//! must happen and *how a partial attempt is recovered*, without owning any
//! storage authority:
//!
//! - [`durability`] classifies how precious a store's data is, and therefore
//!   whether a failure touching it may block an upgrade or must stay
//!   opportunistic.
//! - [`inventory`] carries the planning vocabulary a preflight scan produces.
//! - [`manifest`] is the durable plan plus the forward-only crash checkpoint
//!   that lets an interrupted migration resume from where it stopped.
//!
//! Everything that acquires a lifecycle lease, opens a database, holds a
//! maintenance scope, or drives the rusqlite runtime stays in the root
//! `tracedecay` crate. Where this package needs an effect it must not own —
//! writing a checkpoint with owner-only permissions — it declares a narrow port
//! ([`manifest::CheckpointWriter`]) for the root to satisfy. The root
//! re-exports these modules under their original `crate::migrate::*` paths, so
//! this extraction changes no caller path.

pub mod durability;
pub mod inventory;
pub mod manifest;

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
//! The one-shot crate split moved the rest of the migration subsystem here as
//! well — the preflight scanners ([`inventory`]), the consolidation runtime
//! ([`consolidate`]), legacy Hermes import ([`hermes`]), memory cutover
//! ([`memory_cutover`]), profile backup ([`profile_backup`]), and registry
//! reconstruction ([`registry`]). Those modules acquire leases, open stores,
//! and drive the rusqlite runtime, so they reach the runtime kernel through
//! [`root_seam`], which the landing repoints at `tracedecay-runtime-core`.
//!
//! The root `tracedecay` crate re-exports every module under its original
//! `crate::migrate::*` path, so this extraction changes no caller path.

pub mod consolidate;
pub mod durability;
pub mod final_v2;
pub mod hermes;
pub mod inventory;
pub mod manifest;
pub mod memory_cutover;
pub mod profile_backup;
mod profile_identity;
pub mod registry;
pub mod root_seam;
mod session_runtime;

pub use final_v2::*;

//! Offline backup, restore, inventory, and registry recovery contracts.
//!
//! This package owns the parts of the migration journey that decide *what*
//! must happen and *how a partial attempt is recovered*, without owning any
//! storage authority:
//!
//! - [`durability`] classifies how precious a store's data is, and therefore
//!   whether a failure touching it may block an upgrade or must stay
//!   opportunistic.
//! - [`inventory`] carries the planning vocabulary a preflight scan produces.
//! Runtime schema migration is intentionally absent. Existing stores either
//! match the compiled final schema exactly or require explicit reset.
//!
//! The root `tracedecay` crate re-exports every module under its original
//! `crate::migrate::*` path, so this extraction changes no caller path.

pub mod durability;
pub mod final_v2;
pub mod hermes;
pub mod inventory;
pub mod profile_backup;
mod profile_identity;
pub mod registry;
pub mod root_seam;
mod session_runtime;

pub use final_v2::*;

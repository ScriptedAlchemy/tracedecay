//! Residual migration-era capabilities awaiting their final owners.
//!
//! Only two live modules remain after the Plan-19 residue cutover:
//!
//! - [`final_v2`] owns the released final-V2 fixture vocabulary and the
//!   `FINAL_PROJECT_SCHEMA_VERSION` stamp the doctor compares against.
//! - [`profile_backup`] owns the complete checksummed profile backup and its
//!   isolated restore rehearsal.
//!
//! Registry maintenance lives in `tracedecay-global-db`; the durability
//! journal lives in `tracedecay-runtime-core`. The root `tracedecay` crate
//! re-exports the remaining modules under `crate::migrate::*`.

pub mod final_v2;
pub mod profile_backup;
mod profile_identity;

pub use final_v2::*;

//! Residual migration-era capability awaiting its final owner.
//!
//! Only [`profile_backup`] remains after the Plan-19 residue cutover: the
//! complete checksummed profile backup and its isolated restore rehearsal.
//! Registry maintenance lives in `tracedecay-global-db`; the durability
//! journal lives in `tracedecay-runtime-core`; the doctor compares schema
//! versions against `tracedecay_runtime_core::db::migrations::SCHEMA_VERSION`
//! directly. The root `tracedecay` crate re-exports this module under
//! `crate::migrate::profile_backup`.

pub mod profile_backup;
mod profile_identity;

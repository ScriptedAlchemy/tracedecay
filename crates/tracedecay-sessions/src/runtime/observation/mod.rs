//! Observation admission plumbing.
//!
//! JSONL frame admission, snapshot-file capture, and the shared ingest byte
//! budget sit below host adapters. Modules are re-exported at their previous
//! `crate::runtime::{…}` paths.

pub(in crate::runtime) mod ingest_byte_budget;
pub(in crate::runtime) mod jsonl_observation_admission;
pub mod snapshot_observation;

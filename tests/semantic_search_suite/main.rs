//! Quarantined PR10 semantic benchmark schema tests.
//!
//! This suite includes the preparation packet by path so the schemas remain
//! unregistered and cannot activate semantic behavior before PR10 replay.

#[allow(dead_code)]
#[path = "../../src/semantic_code/evaluation.rs"]
pub(crate) mod evaluation;

mod evaluation_schema;

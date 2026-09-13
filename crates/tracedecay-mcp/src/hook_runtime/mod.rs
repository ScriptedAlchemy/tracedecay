//! Portable hook-runtime error mapping that does not own daemon ingest.

mod errors;

pub use errors::{
    hook_admission_error, map_claude_observation_ingest_error, map_host_admission_outcome,
    map_transcript_ingest_error,
};

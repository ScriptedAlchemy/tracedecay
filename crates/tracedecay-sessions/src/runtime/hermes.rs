//! Hermes Agent transcript source.
//!
//! Hermes does not write transcript files: every conversation lives in a
//! per-profile `SQLite` store at `<profile>/state.db` (tables `sessions` +
//! `messages`), where `<profile>` is `~/.hermes` for the default profile or
//! `~/.hermes/profiles/<name>` for named profiles. A profile maps to exactly
//! one ingest target only when provenance proves a real code project: a
//! legacy `plugins.tracedecay.project_root` pin or the session row's `cwd`.
//! For projectless/gateway sessions, one completed turn may instead prove its
//! project through structured tool-call routing (`project_path`,
//! `project_root`, or a nested project selector). Only that turn is admitted to
//! the project scope; an entire long-running multi-project chat is never assigned
//! by inference.
//! Profile directories are never `TraceDecay` project identities.
//!
//! Each bounded `SQLite` row is admitted through the shared observation privacy,
//! cursor, persistence, duplicate, collision, and projection-queue authority.
//! `SQLite` row ids are generation-local ordering evidence only; native identity
//! is derived from immutable Hermes session and message evidence.

use crate::runtime::source::STRICT_JSONL_BATCH_BYTES;
use tracedecay_runtime_core::privacy::MAX_OBSERVATION_RECORD_BYTES;

mod coverage;
mod ingest;
mod observation;
mod routing;
mod rows;
mod state_db;
#[cfg(test)]
mod tests;

pub use ingest::{
    HermesSweepOutcome, ProjectIngestDestination, ingest_for_project, ingest_for_project_capped,
    ingest_for_project_capped_with_admission,
    ingest_for_project_capped_with_admission_and_cancellation, ingest_for_projects, ingest_homes,
    ingest_homes_capped, ingest_homes_capped_with_admission, ingest_homes_for_projects,
    ingest_user_homes, ingest_user_homes_capped, ingest_user_sessions_capped,
};
pub use ingest::{ingest_legacy_pinned_profile, ingest_user_sessions_capped_with_admission};

#[cfg(all(test, windows))]
use coverage::sqlite_incarnation;
#[cfg(test)]
use observation::{
    HermesAdmissionAction, HermesObservationRecord, HermesProjectionMetadata,
    native_observation_record, normalize_native_observation, observation_source,
    prepare_observation_row, stable_native_id,
};
#[cfg(test)]
use rows::HermesRow;
#[cfg(test)]
use state_db::{
    message_columns, open_read_only_strict, read_new_rows_strict, select_new_messages_sql,
    table_columns, validate_required_schema,
};

const PROVIDER: &str = "hermes";
const OBSERVATION_RETENTION: &str = "transcript.hermes.v1";
/// Maximum messages joined per `SQLite` page (row-count bound before collection).
const CHUNK_ROWS: usize = 2000;
/// Per-value payload bound before `String` materialization (observation record cap).
const MAX_HERMES_VALUE_BYTES: usize = MAX_OBSERVATION_RECORD_BYTES;
/// Identity/text metadata bound (matches `SessionId` canonical max of 512 bytes).
const MAX_HERMES_IDENTITY_BYTES: usize = 512;
/// Cumulative SQL-measured bytes admitted into one page (reuses JSONL batch bound).
const MAX_HERMES_PAGE_BYTES: u64 = STRICT_JSONL_BATCH_BYTES;
/// Aggregate source bytes admitted by an ordinary catch-up sweep.
const DEFAULT_HERMES_SWEEP_BYTES: u64 = MAX_HERMES_PAGE_BYTES;
const MAX_HERMES_PROJECTIONS_PER_DRAIN: usize = 256;

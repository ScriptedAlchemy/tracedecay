//! Root-crate persistence adapters for store-facing contracts.
//!
//! Adapters in this module borrow already-open authoritative stores. They do
//! not discover paths, open connections, or own transaction state.

use std::future::Future;

use tracedecay_store::{TranscriptStore, TranscriptStoreResult, TranscriptWriteBatch};

use crate::sessions::SessionRecord;
use crate::sessions::git_correlation::{CommitSessionRecord, SpanObservation};

pub mod global_db;
pub mod observation;

pub use global_db::GlobalDbTranscriptStore;
pub use observation::GlobalDbObservationStore;

/// Application boundary required by production transcript ingestion.
///
/// The portable store contract owns cursor and transcript writes. The root
/// application extends it with session merge reads and git evidence that must
/// commit in the same authoritative transaction.
pub(crate) trait TranscriptIngestStore: TranscriptStore {
    fn get_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> impl Future<Output = TranscriptStoreResult<Option<SessionRecord>>> + Send;

    fn persist_transcript_batch_with_git_evidence(
        &self,
        batch: TranscriptWriteBatch,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send;
}

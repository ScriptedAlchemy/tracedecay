//! Store contracts the session runtime writes through.
//!
//! The portable [`TranscriptStore`] contract owns cursor and transcript
//! writes. Transcript ingest additionally needs a session merge read and git
//! evidence that commits inside the same authoritative transaction — both
//! expressed purely in session values — so the extension trait belongs here
//! rather than next to the registered-database adapter that implements it.
//!
//! Root wiring: `src/store/mod.rs` must drop its own `TranscriptIngestStore`
//! definition and re-export this one, keeping
//! `impl TranscriptIngestStore for GlobalDbTranscriptStore<'_>` where it is.

use std::future::Future;
use std::path::Path;

use tracedecay_store::{ParseOffset, TranscriptStore, TranscriptStoreResult, TranscriptWriteBatch};

use crate::runtime::SessionRecord;
use crate::runtime::git_correlation::{CommitSessionRecord, SpanObservation};

/// Application boundary required by production transcript ingestion.
pub trait TranscriptIngestStore: TranscriptStore {
    /// Advances a parse offset only when it moves forward, reading the
    /// current offset as the compare-and-set expectation.
    fn advance_parse_offset_monotonic(
        &self,
        cursor_path: &Path,
        offset: ParseOffset,
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send {
        async move {
            let expected = self.get_parse_offset(cursor_path).await?;
            let batch =
                TranscriptWriteBatch::advance_offset(cursor_path.to_path_buf(), expected, offset)?;
            self.persist_transcript_batch(batch).await
        }
    }

    /// Records that a bounded ingest pass touched a project. Defaults to a
    /// no-op so stores without an activity ledger need not implement it.
    fn record_session_ingest_activity(
        &self,
        _project_root: &Path,
        _units: u64,
        _provider: &'static str,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }

    /// Reads one already-persisted session for incremental merge.
    fn get_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> impl Future<Output = TranscriptStoreResult<Option<SessionRecord>>> + Send;

    /// Commits a transcript batch together with the git evidence derived from
    /// it, so correlation rows can never outlive a rolled-back batch.
    fn persist_transcript_batch_with_git_evidence(
        &self,
        batch: TranscriptWriteBatch,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send;
}

//! Store contracts the session runtime writes through.
//!
//! The portable [`TranscriptStore`] contract owns cursor and transcript
//! writes. Transcript ingest additionally needs a session merge read and git
//! evidence that commits inside the same authoritative transaction — both
//! expressed purely in session values — so the extension trait belongs here
//! rather than next to the registered-database adapter that implements it.

use std::future::Future;
use std::path::Path;

use tracedecay_store::{
    ParseOffset, TranscriptStore, TranscriptStoreError, TranscriptStoreResult, TranscriptWriteBatch,
};

use crate::runtime::SessionRecord;
use crate::runtime::git_correlation::{CommitSessionRecord, SpanObservation};

pub trait TranscriptIngestStore: TranscriptStore {
    /// Atomically replaces two typed parse-offset authorities after checking
    /// both exact prior values. Implementations must not expose a partially
    /// updated pair if either comparison, write, or commit fails.
    fn replace_parse_offset_pair(
        &self,
        first: (&Path, ParseOffset, ParseOffset),
        second: (&Path, ParseOffset, ParseOffset),
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send {
        async move {
            let _ = (first, second);
            Err(TranscriptStoreError::Storage {
                operation: "replace parse-offset pair",
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "atomic parse-offset pair replacement is unavailable",
                )),
            })
        }
    }

    /// Replaces a typed parse-offset state with compare-and-set semantics.
    /// Unlike the monotonic helper, this permits a versioned epoch transition
    /// to reset its plain cursor without saturating or packing the value.
    fn replace_parse_offset(
        &self,
        cursor_path: &Path,
        expected: ParseOffset,
        next: ParseOffset,
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send {
        async move {
            let batch =
                TranscriptWriteBatch::advance_offset(cursor_path.to_path_buf(), expected, next)?;
            self.persist_transcript_batch(batch).await
        }
    }

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

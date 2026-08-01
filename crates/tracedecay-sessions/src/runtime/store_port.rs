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

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, ReadSnapshot};
use tracedecay_runtime_core::errors::Result;
use tracedecay_store::{
    ParseOffset, StoreShardIdV1, TranscriptStore, TranscriptStoreResult, TranscriptWriteBatch,
};

use crate::runtime::SessionRecord;
use crate::runtime::git_correlation::{CommitSessionRecord, SpanObservation};

/// A write transaction opened on a registered session authority.
pub trait SessionWriteTxn: QueryExecutor + Executor + Sized + Send {
    /// Commits the transaction, releasing the writer.
    fn commit(self) -> impl Future<Output = Result<()>> + Send;

    /// Rolls the transaction back, releasing the writer without applying it.
    fn rollback(self) -> impl Future<Output = Result<()>> + Send;
}

/// The already-open registered session database a backfill sweep operates on.
///
/// This is the inverted seam for `tracedecay_global_db::RegisteredGlobalDb`.
/// Backfill needs the shard identity it is bound to, the on-disk path (for the
/// sweep lock), a read snapshot, and a write transaction — none of which
/// require the registry, enrollment, or projection surfaces that make the
/// registered database a composition-root type.
///
/// Root wiring: `impl SessionStoreAuthority for RegisteredGlobalDb` alongside
/// the other root store adapters in `src/store/`.
pub trait SessionStoreAuthority: Sync {
    /// Write transaction this authority hands out.
    type WriteTxn<'txn>: SessionWriteTxn
    where
        Self: 'txn;

    /// Shard identity this authority is bound to.
    fn shard_id(&self) -> &StoreShardIdV1;

    /// On-disk database path, used to scope the single-writer sweep lock.
    fn db_path(&self) -> &Path;

    /// Opens a read snapshot.
    fn read_snapshot(&self) -> impl Future<Output = Result<ReadSnapshot>> + Send;

    /// Opens a write transaction.
    fn begin_write_transaction(&self)
    -> impl Future<Output = Result<Self::WriteTxn<'_>>> + Send;
}

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

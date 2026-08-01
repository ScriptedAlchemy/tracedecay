use std::borrow::Borrow;

use std::future::Future;
use std::path::Path;

use tracedecay_store::{
    ParseOffset, TranscriptStore, TranscriptStoreError, TranscriptStoreResult,
    TranscriptWriteBatch, TranscriptWriteKind,
};

use crate::global_db::{RegisteredGlobalDb, TranscriptPersistenceError};
use crate::sessions::git_correlation::{CommitSessionRecord, SpanObservation};
use crate::store::TranscriptIngestStore;

/// Transcript-store adapter over an already-open authoritative
/// [`RegisteredGlobalDb`].
///
/// The adapter deliberately borrows `RegisteredGlobalDb`: runtime ownership,
/// authority checks, and all transaction begin/commit/rollback decisions stay
/// in the registered database implementation.
/// The holder `D` is generic so callers that own an `Arc<RegisteredGlobalDb>`
/// can build a lifetime-free (`'static`) adapter. A borrowed adapter makes the
/// trait impls below apply only "for some specific lifetime", which turns any
/// `Send` proof over a future holding one across an await into a higher-ranked
/// obligation the compiler cannot discharge.
pub struct GlobalDbTranscriptStore<D> {
    db: D,
}

impl<D> GlobalDbTranscriptStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    pub(crate) const fn new(db: D) -> Self {
        Self { db }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
    }

    fn storage_error(operation: &'static str, message: impl Into<String>) -> TranscriptStoreError {
        TranscriptStoreError::Storage {
            operation,
            source: Box::new(std::io::Error::other(message.into())),
        }
    }

    fn path_text(path: &Path) -> String {
        // Preserve the V1 database key format. SQLite stores transcript paths
        // as text, and ingestion historically used the platform path's lossy
        // display form for non-Unicode names.
        path.to_string_lossy().into_owned()
    }

    fn persistence_error(
        cursor_path: &Path,
        error: TranscriptPersistenceError,
    ) -> TranscriptStoreError {
        match error {
            TranscriptPersistenceError::Conflict { expected, actual } => {
                TranscriptStoreError::Conflict {
                    cursor_path: cursor_path.to_path_buf(),
                    expected,
                    actual,
                }
            }
            TranscriptPersistenceError::Storage { operation, source } => {
                TranscriptStoreError::Storage { operation, source }
            }
        }
    }

    async fn persist_batch(
        &self,
        batch: TranscriptWriteBatch,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> TranscriptStoreResult<()> {
        let (cursor_path, kind) = batch.into_parts();
        let cursor_key = Self::path_text(&cursor_path);
        match kind {
            TranscriptWriteKind::AdvanceOffset {
                expected_offset,
                next_offset,
            } => {
                if !commit_records.is_empty() || !span_observations.is_empty() {
                    return Err(Self::storage_error(
                        "persist transcript offset",
                        "offset-only transcript writes cannot contain git evidence",
                    ));
                }

                // Offset-only batches contain no parse products, so advancing
                // across a compatible append winner cannot persist stale rows.
                // Full batches below must never rewrite their observed cursor:
                // their caller has to re-read and reparse after a conflict.
                let mut expected_offset = expected_offset;
                loop {
                    match self
                        .db()
                        .persist_transcript_offset_result(&cursor_key, expected_offset, next_offset)
                        .await
                    {
                        Ok(()) => return Ok(()),
                        Err(TranscriptPersistenceError::Conflict { expected, actual }) => {
                            if actual == next_offset {
                                return Ok(());
                            }
                            let compatible_successor = actual.file_id != 0
                                && actual.file_id == next_offset.file_id
                                && actual.byte_offset > expected.byte_offset
                                && actual.mtime >= expected.mtime
                                && next_offset.byte_offset > actual.byte_offset
                                && next_offset.mtime >= actual.mtime;
                            if !compatible_successor {
                                return Err(Self::persistence_error(
                                    &cursor_path,
                                    TranscriptPersistenceError::Conflict { expected, actual },
                                ));
                            }
                            expected_offset = actual;
                        }
                        Err(error) => {
                            return Err(Self::persistence_error(&cursor_path, error));
                        }
                    }
                }
            }
            TranscriptWriteKind::Upsert {
                session,
                messages,
                expected_offset,
                next_offset,
            } => {
                let batch = crate::global_db::TranscriptBatch {
                    session: *session,
                    messages,
                };
                self.db()
                    .persist_transcript_batch_with_git_evidence_result(
                        &batch,
                        commit_records,
                        span_observations,
                        &cursor_key,
                        expected_offset,
                        next_offset,
                    )
                    .await
                    .map_err(|error| Self::persistence_error(&cursor_path, error))
            }
        }
    }
}

impl<D> TranscriptStore for GlobalDbTranscriptStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    async fn get_parse_offset(&self, cursor_path: &Path) -> TranscriptStoreResult<ParseOffset> {
        let cursor_key = Self::path_text(cursor_path);
        self.db()
            .get_parse_offset_result(&cursor_key)
            .await
            .map(Option::unwrap_or_default)
            .map_err(|error| Self::persistence_error(cursor_path, error))
    }

    async fn persist_transcript_batch(
        &self,
        batch: TranscriptWriteBatch,
    ) -> TranscriptStoreResult<()> {
        self.persist_batch(batch, &[], &[]).await
    }
}

impl<D> TranscriptIngestStore for GlobalDbTranscriptStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    fn advance_parse_offset_monotonic(
        &self,
        cursor_path: &Path,
        offset: ParseOffset,
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send {
        async move {
            self.db()
                .advance_parse_offset_result(&Self::path_text(cursor_path), offset)
                .await
                .map_err(|error| Self::persistence_error(cursor_path, error))
        }
    }

    fn record_session_ingest_activity(
        &self,
        project_root: &Path,
        units: u64,
        provider: &'static str,
    ) -> impl Future<Output = ()> + Send {
        async move {
            crate::application::event_lane::publish(
                self.db(),
                crate::application::event_lane::ActivityFamilyV1::SessionIngest,
                project_root,
                None,
                units,
                Some(provider),
            )
            .await;
        }
    }

    async fn get_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> TranscriptStoreResult<Option<crate::sessions::SessionRecord>> {
        self.db()
            .get_session_result(provider, session_id)
            .await
            .map_err(|error| match error {
                TranscriptPersistenceError::Storage { operation, source } => {
                    TranscriptStoreError::Storage { operation, source }
                }
                TranscriptPersistenceError::Conflict { .. } => Self::storage_error(
                    "load transcript session",
                    "unexpected cursor conflict while loading a session",
                ),
            })
    }

    async fn persist_transcript_batch_with_git_evidence(
        &self,
        batch: TranscriptWriteBatch,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> TranscriptStoreResult<()> {
        self.persist_batch(batch, commit_records, span_observations)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_contains_only_the_borrowed_global_db_handle() {
        fn assert_exact_fields(store: &GlobalDbTranscriptStore<&'_ RegisteredGlobalDb>) {
            let GlobalDbTranscriptStore { db: _ } = store;
        }

        let _ = assert_exact_fields;
        assert_eq!(
            std::mem::size_of::<GlobalDbTranscriptStore<&'static RegisteredGlobalDb>>(),
            std::mem::size_of::<&'static RegisteredGlobalDb>()
        );
        assert_eq!(
            std::mem::align_of::<GlobalDbTranscriptStore<&'static RegisteredGlobalDb>>(),
            std::mem::align_of::<&'static RegisteredGlobalDb>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_keep_the_legacy_lossy_database_key() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let path = std::path::PathBuf::from(OsString::from_vec(b"session-\xff.jsonl".to_vec()));
        assert_eq!(
            GlobalDbTranscriptStore::<&RegisteredGlobalDb>::path_text(&path),
            path.to_string_lossy()
        );
    }
}

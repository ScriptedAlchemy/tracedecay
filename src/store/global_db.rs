use std::path::Path;

use tracedecay_store::{
    ParseOffset, TranscriptStore, TranscriptStoreError, TranscriptStoreResult,
    TranscriptWriteBatch, TranscriptWriteKind,
};

use crate::global_db::{GlobalDb, TranscriptPersistenceError};
use crate::sessions::git_correlation::{CommitSessionRecord, SpanObservation};

/// Transcript-store adapter over an already-open authoritative [`GlobalDb`].
///
/// The adapter deliberately borrows `GlobalDb`: connection ownership and all
/// transaction begin/commit/rollback decisions stay in the root database
/// implementation.
pub struct GlobalDbTranscriptStore<'a> {
    db: &'a GlobalDb,
}

impl<'a> GlobalDbTranscriptStore<'a> {
    pub const fn new(db: &'a GlobalDb) -> Self {
        Self { db }
    }

    fn storage_error(operation: &'static str, message: impl Into<String>) -> TranscriptStoreError {
        TranscriptStoreError::Storage {
            operation,
            source: Box::new(std::io::Error::other(message.into())),
        }
    }

    fn path_text(path: &Path) -> TranscriptStoreResult<&str> {
        path.to_str().ok_or_else(|| {
            Self::storage_error(
                "encode transcript path as UTF-8",
                "transcript path cannot be represented as UTF-8",
            )
        })
    }

    fn persistence_error(
        transcript_path: &Path,
        error: TranscriptPersistenceError,
    ) -> TranscriptStoreError {
        match error {
            TranscriptPersistenceError::Conflict { expected, actual } => {
                TranscriptStoreError::Conflict {
                    transcript_path: transcript_path.to_path_buf(),
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
        let (transcript_path, kind) = batch.into_parts();
        let path = Self::path_text(&transcript_path)?;
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
                self.db
                    .persist_transcript_offset_result(path, expected_offset, next_offset)
                    .await
                    .map_err(|error| Self::persistence_error(&transcript_path, error))
            }
            TranscriptWriteKind::Upsert {
                session,
                messages,
                expected_offset,
                next_offset,
            } => self
                .db
                .persist_transcript_batch_with_git_evidence_result(
                    &session,
                    &messages,
                    commit_records,
                    span_observations,
                    path,
                    expected_offset,
                    next_offset,
                )
                .await
                .map_err(|error| Self::persistence_error(&transcript_path, error)),
        }
    }

    /// Persists the production transcript batch together with root-local git
    /// correlation evidence in the same authoritative transaction.
    pub(crate) async fn persist_transcript_batch_with_git_evidence(
        &self,
        batch: TranscriptWriteBatch,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> TranscriptStoreResult<()> {
        self.persist_batch(batch, commit_records, span_observations)
            .await
    }

    pub(crate) async fn get_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> TranscriptStoreResult<Option<crate::sessions::SessionRecord>> {
        self.db
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
}

impl TranscriptStore for GlobalDbTranscriptStore<'_> {
    async fn get_parse_offset(&self, path: &Path) -> TranscriptStoreResult<ParseOffset> {
        let path = Self::path_text(path)?;
        self.db
            .get_parse_offset_result(path)
            .await
            .map(|offset| offset.unwrap_or_default())
            .map_err(|error| Self::persistence_error(Path::new(path), error))
    }

    async fn persist_transcript_batch(
        &self,
        batch: TranscriptWriteBatch,
    ) -> TranscriptStoreResult<()> {
        self.persist_batch(batch, &[], &[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_contains_only_the_borrowed_global_db_handle() {
        fn assert_exact_fields(store: GlobalDbTranscriptStore<'_>) {
            let GlobalDbTranscriptStore { db: _ } = store;
        }

        let _ = assert_exact_fields;
        assert_eq!(
            std::mem::size_of::<GlobalDbTranscriptStore<'static>>(),
            std::mem::size_of::<&'static GlobalDb>()
        );
        assert_eq!(
            std::mem::align_of::<GlobalDbTranscriptStore<'static>>(),
            std::mem::align_of::<&'static GlobalDb>()
        );
    }
}

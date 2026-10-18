use std::borrow::Borrow;

use std::path::{Path, PathBuf};

use tracedecay_store::{
    ParseOffset, TranscriptStore, TranscriptStoreError, TranscriptStoreResult, TranscriptWriteBatch,
};

use tracedecay_global_db::{RegisteredGlobalDb, TranscriptPersistenceError};
use tracedecay_sessions::runtime::store_port::TranscriptIngestStore;

/// Transcript-store adapter over an already-open authoritative
/// [`RegisteredGlobalDb`].
///
/// The adapter deliberately borrows `RegisteredGlobalDb`: runtime ownership,
/// authority checks, and all transaction begin/commit/rollback decisions stay
/// in the registered database implementation.
/// The holder `D` is generic so callers that own a
/// [`tracedecay_global_db::RegisteredGlobalDbLeaseV1`]
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
    pub const fn new(db: D) -> Self {
        Self { db }
    }

    fn db(&self) -> &RegisteredGlobalDb {
        self.db.borrow()
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
            TranscriptPersistenceError::PairConflict {
                path,
                expected,
                actual,
            } => TranscriptStoreError::Conflict {
                cursor_path: PathBuf::from(path),
                expected,
                actual,
            },
            TranscriptPersistenceError::Storage { operation, source } => {
                TranscriptStoreError::Storage { operation, source }
            }
        }
    }

    async fn persist_batch(&self, batch: TranscriptWriteBatch) -> TranscriptStoreResult<()> {
        let (cursor_path, mut expected_offset, next_offset) = batch.into_parts();
        let cursor_key = Self::path_text(&cursor_path);
        // Offset-only batches contain no parse products, so advancing across a
        // compatible append winner cannot persist stale rows.
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
}

impl<D> TranscriptStore for GlobalDbTranscriptStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    #[tracing::instrument(
        name = "usecases.transcript_store.get_parse_offset",
        level = "trace",
        skip_all
    )]
    async fn get_parse_offset(&self, cursor_path: &Path) -> TranscriptStoreResult<ParseOffset> {
        let cursor_key = Self::path_text(cursor_path);
        self.db()
            .get_parse_offset(&cursor_key)
            .await
            .map(Option::unwrap_or_default)
            .map_err(|error| Self::persistence_error(cursor_path, error))
    }

    #[tracing::instrument(
        name = "usecases.transcript_store.persist_transcript_batch",
        level = "trace",
        skip_all
    )]
    async fn persist_transcript_batch(
        &self,
        batch: TranscriptWriteBatch,
    ) -> TranscriptStoreResult<()> {
        self.persist_batch(batch).await
    }
}

impl<D> TranscriptIngestStore for GlobalDbTranscriptStore<D>
where
    D: Borrow<RegisteredGlobalDb> + Send + Sync,
{
    #[tracing::instrument(
        name = "usecases.transcript_store.replace_parse_offset_pair",
        level = "trace",
        skip_all
    )]
    async fn replace_parse_offset_pair(
        &self,
        first: (&Path, ParseOffset, ParseOffset),
        second: (&Path, ParseOffset, ParseOffset),
    ) -> TranscriptStoreResult<()> {
        let first_path = Self::path_text(first.0);
        let second_path = Self::path_text(second.0);
        self.db()
            .replace_parse_offset_pair_result(
                (&first_path, first.1, first.2),
                (&second_path, second.1, second.2),
            )
            .await
            .map_err(|error| Self::persistence_error(first.0, error))
    }

    #[tracing::instrument(
        name = "usecases.transcript_store.advance_parse_offset",
        level = "trace",
        skip_all
    )]
    async fn advance_parse_offset_monotonic(
        &self,
        cursor_path: &Path,
        offset: ParseOffset,
    ) -> TranscriptStoreResult<()> {
        self.db()
            .advance_parse_offset_result(&Self::path_text(cursor_path), offset)
            .await
            .map_err(|error| Self::persistence_error(cursor_path, error))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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

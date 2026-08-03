use std::future::Future;

use crate::global_db::{GlobalDb, ParseOffset};
use tracedecay_sessions::{SessionMessageRecord, SessionRecord};

pub use tracedecay_sessions::runtime::source::*;

impl TranscriptIngestStore for GlobalDb {
    fn load_cursor(&self, path: &str) -> impl Future<Output = StoredCursor> + Send {
        let path = path.to_string();
        async move {
            let offset = self.get_parse_offset(&path).await.unwrap_or_default();
            StoredCursor {
                position: offset.byte_offset,
                mtime: offset.mtime,
                file_id: offset.file_id,
            }
        }
    }

    fn advance_cursor(&self, path: &str, cursor: StoredCursor) -> impl Future<Output = ()> + Send {
        let path = path.to_string();
        async move {
            self.set_parse_offset(
                &path,
                ParseOffset {
                    byte_offset: cursor.position,
                    mtime: cursor.mtime,
                    file_id: cursor.file_id,
                },
            )
            .await;
        }
    }

    fn existing_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> impl Future<Output = Option<SessionRecord>> + Send {
        let provider = provider.to_string();
        let session_id = session_id.to_string();
        async move { self.get_session(&provider, &session_id).await }
    }

    fn upsert_transcript(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        commit_records: &[tracedecay_sessions::git_correlation::CommitSessionRecord],
        span_observations: &[tracedecay_sessions::git_correlation::SpanObservation],
        path: &str,
        cursor: StoredCursor,
    ) -> impl Future<Output = bool> + Send {
        let session = session.clone();
        let messages = messages.to_vec();
        let commit_records = commit_records.to_vec();
        let span_observations = span_observations.to_vec();
        let path = path.to_string();
        async move {
            self.upsert_transcript_batch_with_git_evidence(
                &session,
                &messages,
                &commit_records,
                &span_observations,
                &path,
                ParseOffset {
                    byte_offset: cursor.position,
                    mtime: cursor.mtime,
                    file_id: cursor.file_id,
                },
            )
            .await
        }
    }
}

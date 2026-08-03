use std::future::Future;
use std::pin::Pin;

use tracedecay_sessions::SessionRecord;
use tracedecay_sessions::runtime::shared::StoredCursor;

pub use tracedecay_sessions::runtime::hermes::*;

impl HermesStore for crate::global_db::GlobalDb {
    fn load_cursor<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = StoredCursor> + Send + 'a>> {
        Box::pin(async move {
            let offset = self.get_parse_offset(path).await.unwrap_or_default();
            StoredCursor {
                position: offset.byte_offset,
                mtime: offset.mtime,
                file_id: offset.file_id,
            }
        })
    }

    fn advance_cursor<'a>(
        &'a self,
        path: &'a str,
        cursor: StoredCursor,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.set_parse_offset(
                path,
                crate::global_db::ParseOffset {
                    byte_offset: cursor.position,
                    mtime: cursor.mtime,
                    file_id: cursor.file_id,
                },
            )
            .await;
        })
    }

    fn upsert_transcript_projection_batches<'a>(
        &'a self,
        batches: &'a [TranscriptBatch],
        path: &'a str,
        cursor: StoredCursor,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            self.upsert_transcript_projection_batches(
                batches,
                path,
                crate::global_db::ParseOffset {
                    byte_offset: cursor.position,
                    mtime: cursor.mtime,
                    file_id: cursor.file_id,
                },
            )
            .await
        })
    }

    fn existing_session<'a>(
        &'a self,
        provider: &'a str,
        session_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<SessionRecord>> + Send + 'a>> {
        Box::pin(async move { self.get_session(provider, session_id).await })
    }
}

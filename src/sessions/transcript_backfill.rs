use std::future::Future;
use std::pin::Pin;

use tracedecay_sessions::SessionMessageRecord;
use tracedecay_sessions::git_correlation::{CommitSessionRecord, SpanObservation};

pub(crate) use tracedecay_sessions::runtime::transcript_backfill::{
    StructuredBackfillStore, backfill_structured_rows, backfill_transcript_facts,
};
pub use tracedecay_sessions::runtime::transcript_backfill::{
    read_structured_backfill_cursor_for_test, try_acquire_structured_backfill_lock,
    write_structured_backfill_cursor_for_test,
};

impl StructuredBackfillStore for crate::global_db::GlobalDb {
    fn db_path(&self) -> &std::path::Path {
        self.db_path()
    }

    fn connection(&self) -> &libsql::Connection {
        self.conn()
    }

    fn insert_absent_session_messages<'a>(
        &'a self,
        messages: &'a [SessionMessageRecord],
    ) -> Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        Box::pin(async move { self.insert_absent_session_messages(messages).await })
    }

    fn git_upsert_commit_session<'a>(
        &'a self,
        record: &'a CommitSessionRecord,
    ) -> Pin<Box<dyn Future<Output = Option<bool>> + Send + 'a>> {
        Box::pin(async move { self.git_upsert_commit_session(record).await.ok() })
    }

    fn git_record_span_observation<'a>(
        &'a self,
        observation: &'a SpanObservation,
        merge_gap_secs: i64,
    ) -> Pin<Box<dyn Future<Output = Option<i64>> + Send + 'a>> {
        Box::pin(async move {
            self.git_record_span_observation(observation, merge_gap_secs)
                .await
                .ok()
        })
    }
}

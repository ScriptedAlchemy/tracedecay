pub use tracedecay_sessions::git_correlation::*;

pub async fn run_backfill(
    session_store: &crate::global_db::GlobalDb,
    analytics_events: &[crate::global_db::AnalyticsEventRecord],
    git: &dyn GitReflogSource,
    opts: &BackfillOptions,
) -> Result<BackfillStats, GitCorrelationError> {
    tracedecay_sessions::runtime::git_correlation::run_backfill_with_analytics(
        session_store,
        analytics_events,
        git,
        opts,
    )
    .await
}

impl GitBackfillStore for crate::global_db::GlobalDb {
    fn session_activity_rows(
        &self,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Vec<SessionActivityRow>, String>> + Send {
        async move { session_activity_rows(self.conn(), limit).await }
    }

    fn session_activity_rows_since(
        &self,
        since_exclusive: i64,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Vec<SessionActivityRow>, String>> + Send {
        async move { session_activity_rows_since(self.conn(), since_exclusive, limit).await }
    }

    fn git_correlation_meta_get(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<i64>, GitCorrelationError>> + Send {
        async move { read_meta_value(self.conn(), key).await }
    }

    fn git_correlation_meta_set(
        &self,
        key: &str,
        value: i64,
    ) -> impl std::future::Future<Output = Result<(), GitCorrelationError>> + Send {
        async move { write_meta_value(self.conn(), key, value).await }
    }

    fn git_record_span_observation(
        &self,
        observation: &SpanObservation,
        merge_gap_secs: i64,
    ) -> impl std::future::Future<Output = Result<i64, GitCorrelationError>> + Send {
        async move { record_span_observation(self.conn(), observation, merge_gap_secs).await }
    }

    fn git_upsert_commit_session(
        &self,
        record: &CommitSessionRecord,
    ) -> impl std::future::Future<Output = Result<bool, GitCorrelationError>> + Send {
        async move { upsert_commit_session(self.conn(), record).await }
    }
}

impl GitBackfillAnalytics for crate::global_db::AnalyticsEventRecord {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

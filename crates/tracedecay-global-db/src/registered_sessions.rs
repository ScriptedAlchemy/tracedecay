use tracedecay_domain::errors::TraceDecayError;
#[cfg(test)]
pub(crate) use tracedecay_sessions::runtime::SessionRecord;
use tracedecay_sessions::runtime::{
    SessionMessageRecord, SessionMessageSearchResult, SessionStoreAccess,
};

use super::{RegisteredGlobalDb, SessionActivityRow, SessionIngestHealth};
#[cfg(test)]
pub(crate) use super::{SessionProviderCoverage, SessionProviderCoverageState};

#[cfg(test)]
pub(crate) use tracedecay_sessions::runtime::store_access::SESSION_MESSAGES_AFTER_SQL;

impl RegisteredGlobalDb {
    #[hotpath::skip]
    pub async fn cursor_session_ingest_health(&self) -> Result<SessionIngestHealth, String> {
        SessionStoreAccess::new(self)
            .cursor_session_ingest_health()
            .await
    }

    #[hotpath::skip]
    pub async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> Result<SessionIngestHealth, String> {
        SessionStoreAccess::new(self)
            .session_ingest_health_for_provider(provider)
            .await
    }

    #[hotpath::skip]
    pub async fn has_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<bool, String> {
        SessionStoreAccess::new(self)
            .has_session_message(provider, message_id)
            .await
    }

    #[hotpath::skip]
    pub async fn session_message_count(&self) -> Result<i64, String> {
        SessionStoreAccess::new(self).session_message_count().await
    }

    #[hotpath::skip]
    pub async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> Result<i64, String> {
        SessionStoreAccess::new(self)
            .session_message_count_for_project(project_key)
            .await
    }

    #[hotpath::skip]
    pub async fn session_messages_after(
        &self,
        provider: &str,
        session_id: &str,
        since_ts: i64,
        limit: usize,
    ) -> Result<Vec<SessionActivityRow>, String> {
        SessionStoreAccess::new(self)
            .session_messages_after(provider, session_id, since_ts, limit)
            .await
    }

    /// Unix seconds of the most recent session activity.
    ///
    /// `Ok(None)` is the truthful "this store holds no timestamped messages";
    /// a failed query or an unreadable timestamp stays an error rather than
    /// masquerading as an idle store.
    #[hotpath::skip]
    pub async fn latest_session_activity_secs(
        &self,
    ) -> tracedecay_domain::errors::Result<Option<i64>> {
        SessionStoreAccess::new(self)
            .latest_session_activity_secs()
            .await
    }

    /// Reads one message by provider and id. `Ok(None)` is truthful absence;
    /// snapshot, query, and row-decode failures stay typed errors.
    #[hotpath::skip]
    pub async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_domain::errors::Result<Option<SessionMessageRecord>> {
        SessionStoreAccess::new(self)
            .get_session_message(provider, message_id)
            .await
    }

    /// Searches message text for a provider, optionally constrained to one project.
    ///
    /// `Ok(vec![])` is the truthful "nothing matched"; snapshot, query, and
    /// row-decode failures are typed errors instead of an empty result page.
    #[hotpath::skip]
    pub async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<Vec<SessionMessageSearchResult>> {
        SessionStoreAccess::new(self)
            .search_session_messages(provider, project_key, query, limit)
            .await
    }

    /// Lists each session's latest canonical goal state, newest first.
    /// Goals with no native timestamp rank after all timestamped goals
    /// instead of being assigned a fabricated epoch-zero time.
    #[hotpath::skip]
    pub async fn recent_session_goals(
        &self,
        project_key: Option<&str>,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<Vec<SessionMessageSearchResult>> {
        SessionStoreAccess::new(self)
            .recent_session_goals(project_key, limit)
            .await
    }

    #[hotpath::skip]
    pub async fn workflow_fact_rows(
        &self,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>, TraceDecayError> {
        SessionStoreAccess::new(self).workflow_fact_rows().await
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use tracedecay_runtime_core::db::engine::Value;

    use super::*;
    use crate::ParseOffset;

    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

    fn session(provider: &str, session_id: &str, transcript_path: &str) -> SessionRecord {
        SessionRecord {
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
            project_key: "/project".to_owned(),
            project_path: "/project".to_owned(),
            title: None,
            started_at: None,
            ended_at: None,
            transcript_path: Some(transcript_path.to_owned()),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    async fn query_plan(
        database: &RegisteredGlobalDb,
        sql: &str,
        params: Vec<Value>,
    ) -> Vec<String> {
        let snapshot = database.read_snapshot().await.unwrap();
        let mut rows = snapshot
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
            .await
            .unwrap();
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            details.push(row.get::<String>(3).unwrap());
        }
        details
    }

    fn assert_scoped_index_plan(details: &[String], index: &str) {
        assert!(
            details.iter().any(|detail| detail.contains(index)),
            "query plan did not use {index}: {details:?}"
        );
        assert!(
            !details
                .iter()
                .any(|detail| detail.contains("INTEGER PRIMARY KEY")),
            "query plan regressed to a global rowid scan: {details:?}"
        );
        assert!(
            !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
            "query plan regressed to a sort: {details:?}"
        );
    }

    async fn insert_interleaved_session_messages(database: &RegisteredGlobalDb, rows: i64) {
        let final_value = rows - 1;
        let transaction = database.begin_write_transaction().await.unwrap();
        transaction
            .execute_batch(&format!(
                "WITH RECURSIVE rows(value) AS (
                    SELECT 0
                    UNION ALL
                    SELECT value + 1 FROM rows WHERE value < {final_value}
                 )
                 INSERT INTO session_messages(
                    provider, message_id, session_id, role, timestamp, ordinal, text,
                    kind, model, tool_names, source_path, source_offset, metadata_json
                 )
                 SELECT
                    'claude',
                    printf('message-%04d', value),
                    CASE WHEN value % 2 = 0 THEN 'target' ELSE 'noise' END,
                    'assistant',
                    1700000000 + ({final_value} - value / 8),
                    CASE WHEN value % 2 = 0 THEN value / 4 ELSE value / 2 END,
                    'payload', NULL, NULL, printf('tool-%04d', {final_value} - value), NULL, NULL, NULL
                 FROM rows;"
            ))
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }

    #[tokio::test]
    async fn cursor_ingest_authority_is_shared_and_provider_scoped() {
        let profile = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .unwrap();
        let cursor_path = profile.path().join("cursor.jsonl");
        let claude_path = profile.path().join("claude.jsonl");
        std::fs::write(&cursor_path, b"0123456789").unwrap();
        std::fs::write(&claude_path, b"01234567890123456789").unwrap();
        assert!(
            database
                .upsert_session(&session(
                    "cursor",
                    "session.cursor",
                    cursor_path.to_str().unwrap()
                ))
                .await
        );
        assert!(
            database
                .upsert_session(&session(
                    "claude",
                    "session.claude",
                    claude_path.to_str().unwrap()
                ))
                .await
        );
        database
            .set_parse_offset(
                cursor_path.to_str().unwrap(),
                ParseOffset {
                    byte_offset: 4,
                    mtime: 100,
                    file_id: 0,
                },
            )
            .await
            .unwrap();
        database
            .set_parse_offset(
                claude_path.to_str().unwrap(),
                ParseOffset {
                    byte_offset: 20,
                    mtime: 200,
                    file_id: 0,
                },
            )
            .await
            .unwrap();
        for frontier in [
            "host-frontier://kimi/discovery/v1",
            "host-frontier://opencode/sql-rowid/v1",
        ] {
            database
                .set_parse_offset(
                    frontier,
                    ParseOffset {
                        byte_offset: 1,
                        mtime: 0,
                        file_id: 1,
                    },
                )
                .await
                .unwrap();
        }
        for (provider, state, deferred_units) in
            [("kimi", 1, 0), ("opencode", 2, 3), ("claude", 3, 1)]
        {
            database
                .set_parse_offset(
                    &format!("host-coverage://{provider}/v1"),
                    ParseOffset {
                        byte_offset: deferred_units,
                        mtime: 1,
                        file_id: state,
                    },
                )
                .await
                .unwrap();
        }

        let runtime_surface = database.cursor_session_ingest_health().await.unwrap();
        let status_surface = database.cursor_session_ingest_health().await.unwrap();
        let all_providers = database
            .session_ingest_health_for_provider(None)
            .await
            .unwrap();

        assert_eq!(runtime_surface, status_surface);
        assert_eq!(runtime_surface.observed_providers, ["cursor"]);
        assert_eq!(runtime_surface.tracked_transcripts, 1);
        assert_eq!(runtime_surface.pending_transcripts, 1);
        assert_eq!(runtime_surface.pending_bytes, 6);
        assert_eq!(runtime_surface.max_transcript_pending_bytes, 6);
        assert_eq!(runtime_surface.last_ingest_unix, Some(100));
        assert_eq!(
            all_providers.observed_providers,
            ["claude", "cursor", "kimi", "opencode"]
        );
        assert_eq!(
            all_providers.provider_coverage,
            [
                SessionProviderCoverage {
                    provider: "claude".into(),
                    state: SessionProviderCoverageState::Unavailable,
                    deferred_units: 1,
                },
                SessionProviderCoverage {
                    provider: "kimi".into(),
                    state: SessionProviderCoverageState::Complete,
                    deferred_units: 0,
                },
                SessionProviderCoverage {
                    provider: "opencode".into(),
                    state: SessionProviderCoverageState::Partial,
                    deferred_units: 3,
                },
            ]
        );
    }

    #[tokio::test]
    async fn interleaved_session_activity_reads_use_covering_index() {
        let profile = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .unwrap();
        assert!(
            database
                .upsert_session(&session("claude", "target", "/tmp/target.jsonl"))
                .await
        );
        assert!(
            database
                .upsert_session(&session("claude", "noise", "/tmp/noise.jsonl"))
                .await
        );

        insert_interleaved_session_messages(database, 2_048).await;

        let activities = database
            .session_messages_after("claude", "target", 1_700_000_000, 512)
            .await
            .unwrap();
        assert_eq!(activities.len(), 512);
        assert!(activities.windows(2).all(|window| {
            (window[0].timestamp, window[0].ordinal) <= (window[1].timestamp, window[1].ordinal)
        }));
        for window in activities.windows(2) {
            if (window[0].timestamp, window[0].ordinal) == (window[1].timestamp, window[1].ordinal)
            {
                assert!(
                    window[0].tool_names > window[1].tool_names,
                    "message_id tie-break did not produce a stable order: {window:?}"
                );
            }
        }

        let activity_plan = query_plan(
            database,
            SESSION_MESSAGES_AFTER_SQL,
            vec![
                Value::Text("claude".to_owned()),
                Value::Text("target".to_owned()),
                Value::Integer(1_700_000_000),
                Value::Integer(512),
            ],
        )
        .await;
        assert_scoped_index_plan(&activity_plan, "idx_session_messages_session_activity");
        assert!(
            activity_plan
                .iter()
                .any(|detail| detail.contains("COVERING INDEX")),
            "activity query plan is not covering: {activity_plan:?}"
        );
    }
}

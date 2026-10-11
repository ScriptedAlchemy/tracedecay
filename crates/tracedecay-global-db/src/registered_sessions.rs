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
    pub async fn cursor_session_ingest_health(&self) -> Result<SessionIngestHealth, String> {
        SessionStoreAccess::new(self)
            .cursor_session_ingest_health()
            .await
    }

    pub async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> Result<SessionIngestHealth, String> {
        SessionStoreAccess::new(self)
            .session_ingest_health_for_provider(provider)
            .await
    }

    pub async fn has_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<bool, String> {
        SessionStoreAccess::new(self)
            .has_session_message(provider, message_id)
            .await
    }

    pub async fn existing_session_message_ids(
        &self,
        provider: &str,
        message_ids: &[String],
    ) -> Result<Vec<String>, String> {
        SessionStoreAccess::new(self)
            .existing_session_message_ids(provider, message_ids)
            .await
    }

    pub async fn session_message_count(&self) -> Result<i64, String> {
        SessionStoreAccess::new(self).session_message_count().await
    }

    pub async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> Result<i64, String> {
        SessionStoreAccess::new(self)
            .session_message_count_for_project(project_key)
            .await
    }

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
    pub async fn latest_session_activity_secs(
        &self,
    ) -> tracedecay_domain::errors::Result<Option<i64>> {
        SessionStoreAccess::new(self)
            .latest_session_activity_secs()
            .await
    }

    /// Reads one message by provider and id. `Ok(None)` is truthful absence;
    /// snapshot, query, and row-decode failures stay typed errors.
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
    pub async fn recent_session_goals(
        &self,
        project_key: Option<&str>,
        limit: usize,
    ) -> tracedecay_domain::errors::Result<Vec<SessionMessageSearchResult>> {
        SessionStoreAccess::new(self)
            .recent_session_goals(project_key, limit)
            .await
    }

    pub async fn workflow_fact_rows(
        &self,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>, TraceDecayError> {
        SessionStoreAccess::new(self).workflow_fact_rows().await
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::ParseOffset;

    use crate::tests::harness::{
        HostAdmissionScope, HostAdmissionTestRuntimeV1, statement_vm_steps,
    };

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

    /// Interleaves `rows` target and noise messages. Timestamps count down from
    /// `newest`; within one timestamp ordinals tie, so the message id decides.
    fn interleaved_messages_sql(first: i64, rows: i64, newest: i64) -> String {
        let last = first + rows - 1;
        format!(
            "WITH RECURSIVE rows(value) AS (
                SELECT {first}
                UNION ALL
                SELECT value + 1 FROM rows WHERE value < {last}
             )
             INSERT INTO lcm_raw_messages(
                provider, message_id, session_id, role, timestamp, ordinal, content,
                kind, model, tool_names, source_path, source_offset, metadata_json,
                content_hash, storage_kind
             )
             SELECT
                'claude',
                printf('message-%06d', value),
                CASE WHEN value % 2 = 0 THEN 'target' ELSE 'noise' END,
                'assistant',
                {newest} - (value - {first}) / 8,
                CASE WHEN value % 2 = 0 THEN value / 4 ELSE value / 2 END,
                'payload', NULL, NULL, printf('tool-%06d', {last} - value), NULL, NULL, NULL,
                'hash', 'inline'
             FROM rows;"
        )
    }

    async fn insert_interleaved_messages(database: &RegisteredGlobalDb, rows: i64) {
        let transaction = database.begin_write_transaction().await.unwrap();
        transaction
            .execute_batch(&interleaved_messages_sql(0, rows, 1_700_000_000 + rows - 1))
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
                    reason: None,
                },
                SessionProviderCoverage {
                    provider: "kimi".into(),
                    state: SessionProviderCoverageState::Complete,
                    deferred_units: 0,
                    reason: None,
                },
                SessionProviderCoverage {
                    provider: "opencode".into(),
                    state: SessionProviderCoverageState::Partial,
                    deferred_units: 3,
                    reason: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn coverage_status_names_refusals_and_skips_packed_reason_codes() {
        let profile = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .unwrap();
        let database = runtime
            .registered_database(HostAdmissionScope::Profile)
            .unwrap();
        // 259 is the unreleased packed shape: state 3 with reason 1 in bits 8+.
        for (provider, file_id) in [("claude", 3), ("kimi", 7), ("opencode", 259), ("pi", 4)] {
            database
                .set_parse_offset(
                    &format!("host-coverage://{provider}/v1"),
                    ParseOffset {
                        byte_offset: 1,
                        mtime: 1,
                        file_id,
                    },
                )
                .await
                .unwrap();
        }

        let health = database
            .session_ingest_health_for_provider(None)
            .await
            .unwrap();

        assert_eq!(
            serde_json::to_value(&health.provider_coverage).unwrap(),
            serde_json::json!([
                {"provider": "claude", "state": "unavailable", "deferred_units": 1},
                {
                    "provider": "kimi",
                    "state": "unavailable",
                    "deferred_units": 1,
                    "reason": "source_identity_unavailable",
                },
                {
                    "provider": "pi",
                    "state": "unavailable",
                    "deferred_units": 1,
                    "reason": "database_missing",
                },
            ])
        );
    }

    #[tokio::test]
    async fn interleaved_session_activity_pages_cost_their_rows_not_the_session() {
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

        insert_interleaved_messages(database, 2_048).await;

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

        // A page reads its own rows: newer activity in the same session, eight
        // times the first batch, must not raise the cost of the oldest page.
        let path = database.db_path().to_path_buf();
        drop(runtime);
        let connection = tracedecay_sql::Connection::open(&path).unwrap();
        let page_steps = || {
            statement_vm_steps(
                &connection,
                SESSION_MESSAGES_AFTER_SQL,
                tracedecay_sql::params!["claude", "target", 1_700_000_000_i64, 512_i64],
            )
        };
        let base = page_steps();
        connection
            .execute_batch(&interleaved_messages_sql(2_048, 16_384, 1_800_000_000))
            .unwrap();
        let grown = page_steps();
        assert!(
            grown <= 2 * base,
            "the oldest 512-row page must cost its rows, not the session's: {base} -> {grown}"
        );
    }
}

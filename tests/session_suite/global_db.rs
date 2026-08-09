use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay::global_db::{AnalyticsEventInsert, AnalyticsEventQuery};
use tracedecay_sessions::runtime::lcm::LcmStorageKind;
use tracedecay_sessions::runtime::{
    SessionMessageRecord, SessionMessageSearchResult, SessionRecord, SessionSearchFilters,
    SessionSearchScope, SessionSearchTimeRange,
};

use crate::common::{global_message as sample_message, global_session as sample_session};

async fn open_isolated_db(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("registered profile runtime")
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

trait RegisteredSessionTestExt {
    async fn append_analytics_event(
        &self,
        event: &AnalyticsEventInsert,
    ) -> tracedecay::errors::Result<i64>;
    async fn query_analytics_events(
        &self,
        query: &AnalyticsEventQuery,
    ) -> tracedecay::errors::Result<Vec<tracedecay::global_db::AnalyticsEventRecord>>;
    async fn get_session(&self, provider: &str, session_id: &str) -> Option<SessionRecord>;
    async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<SessionMessageRecord>;
    async fn upsert_session(&self, session: &SessionRecord) -> bool;
    async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool;
    async fn lcm_load_raw_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay_sessions::runtime::lcm::LcmRawMessage>;
    async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult>;
    async fn search_session_messages_filtered(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
    ) -> Vec<SessionMessageSearchResult>;
    async fn search_session_messages_git_scoped(
        &self,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
        git_filter: &tracedecay_sessions::runtime::git_correlation::GitScopeFilter,
    ) -> Vec<SessionMessageSearchResult>;
    async fn git_record_span_observation(
        &self,
        observation: &tracedecay_sessions::runtime::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> tracedecay::errors::Result<i64>;
    async fn session_message_count(&self) -> tracedecay::errors::Result<i64>;
    async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> tracedecay::errors::Result<i64>;
    async fn set_parse_offset(&self, path: &str, offset: tracedecay::global_db::ParseOffset);
    async fn session_ingest_health(&self) -> tracedecay::global_db::SessionIngestHealth;
    async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> tracedecay::global_db::SessionIngestHealth;
}

impl RegisteredSessionTestExt for HostAdmissionTestRuntimeV1 {
    async fn append_analytics_event(
        &self,
        event: &AnalyticsEventInsert,
    ) -> tracedecay::errors::Result<i64> {
        self.append_profile_analytics_event_for_test(event).await
    }

    async fn query_analytics_events(
        &self,
        query: &AnalyticsEventQuery,
    ) -> tracedecay::errors::Result<Vec<tracedecay::global_db::AnalyticsEventRecord>> {
        self.query_profile_analytics_events_for_test(query).await
    }

    async fn get_session(&self, provider: &str, session_id: &str) -> Option<SessionRecord> {
        self.session_for_test(HostAdmissionScope::Profile, provider, session_id)
            .await
            .expect("registered session lookup")
    }

    async fn get_session_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<SessionMessageRecord> {
        self.session_message_for_test(HostAdmissionScope::Profile, provider, message_id)
            .await
            .expect("registered session message lookup")
    }

    async fn upsert_session(&self, session: &SessionRecord) -> bool {
        self.upsert_session_for_test(HostAdmissionScope::Profile, session)
            .await
            .expect("registered session upsert")
    }

    async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool {
        self.upsert_session_message_for_test(HostAdmissionScope::Profile, message)
            .await
            .unwrap_or(false)
    }

    async fn lcm_load_raw_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay_sessions::runtime::lcm::LcmRawMessage> {
        self.lcm_load_raw_message_for_test(provider, message_id)
            .await
    }

    async fn search_session_messages(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_for_test(
            HostAdmissionScope::Profile,
            provider,
            project_key,
            query,
            limit,
        )
        .await
        .expect("registered session message search")
    }

    async fn search_session_messages_filtered(
        &self,
        provider: &str,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_filtered_for_test(
            HostAdmissionScope::Profile,
            provider,
            project_key,
            query,
            limit,
            filters,
        )
        .await
        .expect("registered filtered session message search")
    }

    async fn search_session_messages_git_scoped(
        &self,
        provider: Option<&str>,
        project_key: Option<&str>,
        query: &str,
        limit: usize,
        filters: SessionSearchFilters<'_>,
        git_filter: &tracedecay_sessions::runtime::git_correlation::GitScopeFilter,
    ) -> Vec<SessionMessageSearchResult> {
        self.search_session_messages_git_scoped_for_test(
            HostAdmissionScope::Profile,
            provider,
            project_key,
            query,
            limit,
            filters,
            git_filter,
        )
        .await
        .expect("registered git-scoped session message search")
    }

    async fn git_record_span_observation(
        &self,
        observation: &tracedecay_sessions::runtime::git_correlation::SpanObservation,
        merge_gap_secs: i64,
    ) -> tracedecay::errors::Result<i64> {
        self.record_session_span_for_test(HostAdmissionScope::Profile, observation, merge_gap_secs)
            .await
    }

    async fn session_message_count(&self) -> tracedecay::errors::Result<i64> {
        self.session_message_count_for_test(HostAdmissionScope::Profile, None)
            .await
    }

    async fn session_message_count_for_project(
        &self,
        project_key: &str,
    ) -> tracedecay::errors::Result<i64> {
        self.session_message_count_for_test(HostAdmissionScope::Profile, Some(project_key))
            .await
    }

    async fn set_parse_offset(&self, path: &str, offset: tracedecay::global_db::ParseOffset) {
        self.set_parse_offset_for_test(HostAdmissionScope::Profile, path, offset)
            .await
            .expect("registered parse offset write");
    }

    async fn session_ingest_health(&self) -> tracedecay::global_db::SessionIngestHealth {
        self.session_ingest_health_for_test(HostAdmissionScope::Profile, None)
            .await
            .expect("registered session ingest health")
    }

    async fn session_ingest_health_for_provider(
        &self,
        provider: Option<&str>,
    ) -> tracedecay::global_db::SessionIngestHealth {
        self.session_ingest_health_for_test(HostAdmissionScope::Profile, provider)
            .await
            .expect("registered provider session ingest health")
    }
}

fn analytics_event(
    session_id: Option<&str>,
    timestamp: i64,
    event_kind: &str,
) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: "project-a".to_string(),
        session_id: session_id.map(ToOwned::to_owned),
        timestamp,
        event_kind: event_kind.to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    }
}

fn analytics_query(
    session_id: Option<&str>,
    event_kind: Option<&str>,
    limit: usize,
) -> AnalyticsEventQuery {
    AnalyticsEventQuery {
        provider: Some("codex".to_string()),
        project_id: Some("project-a".to_string()),
        session_id: session_id.map(ToOwned::to_owned),
        event_kind: event_kind.map(ToOwned::to_owned),
        since: None,
        until: None,
        before_id: None,
        limit,
    }
}

async fn append_analytics_event(
    db: &HostAdmissionTestRuntimeV1,
    event: &AnalyticsEventInsert,
    label: &str,
) -> i64 {
    db.append_analytics_event(event).await.expect(label)
}

#[tokio::test]
async fn global_db_opens_with_session_schema() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;

    assert!(db.get_session("cursor", "missing").await.is_none());
    assert!(
        db.search_session_messages("cursor", None, "not-present", 10)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn project_registry_path_aliases_resolve_exactly_without_active_fallback() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let active_root = tmp.path().join("active");
    let target_root = tmp.path().join("target");
    let nested_target_alias = target_root.join("nested/worktree");
    let missing_root = tmp.path().join("missing");
    std::fs::create_dir_all(&active_root).unwrap();
    std::fs::create_dir_all(&target_root).unwrap();
    std::fs::create_dir_all(&nested_target_alias).unwrap();

    db.upsert_code_project("proj_active", &active_root, None, None, Some("main"))
        .await
        .expect("active project should upsert");
    let target = db
        .upsert_code_project("proj_target", &target_root, None, None, Some("main"))
        .await
        .expect("target project should upsert");
    db.upsert_project_alias(&nested_target_alias, &target.project_id)
        .await
        .expect("nested target alias should upsert");

    let by_root = db
        .project_registry_context_by_alias(&target_root)
        .await
        .expect("target root alias lookup should succeed")
        .expect("target root alias should resolve");
    assert_eq!(by_root.project.project_id, "proj_target");

    let by_nested_alias = db
        .project_registry_context_by_alias(&nested_target_alias)
        .await
        .expect("nested target alias lookup should succeed")
        .expect("nested target alias should resolve");
    assert_eq!(by_nested_alias.project.project_id, "proj_target");

    assert!(
        db.project_registry_context_by_id("proj_missing")
            .await
            .is_none(),
        "unresolved project_id selectors must not synthesize an active-project context"
    );
    assert!(
        db.project_registry_context_by_alias(&missing_root)
            .await
            .expect("missing path alias lookup should succeed")
            .is_none(),
        "unresolved path selectors must not fall back to the active registered project"
    );
}

#[tokio::test]
async fn analytics_events_append_and_query_by_provider_project_session_and_kind() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;

    let tool_event = AnalyticsEventInsert {
        tool_name: Some("tracedecay_context".to_string()),
        tool_category: Some("mcp".to_string()),
        hint_category: Some("search".to_string()),
        hint_id: Some("hint-1".to_string()),
        outcome: Some("success".to_string()),
        metadata_json: Some(r#"{"tokens_saved":42}"#.to_string()),
        ..analytics_event(Some("session-a"), 1_715_000_123, "tool")
    };
    let first_id = append_analytics_event(&db, &tool_event, "append analytics event").await;
    let second_id =
        append_analytics_event(&db, &tool_event, "append identical analytics event").await;
    assert_ne!(first_id, second_id, "analytics storage must be append-only");

    append_analytics_event(
        &db,
        &AnalyticsEventInsert {
            hook_name: Some("pre-tool-use".to_string()),
            skill_name: Some("tracedecay:exploring-code".to_string()),
            outcome: Some("shown".to_string()),
            metadata_json: Some(r#"{"source":"test"}"#.to_string()),
            ..analytics_event(Some("session-a"), 1_715_000_124, "skill")
        },
        "append skill analytics event",
    )
    .await;
    append_analytics_event(
        &db,
        &AnalyticsEventInsert {
            provider: "cursor".to_string(),
            project_id: "project-b".to_string(),
            tool_name: Some("other_tool".to_string()),
            tool_category: Some("local".to_string()),
            outcome: Some("success".to_string()),
            ..analytics_event(Some("session-b"), 1_715_000_125, "tool")
        },
        "append filtered analytics event",
    )
    .await;

    let events = db
        .query_analytics_events(&analytics_query(Some("session-a"), Some("tool"), 10))
        .await
        .expect("query analytics events");

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, first_id);
    assert_eq!(events[1].id, second_id);
    assert_eq!(events[0].provider, "codex");
    assert_eq!(events[0].project_id, "project-a");
    assert_eq!(events[0].session_id.as_deref(), Some("session-a"));
    assert_eq!(events[0].timestamp, 1_715_000_123);
    assert_eq!(events[0].event_kind, "tool");
    assert_eq!(events[0].tool_name.as_deref(), Some("tracedecay_context"));
    assert_eq!(events[0].tool_category.as_deref(), Some("mcp"));
    assert_eq!(events[0].hint_category.as_deref(), Some("search"));
    assert_eq!(events[0].hint_id.as_deref(), Some("hint-1"));
    assert_eq!(events[0].outcome.as_deref(), Some("success"));
    assert_eq!(
        events[0].metadata_json.as_deref(),
        Some(r#"{"tokens_saved":42}"#)
    );
}

#[tokio::test]
async fn analytics_events_query_since_bounds_timestamp() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;

    append_analytics_event(
        &db,
        &analytics_event(Some("session-old"), 1_715_000_100, "tool"),
        "append old analytics event",
    )
    .await;
    let recent_id = append_analytics_event(
        &db,
        &analytics_event(Some("session-new"), 1_715_000_200, "tool"),
        "append recent analytics event",
    )
    .await;

    // A `since` lower bound drops rows older than the boundary; the boundary
    // itself is inclusive.
    let events = db
        .query_analytics_events(&AnalyticsEventQuery {
            since: Some(1_715_000_200),
            limit: 100,
            ..Default::default()
        })
        .await
        .expect("query analytics events since bound");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, recent_id);
    assert_eq!(events[0].timestamp, 1_715_000_200);
}

#[tokio::test]
async fn open_at_upgrades_existing_global_db_with_analytics_events_table() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("global.db");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            ended_at INTEGER,
            transcript_path TEXT,
            metadata_json TEXT,
            PRIMARY KEY(provider, session_id)
        );",
    )
    .unwrap();
    drop(conn);

    let db = HostAdmissionTestRuntimeV1::profile(db_path.parent().expect("profile root"))
        .await
        .expect("registered profile runtime");
    let event = AnalyticsEventInsert {
        hook_name: Some("post-tool-use".to_string()),
        tool_name: Some("shell".to_string()),
        tool_category: Some("local".to_string()),
        outcome: Some("recorded".to_string()),
        metadata_json: Some(r#"{"upgraded":true}"#.to_string()),
        ..analytics_event(None, 1_715_000_126, "hook")
    };
    let id = append_analytics_event(&db, &event, "append analytics event after upgrade").await;

    let events = db
        .query_analytics_events(&analytics_query(None, Some("hook"), 5))
        .await
        .expect("query analytics events after upgrade");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, id);
    assert_eq!(events[0].hook_name.as_deref(), Some("post-tool-use"));

    let index_count = db
        .profile_analytics_indexes_present_for_test()
        .await
        .unwrap();
    assert_eq!(
        index_count, 2,
        "analytics aggregate indexes must migrate on open"
    );
}

#[tokio::test]
async fn analytics_events_preserve_assistant_hook_tool_and_skill_fields() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;

    let assistant_id = append_analytics_event(
        &db,
        &AnalyticsEventInsert {
            hint_category: Some("workflow".to_string()),
            hint_id: Some("hint-assistant".to_string()),
            outcome: Some("shown".to_string()),
            metadata_json: Some(r#"{"model":"gpt-5-codex","turn":1}"#.to_string()),
            ..analytics_event(Some("session-required-fields"), 1_715_000_200, "assistant")
        },
        "append assistant analytics event",
    )
    .await;
    append_analytics_event(
        &db,
        &AnalyticsEventInsert {
            hook_name: Some("post-tool-use".to_string()),
            outcome: Some("ok".to_string()),
            metadata_json: Some(r#"{"exit_code":0}"#.to_string()),
            ..analytics_event(Some("session-required-fields"), 1_715_000_201, "hook")
        },
        "append hook analytics event",
    )
    .await;
    append_analytics_event(
        &db,
        &AnalyticsEventInsert {
            tool_name: Some("tracedecay_context".to_string()),
            tool_category: Some("mcp".to_string()),
            outcome: Some("accepted".to_string()),
            metadata_json: Some(r#"{"tokens_saved":42}"#.to_string()),
            ..analytics_event(Some("session-required-fields"), 1_715_000_202, "tool")
        },
        "append tool analytics event",
    )
    .await;
    append_analytics_event(
        &db,
        &AnalyticsEventInsert {
            skill_name: Some("superpowers:test-driven-development".to_string()),
            outcome: Some("used".to_string()),
            metadata_json: Some(r#"{"stage":"red"}"#.to_string()),
            ..analytics_event(Some("session-required-fields"), 1_715_000_203, "skill")
        },
        "append skill analytics event",
    )
    .await;

    let events = db
        .query_analytics_events(&analytics_query(Some("session-required-fields"), None, 10))
        .await
        .expect("query analytics events");

    assert_eq!(events.len(), 4);
    assert_eq!(events[0].id, assistant_id);
    assert_eq!(events[0].event_kind, "assistant");
    assert_eq!(events[0].hint_category.as_deref(), Some("workflow"));
    assert_eq!(events[0].hint_id.as_deref(), Some("hint-assistant"));
    assert_eq!(events[0].outcome.as_deref(), Some("shown"));
    assert_eq!(
        events[0].metadata_json.as_deref(),
        Some(r#"{"model":"gpt-5-codex","turn":1}"#)
    );
    assert_eq!(events[1].event_kind, "hook");
    assert_eq!(events[1].hook_name.as_deref(), Some("post-tool-use"));
    assert_eq!(events[2].event_kind, "tool");
    assert_eq!(events[2].tool_name.as_deref(), Some("tracedecay_context"));
    assert_eq!(events[2].tool_category.as_deref(), Some("mcp"));
    assert_eq!(events[3].event_kind, "skill");
    assert_eq!(
        events[3].skill_name.as_deref(),
        Some("superpowers:test-driven-development")
    );
    let assistant_events = db
        .query_analytics_events(&analytics_query(
            Some("session-required-fields"),
            Some("assistant"),
            10,
        ))
        .await
        .expect("query assistant analytics events");
    assert_eq!(assistant_events.len(), 1);
}

#[tokio::test]
async fn upsert_session_round_trips_and_updates() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let mut session = sample_session("cursor", "session-1", "project-a");

    db.upsert_session(&session).await;
    session.title = Some("Updated title".to_string());
    session.ended_at = Some(1_715_000_900);
    session.metadata_json = Some(r#"{"source":"test","updated":true}"#.to_string());
    db.upsert_session(&session).await;

    let fetched = db
        .get_session("cursor", "session-1")
        .await
        .expect("session should exist");
    assert_eq!(fetched.project_key, "project-a");
    assert_eq!(fetched.title.as_deref(), Some("Updated title"));
    assert_eq!(fetched.ended_at, Some(1_715_000_900));
    assert_eq!(
        fetched.metadata_json.as_deref(),
        Some(r#"{"source":"test","updated":true}"#)
    );
}

#[tokio::test]
async fn session_message_count_filters_by_project_key() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    for (provider, session_id, project_key, message_id) in [
        ("codex", "session-a", "project-a", "message-a"),
        ("claude", "session-b", "project-a", "message-b"),
        ("cursor", "session-c", "project-b", "message-c"),
    ] {
        db.upsert_session(&sample_session(provider, session_id, project_key))
            .await;
        assert!(
            db.upsert_session_message(&sample_message(
                provider,
                message_id,
                session_id,
                "TraceDecay diagnostics fixture.",
            ))
            .await
        );
    }

    assert_eq!(db.session_message_count().await.unwrap(), 3);
    assert_eq!(
        db.session_message_count_for_project("project-a")
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        db.session_message_count_for_project("missing-project")
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn upsert_session_message_round_trips_and_updates() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    db.upsert_session(&session).await;

    let mut message = sample_message(
        "cursor",
        "message-1",
        "session-1",
        "Initial answer about parsing transcripts.",
    );
    assert!(db.upsert_session_message(&message).await);
    let updated = format!(
        "Updated answer about parsing transcripts.\n{}::updated-tail",
        "x".repeat(tracedecay_sessions::runtime::lcm::MAX_DERIVED_TEXT_CHARS * 2)
    );
    message.text = updated.clone();
    message.tool_names = Some("tracedecay_context".to_string());
    message.source_offset = Some(99);
    assert!(db.upsert_session_message(&message).await);

    let fetched = db
        .get_session_message("cursor", "message-1")
        .await
        .expect("message should exist");
    assert_eq!(fetched.session_id, "session-1");
    assert!(
        fetched
            .text
            .starts_with("Updated answer about parsing transcripts.")
    );
    assert!(
        fetched.text.chars().count() <= tracedecay_sessions::runtime::lcm::MAX_DERIVED_TEXT_CHARS
    );
    assert!(
        fetched
            .text
            .contains(tracedecay_sessions::runtime::lcm::DERIVED_TRUNCATION_MARKER)
    );
    assert_eq!(fetched.tool_names.as_deref(), Some("tracedecay_context"));
    assert_eq!(fetched.source_offset, Some(99));

    let raw = db
        .lcm_load_raw_message("cursor", "message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, updated);
    assert_eq!(raw.content_hash, sha256_hex(&updated));

    let (snippet_text, index_text) = db
        .lcm_raw_message_search_fields_for_test("cursor", "message-1")
        .await
        .expect("raw search fields")
        .expect("raw message should exist");
    assert!(
        snippet_text.chars().count()
            <= tracedecay_sessions::runtime::lcm::MAX_DERIVED_SNIPPET_CHARS
    );
    assert!(snippet_text.contains(tracedecay_sessions::runtime::lcm::DERIVED_TRUNCATION_MARKER));
    assert!(
        index_text.chars().count() <= tracedecay_sessions::runtime::lcm::MAX_DERIVED_TEXT_CHARS
    );
    assert!(index_text.contains(tracedecay_sessions::runtime::lcm::DERIVED_TRUNCATION_MARKER));
}

#[tokio::test]
async fn upsert_session_message_rejects_missing_session_without_orphan_raw() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let message = sample_message("cursor", "orphan-message", "missing-session", "orphan text");

    assert!(!db.upsert_session_message(&message).await);
    assert!(
        db.get_session_message("cursor", "orphan-message")
            .await
            .is_none()
    );
    assert!(
        db.lcm_load_raw_message("cursor", "orphan-message")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn upsert_session_message_rolls_back_raw_when_projection_fails() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    assert!(db.upsert_session(&session).await);

    db.set_session_message_projection_failure_for_test(HostAdmissionScope::Profile, true)
        .await
        .expect("install projection failure fixture");

    let message = sample_message(
        "cursor",
        "message-rollback",
        "session-1",
        "raw before failure",
    );
    assert!(!db.upsert_session_message(&message).await);
    assert!(
        db.lcm_load_raw_message("cursor", "message-rollback")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn upsert_session_message_preserves_oversized_text_losslessly() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    db.upsert_session(&session).await;

    let oversized = format!("{}{}", "x".repeat(300_000), "::lossless-tail");
    let message = sample_message("cursor", "message-1", "session-1", &oversized);
    assert!(db.upsert_session_message(&message).await);

    let compatibility = db
        .get_session_message("cursor", "message-1")
        .await
        .expect("compatibility message should exist");
    assert!(
        compatibility.text.chars().count()
            <= tracedecay_sessions::runtime::lcm::MAX_DERIVED_TEXT_CHARS
    );
    assert!(
        compatibility
            .text
            .contains(tracedecay_sessions::runtime::lcm::DERIVED_TRUNCATION_MARKER)
    );

    let raw = db
        .lcm_load_raw_message("cursor", "message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, oversized);
    assert!(raw.content.ends_with("::lossless-tail"));
    assert!(!raw.legacy_source);
    assert!(!raw.legacy_truncated);
}

#[tokio::test]
async fn upsert_session_message_externalizes_tool_payload_without_indexing_body_or_metadata() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    assert!(db.upsert_session(&session).await);

    let body_secret = "globaldbbodysecretnotindexed";
    let metadata_secret = "globaldbmetadatasecretnotindexed";
    let payload = format!("tool output {body_secret}\n{}", "T".repeat(900_000));
    let mut message = sample_message("cursor", "tool-large", "session-1", &payload);
    message.role = "tool".to_string();
    message.kind = Some("tool_result".to_string());
    message.metadata_json = Some(format!(r#"{{"preview":"{metadata_secret}"}}"#));
    assert!(db.upsert_session_message(&message).await);

    let raw = db
        .lcm_load_raw_message("cursor", "tool-large")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.storage_kind, LcmStorageKind::External);
    assert!(raw.content.is_empty());
    assert!(!raw.content.contains(body_secret));
    assert!(
        !raw.metadata_json
            .as_deref()
            .unwrap_or("")
            .contains(metadata_secret)
    );
    let payload_ref = raw.payload_ref.expect("payload ref");

    let fetched = db
        .get_session_message("cursor", "tool-large")
        .await
        .expect("projection should exist");
    assert!(
        fetched.text.chars().count() <= tracedecay_sessions::runtime::lcm::MAX_DERIVED_TEXT_CHARS
    );
    assert!(!fetched.text.contains(body_secret));
    assert!(
        fetched
            .text
            .contains("[Externalized LCM ingest payload: kind=tool_result;")
    );
    let projection_metadata = fetched.metadata_json.as_deref().unwrap_or("");
    assert!(!projection_metadata.contains(metadata_secret));
    assert!(projection_metadata.contains("\"external_payload\":true"));
    assert!(projection_metadata.contains(&payload_ref));

    let (snippet_text, index_text) = db
        .lcm_raw_message_search_fields_for_test("cursor", "tool-large")
        .await
        .expect("raw search fields")
        .expect("raw message should exist");
    assert!(!snippet_text.contains(body_secret));
    assert!(!index_text.contains(body_secret));
    assert!(!snippet_text.contains(metadata_secret));
    assert!(!index_text.contains(metadata_secret));
    assert_eq!(
        db.lcm_raw_message_fts_count_for_test(body_secret)
            .await
            .expect("body FTS count"),
        0
    );
    assert_eq!(
        db.lcm_raw_message_fts_count_for_test(metadata_secret)
            .await
            .expect("metadata FTS count"),
        0
    );
    assert!(
        db.search_session_messages("cursor", Some("project-a"), body_secret, 10)
            .await
            .is_empty()
    );
    assert!(
        db.search_session_messages("cursor", Some("project-a"), metadata_secret, 10)
            .await
            .is_empty()
    );

    let expanded = db
        .lcm_expand_for_test(tracedecay_sessions::runtime::lcm::LcmExpandRequest {
            provider: "cursor".to_string(),
            session_id: "session-1".to_string(),
            target: tracedecay_sessions::runtime::lcm::LcmExpandTarget::ExternalPayload {
                payload_ref: payload_ref.clone(),
            },
            content_slice: Some(tracedecay_sessions::runtime::lcm::LcmContentSlice {
                offset: 0,
                limit: payload.chars().count(),
            }),
            source_offset: 0,
            source_limit: None,
        })
        .await
        .expect("payload should expand");
    assert_eq!(expanded.content, payload);
}

#[tokio::test]
async fn search_session_messages_uses_fts_and_filters_provider_project() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let cursor_a = sample_session("cursor", "cursor-a", "project-a");
    let cursor_b = sample_session("cursor", "cursor-b", "project-b");
    let codex_a = sample_session("codex", "codex-a", "project-a");
    db.upsert_session(&cursor_a).await;
    db.upsert_session(&cursor_b).await;
    db.upsert_session(&codex_a).await;

    db.upsert_session_message(&sample_message(
        "cursor",
        "cursor-msg-a",
        "cursor-a",
        "The billing ingestion plan is ready.",
    ))
    .await;
    db.upsert_session_message(&sample_message(
        "cursor",
        "cursor-msg-b",
        "cursor-b",
        "The billing ingestion plan belongs to another project.",
    ))
    .await;
    db.upsert_session_message(&sample_message(
        "codex",
        "codex-msg-a",
        "codex-a",
        "The billing ingestion plan belongs to another provider.",
    ))
    .await;

    let results = db
        .search_session_messages("cursor", Some("project-a"), "billing", 10)
        .await;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message.message_id, "cursor-msg-a");
    assert_eq!(results[0].session.project_key, "project-a");
    assert!(results[0].score > 0.0);
}

#[tokio::test]
async fn search_session_messages_applies_hyphen_filter_before_limit() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "cursor-hyphen", "project-a");
    db.upsert_session(&session).await;

    for index in 0..12 {
        db.upsert_session_message(&sample_message(
            "cursor",
            &format!("plain-{index}"),
            "cursor-hyphen",
            &format!("foo bar filler message {index}"),
        ))
        .await;
    }
    db.upsert_session_message(&sample_message(
        "cursor",
        "hyphenated",
        "cursor-hyphen",
        "the literal foo-bar marker should survive limiting",
    ))
    .await;

    let results = db
        .search_session_messages("cursor", Some("project-a"), "foo-bar", 10)
        .await;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message.message_id, "hyphenated");
}

#[tokio::test]
async fn search_session_messages_git_scoped_by_branch_with_hyphen_term() {
    use tracedecay_sessions::runtime::git_correlation::{
        GitScopeFilter, SpanObservation, SpanSource,
    };

    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "cursor-scoped", "project-a");
    db.upsert_session(&session).await;
    db.upsert_session_message(&sample_message(
        "cursor",
        "scoped-msg",
        "cursor-scoped",
        "the literal foo-bar marker on a scoped branch",
    ))
    .await;

    // The session was active on `feat/x`; record a span so the scoping EXISTS
    // subquery has a row to match against.
    db.git_record_span_observation(
        &SpanObservation {
            provider: "cursor".to_string(),
            session_id: "cursor-scoped".to_string(),
            thread_id: None,
            branch: Some("feat/x".to_string()),
            worktree: "/repo".to_string(),
            ts: 1_000,
            source: SpanSource::HookRoute,
        },
        600,
    )
    .await
    .unwrap();

    let filters = SessionSearchFilters {
        scope: SessionSearchScope::All,
        message_type: Default::default(),
        parent_session_id: None,
        time_range: SessionSearchTimeRange::default(),
    };

    // A hyphenated query term appends a numbered placeholder *after* the git
    // EXISTS predicate's anonymous placeholders; the match must still resolve.
    let matched = db
        .search_session_messages_git_scoped(
            Some("cursor"),
            Some("project-a"),
            "foo-bar",
            10,
            filters,
            &GitScopeFilter::from_args(Some("feat/x"), None, None).unwrap(),
        )
        .await;
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].message.message_id, "scoped-msg");

    // A branch the session was never on excludes the message.
    let excluded = db
        .search_session_messages_git_scoped(
            Some("cursor"),
            Some("project-a"),
            "foo-bar",
            10,
            filters,
            &GitScopeFilter::from_args(Some("other"), None, None).unwrap(),
        )
        .await;
    assert!(excluded.is_empty());
}

#[tokio::test]
async fn search_session_messages_filters_by_message_timestamp() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "cursor-time", "project-a");
    db.upsert_session(&session).await;

    let mut old = sample_message(
        "cursor",
        "old-time-msg",
        "cursor-time",
        "the orchard clock marker appears before the window",
    );
    old.timestamp = Some(10);
    db.upsert_session_message(&old).await;

    let mut target = sample_message(
        "cursor",
        "target-time-msg",
        "cursor-time",
        "the orchard clock marker appears inside the window",
    );
    target.timestamp = Some(20);
    db.upsert_session_message(&target).await;

    let mut new = sample_message(
        "cursor",
        "new-time-msg",
        "cursor-time",
        "the orchard clock marker appears after the window",
    );
    new.timestamp = Some(30);
    db.upsert_session_message(&new).await;

    let results = db
        .search_session_messages_filtered(
            "cursor",
            Some("project-a"),
            "orchard clock marker",
            10,
            SessionSearchFilters {
                scope: SessionSearchScope::All,
                message_type: Default::default(),
                parent_session_id: None,
                time_range: SessionSearchTimeRange {
                    start_time: Some(15),
                    end_time: Some(25),
                },
            },
        )
        .await;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message.message_id, "target-time-msg");
}

#[tokio::test]
async fn open_at_upgrades_existing_sessions_table_with_parent_columns() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let db_path = tracedecay_sessions::runtime::user_sessions_db_path(&profile_root);
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            ended_at INTEGER,
            transcript_path TEXT,
            metadata_json TEXT,
            PRIMARY KEY(provider, session_id)
        );
        INSERT INTO sessions (
            provider, session_id, project_key, project_path, title, started_at,
            ended_at, transcript_path, metadata_json
        ) VALUES (
            'cursor', 'old-parent', 'project-a', '/tmp/project', 'Old title',
            1715000000, NULL, '/tmp/project/old.jsonl', '{\"source\":\"old\"}'
        );",
    )
    .unwrap();
    drop(conn);

    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .expect("registered profile runtime");
    let session = db
        .get_session("cursor", "old-parent")
        .await
        .expect("old row should survive schema upgrade");

    assert_eq!(session.parent_session_id, None);
    assert!(!session.is_subagent);
    assert_eq!(session.agent_id, None);
    assert_eq!(session.parent_tool_use_id, None);

    let child = SessionRecord {
        session_id: "child-agent".to_string(),
        parent_session_id: Some("old-parent".to_string()),
        is_subagent: true,
        agent_id: Some("child-agent".to_string()),
        ..sample_session("cursor", "child-agent", "project-a")
    };
    assert!(db.upsert_session(&child).await);

    let fetched = db
        .get_session("cursor", "child-agent")
        .await
        .expect("child row should round-trip");
    assert_eq!(fetched.parent_session_id.as_deref(), Some("old-parent"));
    assert!(fetched.is_subagent);
    assert_eq!(fetched.agent_id.as_deref(), Some("child-agent"));
}

#[tokio::test]
async fn search_session_messages_filters_parent_and_subagent_scope() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let parent = sample_session("cursor", "parent", "project-a");
    let child = SessionRecord {
        session_id: "agent-worker".to_string(),
        parent_session_id: Some("parent".to_string()),
        is_subagent: true,
        agent_id: Some("worker".to_string()),
        ..sample_session("cursor", "agent-worker", "project-a")
    };
    db.upsert_session(&parent).await;
    db.upsert_session(&child).await;
    db.upsert_session_message(&sample_message(
        "cursor",
        "parent-msg",
        "parent",
        "The orchard dispatch plan is ready.",
    ))
    .await;
    db.upsert_session_message(&sample_message(
        "cursor",
        "child-msg",
        "agent-worker",
        "The orchard dispatch result came from the worker.",
    ))
    .await;

    let all = db
        .search_session_messages("cursor", Some("project-a"), "orchard dispatch", 10)
        .await;
    assert_eq!(all.len(), 2);

    let parents_only = db
        .search_session_messages_filtered(
            "cursor",
            Some("project-a"),
            "orchard dispatch",
            10,
            SessionSearchFilters {
                scope: SessionSearchScope::ParentsOnly,
                message_type: Default::default(),
                parent_session_id: None,
                time_range: SessionSearchTimeRange::default(),
            },
        )
        .await;
    assert_eq!(parents_only.len(), 1);
    assert_eq!(parents_only[0].session.session_id, "parent");

    let subagents_only = db
        .search_session_messages_filtered(
            "cursor",
            Some("project-a"),
            "orchard dispatch",
            10,
            SessionSearchFilters {
                scope: SessionSearchScope::SubagentsOnly,
                message_type: Default::default(),
                parent_session_id: Some("parent"),
                time_range: SessionSearchTimeRange::default(),
            },
        )
        .await;
    assert_eq!(subagents_only.len(), 1);
    assert_eq!(subagents_only[0].session.session_id, "agent-worker");
    assert_eq!(
        subagents_only[0].session.parent_session_id.as_deref(),
        Some("parent")
    );
}

#[tokio::test]
async fn search_session_messages_collapses_parent_prompt_copies_from_eight_subagents() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let prompt = "Open pull requests to fix any issues.";
    let parent = sample_session("codex", "parent", "project-a");
    db.upsert_session(&parent).await;
    db.upsert_session_message(&sample_message("codex", "parent-prompt", "parent", prompt))
        .await;

    for index in 0..8 {
        let session_id = format!("agent-worker-{index}");
        let child = SessionRecord {
            session_id: session_id.clone(),
            parent_session_id: Some("parent".to_string()),
            is_subagent: true,
            agent_id: Some(format!("worker-{index}")),
            ..sample_session("codex", &session_id, "project-a")
        };
        db.upsert_session(&child).await;
        db.upsert_session_message(&sample_message(
            "codex",
            &format!("child-prompt-{index}"),
            &session_id,
            prompt,
        ))
        .await;
    }

    let results = db
        .search_session_messages("codex", Some("project-a"), "open pull requests", 10)
        .await;

    assert_eq!(results.len(), 1, "copied child prompts must collapse");
    assert_eq!(results[0].session.session_id, "parent");
}

// ---------------------------------------------------------------------------
// Transcript ingest health
// ---------------------------------------------------------------------------

/// `session_ingest_health` must report the un-ingested tail per transcript:
/// fully-ingested transcripts contribute nothing, partially-ingested ones
/// contribute their pending bytes, and the per-transcript maximum drives the
/// stalled-ingest detection in `tracedecay_status` / doctor.
#[tokio::test]
async fn session_ingest_health_reports_pending_transcript_backlog() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;

    let drained = tmp.path().join("drained.jsonl");
    std::fs::write(&drained, "x".repeat(100)).unwrap();
    let backlogged = tmp.path().join("backlogged.jsonl");
    std::fs::write(&backlogged, "y".repeat(500)).unwrap();
    let missing = tmp.path().join("missing.jsonl");

    for (session_id, path) in [
        ("s-drained", &drained),
        ("s-backlogged", &backlogged),
        ("s-missing", &missing),
    ] {
        let mut session = sample_session("cursor", session_id, "proj");
        session.transcript_path = Some(path.to_string_lossy().to_string());
        db.upsert_session(&session).await;
    }

    // drained: offset == file size; backlogged: 200 of 500 bytes ingested.
    let cursor = |byte_offset, mtime| tracedecay::global_db::ParseOffset {
        byte_offset,
        mtime,
        file_id: 0,
    };
    db.set_parse_offset(&drained.to_string_lossy(), cursor(100, 1_000))
        .await;
    db.set_parse_offset(&backlogged.to_string_lossy(), cursor(200, 2_000))
        .await;

    let health = db.session_ingest_health().await;
    assert_eq!(health.tracked_transcripts, 2, "missing files are skipped");
    assert_eq!(health.pending_transcripts, 1);
    assert_eq!(health.pending_bytes, 300);
    assert_eq!(health.max_transcript_pending_bytes, 300);
    assert_eq!(
        health.last_ingest_unix,
        Some(2_000),
        "the newest recorded ingest mtime should be reported"
    );
}

#[tokio::test]
async fn session_ingest_health_can_filter_by_provider() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;

    let cursor_transcript = tmp.path().join("cursor.jsonl");
    std::fs::write(&cursor_transcript, "x".repeat(100)).unwrap();
    let claude_transcript = tmp.path().join("claude.jsonl");
    std::fs::write(&claude_transcript, "y".repeat(500)).unwrap();

    for (provider, session_id, path) in [
        ("cursor", "cursor-session", &cursor_transcript),
        ("claude", "claude-session", &claude_transcript),
    ] {
        let mut session = sample_session(provider, session_id, "proj");
        session.transcript_path = Some(path.to_string_lossy().to_string());
        db.upsert_session(&session).await;
    }

    let cursor = |byte_offset, mtime| tracedecay::global_db::ParseOffset {
        byte_offset,
        mtime,
        file_id: 0,
    };
    db.set_parse_offset(&cursor_transcript.to_string_lossy(), cursor(100, 1_000))
        .await;
    db.set_parse_offset(&claude_transcript.to_string_lossy(), cursor(200, 2_000))
        .await;

    let all_health = db.session_ingest_health().await;
    assert_eq!(all_health.pending_transcripts, 1);
    assert_eq!(all_health.pending_bytes, 300);

    let cursor_health = db.session_ingest_health_for_provider(Some("cursor")).await;
    assert_eq!(cursor_health.tracked_transcripts, 1);
    assert_eq!(cursor_health.pending_transcripts, 0);

    let claude_health = db.session_ingest_health_for_provider(Some("claude")).await;
    assert_eq!(claude_health.tracked_transcripts, 1);
    assert_eq!(claude_health.pending_transcripts, 1);
    assert_eq!(claude_health.pending_bytes, 300);
}

#[tokio::test]
async fn hook_analytics_import_is_incremental_and_idempotent() {
    use tracedecay::analytics_bridge::HookImportSource;

    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let jsonl = tmp.path().join("hook_analytics.jsonl");
    std::fs::write(
        &jsonl,
        concat!(
            r#"{"agent":"claude","event":"hook_invoked","hook_name":"preToolUse","session_id":"s1","ts_unix_ms":1783000000000}"#,
            "\n",
            r#"{"agent":"codex","event":"hint_emitted","category":"search","ts_unix_ms":1783000001000}"#,
            "\n",
        ),
    )
    .unwrap();
    let sources = vec![HookImportSource {
        path: jsonl.clone(),
        default_project_root: Some(tmp.path().to_path_buf()),
    }];

    let outcome = db.import_profile_hook_analytics_for_test(&sources).await;
    assert_eq!(outcome.imported(), 2);
    assert!(outcome.sources[0].error.is_none());

    // Re-running without new rows imports nothing.
    let outcome = db.import_profile_hook_analytics_for_test(&sources).await;
    assert_eq!(outcome.imported(), 0);

    // Appending one complete row plus a partial trailing line imports only
    // the complete row; the partial tail stays unconsumed for the next run.
    let mut text = std::fs::read_to_string(&jsonl).unwrap();
    text.push_str(
        r#"{"agent":"cursor","event":"hook_invoked","hook_name":"postToolUse","ts_unix_ms":1783000002000}"#,
    );
    text.push('\n');
    text.push_str(r#"{"agent":"kiro","event":"hook_invo"#);
    std::fs::write(&jsonl, text).unwrap();
    let outcome = db.import_profile_hook_analytics_for_test(&sources).await;
    assert_eq!(outcome.imported(), 1);

    let events = db
        .query_analytics_events(&AnalyticsEventQuery {
            provider: None,
            project_id: None,
            session_id: None,
            event_kind: None,
            since: None,
            until: None,
            before_id: None,
            limit: 100,
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 3);
    let providers: Vec<&str> = events.iter().map(|event| event.provider.as_str()).collect();
    assert!(providers.contains(&"hook_claude"));
    assert!(providers.contains(&"hook_codex"));
    assert!(providers.contains(&"hook_cursor"));
    let expected_project = HostAdmissionTestRuntimeV1::canonical_project_key(tmp.path());
    assert!(
        events
            .iter()
            .all(|event| event.project_id == expected_project),
        "unattributed rows should fall back to the source's default project"
    );
}

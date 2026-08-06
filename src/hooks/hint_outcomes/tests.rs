use tempfile::TempDir;
use tracedecay_application::{
    HintEmission, HintOutcomeCorrelationPort, HintOutcomeObservation, HintOutcomePortError,
    HintOutcomePortFuture, HintOutcomePortOperation, HintToolActivity,
};

use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use crate::global_db::{AnalyticsEventInsert, AnalyticsEventQuery};
use crate::sessions::{SessionMessageRecord, SessionRecord};

use super::{
    HORIZON_TOOL_STEPS, HintOutcomeStats, Resolution, ToolStep, correlate_hint_outcomes, resolve,
    tool_matches_expected,
};

const PROJECT: &str = "proj_hint_outcomes";
const HINT_TS: i64 = 1_000_000;

struct FailingPort {
    operation: HintOutcomePortOperation,
}

impl FailingPort {
    fn result<T>(
        &self,
        operation: HintOutcomePortOperation,
        value: T,
    ) -> Result<T, HintOutcomePortError> {
        if self.operation == operation {
            Err(HintOutcomePortError::new(operation, "injected failure"))
        } else {
            Ok(value)
        }
    }
}

impl HintOutcomeCorrelationPort for FailingPort {
    fn resolved_hint_ids<'a>(
        &'a self,
        _project_id: &'a str,
        _limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<String>> {
        Box::pin(
            async move { self.result(HintOutcomePortOperation::QueryResolvedHints, Vec::new()) },
        )
    }

    fn emitted_hints<'a>(
        &'a self,
        project_id: &'a str,
        _limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintEmission>> {
        Box::pin(async move {
            self.result(
                HintOutcomePortOperation::QueryEmittedHints,
                vec![HintEmission {
                    provider: "hook_claude".to_owned(),
                    project_id: project_id.to_owned(),
                    session_id: "session-1".to_owned(),
                    timestamp: HINT_TS,
                    category: "search".to_owned(),
                    hint_id: "hint-1".to_owned(),
                }],
            )
        })
    }

    fn session_tool_activity<'a>(
        &'a self,
        _provider: &'a str,
        _session_id: &'a str,
        _after_timestamp: i64,
        _limit: u32,
    ) -> HintOutcomePortFuture<'a, Vec<HintToolActivity>> {
        Box::pin(async move {
            self.result(
                HintOutcomePortOperation::QuerySessionActivity,
                vec![HintToolActivity {
                    timestamp: HINT_TS + 1,
                    tool_names: vec!["tracedecay_context".to_owned()],
                }],
            )
        })
    }

    fn append_outcomes<'a>(
        &'a self,
        _outcomes: &'a [HintOutcomeObservation],
    ) -> HintOutcomePortFuture<'a, ()> {
        Box::pin(async move { self.result(HintOutcomePortOperation::AppendOutcomes, ()) })
    }
}

async fn open_db(dir: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .expect("open registered profile runtime")
}

async fn seed_session(db: &HostAdmissionTestRuntimeV1, provider: &str, session_id: &str) {
    let ok = db
        .upsert_session_for_test(
            HostAdmissionScope::Profile,
            &SessionRecord {
                provider: provider.to_string(),
                session_id: session_id.to_string(),
                project_key: PROJECT.to_string(),
                project_path: "/tmp/proj".to_string(),
                title: None,
                started_at: Some(HINT_TS),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
        )
        .await
        .expect("upsert session through retained runtime");
    assert!(ok, "session should upsert");
}

/// Builder for a seeded post-hint session message, keeping the seed helper to a
/// single struct argument (clippy `too_many_arguments`) while staying readable.
#[derive(Clone, Copy)]
struct Msg<'a> {
    provider: &'a str,
    session_id: &'a str,
    ordinal: i64,
    ts: i64,
    kind: Option<&'a str>,
    tool_names: Option<&'a str>,
    metadata_json: Option<&'a str>,
}

impl<'a> Msg<'a> {
    fn new(provider: &'a str, session_id: &'a str, ts: i64) -> Self {
        Self {
            provider,
            session_id,
            ordinal: 1,
            ts,
            kind: None,
            tool_names: None,
            metadata_json: None,
        }
    }

    fn tools(mut self, names: &'a str) -> Self {
        self.tool_names = Some(names);
        self
    }

    fn kind(mut self, kind: &'a str) -> Self {
        self.kind = Some(kind);
        self
    }

    fn metadata(mut self, metadata_json: &'a str) -> Self {
        self.metadata_json = Some(metadata_json);
        self
    }
}

async fn seed_message(db: &HostAdmissionTestRuntimeV1, msg: Msg<'_>) {
    let ok = db
        .upsert_session_message_for_test(
            HostAdmissionScope::Profile,
            &SessionMessageRecord {
                provider: msg.provider.to_string(),
                message_id: format!("{}:{}", msg.session_id, msg.ordinal),
                session_id: msg.session_id.to_string(),
                role: "assistant".to_string(),
                timestamp: Some(msg.ts),
                ordinal: msg.ordinal,
                text: "activity".to_string(),
                kind: msg.kind.map(str::to_string),
                model: None,
                tool_names: msg.tool_names.map(str::to_string),
                source_path: None,
                source_offset: Some(msg.ordinal),
                metadata_json: msg.metadata_json.map(str::to_string),
            },
        )
        .await
        .expect("upsert session message through retained runtime");
    assert!(ok, "session message should upsert");
}

async fn seed_hint_emitted(
    db: &HostAdmissionTestRuntimeV1,
    session_id: &str,
    hint_id: &str,
    category: &str,
) {
    seed_hint_emitted_for(db, "hook_claude", session_id, hint_id, category).await;
}

async fn seed_hint_emitted_for(
    db: &HostAdmissionTestRuntimeV1,
    provider: &str,
    session_id: &str,
    hint_id: &str,
    category: &str,
) {
    db.append_profile_analytics_event_for_test(&AnalyticsEventInsert {
        provider: provider.to_string(),
        project_id: PROJECT.to_string(),
        session_id: Some(session_id.to_string()),
        timestamp: HINT_TS,
        event_kind: "hint_emitted".to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: Some(category.to_string()),
        hint_id: Some(hint_id.to_string()),
        outcome: Some("observed".to_string()),
        metadata_json: None,
    })
    .await
    .expect("hint_emitted should append");
}

async fn outcome_events(
    db: &HostAdmissionTestRuntimeV1,
) -> Vec<crate::global_db::AnalyticsEventRecord> {
    db.query_profile_analytics_events_for_test(&AnalyticsEventQuery {
        project_id: Some(PROJECT.to_string()),
        event_kind: Some("hint_outcome".to_string()),
        limit: 100,
        ..Default::default()
    })
    .await
    .expect("query outcomes")
}

async fn correlate(db: &HostAdmissionTestRuntimeV1, now_secs: i64) -> HintOutcomeStats {
    db.correlate_hint_outcomes_for_test(HostAdmissionScope::Profile, PROJECT, now_secs)
        .await
        .expect("correlate hint outcomes through application port")
}

#[tokio::test]
async fn matching_tool_after_hint_resolves_acted() {
    let dir = TempDir::new().unwrap();
    let db = open_db(&dir).await;
    seed_session(&db, "claude", "s1").await;
    seed_hint_emitted(&db, "s1", "h1", "search").await;
    // A matching tracedecay tool (search category expects tracedecay_context)
    // fires shortly after the hint, MCP-prefixed as a client would report it.
    seed_message(
        &db,
        Msg::new("claude", "s1", HINT_TS + 60).tools("mcp__tracedecay__tracedecay_context"),
    )
    .await;

    let stats = correlate(&db, HINT_TS + 120).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 1,
            acted: 1,
            ignored: 0,
            unresolved: 0,
        }
    );
    let outcomes = outcome_events(&db).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].outcome.as_deref(), Some("acted"));
    assert_eq!(outcomes[0].hint_id.as_deref(), Some("h1"));
    assert_eq!(outcomes[0].hint_category.as_deref(), Some("search"));
}

#[tokio::test]
async fn codex_tool_event_row_resolves_acted() {
    let dir = TempDir::new().unwrap();
    let db = open_db(&dir).await;
    seed_session(&db, "codex", "c1").await;
    seed_hint_emitted_for(&db, "hook_codex", "c1", "hc", "impact").await;
    // Codex records a dedicated kind='tool_event' row carrying the tool name.
    seed_message(
        &db,
        Msg::new("codex", "c1", HINT_TS + 30)
            .kind("tool_event")
            .tools("tracedecay_impact"),
    )
    .await;

    let stats = correlate(&db, HINT_TS + 60).await;
    assert_eq!(stats.acted, 1);
    let outcomes = outcome_events(&db).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].outcome.as_deref(), Some("acted"));
    assert_eq!(outcomes[0].tool_name.as_deref(), Some("tracedecay_impact"));
}

#[tokio::test]
async fn claude_metadata_tool_events_resolve_acted() {
    let dir = TempDir::new().unwrap();
    let db = open_db(&dir).await;
    seed_session(&db, "claude", "s1").await;
    seed_hint_emitted(&db, "s1", "hm", "file_read").await;
    // Claude/Cursor carry bounded tool metadata on the message row instead of
    // the tool_names column.
    seed_message(
        &db,
        Msg::new("claude", "s1", HINT_TS + 45)
            .metadata(r#"{"tool_events":[{"type":"tool_use","tool_name":"tracedecay_outline"}]}"#),
    )
    .await;

    let stats = correlate(&db, HINT_TS + 90).await;
    assert_eq!(stats.acted, 1);
}

#[tokio::test]
async fn non_matching_activity_past_horizon_resolves_ignored() {
    let dir = TempDir::new().unwrap();
    let db = open_db(&dir).await;
    seed_session(&db, "claude", "s1").await;
    seed_hint_emitted(&db, "s1", "h2", "search").await;
    // Native reads only — no tracedecay search tool — and the wall clock is now
    // past the 30-minute horizon, so the window is closed with no match.
    seed_message(&db, Msg::new("claude", "s1", HINT_TS + 60).tools("Read")).await;

    let stats = correlate(&db, HINT_TS + 2_000).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 1,
            acted: 0,
            ignored: 1,
            unresolved: 0,
        }
    );
    let outcomes = outcome_events(&db).await;
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].outcome.as_deref(), Some("ignored"));
    assert!(outcomes[0].tool_name.is_none());
}

#[tokio::test]
async fn no_post_hint_activity_stays_unresolved() {
    let dir = TempDir::new().unwrap();
    let db = open_db(&dir).await;
    seed_session(&db, "claude", "s1").await;
    seed_hint_emitted(&db, "s1", "h3", "search").await;
    // Only a pre-hint message exists; nothing is ingested after the hint yet.
    seed_message(
        &db,
        Msg::new("claude", "s1", HINT_TS - 30).tools("tracedecay_context"),
    )
    .await;

    let stats = correlate(&db, HINT_TS + 120).await;
    assert_eq!(
        stats,
        HintOutcomeStats {
            scanned: 1,
            acted: 0,
            ignored: 0,
            unresolved: 1,
        }
    );
    assert!(outcome_events(&db).await.is_empty());
}

#[tokio::test]
async fn short_quiet_session_before_wall_clock_horizon_stays_unresolved() {
    let dir = TempDir::new().unwrap();
    let db = open_db(&dir).await;
    seed_session(&db, "claude", "s1").await;
    seed_hint_emitted(&db, "s1", "h4", "search").await;
    // A single non-matching step, fewer than the step horizon, and the wall
    // clock has not yet reached the time horizon: the window is still open.
    seed_message(&db, Msg::new("claude", "s1", HINT_TS + 60).tools("Read")).await;

    let stats = correlate(&db, HINT_TS + 120).await;
    assert_eq!(stats.unresolved, 1);
    assert_eq!(stats.acted, 0);
    assert_eq!(stats.ignored, 0);
    assert!(outcome_events(&db).await.is_empty());
}

#[tokio::test]
async fn correlation_is_idempotent_across_runs() {
    let dir = TempDir::new().unwrap();
    let db = open_db(&dir).await;
    seed_session(&db, "claude", "s1").await;
    seed_hint_emitted(&db, "s1", "h1", "search").await;
    seed_message(
        &db,
        Msg::new("claude", "s1", HINT_TS + 60).tools("tracedecay_context"),
    )
    .await;

    let first = correlate(&db, HINT_TS + 120).await;
    assert_eq!(first.acted, 1);
    // Re-running must not re-scan or re-write the already-resolved hint.
    let second = correlate(&db, HINT_TS + 240).await;
    assert_eq!(
        second,
        HintOutcomeStats {
            scanned: 0,
            acted: 0,
            ignored: 0,
            unresolved: 0,
        }
    );
    assert_eq!(outcome_events(&db).await.len(), 1);
}

#[tokio::test]
async fn query_failure_is_typed_instead_of_becoming_an_empty_pass() {
    let error = correlate_hint_outcomes(
        &FailingPort {
            operation: HintOutcomePortOperation::QueryResolvedHints,
        },
        PROJECT,
        HINT_TS + 120,
    )
    .await
    .unwrap_err();

    assert_eq!(error.operation(), "query_resolved_hints");
    assert_eq!(error.detail(), "injected failure");
}

#[tokio::test]
async fn append_failure_is_typed_after_a_resolved_observation() {
    let error = correlate_hint_outcomes(
        &FailingPort {
            operation: HintOutcomePortOperation::AppendOutcomes,
        },
        PROJECT,
        HINT_TS + 120,
    )
    .await
    .unwrap_err();

    assert_eq!(error.operation(), "append_outcomes");
    assert_eq!(error.detail(), "injected failure");
}

#[test]
fn tool_matches_expected_tolerates_prefixes_and_boundaries() {
    let expected = ["tracedecay_context", "tracedecay_search"];
    assert!(tool_matches_expected("tracedecay_context", &expected));
    assert!(tool_matches_expected(
        "mcp__tracedecay__tracedecay_context",
        &expected
    ));
    assert!(tool_matches_expected(
        "mcp__plugin_tracedecay_tracedecay__tracedecay_search",
        &expected
    ));
    assert!(tool_matches_expected("TraceDecay-Context", &expected));
    // A different tool that merely shares a suffix fragment must not match.
    assert!(!tool_matches_expected(
        "tracedecay_signature_search",
        &expected
    ));
    assert!(!tool_matches_expected("Read", &expected));
}

#[test]
fn resolve_step_horizon_closes_window_without_wall_clock() {
    let expected = ["tracedecay_context"];
    // HORIZON_TOOL_STEPS non-matching steps, all inside the time horizon, and
    // `now` still before the wall-clock horizon: the step horizon alone closes
    // the window as ignored.
    let steps: Vec<ToolStep> = (0..HORIZON_TOOL_STEPS as i64)
        .map(|i| ToolStep {
            ts: HINT_TS + i + 1,
            tools: vec!["Read".to_string()],
        })
        .collect();
    let resolution = resolve(HINT_TS, &steps, &expected, HINT_TS + 5);
    assert!(matches!(resolution, Some(Resolution::Ignored)));

    // The same steps but with a matching tool in the last slot resolve acted.
    let mut acted_steps = steps;
    acted_steps.last_mut().unwrap().tools = vec!["tracedecay_context".to_string()];
    let resolution = resolve(HINT_TS, &acted_steps, &expected, HINT_TS + 5);
    assert!(matches!(resolution, Some(Resolution::Acted(_))));
}

use tempfile::TempDir;

use crate::global_db::{AnalyticsEventInsert, AnalyticsEventQuery, GlobalDb};
use crate::sessions::{SessionMessageRecord, SessionRecord};

use super::{
    HORIZON_TOOL_STEPS, HintOutcomeStats, Resolution, ToolStep, correlate_hint_outcomes, resolve,
    tool_matches_expected,
};

const PROJECT: &str = "proj_hint_outcomes";
const HINT_TS: i64 = 1_000_000;

async fn open_db(dir: &TempDir) -> GlobalDb {
    GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db")
}

async fn seed_session(db: &GlobalDb, provider: &str, session_id: &str) {
    let ok = db
        .upsert_session(&SessionRecord {
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
        })
        .await;
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

async fn seed_message(db: &GlobalDb, msg: Msg<'_>) {
    let ok = db
        .upsert_session_message(&SessionMessageRecord {
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
        })
        .await;
    assert!(ok, "session message should upsert");
}

async fn seed_hint_emitted(db: &GlobalDb, session_id: &str, hint_id: &str, category: &str) {
    seed_hint_emitted_for(db, "hook_claude", session_id, hint_id, category).await;
}

async fn seed_hint_emitted_for(
    db: &GlobalDb,
    provider: &str,
    session_id: &str,
    hint_id: &str,
    category: &str,
) {
    db.append_analytics_event(&AnalyticsEventInsert {
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

async fn outcome_events(db: &GlobalDb) -> Vec<crate::global_db::AnalyticsEventRecord> {
    db.query_analytics_events(&AnalyticsEventQuery {
        project_id: Some(PROJECT.to_string()),
        event_kind: Some("hint_outcome".to_string()),
        limit: 100,
        ..Default::default()
    })
    .await
    .expect("query outcomes")
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

    let stats = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 120).await;
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

    let stats = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 60).await;
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

    let stats = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 90).await;
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

    let stats = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 2_000).await;
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

    let stats = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 120).await;
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

    let stats = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 120).await;
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

    let first = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 120).await;
    assert_eq!(first.acted, 1);
    // Re-running must not re-scan or re-write the already-resolved hint.
    let second = correlate_hint_outcomes(&db, &db, PROJECT, HINT_TS + 240).await;
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

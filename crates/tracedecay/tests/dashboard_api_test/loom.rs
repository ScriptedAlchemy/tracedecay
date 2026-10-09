use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::time::Duration;

use crate::dashboard_api_support::*;
use serde_json::json;
use tracedecay_dashboard_api::{
    DashboardGitCorrelationReadFutureV1, DashboardGitCorrelationReadPortV1,
};
use tracedecay_sessions::runtime::git_correlation::{
    DEFAULT_SPAN_MERGE_GAP_SECS, SpanObservation, SpanSource,
};

/// A dashboard started while its project is still opening reports the session
/// authority as opening on every read, then serves the seeded sessions once
/// the project's publication delivers its session store, without a restart.
#[test]
fn dashboard_started_during_project_open_serves_sessions_once_open_completes() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let project_open = ProjectOpenPublication::default();
        let fixture = start_dashboard_fixture_while_opening(project_open.clone()).await;
        let agent = http_agent();
        let capabilities_url = format!("{}/api/capabilities", fixture.base_url);
        let temporal_url = format!("{}/api/loom/temporal?limit=25", fixture.base_url);

        for _ in 0..5 {
            let (status, capabilities) = get_json(&agent, &capabilities_url);
            assert_eq!(status, 200, "{capabilities}");
            assert_eq!(capabilities["session_authority"], "opening");
            assert_eq!(capabilities["features"]["lcm"], false);
        }
        let (event, frame) = first_event_frame(&agent, &format!("{}/api/events", fixture.base_url));
        assert_eq!(event, "heartbeat", "{frame}");
        assert_eq!(frame["kind"]["family"], "heartbeat");
        assert_eq!(frame["event_revision"], 1);
        let (status, opening) = get_json(&agent, &temporal_url);
        assert_eq!(status, 200, "{opening}");
        assert_eq!(opening["domain_state"], "loading");
        assert_eq!(opening["payload"]["available"], false);
        assert_eq!(opening["payload"]["total"], 0);

        project_open.publish_ready();

        let (status, capabilities) = get_json(&agent, &capabilities_url);
        assert_eq!(status, 200, "{capabilities}");
        assert_eq!(capabilities["session_authority"], "ready");
        assert_eq!(capabilities["features"]["lcm"], true);
        let (status, ready) = get_json(&agent, &temporal_url);
        assert_eq!(status, 200, "{ready}");
        assert_eq!(ready["domain_state"], "partial");
        assert_eq!(ready["payload"]["available"], true);
        assert_eq!(ready["payload"]["total"], 1);
        assert_eq!(
            ready["payload"]["sessions"][0]["session_id"],
            "sess-dashboard-1"
        );
    });
}

/// A project whose open finishes without a session store stops reporting
/// `opening`: the dashboard records the terminal state it was published.
#[test]
fn dashboard_reports_unavailable_when_the_open_publishes_no_sessions() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let project_open = ProjectOpenPublication::default();
        let fixture = start_dashboard_fixture_while_opening(project_open.clone()).await;
        let agent = http_agent();
        let capabilities_url = format!("{}/api/capabilities", fixture.base_url);

        let (_, capabilities) = get_json(&agent, &capabilities_url);
        assert_eq!(capabilities["session_authority"], "opening");

        project_open.publish_unavailable();

        for _ in 0..2 {
            let (status, capabilities) = get_json(&agent, &capabilities_url);
            assert_eq!(status, 200, "{capabilities}");
            assert_eq!(capabilities["session_authority"], "unavailable");
            assert_eq!(capabilities["features"]["lcm"], false);
        }
    });
}

/// Reads the first SSE frame the stream at `url` delivers within the agent's
/// request timeout.
fn first_event_frame(agent: &ureq::Agent, url: &str) -> (String, serde_json::Value) {
    let response = agent
        .get(url)
        .call()
        .unwrap_or_else(|error| panic!("GET {url}: {error}"));
    assert_eq!(response.status().as_u16(), 200);
    let mut lines = BufReader::new(response.into_body().into_reader()).lines();
    let mut event = None;
    loop {
        let line = lines
            .next()
            .unwrap_or_else(|| panic!("{url} closed before its first frame"))
            .unwrap_or_else(|error| panic!("{url} delivered no frame in time: {error}"));
        if let Some(name) = line.strip_prefix("event: ") {
            event = Some(name.to_owned());
        } else if let Some(data) = line.strip_prefix("data: ") {
            let frame = serde_json::from_str(data)
                .unwrap_or_else(|error| panic!("{url} frame is not JSON: {error}: {data}"));
            return (event.unwrap_or_default(), frame);
        }
    }
}

#[test]
fn loom_temporal_endpoint_reads_recorded_ends_and_causal_authorities() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let mut session = fixture
            .host_runtime
            .session_for_test(HostAdmissionScope::Project, "cursor", "sess-dashboard-1")
            .await
            .expect("read seeded Loom session")
            .expect("seeded Loom session");
        session.ended_at = Some(1_700_001_090);
        session.metadata_json = Some(
            json!({
                "edited_files": [
                    {"path": "src/lib.rs", "change_type": "edit", "hunks": 1,
                     "edited_at_micros": 1_700_001_050_000_000i64},
                    {"path": "src/runtime.rs", "change_type": "edit", "hunks": 2}
                ]
            })
            .to_string(),
        );
        assert!(
            fixture
                .host_runtime
                .upsert_session_for_test(HostAdmissionScope::Project, &session)
                .await
                .expect("update Loom session")
        );
        let delegated = SessionRecord {
            session_id: "sess-dashboard-child".to_string(),
            title: Some("Delegated explorer".to_string()),
            started_at: Some(1_700_001_030),
            ended_at: Some(1_700_001_060),
            metadata_json: None,
            parent_session_id: Some("sess-dashboard-1".to_string()),
            is_subagent: true,
            agent_id: Some("explorer".to_string()),
            parent_tool_use_id: Some("toolu_fork_01".to_string()),
            ..session.clone()
        };
        assert!(
            fixture
                .host_runtime
                .upsert_session_for_test(HostAdmissionScope::Project, &delegated)
                .await
                .expect("seed delegated Loom session")
        );
        fixture
            .host_runtime
            .record_project_span_for_test(
                &SpanObservation {
                    provider: "cursor".to_string(),
                    session_id: "sess-dashboard-1".to_string(),
                    thread_id: None,
                    branch: Some("main".to_string()),
                    worktree: fixture.project_root.display().to_string(),
                    ts: 1_700_001_020,
                    source: SpanSource::Ingest,
                },
                DEFAULT_SPAN_MERGE_GAP_SECS,
            )
            .await
            .expect("record Loom branch/worktree span");

        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!("{}/api/loom/temporal?limit=200", fixture.base_url),
        );

        assert_eq!(status, 200, "{envelope}");
        assert_eq!(envelope["schema_revision"], 1);
        assert_eq!(envelope["domain_state"], "partial");
        assert_eq!(envelope["payload"]["available"], true);
        // Newest start first: the delegated child leads, then its parent.
        let sessions = &envelope["payload"]["sessions"];
        assert_eq!(sessions[0]["session_id"], "sess-dashboard-child");
        assert_eq!(sessions[0]["parent_session_id"], "sess-dashboard-1");
        assert_eq!(sessions[0]["parent_tool_use_id"], "toolu_fork_01");
        assert_eq!(sessions[0]["is_subagent"], true);
        assert_eq!(sessions[1]["session_id"], "sess-dashboard-1");
        assert_eq!(sessions[1]["ended_at"], 1_700_001_090);
        assert!(
            sessions[1].get("parent_session_id").is_none()
                && sessions[1].get("parent_tool_use_id").is_none(),
            "an unrecorded parent is absent, never null-as-root: {}",
            sessions[1]
        );

        // Files are served in path order; only the recorded timestamp is carried.
        let edited_files = &envelope["payload"]["edited_files"];
        assert_eq!(edited_files[0]["path"], "src/lib.rs");
        assert_eq!(
            edited_files[0]["edited_at_micros"],
            1_700_001_050_000_000i64
        );
        assert_eq!(edited_files[1]["path"], "src/runtime.rs");
        assert!(
            edited_files[1].get("edited_at_micros").is_none(),
            "a rollup without a recorded time must not fabricate one: {}",
            edited_files[1]
        );
        assert_eq!(edited_files.as_array().map(Vec::len), Some(2));
        assert_eq!(envelope["payload"]["branch_spans"][0]["branch"], "main");

        let statuses = envelope["payload"]["source_statuses"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom source statuses should be an array: {envelope}"));
        let source = |id: &str| {
            statuses
                .iter()
                .find(|status| status["id"] == id)
                .unwrap_or_else(|| panic!("missing Loom source {id}: {envelope}"))
        };
        assert_eq!(source("session_commit")["state"], "ready");
        assert_eq!(source("session_file")["state"], "partial");
        assert_eq!(source("branch_worktree")["state"], "ready");
        assert_eq!(source("subagent_spawn")["state"], "ready");
        assert_eq!(source("subagent_spawn")["coverage"]["matched"], 1);
        assert_eq!(source("session_tool")["state"], "ready");
        assert_eq!(source("session_test")["state"], "ready");
        assert_eq!(source("session_test")["item_count"], 0);
        assert_eq!(statuses.len(), 7);
    });
}

fn loom_message(
    session_id: &str,
    message_id: &str,
    ordinal: i64,
    timestamp: Option<i64>,
    kind: &str,
    tool_names: Option<&str>,
    text: &str,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp,
        ordinal,
        text: text.to_string(),
        kind: Some(kind.to_string()),
        model: None,
        tool_names: tool_names.map(str::to_string),
        source_path: None,
        source_offset: None,
        metadata_json: None,
    }
}

fn loom_source<'a>(envelope: &'a Value, id: &str) -> &'a Value {
    envelope["payload"]["source_statuses"]
        .as_array()
        .and_then(|statuses| statuses.iter().find(|status| status["id"] == id))
        .unwrap_or_else(|| panic!("missing Loom source {id}: {envelope}"))
}

/// The page's tool-call and pull-request events are served at the time the
/// host recorded them, oldest first across sessions whatever their stored
/// order, and a row recorded without a time is counted, never placed.
#[test]
fn loom_temporal_serves_recorded_tool_and_pull_request_events_in_recorded_time_order() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let session = |id: &str, started_at: i64| SessionRecord {
            provider: "cursor".to_string(),
            session_id: id.to_string(),
            project_key: fixture.host_runtime.project_id().as_str().to_string(),
            project_path: fixture.project_root.display().to_string(),
            title: Some(id.to_string()),
            started_at: Some(started_at),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        let worker = session("sess-events-worker", 1_700_100_000);
        let reviewer = session("sess-events-reviewer", 1_700_100_050);
        let batches = [
            (
                &worker,
                vec![
                    // Stored first, recorded last.
                    loom_message(&worker.session_id, "w-edit", 1, Some(1_700_100_300), "tool_call", Some("Edit"), "edit lib.rs"),
                    loom_message(&worker.session_id, "w-read", 2, Some(1_700_100_010), "message", Some("Read,Grep"), "read"),
                    loom_message(&worker.session_id, "w-undated", 3, None, "tool_call", Some("Bash"), "cargo test"),
                    loom_message(&worker.session_id, "w-chat", 4, Some(1_700_100_020), "message", None, "plain turn"),
                ],
            ),
            (
                &reviewer,
                vec![
                    loom_message(
                        &reviewer.session_id,
                        "r-pr",
                        1,
                        Some(1_700_100_200),
                        "git_pull_request",
                        None,
                        r#"{"type":"pr-link","prNumber":42,"prUrl":"https://github.com/acme/app/pull/42"}"#,
                    ),
                    loom_message(&reviewer.session_id, "r-link", 2, Some(1_700_100_250), "pr_link", None, "https://github.com/acme/app/pull/43"),
                    loom_message(&reviewer.session_id, "r-bash", 3, Some(1_700_100_100), "tool_call", Some("Bash"), "gh pr view"),
                ],
            ),
        ];
        for (record, messages) in batches {
            fixture
                .host_runtime
                .seed_session_messages_for_test(
                        HostAdmissionScope::Project,
                        record,
                        &messages,
                    )
                .await
                .unwrap_or_else(|error| panic!("seed {}: {error}", record.session_id));
        }

        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!("{}/api/loom/temporal?limit=200", fixture.base_url),
        );
        assert_eq!(status, 200, "{envelope}");
        assert_eq!(
            envelope["payload"]["events"],
            json!([
                {"provider": "cursor", "session_id": "sess-events-worker", "kind": "tool_call",
                 "message_id": "w-read", "ordinal": 2, "recorded_at": 1_700_100_010, "label": "Read,Grep"},
                {"provider": "cursor", "session_id": "sess-events-reviewer", "kind": "tool_call",
                 "message_id": "r-bash", "ordinal": 3, "recorded_at": 1_700_100_100, "label": "Bash"},
                {"provider": "cursor", "session_id": "sess-events-reviewer", "kind": "pull_request",
                 "message_id": "r-pr", "ordinal": 1, "recorded_at": 1_700_100_200,
                 "label": "https://github.com/acme/app/pull/42"},
                {"provider": "cursor", "session_id": "sess-events-reviewer", "kind": "pull_request",
                 "message_id": "r-link", "ordinal": 2, "recorded_at": 1_700_100_250,
                 "label": "https://github.com/acme/app/pull/43"},
                {"provider": "cursor", "session_id": "sess-events-worker", "kind": "tool_call",
                 "message_id": "w-edit", "ordinal": 1, "recorded_at": 1_700_100_300, "label": "Edit"},
            ])
        );

        let tools = loom_source(&envelope, "session_tool");
        assert_eq!(tools["state"], "partial", "{tools}");
        assert_eq!(tools["item_count"], 3);
        assert_eq!(tools["coverage"]["eligible"], 4);
        assert_eq!(tools["coverage"]["matched"], 3);
        assert_eq!(tools["coverage"]["omitted"], 1);
        let pull_requests = loom_source(&envelope, "session_pull_request");
        assert_eq!(pull_requests["state"], "partial", "{pull_requests}");
        assert_eq!(pull_requests["item_count"], 2);
        assert_eq!(pull_requests["coverage"]["eligible"], 2);
        assert_eq!(pull_requests["coverage"]["omitted"], 0);
        let tests = loom_source(&envelope, "session_test");
        assert_eq!(tests["state"], "ready", "{tests}");
        assert_eq!(tests["item_count"], 0);
        assert_eq!(tests["coverage"]["eligible"], 0);
        assert_eq!(tests["required_authority"], Value::Null);
    });
}

/// A page whose sessions recorded no tool call or pull request answers an
/// empty event list with complete zero coverage, not an unavailable source.
#[test]
fn loom_temporal_serves_an_empty_event_stream_as_complete_zero_coverage() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let quiet = SessionRecord {
            provider: "cursor".to_string(),
            session_id: "sess-quiet".to_string(),
            project_key: fixture.host_runtime.project_id().as_str().to_string(),
            project_path: fixture.project_root.display().to_string(),
            title: Some("Quiet session".to_string()),
            started_at: Some(1_700_200_000),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        fixture
            .host_runtime
            .seed_session_messages_for_test(
                HostAdmissionScope::Project,
                &quiet,
                &[loom_message(
                    &quiet.session_id,
                    "q-chat",
                    1,
                    Some(1_700_200_010),
                    "message",
                    None,
                    "hello",
                )],
            )
            .await
            .unwrap_or_else(|error| panic!("seed quiet session: {error}"));

        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!("{}/api/loom/temporal?limit=200", fixture.base_url),
        );
        assert_eq!(status, 200, "{envelope}");
        assert_eq!(
            envelope["payload"]["sessions"][0]["session_id"],
            "sess-quiet"
        );
        assert_eq!(envelope["payload"]["sessions"][0]["messages"], 1);
        assert_eq!(envelope["payload"]["events"], json!([]));
        for id in ["session_tool", "session_pull_request"] {
            let source = loom_source(&envelope, id);
            assert_eq!(source["item_count"], 0, "{source}");
            assert_eq!(source["coverage"]["eligible"], 0, "{source}");
            assert_eq!(source["coverage"]["completeness"], "complete", "{source}");
        }
        assert_eq!(loom_source(&envelope, "session_tool")["state"], "ready");
    });
}

fn write_rollout(path: &std::path::Path, records: &[serde_json::Value]) {
    let body = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    std::fs::write(path, body).unwrap_or_else(|error| panic!("write rollout fixture: {error}"));
}

/// A Codex session tree ingested through production capture: the parent
/// rollout alone records which `spawn_agent` call started each child. The Loom
/// temporal read serves each child's `parent_tool_use_id`, and the parent's
/// transcript page serves a tool call carrying that same id, the pair the Loom
/// draws as an EXACT fork on the call. A child whose parent recorded no
/// spawning call keeps its parent and is counted as omitted coverage.
#[test]
fn loom_forks_bind_to_the_spawning_call_recorded_by_the_parent_transcript() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let home = fixture
            .host_runtime
            .profile()
            .home()
            .expect("fixture profile names its home")
            .to_path_buf();
        let dir = home.join(".codex/sessions/2026/09/02");
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = fixture.project_root.to_string_lossy().into_owned();
        let parent = "01a05f21-0000-7000-8000-00000000a001";
        let spawned = "01a05f21-0000-7000-8000-00000000c001";
        let unspawned = "01a05f21-0000-7000-8000-00000000c002";
        // More transcript records than a ranked search would keep from one
        // source, so the page must serve the whole window for the spawn call
        // to be loaded at all.
        let mut parent_records = vec![serde_json::json!({
            "timestamp": "2026-09-02T00:00:00.000Z", "type": "session_meta",
            "payload": {"id": parent, "cwd": cwd, "model_provider": "openai"}})];
        parent_records.extend((0..6).map(|step| {
            serde_json::json!({"timestamp": format!("2026-09-02T00:00:0{step}.500Z"), "type": "event_msg",
                "payload": {"type": "agent_message", "message": format!("Planning step {step}.")}})
        }));
        parent_records.extend([
            serde_json::json!({"timestamp": "2026-09-02T00:00:10.000Z", "type": "response_item",
                "payload": {"type": "function_call", "name": "spawn_agent", "call_id": "call_loom_spawn",
                    "arguments": "{\"agent_type\":\"explorer\"}"}}),
            serde_json::json!({"timestamp": "2026-09-02T00:00:11.000Z", "type": "event_msg",
                "payload": {"type": "item_completed", "thread_id": parent, "item": {
                    "type": "SubAgentActivity", "id": "call_loom_spawn", "kind": "started",
                    "agent_thread_id": spawned, "agent_path": "/root/explorer"}}}),
            serde_json::json!({"timestamp": "2026-09-02T00:01:00.000Z", "type": "event_msg",
                "payload": {"type": "agent_message", "message": "Explorer finished."}}),
        ]);
        write_rollout(
            &dir.join(format!("rollout-2026-09-02T00-00-00-{parent}.jsonl")),
            &parent_records,
        );
        for (child, start) in [(spawned, "00-00-11"), (unspawned, "00-00-20")] {
            let at = format!("2026-09-02T{}.500Z", start.replace('-', ":"));
            write_rollout(
                &dir.join(format!("rollout-2026-09-02T{start}-{child}.jsonl")),
                &[
                    serde_json::json!({"timestamp": at, "type": "session_meta", "payload": {
                        "id": child, "parent_thread_id": parent, "cwd": cwd, "thread_source": "subagent",
                        "source": {"subagent": {"thread_spawn": {"parent_thread_id": parent, "depth": 1}}},
                        "model_provider": "openai"}}),
                    serde_json::json!({"timestamp": "2026-09-02T00:00:40.000Z", "type": "event_msg",
                        "payload": {"type": "agent_message", "message": format!("child {child} done")}}),
                ],
            );
        }
        fixture
            .host_runtime
            .ingest_project_provider_for_test(
                &fixture.project_root,
                tracedecay_sessions::runtime::SessionProvider::Codex,
            )
            .await
            .expect("ingest the Codex session tree");
        fixture
            .host_runtime
            .materialize_session_temporal_refresh_for_test(parent)
            .await
            .expect("materialize the parent transcript's temporal refresh");

        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!("{}/api/loom/temporal?limit=200", fixture.base_url),
        );
        assert_eq!(status, 200, "{envelope}");
        let sessions = envelope["payload"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom sessions: {envelope}"));
        let row = |id: &str| {
            sessions
                .iter()
                .find(|session| session["provider"] == "codex" && session["session_id"] == id)
                .unwrap_or_else(|| panic!("missing Loom session {id}: {envelope}"))
        };
        assert_eq!(row(spawned)["parent_session_id"], parent);
        assert_eq!(row(spawned)["parent_tool_use_id"], "call_loom_spawn");
        assert_eq!(row(unspawned)["parent_session_id"], parent);
        assert!(
            row(unspawned).get("parent_tool_use_id").is_none(),
            "a child whose parent recorded no spawn has no call: {}",
            row(unspawned)
        );
        let spawn_status = envelope["payload"]["source_statuses"]
            .as_array()
            .and_then(|statuses| statuses.iter().find(|status| status["id"] == "subagent_spawn"))
            .unwrap_or_else(|| panic!("missing subagent_spawn status: {envelope}"));
        assert_eq!(spawn_status["state"], "partial", "{spawn_status}");
        assert_eq!(spawn_status["providers"], serde_json::json!(["codex"]));
        assert_eq!(spawn_status["coverage"]["eligible"], 2);
        assert_eq!(spawn_status["coverage"]["matched"], 1);
        assert_eq!(spawn_status["coverage"]["omitted"], 1);
        // The spawning call is on the temporal read itself, at the time the
        // parent rollout recorded it, so every lane binds its fork without
        // loading the parent transcript.
        let spawn_events: Vec<&serde_json::Value> = envelope["payload"]["events"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom events: {envelope}"))
            .iter()
            .filter(|event| event["tool_use_id"] == "call_loom_spawn")
            .collect();
        assert_eq!(spawn_events.len(), 1, "{envelope}");
        assert_eq!(spawn_events[0]["session_id"], parent);
        assert_eq!(spawn_events[0]["kind"], "tool_call");
        assert_eq!(spawn_events[0]["label"], "spawn_agent");
        assert_eq!(spawn_events[0]["recorded_at"], 1_788_307_210);

        let (status, page) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/session/{parent}?limit=50",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200, "{page}");
        let messages = page["payload"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("parent transcript page: {page}"));
        assert_eq!(
            Some(messages.len() as u64),
            page["payload"]["counts"]["message_count"].as_u64(),
            "the transcript page serves every recorded message: {page}"
        );
        let spawn_calls: Vec<&serde_json::Value> = messages
            .iter()
            .filter(|message| message["tool_use_id"] == "call_loom_spawn")
            .collect();
        assert_eq!(spawn_calls.len(), 1, "{page}");
        assert_eq!(spawn_calls[0]["tool_name"], "spawn_agent");
    });
}

const LARGE_HISTORY_SESSIONS: usize = 2_000;
const LARGE_HISTORY_MESSAGES: usize = 12;

#[test]
fn default_message_timeline_reports_bounded_counts_for_large_history() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let session_count = 3_000_u64;
        let mut session_ids = Vec::new();
        for index in 0..session_count {
            let session =
                large_history_session(&fixture.host_runtime, &fixture.project_root, index as usize);
            assert!(
                fixture
                    .host_runtime
                    .upsert_session_for_test(HostAdmissionScope::Project, &session)
                    .await
                    .unwrap()
            );
            fixture
                .host_runtime
                .seed_session_message_observation_for_test(
                    tracedecay::dashboard::observation_seed::DashboardSessionMessageSeedV1 {
                        project_id: fixture.host_runtime.project_id().as_str(),
                        provider: "cursor",
                        session_id: &session.session_id,
                        message_id: &format!("{}-message", session.session_id),
                        role: "user",
                        content: "A canonical timeline message.",
                        model: None,
                        timestamp: session.started_at.unwrap(),
                        ordinal: 1,
                    },
                )
                .await
                .unwrap();
            session_ids.push(session.session_id);
        }

        fixture
            .host_runtime
            .materialize_session_temporal_refreshes_for_test(
                &session_ids.iter().map(String::as_str).collect::<Vec<_>>(),
            )
            .await
            .unwrap();
        let (status, timeline) = get_json(
            &http_agent(),
            &format!(
                "{}/api/plugins/hermes-lcm/timeline?bucket=day&limit=400",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200, "{timeline}");
        let buckets = timeline["payload"]["buckets"]
            .as_array()
            .unwrap_or_else(|| panic!("large timeline must return bounded data: {timeline}"));
        let counted: u64 = buckets
            .iter()
            .map(|bucket| bucket["count"].as_u64().unwrap())
            .sum();
        assert!(counted > 0 && counted <= session_count, "{timeline}");
        assert_eq!(timeline["coverage"]["examined"], counted, "{timeline}");
        if counted < session_count {
            assert_eq!(timeline["domain_state"], "partial", "{timeline}");
            assert_eq!(
                timeline["coverage"]["completeness"], "partial",
                "{timeline}"
            );
        } else {
            assert_eq!(
                timeline["coverage"]["completeness"], "complete",
                "{timeline}"
            );
        }
    });
}

fn large_history_session(
    project: &DashboardTestRuntimeV1,
    root: &Path,
    index: usize,
) -> SessionRecord {
    SessionRecord {
        provider: "cursor".to_string(),
        session_id: format!("large-{index:04}"),
        project_key: project.project_id().as_str().to_string(),
        project_path: root.display().to_string(),
        title: Some(format!("Large history session {index}")),
        started_at: Some(1_700_000_000 + i64::try_from(index * 600).expect("start offset")),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn large_history_messages(session: &SessionRecord) -> Vec<SessionMessageRecord> {
    let started_at = session.started_at.expect("large history start");
    (0..LARGE_HISTORY_MESSAGES)
        .map(|ordinal| {
            let offset = i64::try_from(ordinal).expect("message ordinal");
            message(
                &format!("{}-msg-{ordinal:02}", session.session_id),
                &session.session_id,
                if ordinal % 2 == 0 {
                    "user"
                } else {
                    "assistant"
                },
                offset + 1,
                "synthetic Loom history turn",
                MessageDetails {
                    timestamp: started_at + offset * 10,
                    model: (ordinal % 2 == 1).then_some("gpt-large"),
                    metadata_json: None,
                },
            )
        })
        .collect()
}

#[test]
fn loom_temporal_serves_one_bounded_page_of_a_large_history() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        for index in 0..LARGE_HISTORY_SESSIONS {
            let session =
                large_history_session(&fixture.host_runtime, &fixture.project_root, index);
            fixture
                .host_runtime
                .seed_session_messages_for_test(
                    HostAdmissionScope::Project,
                    &session,
                    &large_history_messages(&session),
                )
                .await
                .unwrap_or_else(|error| panic!("seed {}: {error}", session.session_id));
        }

        // The suite's standard HTTP client gives up after four seconds; the
        // first page must arrive inside that budget regardless of how much
        // history precedes it.
        let agent = http_agent();
        let (status, first) = get_json(
            &agent,
            &format!("{}/api/loom/temporal?limit=25&offset=0", fixture.base_url),
        );
        assert_eq!(status, 200, "{first}");
        assert_eq!(first["payload"]["total"], 2_000);
        let sessions = first["payload"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom sessions should be an array: {first}"));
        assert_eq!(sessions.len(), 25);
        assert_eq!(sessions[0]["session_id"], "large-1999");
        assert_eq!(sessions[0]["messages"], 12);
        assert_eq!(sessions[0]["started_at"], 1_701_199_400);
        assert_eq!(sessions[0]["last_message_at"], 1_701_199_510);
        assert_eq!(sessions[0]["models"], json!([{ "model": "gpt-large" }]));
        assert_eq!(sessions[24]["session_id"], "large-1975");
        assert_eq!(first["coverage"]["examined"], 25);
        assert_eq!(first["coverage"]["denominator"], 2_000);

        let (status, last) = get_json(
            &agent,
            &format!(
                "{}/api/loom/temporal?limit=25&offset=1990",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200, "{last}");
        let sessions = last["payload"]["sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("Loom sessions should be an array: {last}"));
        assert_eq!(sessions.len(), 10);
        assert_eq!(sessions[0]["session_id"], "large-0009");
        assert_eq!(sessions[9]["session_id"], "large-0000");
        assert_eq!(sessions[9]["messages"], 12);
        assert_eq!(sessions[9]["last_message_at"], 1_700_000_110);
    });
}

/// A Git evidence read that never answers, the shape of a projection read
/// stuck behind store convergence.
struct StalledGitCorrelationRead;

impl DashboardGitCorrelationReadPortV1 for StalledGitCorrelationRead {
    fn read(&self, _session_ids: BTreeSet<String>) -> DashboardGitCorrelationReadFutureV1<'_> {
        Box::pin(std::future::pending())
    }
}

#[test]
fn loom_temporal_read_past_its_deadline_answers_the_typed_timeout() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_with_git_correlation_authority(Arc::new(
            StalledGitCorrelationRead,
        ))
        .await;

        // Longer than the route's admitted request deadline, so the answer
        // comes from the route rather than from the client giving up.
        let agent = http_agent_with_timeout(Duration::from_secs(90));
        let url = format!("{}/api/loom/temporal?limit=25&offset=0", fixture.base_url);
        let (status, body) = response_to_json(
            agent
                .get(&url)
                .call()
                .unwrap_or_else(|error| panic!("GET {url} got no answer: {error}")),
        );

        assert_eq!(status, 504, "{body}");
        assert_eq!(body["domain_state"], "timed_out");
        assert_eq!(body["payload"], Value::Null);
        assert_eq!(
            body["coverage"]["omission_reasons"][0],
            "loom_temporal_read_timed_out"
        );
        assert_eq!(
            body["legal_actions"],
            json!([{
                "kind": "refresh",
                "operation": "use-case.dashboard.loom.temporal.refresh"
            }])
        );
    });
}

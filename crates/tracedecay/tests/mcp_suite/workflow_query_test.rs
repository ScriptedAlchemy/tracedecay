//! End-to-end tests for the workflow-run query surface: `tracedecay_workflows`
//! lists runs for a thread or git ref, shows one run, and drills into one agent.
//! Everything is driven through the real `handle_tool_call` dispatch against a
//! temp `~/.claude` fixture tree plus a seeded `sessions.db`, mirroring
//! `git_correlation_test.rs`.

#![cfg(feature = "test-transport")]

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use tracedecay::mcp::McpServer;
use tracedecay_project::project::TraceDecay;
use tracedecay_project::test_support::host_admission::{
    HostAdmissionTestRuntimeV1, ProjectScopedTestRuntimeV1,
};
use tracedecay_sessions::runtime::git_correlation::{
    DEFAULT_SPAN_MERGE_GAP_SECS, SpanObservation, SpanSource, normalize_worktree,
};

use crate::common;
use crate::support::extract_tool_result_json as extract_json;

// Fixture identity, shared across the on-disk tree and the seeded DB rows.
const SLUG: &str = "-home-zack-projects-fixture";
const SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";
const RUN_ID: &str = "wf_fixture-run-01";
const AGENT_MINE_ID: &str = "a17141dbe5a308242";
const AGENT_RUN_ID: &str = "aa09ec4d07fccc915";
const AGENT_MINE_LABEL: &str = "mine:claude-transcripts";
const AGENT_RUN_LABEL: &str = "run:eval-batch";

/// Materializes a workflow run on disk under `<home>/.claude/projects/...`,
/// shaped exactly like a real run: a parent transcript recording `cwd` (so the
/// run attributes to `project_root`), a `workflows/<run_id>.json` meta with two
/// `workflow_agent` progress rows, the two `agent-<id>.jsonl` transcripts (each
/// with an assistant `usage`), and a `journal.jsonl`.
fn write_workflow_fixture(home: &Path, project_root: &Path) {
    let cwd = project_root.to_string_lossy().to_string();
    let session_dir = home
        .join(".claude")
        .join("projects")
        .join(SLUG)
        .join(SESSION_ID);
    let workflows_dir = session_dir.join("workflows");
    let agents_dir = session_dir.join("subagents").join("workflows").join(RUN_ID);
    std::fs::create_dir_all(&workflows_dir).unwrap_or_else(|e| panic!("workflows dir: {e}"));
    std::fs::create_dir_all(&agents_dir).unwrap_or_else(|e| panic!("agents dir: {e}"));

    // Parent transcript sits at <slug>/<session_id>.jsonl (sibling of the
    // <session_id> dir) and carries the owning session's cwd.
    let parent_transcript = session_dir.with_extension("jsonl");
    std::fs::write(
        &parent_transcript,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "sessionId": SESSION_ID,
                "cwd": cwd,
                "timestamp": "2026-07-04T05:00:00.000Z",
                "message": {"role": "user", "content": "kick off the eval workflow"}
            })
        ),
    )
    .unwrap_or_else(|e| panic!("parent transcript: {e}"));

    // Run meta + result.
    let meta = json!({
        "runId": RUN_ID,
        "workflowName": "tracedecay-triggering-evals",
        "summary": "Mine real transcripts into a broad eval corpus\nthen score them",
        "status": "completed",
        "startTime": 1_783_142_254_914_i64,
        "durationMs": 983_890_i64,
        "agentCount": 2,
        "defaultModel": "claude-fable-5",
        "phases": [
            {"title": "Mine", "detail": "harvest scenarios"},
            {"title": "Run", "detail": "run it", "model": "fable"}
        ],
        "result": {"scored": 45, "scenarios": 36},
        "workflowProgress": [
            {"type": "workflow_phase", "phaseTitle": "Mine"},
            {
                "type": "workflow_agent",
                "label": AGENT_MINE_LABEL,
                "phaseTitle": "Mine",
                "phaseIndex": 1,
                "agentId": AGENT_MINE_ID,
                "model": "claude-fable-5",
                "state": "done",
                "startedAt": 1_783_142_254_936_i64,
                "lastProgressAt": 1_783_142_255_936_i64
            },
            {
                "type": "workflow_agent",
                "label": AGENT_RUN_LABEL,
                "phaseTitle": "Run",
                "agentId": AGENT_RUN_ID,
                "state": "in_progress",
                "startedAt": 1_783_142_260_000_i64
            }
        ]
    });
    std::fs::write(
        workflows_dir.join(format!("{RUN_ID}.json")),
        serde_json::to_string_pretty(&meta).unwrap_or_else(|e| panic!("meta json: {e}")),
    )
    .unwrap_or_else(|e| panic!("write meta: {e}"));

    // Per-agent transcripts (cwd + an assistant usage so tokens/session id fill).
    for (agent_id, in_tok, out_tok) in [
        (AGENT_MINE_ID, 100_i64, 40_i64),
        (AGENT_RUN_ID, 10_i64, 8_i64),
    ] {
        let body = format!(
            "{}\n{}\n",
            json!({
                "type": "user",
                "isSidechain": true,
                "sessionId": format!("agent-{agent_id}"),
                "cwd": cwd,
                "gitBranch": "feat/evals",
                "timestamp": "2026-07-04T05:17:34.967Z",
                "message": {"role": "user", "content": "do the phase work"}
            }),
            json!({
                "type": "assistant",
                "isSidechain": true,
                "sessionId": format!("agent-{agent_id}"),
                "timestamp": "2026-07-04T05:18:00.000Z",
                "message": {
                    "role": "assistant",
                    "usage": {"input_tokens": in_tok, "output_tokens": out_tok}
                }
            }),
        );
        std::fs::write(agents_dir.join(format!("agent-{agent_id}.jsonl")), body)
            .unwrap_or_else(|e| panic!("agent transcript: {e}"));
        std::fs::write(
            agents_dir.join(format!("agent-{agent_id}.meta.json")),
            json!({"agentType": "general", "spawnDepth": 1}).to_string(),
        )
        .unwrap_or_else(|e| panic!("agent meta: {e}"));
    }

    // Journal: both agents started, the mine agent finished.
    std::fs::write(
        agents_dir.join("journal.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            json!({"type": "started", "agentId": AGENT_MINE_ID}),
            json!({"type": "started", "agentId": AGENT_RUN_ID}),
            json!({"type": "result", "agentId": AGENT_MINE_ID}),
        ),
    )
    .unwrap_or_else(|e| panic!("journal: {e}"));
}

fn span(session_id: &str, branch: &str, worktree: &str, ts: i64) -> SpanObservation {
    SpanObservation {
        provider: "claude".to_string(),
        session_id: session_id.to_string(),
        thread_id: None,
        branch: Some(branch.to_string()),
        worktree: worktree.to_string(),
        ts,
        source: SpanSource::Ingest,
    }
}

/// Drives the raw MCP connection for markdown because the shared retained
/// fixture helper intentionally unwraps only JSON evidence envelopes.
async fn call_md(cg: &TraceDecay, tool: &str, mut args: Value) -> String {
    if let Some(obj) = args.as_object_mut() {
        obj.insert("format".to_owned(), json!("markdown"));
    }
    let runtime = cg
        .test_runtime_for_test()
        .expect("init retains registered project session runtime");
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("workflow fixture has a registered project identity");
    runtime
        .upsert_code_project(&project_id, cg.project_root(), None, None, None)
        .await
        .unwrap_or_else(|error| panic!("register workflow fixture project: {error}"));
    let graph = Box::pin(TraceDecay::open_with_options(
        cg.project_root(),
        crate::support::graph_open_options(cg),
    ))
    .await
    .unwrap_or_else(|error| panic!("open workflow fixture graph: {error}"));
    let server = Box::pin(McpServer::new_with_host_admission_test_runtime_for_test(
        graph,
        None,
        ProjectScopedTestRuntimeV1::new(runtime)
            .expect("workflow fixture runtime is project scoped"),
    ))
    .await
    .unwrap_or_else(|error| panic!("construct workflow fixture server: {error}"));
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": args,
        },
    })
    .to_string();
    let response =
        crate::mcp_server_test::run_client_connection_with_messages(server.clone(), vec![request])
            .await
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{tool} should return one MCP response"));
    server.shutdown().await;
    let response: Value = serde_json::from_str(&response)
        .unwrap_or_else(|error| panic!("{tool} should return MCP JSON: {error}"));
    assert!(
        response.get("error").is_none(),
        "{tool} should succeed: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} result should carry text content: {response}"))
        .to_string()
}

async fn call(
    cg: &TraceDecay,
    _runtime: &HostAdmissionTestRuntimeV1,
    tool: &str,
    mut args: Value,
) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    let result = crate::support::handle_tool_call(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
    extract_json(&result)
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn workflow_queries_distinguish_missing_schema_from_empty_results() {
    let (_env, project_root) = common::IsolatedHome::new();
    let cg = TraceDecay::init(&project_root)
        .await
        .unwrap_or_else(|error| panic!("init project: {error}"));
    // Reuse the runtime retained by init. A second HostAdmissionTestRuntimeV1
    // daemon scope on the same profile overlaps the init-held authority.
    let runtime = cg
        .test_runtime_for_test()
        .expect("init retains registered project session runtime");
    runtime
        .drop_project_workflow_schema_for_test()
        .await
        .unwrap_or_else(|error| panic!("drop workflow schema: {error}"));

    for args in [
        json!({"session_id": SESSION_ID}),
        json!({"run_id": RUN_ID}),
        json!({"branch": "main"}),
    ] {
        let error = crate::support::handle_tool_call(&cg, "tracedecay_workflows", args, None, None)
            .await
            .expect_err("a missing workflow index must be a retained problem")
            .to_string();
        assert!(
            error.contains("workflow_index_not_built"),
            "a missing workflow schema must take precedence over empty or missing results"
        );
        assert!(error.contains("\"kind\":\"unavailable\""), "{error}");
    }
}

/// Ingests the on-disk fixture and drives the three `tracedecay_workflows`
/// modes plus the git-scope list end to end.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn workflows_query_surface_end_to_end() {
    let (env, project_root) = common::IsolatedHome::new();
    let home = env.home().to_path_buf();

    let cg = TraceDecay::init(&project_root)
        .await
        .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();

    // The fixture's agent transcripts record cwd == the canonical project root
    // so the ingest sweep attributes the run to this project.
    write_workflow_fixture(&home, cg.project_root());

    // Reuse the runtime retained by init (same overlap reason as the missing-
    // schema workflow query above).
    let runtime = cg
        .test_runtime_for_test()
        .expect("init retains registered project session runtime");

    let stats = runtime
        .ingest_workflows_for_test(&home, cg.project_root())
        .await
        .unwrap_or_else(|error| panic!("ingest workflows: {error}"));
    assert_eq!(stats.runs_ingested, 1, "one run ingested: {stats:?}");
    assert_eq!(stats.agents_ingested, 2, "two agents ingested: {stats:?}");

    // (a) session mode: list runs spawned by the parent thread.
    let by_session = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "session_id": SESSION_ID }),
    )
    .await;
    assert_eq!(by_session["mode"], "session", "{by_session}");
    assert_eq!(by_session["count"], 1, "{by_session}");
    assert_eq!(by_session["runs"][0]["run_id"], RUN_ID, "{by_session}");
    assert_eq!(by_session["runs"][0]["name"], "tracedecay-triggering-evals");
    assert_eq!(by_session["runs"][0]["agent_count"], 2);

    // (b) run mode: one run shows its phases + the two-agent roster + summary.
    let by_run = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "run_id": RUN_ID }),
    )
    .await;
    assert_eq!(by_run["mode"], "run", "{by_run}");
    assert_eq!(by_run["found"], true, "{by_run}");
    assert_eq!(by_run["agent_count"], 2, "{by_run}");
    assert_eq!(by_run["run"]["parent_session_id"], SESSION_ID);
    // Summary is carried (multi-line in the fixture; stored one-lined).
    assert!(
        by_run["run"]["result_summary"]
            .as_str()
            .unwrap_or_default()
            .contains("Mine real transcripts"),
        "{by_run}"
    );
    let agent_labels: Vec<String> = by_run["agents"]
        .as_array()
        .unwrap_or_else(|| panic!("agents should be an array: {by_run}"))
        .iter()
        .map(|a| a["agent_label"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        agent_labels.contains(&AGENT_MINE_LABEL.to_string()),
        "{by_run}"
    );
    assert!(
        agent_labels.contains(&AGENT_RUN_LABEL.to_string()),
        "{by_run}"
    );

    // Retained markdown leads with a bounded evidence payload before status and
    // provenance; `--json` remains the complete typed result.
    let run_md = call_md(&cg, "tracedecay_workflows", json!({ "run_id": RUN_ID })).await;
    assert!(
        run_md.starts_with("## workflows\n\n### Payload\n\n"),
        "{run_md}"
    );
    assert!(run_md.contains("\"result_summary\""), "{run_md}");
    assert!(run_md.contains("- Status: `success`"), "{run_md}");
    assert!(run_md.contains("- Evidence: `"), "{run_md}");
    assert!(run_md.contains("- Provenance: `"), "{run_md}");

    // (c) agent drill: one agent surfaces its transcript path + replay hint. The
    // mine agent had a real transcript, so ingest recorded its transcript_path.
    let drill = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "run_id": RUN_ID, "agent_label": AGENT_MINE_LABEL }),
    )
    .await;
    assert_eq!(drill["mode"], "agent", "{drill}");
    assert_eq!(drill["found"], true, "{drill}");
    assert_eq!(drill["agent"]["agent_label"], AGENT_MINE_LABEL);
    let transcript = drill["agent"]["transcript_path"]
        .as_str()
        .unwrap_or_default();
    assert!(
        transcript.ends_with(&format!("agent-{AGENT_MINE_ID}.jsonl")),
        "drill transcript path: {drill}"
    );
    // Tokens summed from the transcript usage (100+40).
    assert_eq!(drill["agent"]["tokens"], 140, "{drill}");

    // (d) git-scope mode: after a span places the parent thread on a branch,
    // the run surfaces via the parent-session span join.
    let worktree = project_key.clone();
    runtime
        .record_project_span_for_test(
            &span(SESSION_ID, "feat/evals", &worktree, 1_783_142_254),
            DEFAULT_SPAN_MERGE_GAP_SECS,
        )
        .await
        .unwrap_or_else(|e| panic!("record span: {e}"));

    let by_branch = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "branch": "feat/evals" }),
    )
    .await;
    assert_eq!(by_branch["mode"], "git_scope", "{by_branch}");
    assert_eq!(by_branch["count"], 1, "{by_branch}");
    assert_eq!(by_branch["runs"][0]["run_id"], RUN_ID, "{by_branch}");

    // A branch nothing ran on returns no runs.
    let started = Instant::now();
    let by_absent = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "branch": "feat/absent" }),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a bounded empty workflow listing took {:?}",
        started.elapsed()
    );
    assert_eq!(by_absent["count"], 0, "{by_absent}");

    drop(runtime);
    cg.close();
}

const PHASE_JSON: &str = r#"[{"detail":"harvest scenarios","title":"Mine"},{"detail":"run it","model":"fable","title":"Run"}]"#;
const RESULT_SUMMARY: &str = "Mine real transcripts into a broad eval corpus then score them";
const DESCRIPTION: &str = "Mine real transcripts into a broad eval corpus\nthen score them";
const STARTED_TS: i64 = 1_783_142_254;
const ENDED_TS: i64 = 1_783_143_237;
const AGENT_ENDED_TS: i64 = 1_783_142_280;

fn agent_transcript(home: &Path, agent_id: &str) -> String {
    home.join(".claude")
        .join("projects")
        .join(SLUG)
        .join(SESSION_ID)
        .join("subagents")
        .join("workflows")
        .join(RUN_ID)
        .join(format!("agent-{agent_id}.jsonl"))
        .to_string_lossy()
        .into_owned()
}

fn expected_run() -> Value {
    json!({
        "run_id": RUN_ID,
        "parent_session_id": SESSION_ID,
        "name": "tracedecay-triggering-evals",
        "description": DESCRIPTION,
        "phase_json": PHASE_JSON,
        "status": "completed",
        "started_ts": STARTED_TS,
        "ended_ts": ENDED_TS,
        "result_summary": RESULT_SUMMARY,
        "agent_count": 2
    })
}

fn expected_agent(
    home: &Path,
    label: &str,
    agent_id: &str,
    phase: &str,
    status: &str,
    tokens: i64,
    started_ts: i64,
) -> Value {
    json!({
        "run_id": RUN_ID,
        "agent_label": label,
        "agent_id": agent_id,
        "phase": phase,
        "transcript_path": agent_transcript(home, agent_id),
        "agent_session_id": format!("agent-{agent_id}"),
        "status": status,
        "model": "claude-fable-5",
        "tokens": tokens,
        "started_ts": started_ts,
        "ended_ts": AGENT_ENDED_TS
    })
}

fn refusal_envelope(error: &str) -> Value {
    let marker = "answered with a retained refusal: ";
    let (_, body) = error.split_once(marker).unwrap_or_else(|| {
        panic!("tracedecay_workflows refusal was not a retained problem: {error}")
    });
    serde_json::from_str(body).unwrap_or_else(|parse_error| {
        panic!("tracedecay_workflows refusal was not JSON: {parse_error}\n{error}")
    })
}

fn stable_problem(envelope: &Value) -> Value {
    let mut problem = envelope
        .get("problem")
        .cloned()
        .unwrap_or_else(|| panic!("refusal has no problem record: {envelope}"));
    let request_id = envelope["request_id"].clone();
    assert_eq!(problem["request_id"], request_id, "{envelope}");
    assert_eq!(problem["trace_id"], request_id, "{envelope}");
    let Some(object) = problem.as_object_mut() else {
        panic!("refusal problem is not an object: {problem}");
    };
    object.insert("request_id".to_owned(), json!("<request>"));
    object.insert("trace_id".to_owned(), json!("<request>"));
    problem
}

fn assert_refusal(error: &str, problem: Value) {
    let envelope = refusal_envelope(error);
    assert_eq!(
        envelope["contract"],
        json!({
            "schema_id": "schema.application.retained.workflows.result",
            "schema_revision": 1
        }),
        "{envelope}"
    );
    assert_eq!(stable_problem(&envelope), problem, "{envelope}");
}

fn invalid_request_problem() -> Value {
    json!({
        "revision": 1,
        "kind": "invalid_request",
        "code": "application.retained.invalid-request",
        "message": "The retained operation request is invalid.",
        "diagnostic": {
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid."
        },
        "detail": null,
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": false,
        "retry": "never",
        "retry_scope": null,
        "retry_after_millis": null,
        "cancellation_stage": null,
        "unavailable_classification": null,
        "execution_failure_classification": null,
        "request_id": "<request>",
        "trace_id": "<request>",
        "details": [],
        "legal_actions": ["correct_request"],
        "coverage": null
    })
}

fn unbuilt_index_problem() -> Value {
    json!({
        "revision": 1,
        "kind": "unavailable",
        "code": "application.retained.authority-unavailable",
        "message": "The retained operation authority is unavailable: workflow_index_not_built: the workflow index has not been built for this project yet",
        "diagnostic": {
            "code": "application.retained.authority-unavailable",
            "message": "The retained operation authority is unavailable: workflow_index_not_built: the workflow index has not been built for this project yet"
        },
        "detail": null,
        "committed_receipt": null,
        "owning_layer": "application",
        "terminality": "pre_admission",
        "retryable": true,
        "retry": "after_delay",
        "retry_scope": "same_request",
        "retry_after_millis": 250,
        "cancellation_stage": null,
        "unavailable_classification": "authority",
        "execution_failure_classification": null,
        "request_id": "<request>",
        "trace_id": "<request>",
        "details": [],
        "legal_actions": ["retry"],
        "coverage": null
    })
}

async fn refuse(cg: &TraceDecay, args: Value) -> String {
    crate::support::handle_tool_call(cg, "tracedecay_workflows", args, None, None)
        .await
        .expect_err("tracedecay_workflows should refuse this request")
        .to_string()
}

/// Calls `tracedecay_workflows` through MCP `tools/call` and compares each
/// observed document with the result a caller can act on.
#[cfg(feature = "test-transport")]
#[tokio::test]
async fn workflows_tool_returns_literal_query_documents() {
    let (env, project_root) = common::IsolatedHome::new();
    let home = env.home().to_path_buf();
    let cg = TraceDecay::init(&project_root)
        .await
        .unwrap_or_else(|error| panic!("init project: {error}"));
    let project_key = cg.project_root().to_string_lossy().to_string();
    write_workflow_fixture(&home, cg.project_root());
    let runtime = cg
        .test_runtime_for_test()
        .expect("init retains registered project session runtime");
    let stats = runtime
        .ingest_workflows_for_test(&home, cg.project_root())
        .await
        .unwrap_or_else(|error| panic!("ingest workflows: {error}"));
    assert_eq!(stats.runs_ingested, 1);
    assert_eq!(stats.agents_ingested, 2);

    let mine = expected_agent(
        &home,
        AGENT_MINE_LABEL,
        AGENT_MINE_ID,
        "Mine",
        "completed",
        140,
        STARTED_TS,
    );
    let run_agent = expected_agent(
        &home,
        AGENT_RUN_LABEL,
        AGENT_RUN_ID,
        "Run",
        "running",
        18,
        1_783_142_260,
    );
    let run = expected_run();

    let by_session = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "session_id": SESSION_ID }),
    )
    .await;
    assert_eq!(
        by_session,
        json!({
            "status": "ok",
            "count": 1,
            "mode": "session",
            "runs": [run.clone()],
            "session_id": SESSION_ID
        })
    );

    let by_run = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "run_id": RUN_ID }),
    )
    .await;
    assert_eq!(
        by_run,
        json!({
            "status": "ok",
            "agent_count": 2,
            "agents": [mine.clone(), run_agent],
            "agents_complete": true,
            "agents_coverage": "complete",
            "agents_returned": 2,
            "found": true,
            "mode": "run",
            "run": run.clone(),
            "run_id": RUN_ID
        })
    );

    let bounded = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "run_id": RUN_ID, "limit": 1 }),
    )
    .await;
    assert_eq!(
        bounded,
        json!({
            "status": "ok",
            "agent_count": 2,
            "agents": [mine.clone()],
            "agents_complete": false,
            "agents_coverage": "bounded_prefix",
            "agents_returned": 1,
            "found": true,
            "mode": "run",
            "run": run.clone(),
            "run_id": RUN_ID
        })
    );

    let drill = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "run_id": RUN_ID, "agent_label": AGENT_MINE_LABEL }),
    )
    .await;
    assert_eq!(
        drill,
        json!({
            "status": "ok",
            "agent": mine,
            "agent_count": 2,
            "agent_label": AGENT_MINE_LABEL,
            "agents_returned": 1,
            "found": true,
            "lookup_complete": true,
            "lookup_coverage": "conclusive",
            "mode": "agent",
            "run": run.clone(),
            "run_id": RUN_ID
        })
    );

    let missing_agent = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "run_id": RUN_ID, "agent_label": "missing:label" }),
    )
    .await;
    assert_eq!(
        missing_agent,
        json!({
            "status": "ok",
            "agent_count": 2,
            "agent_label": "missing:label",
            "agents_returned": 0,
            "found": false,
            "lookup_complete": true,
            "lookup_coverage": "conclusive",
            "mode": "agent",
            "run": run.clone(),
            "run_id": RUN_ID
        })
    );

    let missing_run = json!({
        "status": "ok",
        "count": 0,
        "found": false,
        "mode": "run",
        "run_id": "wf_missing",
        "runs": []
    });
    assert_eq!(
        call(
            &cg,
            &runtime,
            "tracedecay_workflows",
            json!({ "run_id": "wf_missing" }),
        )
        .await,
        missing_run.clone()
    );
    assert_eq!(
        call(
            &cg,
            &runtime,
            "tracedecay_workflows",
            json!({ "run_id": "wf_missing", "agent_label": AGENT_MINE_LABEL }),
        )
        .await,
        missing_run
    );

    runtime
        .record_project_span_for_test(
            &span(SESSION_ID, "feat/evals", &project_key, STARTED_TS),
            DEFAULT_SPAN_MERGE_GAP_SECS,
        )
        .await
        .unwrap_or_else(|error| panic!("record span: {error}"));

    let by_branch = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "branch": "feat/evals" }),
    )
    .await;
    assert_eq!(
        by_branch,
        json!({
            "status": "ok",
            "count": 1,
            "git_filter": { "branch": "feat/evals", "worktree": null, "commit": null },
            "mode": "git_scope",
            "runs": [run.clone()]
        })
    );
    let by_worktree = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "worktree": project_key }),
    )
    .await;
    assert_eq!(
        by_worktree,
        json!({
            "status": "ok",
            "count": 1,
            "git_filter": {
                "branch": null,
                "worktree": normalize_worktree(&project_key),
                "commit": null
            },
            "mode": "git_scope",
            "runs": [run]
        })
    );
    let by_absent = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "branch": "feat/absent" }),
    )
    .await;
    assert_eq!(
        by_absent,
        json!({
            "status": "ok",
            "count": 0,
            "git_filter": { "branch": "feat/absent", "worktree": null, "commit": null },
            "mode": "git_scope",
            "runs": []
        })
    );
    let by_commit = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "commit": "ABC123" }),
    )
    .await;
    assert_eq!(
        by_commit,
        json!({
            "status": "ok",
            "count": 0,
            "git_filter": { "branch": null, "worktree": null, "commit": "abc123" },
            "mode": "git_scope",
            "runs": []
        })
    );

    let invalid = invalid_request_problem();
    for args in [
        json!({}),
        json!({ "session_id": SESSION_ID, "run_id": RUN_ID }),
        json!({ "run_id": RUN_ID, "agent_label": " " }),
        json!({ "commit": "zz" }),
        json!({ "session_id": SESSION_ID, "limit": 0 }),
    ] {
        assert_refusal(&refuse(&cg, args).await, invalid.clone());
    }

    runtime
        .drop_project_workflow_schema_for_test()
        .await
        .unwrap_or_else(|error| panic!("drop workflow schema: {error}"));
    assert_refusal(
        &refuse(&cg, json!({ "session_id": SESSION_ID })).await,
        unbuilt_index_problem(),
    );
    assert_refusal(&refuse(&cg, json!({})).await, invalid);

    drop(runtime);
    cg.close();
}

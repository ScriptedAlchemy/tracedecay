//! End-to-end tests for the workflow-run query surface: `tracedecay_workflows`
//! (list runs for a thread / for a git ref, show one run, drill one agent) and
//! the `workflow_run` / `workflow_agent` agent-precision filter on
//! `tracedecay_message_search`. Everything is driven through the real
//! `handle_tool_call` dispatch against a temp `~/.claude` fixture tree plus a
//! seeded `sessions.db`, mirroring `git_correlation_test.rs`.

use std::path::Path;

use serde_json::{json, Value};

use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::git_correlation::{
    SpanObservation, SpanSource, DEFAULT_SPAN_MERGE_GAP_SECS,
};
use tracedecay::sessions::workflow_ingest::ingest_workflow_runs;
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::TraceDecay;

use crate::common;

// Fixture identity, shared across the on-disk tree and the seeded DB rows.
const SLUG: &str = "-home-zack-projects-fixture";
const SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";
const RUN_ID: &str = "wf_fixture-run-01";
const AGENT_MINE_ID: &str = "a17141dbe5a308242";
const AGENT_RUN_ID: &str = "aa09ec4d07fccc915";
const AGENT_MINE_LABEL: &str = "mine:claude-transcripts";
const AGENT_RUN_LABEL: &str = "run:eval-batch";

/// Absolute path of the `agent-<id>.jsonl` transcript inside the fixture tree.
/// This is exactly what the ingest sweep records as `transcript_path`, and what
/// the seeded `session_messages.source_path` must equal for the workflow-scoped
/// search join to fire.
fn agent_transcript_path(home: &Path, agent_id: &str) -> String {
    home.join(".claude")
        .join("projects")
        .join(SLUG)
        .join(SESSION_ID)
        .join("subagents")
        .join("workflows")
        .join(RUN_ID)
        .join(format!("agent-{agent_id}.jsonl"))
        .to_string_lossy()
        .to_string()
}

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

/// A session row for the run's parent thread, so a recorded git span attributes
/// to a session the store knows about (mirrors ClaudeSource's parent session).
fn parent_session(project_key: &str) -> SessionRecord {
    SessionRecord {
        provider: "claude".to_string(),
        session_id: SESSION_ID.to_string(),
        project_key: project_key.to_string(),
        project_path: project_key.to_string(),
        title: Some("workflow parent thread".to_string()),
        started_at: Some(1_783_142_254),
        ended_at: None,
        transcript_path: Some(format!("{SESSION_ID}.jsonl")),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

/// A subagent session row for one workflow agent, so its messages join back to a
/// session the store knows about (the message-search JOIN requires it) — the
/// shape ClaudeSource would persist for a sidechain transcript.
fn agent_session(home: &Path, project_key: &str, agent_id: &str) -> SessionRecord {
    SessionRecord {
        provider: "claude".to_string(),
        session_id: format!("agent-{agent_id}"),
        project_key: project_key.to_string(),
        project_path: project_key.to_string(),
        title: Some(format!("agent {agent_id}")),
        started_at: Some(1_783_142_260),
        ended_at: None,
        transcript_path: Some(agent_transcript_path(home, agent_id)),
        metadata_json: None,
        parent_session_id: Some(SESSION_ID.to_string()),
        is_subagent: true,
        agent_id: Some(agent_id.to_string()),
        parent_tool_use_id: None,
    }
}

/// A message row standing in for one line of an agent transcript: `session_id`
/// is the agent's own session id and `source_path` is the agent-`<id>`.jsonl
/// file — the two keys the workflow-scoped search join matches on.
fn agent_message(
    home: &Path,
    agent_id: &str,
    message_id: &str,
    text: &str,
) -> SessionMessageRecord {
    let transcript = agent_transcript_path(home, agent_id);
    SessionMessageRecord {
        provider: "claude".to_string(),
        message_id: message_id.to_string(),
        session_id: format!("agent-{agent_id}"),
        role: "assistant".to_string(),
        timestamp: Some(1_783_142_260),
        ordinal: 1,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: Some("claude-fable-5".to_string()),
        tool_names: None,
        source_path: Some(transcript),
        source_offset: Some(0),
        metadata_json: None,
    }
}

fn extract_json(result: &tracedecay::mcp::ToolResult) -> Value {
    let text = result.value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result should carry text content: {}", result.value));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tool result should be JSON: {e}\n{text}"))
}

/// Renders a tool call as markdown (no `format:"json"` override) so tests can
/// assert on the summary-first markdown surface.
async fn call_md(cg: &TraceDecay, tool: &str, args: Value) -> String {
    let result = tracedecay::mcp::handle_tool_call(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
    result.value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} result should carry text content: {}", result.value))
        .to_string()
}

async fn call(cg: &TraceDecay, tool: &str, mut args: Value) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    let result = tracedecay::mcp::handle_tool_call(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
    extract_json(&result)
}

fn search_session_ids(payload: &Value) -> Vec<String> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("search results should be an array: {payload}"))
        .iter()
        .map(|hit| {
            hit["session"]["session_id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Ingests the on-disk fixture and drives the three `tracedecay_workflows`
/// modes plus the git-scope list end to end.
#[tokio::test]
async fn workflows_query_surface_end_to_end() {
    let (env, project_root) = common::IsolatedEnv::acquire().await;
    let home = env.home().to_path_buf();

    let cg = TraceDecay::init(&project_root)
        .await
        .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();

    // The fixture's agent transcripts record cwd == the canonical project root
    // so the ingest sweep attributes the run to this project.
    write_workflow_fixture(&home, cg.project_root());

    let db_path = cg.store_layout().sessions_db_path.clone();
    let db = GlobalDb::open_at(&db_path)
        .await
        .unwrap_or_else(|| panic!("open sessions.db"));

    // The public ingest entrypoint reads $HOME (isolated to the tempdir), so it
    // sweeps our fixture tree.
    let stats = ingest_workflow_runs(&db, cg.project_root()).await;
    assert_eq!(stats.runs_ingested, 1, "one run ingested: {stats:?}");
    assert_eq!(stats.agents_ingested, 2, "two agents ingested: {stats:?}");

    // (a) session mode: list runs spawned by the parent thread.
    let by_session = call(
        &cg,
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
    let by_run = call(&cg, "tracedecay_workflows", json!({ "run_id": RUN_ID })).await;
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

    // Markdown for the run detail is summary-first (phases + agents headings,
    // no leaked JSON object).
    let run_md = call_md(&cg, "tracedecay_workflows", json!({ "run_id": RUN_ID })).await;
    assert!(run_md.contains("Workflow Run"), "{run_md}");
    assert!(run_md.contains("Phases"), "{run_md}");
    assert!(run_md.contains("Agents"), "{run_md}");
    assert!(!run_md.contains("\"result_summary\""), "{run_md}");

    // (c) agent drill: one agent surfaces its transcript path + replay hint. The
    // mine agent had a real transcript, so ingest recorded its transcript_path.
    let drill = call(
        &cg,
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
    db.git_record_span_observation(
        &span(SESSION_ID, "feat/evals", &worktree, 1_783_142_254),
        DEFAULT_SPAN_MERGE_GAP_SECS,
    )
    .await
    .unwrap_or_else(|e| panic!("record span: {e}"));

    let by_branch = call(
        &cg,
        "tracedecay_workflows",
        json!({ "branch": "feat/evals" }),
    )
    .await;
    assert_eq!(by_branch["mode"], "git_scope", "{by_branch}");
    assert_eq!(by_branch["count"], 1, "{by_branch}");
    assert_eq!(by_branch["runs"][0]["run_id"], RUN_ID, "{by_branch}");

    // A branch nothing ran on returns no runs.
    let by_absent = call(
        &cg,
        "tracedecay_workflows",
        json!({ "branch": "feat/absent" }),
    )
    .await;
    assert_eq!(by_absent["count"], 0, "{by_absent}");

    drop(db);
    cg.close();
}

/// Drives the `workflow_run` / `workflow_agent` agent-precision filter on
/// `tracedecay_message_search`.
#[tokio::test]
async fn message_search_workflow_scope_narrows_to_run_agents() {
    let (env, project_root) = common::IsolatedEnv::acquire().await;
    let home = env.home().to_path_buf();

    let cg = TraceDecay::init(&project_root)
        .await
        .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();

    write_workflow_fixture(&home, cg.project_root());

    let db_path = cg.store_layout().sessions_db_path.clone();
    let db = GlobalDb::open_at(&db_path)
        .await
        .unwrap_or_else(|| panic!("open sessions.db"));

    // Index the run + agents (sets each agent's transcript_path).
    let stats = ingest_workflow_runs(&db, cg.project_root()).await;
    assert_eq!(stats.runs_ingested, 1, "{stats:?}");

    // Seed the parent thread + the two agent subagent sessions, then two agent
    // messages whose source_path equals the agents' transcript files, plus an
    // unrelated session whose message shares the query term but belongs to no
    // workflow agent. Sessions come first: the message-search JOIN drops a
    // message whose (provider, session_id) has no session row.
    assert!(db.upsert_session(&parent_session(&project_key)).await);
    assert!(
        db.upsert_session(&agent_session(&home, &project_key, AGENT_MINE_ID))
            .await
    );
    assert!(
        db.upsert_session(&agent_session(&home, &project_key, AGENT_RUN_ID))
            .await
    );
    assert!(
        db.upsert_session(&SessionRecord {
            session_id: "unrelated-thread".to_string(),
            ..parent_session(&project_key)
        })
        .await
    );
    assert!(
        db.upsert_session_message(&agent_message(
            &home,
            AGENT_MINE_ID,
            "mine-m1",
            "sifted transcripts into eval scenarios harvest",
        ))
        .await
    );
    assert!(
        db.upsert_session_message(&agent_message(
            &home,
            AGENT_RUN_ID,
            "run-m1",
            "executed the eval scenarios batch harvest",
        ))
        .await
    );
    // Off-run noise: same term, different session, not an agent of the run.
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            session_id: "unrelated-thread".to_string(),
            message_id: "noise-m1".to_string(),
            source_path: Some("/somewhere/unrelated.jsonl".to_string()),
            ..agent_message(
                &home,
                AGENT_MINE_ID,
                "noise-m1",
                "harvest happening elsewhere"
            )
        })
        .await
    );

    // workflow_run scopes to BOTH agents of the run, excluding the off-run noise.
    let by_run = call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "harvest",
            "provider": "claude",
            "catch_up": false,
            "workflow_run": RUN_ID,
        }),
    )
    .await;
    assert_eq!(by_run["workflow_filter_applied"], true, "{by_run}");
    assert_eq!(by_run["workflow_run"], RUN_ID, "{by_run}");
    assert_eq!(
        by_run["workflow_run_parent_session"], SESSION_ID,
        "{by_run}"
    );
    let run_sessions = search_session_ids(&by_run);
    assert!(
        run_sessions.contains(&format!("agent-{AGENT_MINE_ID}")),
        "{by_run}"
    );
    assert!(
        run_sessions.contains(&format!("agent-{AGENT_RUN_ID}")),
        "{by_run}"
    );
    assert!(
        !run_sessions.contains(&"unrelated-thread".to_string()),
        "off-run message leaked: {by_run}"
    );

    // workflow_agent narrows to just the one agent.
    let by_agent = call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "harvest",
            "provider": "claude",
            "catch_up": false,
            "workflow_run": RUN_ID,
            "workflow_agent": AGENT_MINE_LABEL,
        }),
    )
    .await;
    assert_eq!(by_agent["workflow_agent"], AGENT_MINE_LABEL, "{by_agent}");
    let agent_sessions = search_session_ids(&by_agent);
    assert_eq!(
        agent_sessions,
        vec![format!("agent-{AGENT_MINE_ID}")],
        "{by_agent}"
    );

    // Markdown surface names the scoped run + agent, no leaked JSON object.
    let md = call_md(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "harvest",
            "provider": "claude",
            "catch_up": false,
            "workflow_run": RUN_ID,
            "workflow_agent": AGENT_MINE_LABEL,
        }),
    )
    .await;
    assert!(md.contains("workflow filter"), "{md}");
    assert!(md.contains(RUN_ID), "{md}");
    assert!(md.contains(AGENT_MINE_LABEL), "{md}");
    assert!(!md.contains("\"workflow_run\""), "{md}");

    drop(db);
    cg.close();
}

/// A workflow-scoped search against a store that predates the workflow-index
/// schema returns empty rather than erroring on a missing table.
#[tokio::test]
async fn message_search_workflow_scope_empty_without_workflow_tables() {
    let (_env, project_root) = common::IsolatedEnv::acquire().await;

    let cg = TraceDecay::init(&project_root)
        .await
        .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();

    let db_path = cg.store_layout().sessions_db_path.clone();
    let db = GlobalDb::open_at(&db_path)
        .await
        .unwrap_or_else(|| panic!("open sessions.db"));

    // Seed a plain message but NEVER create the workflow-index tables.
    assert!(db.upsert_session(&parent_session(&project_key)).await);
    assert!(
        db.upsert_session_message(&SessionMessageRecord {
            session_id: SESSION_ID.to_string(),
            message_id: "m1".to_string(),
            source_path: Some(format!("{SESSION_ID}.jsonl")),
            ..agent_message(project_root.as_path(), AGENT_MINE_ID, "m1", "harvest text")
        })
        .await
    );

    // Sanity: the same query matches without the workflow filter.
    let unscoped = call(
        &cg,
        "tracedecay_message_search",
        json!({ "query": "harvest", "provider": "claude", "catch_up": false }),
    )
    .await;
    assert!(unscoped["count"].as_i64().unwrap_or(0) >= 1, "{unscoped}");

    // With the workflow filter, a store lacking workflow_agents yields nothing.
    let scoped = call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "harvest",
            "provider": "claude",
            "catch_up": false,
            "workflow_run": RUN_ID,
        }),
    )
    .await;
    assert_eq!(scoped["workflow_filter_applied"], true, "{scoped}");
    assert_eq!(scoped["count"], 0, "{scoped}");

    drop(db);
    cg.close();
}

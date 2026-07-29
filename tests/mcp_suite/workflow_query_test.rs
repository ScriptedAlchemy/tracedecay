//! End-to-end tests for the workflow-run query surface: `tracedecay_workflows`
//! lists runs for a thread or git ref, shows one run, and drills into one agent.
//! Everything is driven through the real `handle_tool_call` dispatch against a
//! temp `~/.claude` fixture tree plus a seeded `sessions.db`, mirroring
//! `git_correlation_test.rs`.

use std::path::Path;

use serde_json::{Value, json};

use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::sessions::git_correlation::{
    DEFAULT_SPAN_MERGE_GAP_SECS, SpanObservation, SpanSource,
};
use tracedecay::tracedecay::TraceDecay;
use tracedecay_domain::ProjectId;

use crate::common;

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

fn extract_json(result: &tracedecay::mcp::ToolResult) -> Value {
    let text = result.value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result should carry text content: {}", result.value));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tool result should be JSON: {e}\n{text}"))
}

/// Renders a tool call as markdown (no `format:"json"` override) so tests can
/// assert on the summary-first markdown surface.
async fn call_md(
    cg: &TraceDecay,
    runtime: &HostAdmissionTestRuntimeV1,
    tool: &str,
    args: Value,
) -> String {
    let result = runtime
        .call_mcp_tool_for_test(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
    result.value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} result should carry text content: {}", result.value))
        .to_string()
}

async fn call(
    cg: &TraceDecay,
    runtime: &HostAdmissionTestRuntimeV1,
    tool: &str,
    mut args: Value,
) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    let result = runtime
        .call_mcp_tool_for_test(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
    extract_json(&result)
}

/// Ingests the on-disk fixture and drives the three `tracedecay_workflows`
/// modes plus the git-scope list end to end.
#[tokio::test]
async fn workflows_query_surface_end_to_end() {
    let _env_lock = crate::mcp_handler_test::GLOBAL_DB_ENV_LOCK.lock().await;
    let (env, project_root) = common::IsolatedEnv::acquire().await;
    let home = env.home().to_path_buf();

    let cg = TraceDecay::init(&project_root)
        .await
        .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();

    // The fixture's agent transcripts record cwd == the canonical project root
    // so the ingest sweep attributes the run to this project.
    write_workflow_fixture(&home, cg.project_root());

    // The isolated fixture checkout is not a git repository, so the repository
    // identity marker (which lives in the git common dir) never exists here.
    // The enrollment marker `init` wrote is this checkout's naming authority
    // and carries the same project id the store was opened under.
    let marker = tracedecay::storage::read_enrollment_marker(cg.project_root())
        .unwrap_or_else(|error| panic!("read project identity: {error}"))
        .unwrap_or_else(|| panic!("project enrollment marker"));
    let project_id =
        ProjectId::new(marker.project_id).unwrap_or_else(|error| panic!("project id: {error}"));
    let runtime = HostAdmissionTestRuntimeV1::project(
        env.home().join(".tracedecay"),
        cg.project_root(),
        project_id,
    )
    .await
    .unwrap_or_else(|error| panic!("registered session runtime: {error}"));

    let stats = runtime
        .ingest_workflows_for_test(cg.project_root())
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

    // Markdown for the run detail is summary-first (phases + agents headings,
    // no leaked JSON object).
    let run_md = call_md(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "run_id": RUN_ID }),
    )
    .await;
    assert!(run_md.contains("Workflow Run"), "{run_md}");
    assert!(run_md.contains("Phases"), "{run_md}");
    assert!(run_md.contains("Agents"), "{run_md}");
    assert!(!run_md.contains("\"result_summary\""), "{run_md}");

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
    let by_absent = call(
        &cg,
        &runtime,
        "tracedecay_workflows",
        json!({ "branch": "feat/absent" }),
    )
    .await;
    assert_eq!(by_absent["count"], 0, "{by_absent}");

    drop(runtime);
    cg.close();
}

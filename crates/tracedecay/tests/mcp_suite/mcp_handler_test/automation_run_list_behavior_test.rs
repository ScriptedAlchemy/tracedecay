//! What an agent receives from `tracedecay_automation_run_list`.
//!
//! Each case opens the production MCP server for one active project and sends
//! `tools/call`. The assertions are the text or JSON the client observes, not
//! the ledger reader the handler happens to call.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::mcp::McpServer;
use tracedecay_automation::backend::AgentTaskKind;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifact, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    append_run_record,
};

use crate::fixture;
use crate::mcp_server_test::run_client_connection_with_messages;
use crate::mcp_server_test::support::{jsonrpc_request, response_with_id};
use crate::support::{
    TestEnv, TestTraceDecay, close_test_graph, init_test_project, real_mcp_server,
};

const STARTED_AT: &str = "1782283199";

struct ServedProject {
    dashboard_root: PathBuf,
    server: Arc<McpServer>,
    _env: TestEnv,
}

async fn serve_project(root: &Path) -> ServedProject {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let (graph, env) = init_test_project(root).await;
    let dashboard_root = graph.store_layout().dashboard_root.clone();
    let server = real_mcp_server(graph).await;
    ServedProject {
        dashboard_root,
        server,
        _env: env,
    }
}

async fn list_call(server: &Arc<McpServer>, arguments: Value) -> Value {
    let responses = run_client_connection_with_messages(
        Arc::clone(server),
        vec![jsonrpc_request(
            json!(7),
            "tools/call",
            json!({
                "name": "tracedecay_automation_run_list",
                "arguments": arguments,
            }),
        )],
    )
    .await;
    response_with_id(&responses, json!(7))
}

fn tool_text(response: &Value) -> &str {
    assert!(
        response["error"].is_null(),
        "tools/call must succeed: {response}"
    );
    let content = response["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/call must return content: {response}"));
    assert_eq!(
        content.len(),
        1,
        "the list answer must be one text block, got {response}"
    );
    content[0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tools/call text missing: {response}"))
}

fn tool_json(response: &Value) -> Value {
    let text = tool_text(response);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("list JSON: {error}\n{text}"))
}

fn artifact(kind: &str) -> AutomationRunArtifact {
    AutomationRunArtifact {
        schema_version: 1,
        kind: kind.to_owned(),
        path: format!("hidden/{kind}.json"),
        sha256: format!("sha256:hidden-{kind}"),
        summary: Some(format!("{kind} payload must stay off the list")),
        created_at: "1782283400".to_owned(),
    }
}

fn record(
    run_id: &str,
    task: AgentTaskKind,
    task_key: &str,
    trigger: AutomationTrigger,
    status: AutomationRunStatus,
    completed_at: &str,
    model: &str,
    reviewed: usize,
    accepted: usize,
    rejected: usize,
    skipped: usize,
    error: Option<&str>,
    artifacts: Vec<AutomationRunArtifact>,
) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_owned(),
        trigger,
        task,
        task_key: Some(task_key.to_owned()),
        backend: "codex_app_server".to_owned(),
        backend_identity: None,
        host_mode: Some("standalone".to_owned()),
        prompt_version: Some(format!("{task_key}:v1")),
        response_schema: None,
        strict_json: Some(true),
        model: Some(model.to_owned()),
        status,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: reviewed,
        accepted_count: accepted,
        rejected_count: rejected,
        skipped_count: skipped,
        error: error.map(str::to_owned),
        error_classification: None,
        error_retryable: None,
        backend_attempt_count: 1,
        backend_attempts: Vec::new(),
        fallback_status: None,
        session_evidence_budget_stage: None,
        report_ref: None,
        artifacts,
        started_at: STARTED_AT.to_owned(),
        completed_at: completed_at.to_owned(),
        completed_at_micros: None,
    }
}

async fn append(dashboard_root: &Path, row: &AutomationRunLedgerRecord) {
    append_run_record(dashboard_root, row)
        .await
        .expect("active project ledger should accept the seeded run");
}

#[tokio::test]
async fn automation_run_list_reports_an_empty_active_ledger() {
    let dir = TempDir::new().unwrap();
    let served = serve_project(dir.path()).await;

    let markdown = list_call(&served.server, json!({})).await;
    assert_eq!(
        tool_text(&markdown),
        "\
## Automation Runs
**status:** ok
**count:** 0
**limit:** 50
**has_more:** false
**malformed_row_count:** 0
**completeness:** known

### Runs
_No automation runs recorded._
"
    );

    let json_page = list_call(&served.server, json!({"format": "json"})).await;
    assert_eq!(
        tool_json(&json_page),
        json!({
            "status": "ok",
            "scope": "active_project",
            "runs": [],
            "count": 0,
            "limit": 50,
            "has_more": false,
            "malformed_row_count": 0,
            "completeness": "known"
        })
    );

    let bounded = list_call(&served.server, json!({"format": "json", "limit": 201})).await;
    assert_eq!(
        tool_json(&bounded),
        json!({
            "status": "ok",
            "scope": "active_project",
            "runs": [],
            "count": 0,
            "limit": 200,
            "has_more": false,
            "malformed_row_count": 0,
            "completeness": "known"
        })
    );
}

#[tokio::test]
async fn automation_run_list_returns_the_newest_deduped_page() {
    let dir = TempDir::new().unwrap();
    let served = serve_project(dir.path()).await;
    let queued = record(
        "run-reflected",
        AgentTaskKind::SessionReflector,
        "session_reflector",
        AutomationTrigger::Scheduler,
        AutomationRunStatus::Queued,
        "1782283200",
        "queued-model",
        0,
        0,
        0,
        0,
        Some("queued snapshot must not be listed"),
        Vec::new(),
    );
    append(&served.dashboard_root, &queued).await;
    append(
        &served.dashboard_root,
        &record(
            "run-curated",
            AgentTaskKind::MemoryCurator,
            "memory_curator",
            AutomationTrigger::ManualCli,
            AutomationRunStatus::Succeeded,
            "1782283250",
            "curator-model",
            3,
            2,
            1,
            0,
            None,
            Vec::new(),
        ),
    )
    .await;
    append(
        &served.dashboard_root,
        &record(
            "run-reflected",
            AgentTaskKind::SessionReflector,
            "session_reflector",
            AutomationTrigger::Scheduler,
            AutomationRunStatus::Succeeded,
            "1782283400",
            "live-model",
            9,
            7,
            2,
            1,
            None,
            vec![artifact("traces"), artifact("codex_handoff")],
        ),
    )
    .await;
    append(&served.dashboard_root, &queued).await;

    let reflected = json!({
        "run_id": "run-reflected",
        "task": "session_reflector",
        "task_key": "session_reflector",
        "trigger": "scheduler",
        "backend": "codex_app_server",
        "model": "live-model",
        "status": "succeeded",
        "reviewed_count": 9,
        "accepted_count": 7,
        "rejected_count": 2,
        "skipped_count": 1,
        "error": null,
        "started_at": STARTED_AT,
        "completed_at": "1782283400",
        "artifact_kinds": ["traces", "codex_handoff"]
    });
    let curated = json!({
        "run_id": "run-curated",
        "task": "memory_curator",
        "task_key": "memory_curator",
        "trigger": "manual_cli",
        "backend": "codex_app_server",
        "model": "curator-model",
        "status": "succeeded",
        "reviewed_count": 3,
        "accepted_count": 2,
        "rejected_count": 1,
        "skipped_count": 0,
        "error": null,
        "started_at": STARTED_AT,
        "completed_at": "1782283250",
        "artifact_kinds": []
    });
    let page = json!({
        "status": "ok",
        "scope": "active_project",
        "runs": [reflected.clone(), curated],
        "count": 2,
        "limit": 10,
        "has_more": false,
        "malformed_row_count": 0,
        "completeness": "known"
    });

    let markdown = list_call(&served.server, json!({"limit": 10})).await;
    assert_eq!(
        tool_text(&markdown),
        "\
## Automation Runs
**status:** ok
**count:** 2
**limit:** 10
**has_more:** false
**malformed_row_count:** 0
**completeness:** known

### Runs
- **run-reflected** - task: session_reflector; status: succeeded; completed_at: 1782283400
- **run-curated** - task: memory_curator; status: succeeded; completed_at: 1782283250
"
    );

    let first = list_call(&served.server, json!({"format": "json", "limit": 10})).await;
    let second = list_call(&served.server, json!({"format": "json", "limit": 10})).await;
    assert_eq!(tool_json(&first), page);
    assert_eq!(tool_json(&second), page);

    let partial = list_call(&served.server, json!({"format": "json", "limit": 1})).await;
    assert_eq!(
        tool_json(&partial),
        json!({
            "status": "ok",
            "scope": "active_project",
            "runs": [reflected],
            "count": 1,
            "limit": 1,
            "has_more": true,
            "malformed_row_count": 0,
            "completeness": "partial"
        })
    );
}

#[tokio::test]
async fn automation_run_list_reports_malformed_rows_as_partial() {
    let dir = TempDir::new().unwrap();
    let served = serve_project(dir.path()).await;
    append(
        &served.dashboard_root,
        &record(
            "run-kept-older",
            AgentTaskKind::MemoryCurator,
            "memory_curator",
            AutomationTrigger::Dashboard,
            AutomationRunStatus::Failed,
            "1782283200",
            "older-model",
            1,
            0,
            1,
            0,
            Some("curator backend failed"),
            Vec::new(),
        ),
    )
    .await;
    append(
        &served.dashboard_root,
        &record(
            "run-kept-newer",
            AgentTaskKind::SkillWriter,
            "skill_writer",
            AutomationTrigger::ManualMcp,
            AutomationRunStatus::Skipped,
            "1782283300",
            "newer-model",
            4,
            0,
            0,
            4,
            Some("skill writer skipped"),
            Vec::new(),
        ),
    )
    .await;
    let ledger = served.dashboard_root.join("automation_runs.jsonl");
    let mut file = OpenOptions::new()
        .append(true)
        .open(&ledger)
        .expect("seeded ledger");
    writeln!(file, "not json").unwrap();

    let newer = json!({
        "run_id": "run-kept-newer",
        "task": "skill_writer",
        "task_key": "skill_writer",
        "trigger": "manual_mcp",
        "backend": "codex_app_server",
        "model": "newer-model",
        "status": "skipped",
        "reviewed_count": 4,
        "accepted_count": 0,
        "rejected_count": 0,
        "skipped_count": 4,
        "error": "skill writer skipped",
        "started_at": STARTED_AT,
        "completed_at": "1782283300",
        "artifact_kinds": []
    });
    let older = json!({
        "run_id": "run-kept-older",
        "task": "memory_curator",
        "task_key": "memory_curator",
        "trigger": "dashboard",
        "backend": "codex_app_server",
        "model": "older-model",
        "status": "failed",
        "reviewed_count": 1,
        "accepted_count": 0,
        "rejected_count": 1,
        "skipped_count": 0,
        "error": "curator backend failed",
        "started_at": STARTED_AT,
        "completed_at": "1782283200",
        "artifact_kinds": []
    });

    let full = list_call(&served.server, json!({"format": "json", "limit": 10})).await;
    assert_eq!(
        tool_json(&full),
        json!({
            "status": "ok",
            "scope": "active_project",
            "runs": [newer.clone(), older],
            "count": 2,
            "limit": 10,
            "has_more": false,
            "malformed_row_count": 1,
            "completeness": "partial"
        })
    );

    let head = list_call(&served.server, json!({"format": "json", "limit": 1})).await;
    assert_eq!(
        tool_json(&head),
        json!({
            "status": "ok",
            "scope": "active_project",
            "runs": [newer],
            "count": 1,
            "limit": 1,
            "has_more": true,
            "malformed_row_count": 1,
            "completeness": "partial"
        })
    );
}

#[tokio::test]
async fn automation_run_list_reads_only_the_active_project_ledger() {
    let dir = TempDir::new().unwrap();
    let served = serve_project(&dir.path().join("active")).await;

    let foreign_root = dir.path().join("foreign");
    fs::create_dir_all(foreign_root.join("src")).unwrap();
    fs::write(foreign_root.join("src/lib.rs"), "pub fn foreign() {}\n").unwrap();
    let foreign = TestTraceDecay::new(
        fixture::init_project_from_template(&foreign_root)
            .await
            .expect("foreign project"),
    );
    append(
        &foreign.store_layout().dashboard_root,
        &record(
            "run-foreign-only",
            AgentTaskKind::MemoryCurator,
            "memory_curator",
            AutomationTrigger::Scheduler,
            AutomationRunStatus::Succeeded,
            "1782283500",
            "foreign-model",
            99,
            99,
            0,
            0,
            None,
            Vec::new(),
        ),
    )
    .await;
    close_test_graph(foreign).await;

    append(
        &served.dashboard_root,
        &record(
            "run-active-only",
            AgentTaskKind::SessionReflector,
            "session_reflector",
            AutomationTrigger::Scheduler,
            AutomationRunStatus::Succeeded,
            "1782283100",
            "active-model",
            5,
            4,
            1,
            0,
            None,
            Vec::new(),
        ),
    )
    .await;

    let listed = list_call(&served.server, json!({"format": "json"})).await;
    assert_eq!(
        tool_json(&listed),
        json!({
            "status": "ok",
            "scope": "active_project",
            "runs": [{
                "run_id": "run-active-only",
                "task": "session_reflector",
                "task_key": "session_reflector",
                "trigger": "scheduler",
                "backend": "codex_app_server",
                "model": "active-model",
                "status": "succeeded",
                "reviewed_count": 5,
                "accepted_count": 4,
                "rejected_count": 1,
                "skipped_count": 0,
                "error": null,
                "started_at": STARTED_AT,
                "completed_at": "1782283100",
                "artifact_kinds": []
            }],
            "count": 1,
            "limit": 50,
            "has_more": false,
            "malformed_row_count": 0,
            "completeness": "known"
        })
    );
}

#[tokio::test]
async fn automation_run_list_refuses_a_non_directory_dashboard_root() {
    let dir = TempDir::new().unwrap();
    let served = serve_project(dir.path()).await;
    if served.dashboard_root.is_dir() {
        fs::remove_dir_all(&served.dashboard_root).unwrap();
    }
    if let Some(parent) = served.dashboard_root.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&served.dashboard_root, "not a dashboard directory\n").unwrap();

    let response = list_call(&served.server, json!({"format": "json"})).await;
    assert!(response["result"].is_null(), "{response}");
    assert_eq!(
        response["error"],
        json!({
            "code": -32603,
            "message": "tool project route failed: reason_code=automation_run_ledger_unavailable retryable=true: automation run ledger is unavailable during list: config error: automation dashboard root is not a directory",
            "data": {
                "tool": "tracedecay_automation_run_list",
                "reason_code": "automation_run_ledger_unavailable",
                "retryable": true,
                "detail": "automation run ledger is unavailable during list: config error: automation dashboard root is not a directory"
            }
        })
    );
}

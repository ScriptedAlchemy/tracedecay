//! A store left in the published workflow shape, through the production
//! composition and its MCP server: code reads serve while the background
//! upgrade is parked and while a held write lock fails it, workflow tools answer
//! the typed retryable problem throughout, and the store's own convergence task
//! finishes the move once the obstruction clears, with no reopen or restart.
//!
//! The test gate parks each attempt rather than timing around it, and the store
//! is held with a real `SQLite` write lock.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_mcp::{JsonRpcRequest, JsonRpcResponse};
use tracedecay_store::StoreShardIdV1;
use tracedecay_store_runtime::{
    DaemonSessionRuntimeRegistryV1, RegisteredSchemaConvergenceStatus,
    RegisteredSchemaConvergenceTestGate,
};

use super::project_server_capacity_journey_test::{
    open_project, seed_project_sessions_pending_convergence,
};
use crate::daemon::production_harness::ProductionProjectCompositionHarnessV1;
use crate::daemon::project_composition::ProductionProjectComposition;
use crate::daemon::store_runtime_tests::{
    PUBLISHED_WORKFLOW_DIGEST, age_session_store_to_published_workflow_shape,
    published_workflow_state,
};
use crate::test_support::git::git;

const PROBE: &str = "published_workflow_recovery_probe";
const STATE_DEADLINE: Duration = Duration::from_secs(90);

fn committed_project(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("project source root");
    std::fs::write(
        root.join("src/lib.rs"),
        format!("pub fn {PROBE}() -> usize {{ 1 }}\n"),
    )
    .expect("project source");
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["config", "user.name", "TraceDecay Test"]);
    git(
        root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    git(root, &["commit", "-qm", "seed project"]);
}

async fn call(
    composition: &ProductionProjectComposition,
    tool: &str,
    arguments: Value,
) -> JsonRpcResponse {
    let request: JsonRpcRequest = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    }))
    .expect("tool request");
    composition
        .server
        .handle_request(&request)
        .await
        .expect("tool response")
}

/// Ordinary code retrieval finds the project's symbol whatever the upgrade's
/// state.
async fn assert_code_reads_serve(composition: &ProductionProjectComposition, state: &str) {
    let response = call(
        composition,
        "tracedecay_search",
        json!({"query": PROBE, "format": "json"}),
    )
    .await;
    assert!(
        response.error.is_none(),
        "code search must serve while {state}: {response:?}"
    );
    let answer = serde_json::to_string(&response.result).expect("search result text");
    assert!(
        answer.contains(PROBE),
        "code search must find the symbol while {state}: {answer}"
    );
}

/// A workflow tool's typed refusal, as an MCP client reads it.
async fn assert_workflow_tools_recovering(composition: &ProductionProjectComposition, state: &str) {
    let response = call(
        composition,
        "tracedecay_workflow_list_definitions",
        json!({"format": "json"}),
    )
    .await;
    let wire = serde_json::to_value(&response).expect("tool response wire");
    let problem = wire
        .pointer("/result/structuredContent/problem")
        .unwrap_or_else(|| panic!("no typed problem while {state}: {wire}"));
    assert_eq!(problem["kind"], "unavailable", "{state}: {problem}");
    assert_eq!(
        problem["code"], "workflow.authority_recovering",
        "{state}: {problem}"
    );
    assert_eq!(problem["retry"], "after_delay", "{state}: {problem}");
}

/// Releases every other shard's convergence past the gate until this store's
/// first attempt is the one parked there. One permit admits one shard at a time.
async fn park_first_attempt(
    registry: &DaemonSessionRuntimeRegistryV1,
    gate: &RegisteredSchemaConvergenceTestGate,
    shard: &StoreShardIdV1,
) {
    tokio::time::timeout(STATE_DEADLINE, async {
        let mut released = 0;
        loop {
            if registry.registered_schema_convergence_status(shard)
                == Some(RegisteredSchemaConvergenceStatus::Running)
            {
                return;
            }
            if registry.registered_schema_convergence_execution_count_for_test() > released {
                gate.release();
                released += 1;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the store's first attempt must reach the gate");
}

async fn wait_for_status(
    registry: &DaemonSessionRuntimeRegistryV1,
    shard: &StoreShardIdV1,
    accepted: impl Fn(&RegisteredSchemaConvergenceStatus) -> bool,
) -> RegisteredSchemaConvergenceStatus {
    tokio::time::timeout(STATE_DEADLINE, async {
        loop {
            if let Some(status) = registry.registered_schema_convergence_status(shard)
                && accepted(&status)
            {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the store's upgrade must reach the expected state")
}

/// What the status tool tells an MCP client about a store between attempts.
async fn assert_status_reports_retrying(composition: &ProductionProjectComposition) {
    let response = call(composition, "tracedecay_status", json!({"format": "json"})).await;
    let wire = serde_json::to_value(&response).expect("tool response wire");
    let status: Value = serde_json::from_str(
        wire.pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("status has no JSON text: {wire}")),
    )
    .expect("status JSON");
    let convergence = &status["schema_convergence"];
    assert_eq!(convergence["status"], "retrying", "{convergence}");
    let retrying = convergence["findings"]
        .as_array()
        .expect("convergence findings")
        .iter()
        .find(|finding| finding["state"] == "retrying")
        .unwrap_or_else(|| panic!("no retrying finding: {convergence}"));
    assert!(
        retrying["degraded_row"].is_string(),
        "the attempt's failure stays visible: {retrying}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn code_reads_serve_while_a_transient_lock_blocks_the_workflow_upgrade() {
    let isolation = TempDir::new().expect("production harness isolation");
    let bootstrap_project = isolation.path().join("bootstrap-project");
    let target_project = isolation.path().join("target-project");
    committed_project(&bootstrap_project);
    committed_project(&target_project);
    let harness = ProductionProjectCompositionHarnessV1::open_with_session_maintenance_for_test(
        isolation.path(),
        std::iter::once(bootstrap_project),
    )
    .await
    .expect("production harness authority");

    let project_id = tracedecay_domain::ProjectId::new("project.published-workflow-journey")
        .expect("typed project identity");
    seed_project_sessions_pending_convergence(harness.profile_root(), &target_project, &project_id)
        .await;
    let sessions_path = tracedecay_runtime_core::storage::profile_sharded_data_root(
        harness.profile_root(),
        project_id.as_str(),
    )
    .join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
    age_session_store_to_published_workflow_shape(&sessions_path);
    let published = published_workflow_state(&sessions_path);

    let identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(harness.profile_root())
            .expect("durable harness profile identity");
    let shard = StoreShardIdV1::project_sessions(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id,
    );
    let registry = harness
        .resources
        .as_ref()
        .expect("production harness resources")
        .store_administration
        .session_runtime_registry()
        .await
        .expect("session runtime registry");
    let gate = registry.block_registered_schema_convergence_for_test();

    let (composition, serving) = open_project(&harness, &target_project, "recovery", PROBE)
        .await
        .expect("a published store must not keep the project from opening");
    park_first_attempt(&registry, &gate, &shard).await;

    assert_code_reads_serve(&composition, "the upgrade is parked").await;
    assert_workflow_tools_recovering(&composition, "the upgrade is parked").await;
    assert_eq!(published_workflow_state(&sessions_path), published);

    let peer = rusqlite::Connection::open(&sessions_path).expect("open the session store");
    peer.execute_batch("BEGIN IMMEDIATE")
        .expect("hold the write lock");
    gate.release();
    wait_for_status(&registry, &shard, |status| {
        matches!(status, RegisteredSchemaConvergenceStatus::Retrying { .. })
    })
    .await;
    assert_status_reports_retrying(&composition).await;

    assert_code_reads_serve(&composition, "a held lock fails the upgrade").await;
    assert_workflow_tools_recovering(&composition, "a held lock fails the upgrade").await;
    assert!(
        registry.reset_required_stores().is_empty(),
        "a transient failure is not a reset"
    );

    peer.execute_batch("ROLLBACK").expect("drop the write lock");
    drop(peer);
    assert_eq!(published_workflow_state(&sessions_path), published);
    gate.release();
    wait_for_status(&registry, &shard, |status| {
        *status == RegisteredSchemaConvergenceStatus::Complete
    })
    .await;

    let response = call(
        &composition,
        "tracedecay_workflow_list_definitions",
        json!({"format": "json"}),
    )
    .await;
    let wire = serde_json::to_value(&response).expect("tool response wire");
    assert!(
        wire.pointer("/result/structuredContent/problem").is_none() && response.error.is_none(),
        "workflow tools serve after the retry commits, over the same mount: {wire}"
    );
    assert_code_reads_serve(&composition, "the upgrade has committed").await;
    let (digest, rows) = published_workflow_state(&sessions_path);
    assert_ne!(digest, PUBLISHED_WORKFLOW_DIGEST);
    assert_eq!(rows, published.1);

    drop((composition, serving));
    harness.shutdown().await;
}

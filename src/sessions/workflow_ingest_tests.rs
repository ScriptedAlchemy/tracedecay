use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tracedecay_domain::ProjectId;
use tracedecay_global_db::RegisteredGlobalDb;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::sessions::workflow_index::{
    INGEST_WATERMARK_KEY, RegisteredWorkflowIndexSnapshot, WorkflowStatus, read_ingest_watermark,
};
use crate::sessions::workflow_ingest::WorkflowIngestStats;
use crate::store::GlobalDbWorkflowStore;

static WORKFLOW_TEST_NONCE: AtomicU64 = AtomicU64::new(1);

struct WorkflowTestStore {
    database: Arc<RegisteredGlobalDb>,
    project_id: ProjectId,
    _registry: DaemonSessionRuntimeRegistryV1,
    _scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _profile: tempfile::TempDir,
}

impl WorkflowTestStore {
    fn workflow_store(&self) -> GlobalDbWorkflowStore<&RegisteredGlobalDb> {
        GlobalDbWorkflowStore::new(self.database.as_ref())
    }

    async fn index_reader(&self) -> RegisteredWorkflowIndexSnapshot {
        self.workflow_store()
            .open_workflow_index_snapshot()
            .await
            .unwrap()
    }
}

async fn workflow_test_store(project_root: &Path) -> WorkflowTestStore {
    let profile = tempfile::tempdir().unwrap();
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile.path().join("profile")).unwrap();
    let nonce = WORKFLOW_TEST_NONCE.fetch_add(1, Ordering::Relaxed);
    let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        identity.profile_root(),
        nonce,
        &format!("workflow-ingest-test-{nonce}"),
    )
    .unwrap();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .unwrap();
    let project_id = ProjectId::new(format!("project.workflow-ingest-{nonce}")).unwrap();
    tracedecay_runtime_core::storage::write_enrollment_marker(
        project_root,
        &tracedecay_runtime_core::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let database = registry
        .project_sessions(project_id.clone(), [project_root.to_path_buf()])
        .await
        .unwrap();
    WorkflowTestStore {
        database,
        project_id,
        _registry: registry,
        _scope: scope,
        _profile: profile,
    }
}

fn sample_meta() -> serde_json::Value {
    serde_json::json!({
        "runId": "wf_d0bf6fa4-48f",
        "workflowName": "tracedecay-triggering-evals",
        "summary": "Mine real transcripts into a broad eval corpus",
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
                "label": "mine:claude-transcripts",
                "phaseTitle": "Mine",
                "phaseIndex": 1,
                "agentId": "a17141dbe5a308242",
                "model": "claude-fable-5",
                "state": "done",
                "startedAt": 1_783_142_254_936_i64,
                "lastProgressAt": 1_783_142_255_936_i64
            },
            {
                "type": "workflow_agent",
                "label": "",
                "phaseTitle": "Run",
                "agentId": "aa09ec4d07fccc915",
                "state": "in_progress",
                "startedAt": 1_783_142_260_000_i64
            }
        ]
    })
}

fn write_fixture(home: &Path, session_id: &str, project_cwd: &Path) -> PathBuf {
    let projects = home.join(".claude").join("projects");
    let slug = projects.join("dummy-slug");
    let session_dir = slug.join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        slug.join(format!("{session_id}.jsonl")),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "user",
                "cwd": project_cwd.to_string_lossy(),
                "sessionId": session_id,
                "timestamp": "2026-07-04T05:00:00.000Z",
            })
        ),
    )
    .unwrap();

    let workflows = session_dir.join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("wf_meta.json"),
        serde_json::to_string(&sample_meta()).unwrap(),
    )
    .unwrap();
    let run_dir = session_dir
        .join("subagents")
        .join("workflows")
        .join("wf_meta");
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("agent-a17141dbe5a308242.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "sessionId": "agent-sess",
                "timestamp": "2026-07-04T05:17:34.967Z",
                "message": {"usage": {"input_tokens": 100, "output_tokens": 40}},
            })
        ),
    )
    .unwrap();

    let orphan = session_dir
        .join("subagents")
        .join("workflows")
        .join("wf_orphan");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("agent-b1.jsonl"), "\n").unwrap();
    std::fs::write(
        orphan.join("journal.jsonl"),
        format!("{}\n", r#"{"type":"started","agentId":"b1"}"#),
    )
    .unwrap();
    projects
}

#[tokio::test]
async fn sweep_ingests_runs_scoped_to_project_and_is_incremental() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let projects = write_fixture(home.path(), "sess-1", &project_root);
    let store = workflow_test_store(&project_root).await;

    let stats = store
        .workflow_store()
        .ingest_workflow_runs_from(&store.project_id, &project_root, &projects)
        .await;
    assert_eq!(stats.runs_ingested, 2);
    assert_eq!(stats.agents_ingested, 3);

    let reader = store.index_reader().await;
    let runs = reader.runs_for_session("sess-1", 10).await.unwrap();
    let ids: Vec<&str> = runs.iter().map(|run| run.run_id.as_str()).collect();
    assert!(ids.contains(&"wf_d0bf6fa4-48f"));
    assert!(ids.contains(&"wf_orphan"));
    let meta_run = reader.run_for_id("wf_d0bf6fa4-48f").await.unwrap().unwrap();
    assert_eq!(meta_run.parent_session_id, "sess-1");
    assert_eq!(meta_run.status, WorkflowStatus::Completed);
    let agents = reader.agents_for_run("wf_d0bf6fa4-48f", 10).await.unwrap();
    assert_eq!(agents.len(), 2);
    let enriched = agents
        .iter()
        .find(|agent| agent.agent_id == "a17141dbe5a308242")
        .unwrap();
    assert_eq!(enriched.tokens, 140);
    assert!(
        enriched
            .transcript_path
            .as_deref()
            .unwrap()
            .ends_with("agent-a17141dbe5a308242.jsonl")
    );
    assert_eq!(
        reader
            .run_for_id("wf_orphan")
            .await
            .unwrap()
            .unwrap()
            .status,
        WorkflowStatus::Running
    );

    let again = store
        .workflow_store()
        .ingest_workflow_runs_from(&store.project_id, &project_root, &projects)
        .await;
    assert_eq!(again, WorkflowIngestStats::default());
}

#[tokio::test]
async fn sweep_skips_runs_owned_by_a_different_project() {
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let projects = write_fixture(home.path(), "sess-x", other.path());
    let target = tempfile::tempdir().unwrap();
    let target_root = target.path().canonicalize().unwrap();
    let store = workflow_test_store(&target_root).await;

    let stats = store
        .workflow_store()
        .ingest_workflow_runs_from(&store.project_id, &target_root, &projects)
        .await;
    assert_eq!(stats, WorkflowIngestStats::default());
    assert!(
        store
            .index_reader()
            .await
            .runs_for_session("sess-x", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

fn set_mtime(path: &Path, unix_secs: u64) {
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_unix_time(i64::try_from(unix_secs).unwrap(), 0),
    )
    .unwrap();
}

#[tokio::test]
async fn other_project_run_does_not_advance_watermark() {
    const FUTURE: u64 = 4_102_444_800;

    let home = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let target_root = target.path().canonicalize().unwrap();
    let projects = write_fixture(home.path(), "sess-target", &target_root);
    let other = tempfile::tempdir().unwrap();
    write_fixture(home.path(), "sess-other", other.path());
    set_mtime(
        &projects
            .join("dummy-slug")
            .join("sess-other")
            .join("workflows")
            .join("wf_meta.json"),
        FUTURE,
    );

    let store = workflow_test_store(&target_root).await;
    let stats = store
        .workflow_store()
        .ingest_workflow_runs_from(&store.project_id, &target_root, &projects)
        .await;
    assert_eq!(stats.runs_ingested, 2);
    assert!(
        store
            .index_reader()
            .await
            .runs_for_session("sess-other", 10)
            .await
            .unwrap()
            .is_empty()
    );

    let snapshot = store.database.read_snapshot().await.unwrap();
    let watermark = read_ingest_watermark(&snapshot, INGEST_WATERMARK_KEY).await;
    assert!(
        watermark > 0 && watermark < i64::try_from(FUTURE).unwrap(),
        "out-of-project run advanced the watermark to {watermark} (>= {FUTURE})"
    );

    let orphan_dir = target_root_orphan_dir(&projects, "sess-target");
    std::fs::write(orphan_dir.join("agent-b2.jsonl"), "\n").unwrap();
    set_mtime(
        &orphan_dir.join("agent-b2.jsonl"),
        u64::try_from(watermark).unwrap() + 60,
    );
    set_mtime(&orphan_dir, u64::try_from(watermark).unwrap() + 60);
    let again = store
        .workflow_store()
        .ingest_workflow_runs_from(&store.project_id, &target_root, &projects)
        .await;
    assert_eq!(
        again.runs_ingested, 1,
        "the still-Running target run must be re-ingested, not stranded"
    );
}

fn target_root_orphan_dir(projects: &Path, session_id: &str) -> PathBuf {
    projects
        .join("dummy-slug")
        .join(session_id)
        .join("subagents")
        .join("workflows")
        .join("wf_orphan")
}

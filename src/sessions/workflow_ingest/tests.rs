use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::workflow_index::{
    INGEST_WATERMARK_KEY, RegisteredWorkflowIndexSnapshot, read_ingest_watermark,
};

static WORKFLOW_TEST_NONCE: AtomicU64 = AtomicU64::new(1);

struct WorkflowTestStore {
    database: Arc<RegisteredGlobalDb>,
    project_id: ProjectId,
    _registry: DaemonSessionRuntimeRegistryV1,
    _scope: crate::db::DaemonDatabaseScope,
    _profile: tempfile::TempDir,
}

async fn workflow_test_store(project_root: &Path) -> WorkflowTestStore {
    let profile = tempfile::tempdir().unwrap();
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile.path().join("profile")).unwrap();
    let nonce = WORKFLOW_TEST_NONCE.fetch_add(1, Ordering::Relaxed);
    let scope = crate::db::enter_daemon_database_scope(
        identity.profile_root(),
        nonce,
        &format!("workflow-ingest-test-{nonce}"),
    )
    .unwrap();
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .unwrap();
    let project_id = ProjectId::new(format!("project.workflow-ingest-{nonce}")).unwrap();
    crate::storage::write_enrollment_marker(
        project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
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

fn sample_meta() -> Value {
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

#[test]
fn parse_run_from_meta_maps_fields_and_folds_status() {
    let (run, agents) = parse_run_from_meta("wf_fallback", "sess-parent", &sample_meta());

    assert_eq!(run.run_id, "wf_d0bf6fa4-48f"); // runId wins over the dir name
    assert_eq!(run.parent_session_id, "sess-parent");
    assert_eq!(run.name.as_deref(), Some("tracedecay-triggering-evals"));
    assert_eq!(run.status, WorkflowStatus::Completed);
    // startTime ms -> secs.
    assert_eq!(run.started_ts, Some(1_783_142_254));
    // started + durationMs/1000.
    assert_eq!(run.ended_ts, Some(1_783_142_254 + 983));
    // agentCount from meta, not roster length.
    assert_eq!(run.agent_count, 2);
    // `summary` present -> used verbatim (one-lined).
    assert_eq!(
        run.result_summary.as_deref(),
        Some("Mine real transcripts into a broad eval corpus")
    );

    // phase_json round-trips as a JSON array of the phases.
    let phases: Value = serde_json::from_str(run.phase_json.as_deref().unwrap()).unwrap();
    assert!(phases.is_array());
    assert_eq!(phases.as_array().unwrap().len(), 2);
    assert_eq!(phases[0]["title"], "Mine");

    // Only the two workflow_agent rows, in order; workflow_phase is dropped.
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].agent_label, "mine:claude-transcripts");
    assert_eq!(agents[0].phase.as_deref(), Some("Mine"));
    assert_eq!(agents[0].status, WorkflowStatus::Completed);
    assert_eq!(agents[0].model.as_deref(), Some("claude-fable-5"));
    assert_eq!(agents[0].started_ts, Some(1_783_142_254));
    assert_eq!(agents[0].ended_ts, Some(1_783_142_255));
    // Empty label falls back to the agent id; missing model backfills from
    // defaultModel; state folds `in_progress` -> Running.
    assert_eq!(agents[1].agent_label, "aa09ec4d07fccc915");
    assert_eq!(agents[1].model.as_deref(), Some("claude-fable-5"));
    assert_eq!(agents[1].status, WorkflowStatus::Running);
}

#[test]
fn result_summary_truncates_json_result_when_no_summary() {
    let mut meta = sample_meta();
    meta.as_object_mut().unwrap().remove("summary");
    let long = "x ".repeat(2000);
    meta.as_object_mut()
        .unwrap()
        .insert("result".to_string(), Value::String(long));
    let summary = run_result_summary(&meta).unwrap();
    // Single-char ellipsis convention: at most `CAP` content chars + `…`.
    assert!(summary.chars().count() <= RESULT_SUMMARY_CAP + 1);
    assert!(summary.ends_with('…'));
    // Whitespace collapsed to single spaces.
    assert!(!summary.contains("  "));
}

#[test]
fn result_summary_prefers_summary_over_result() {
    let meta = sample_meta();
    // Even though `result` is a dict, the `summary` string wins.
    assert_eq!(
        run_result_summary(&meta).as_deref(),
        Some("Mine real transcripts into a broad eval corpus")
    );
}

#[test]
fn roster_extracts_only_workflow_agents() {
    let meta = sample_meta();
    let roster = parse_roster("wf_x", &meta, Some("fallback-model"));
    assert_eq!(roster.len(), 2);
    assert!(roster.iter().all(|agent| agent.run_id == "wf_x"));
    let labels: Vec<&str> = roster.iter().map(|a| a.agent_label.as_str()).collect();
    assert_eq!(labels, vec!["mine:claude-transcripts", "aa09ec4d07fccc915"]);
}

#[test]
fn transcript_tokens_sum_input_and_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-test.jsonl");
    let body = concat!(
        r#"{"type":"user","sessionId":"agent-sess","timestamp":"2026-07-04T05:17:34.967Z","message":{"role":"user","content":"hi"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-04T05:18:00.000Z","message":{"role":"assistant","usage":{"input_tokens":100,"output_tokens":40}}}"#,
        "\n",
        "   \n",
        r#"not json"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-04T05:25:32.232Z","message":{"role":"assistant","usage":{"input_tokens":10,"output_tokens":8,"cache_read_input_tokens":999}}}"#,
        "\n",
    );
    std::fs::write(&path, body).unwrap();
    let summary = summarize_transcript_file(&path);
    // 100+40 + 10+8 (cache_* excluded).
    assert_eq!(summary.tokens, 158);
    assert_eq!(summary.session_id.as_deref(), Some("agent-sess"));
    assert_eq!(
        summary.first_ts,
        parse_timestamp("2026-07-04T05:17:34.967Z").map(|s| s as i64)
    );
    assert_eq!(
        summary.last_ts,
        parse_timestamp("2026-07-04T05:25:32.232Z").map(|s| s as i64)
    );
}

#[test]
fn dir_only_run_is_running_with_journal_roster() {
    let dir = tempfile::tempdir().unwrap();
    let journal = concat!(
        r#"{"type":"started","agentId":"a1"}"#,
        "\n",
        r#"{"type":"started","agentId":"a2"}"#,
        "\n",
        r#"{"type":"result","agentId":"a1"}"#,
        "\n",
        r#"{"type":"started","agentId":""}"#,
        "\n",
    );
    std::fs::write(dir.path().join("journal.jsonl"), journal).unwrap();
    let events = read_journal(dir.path());
    // Empty agentId dropped; three valid events remain.
    assert_eq!(events.len(), 3);
    // a1 has a terminal result -> Completed; a2 only started -> Running.
    assert_eq!(
        journal_agent_status(&events, "a1"),
        WorkflowStatus::Completed
    );
    assert_eq!(journal_agent_status(&events, "a2"), WorkflowStatus::Running);
    assert_eq!(
        journal_agent_status(&events, "absent"),
        WorkflowStatus::Unknown
    );
}

#[test]
fn dir_only_run_from_disk_yields_running_and_roster() {
    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path();
    // Two agent transcripts + a journal naming a3 that has no file yet.
    std::fs::write(
        agents_dir.join("agent-a1.jsonl"),
        format!(
            "{}\n",
            r#"{"sessionId":"s","timestamp":"2026-07-04T05:00:00.000Z","message":{"usage":{"input_tokens":5,"output_tokens":5}}}"#
        ),
    )
    .unwrap();
    std::fs::write(agents_dir.join("agent-a1.meta.json"), "{}").unwrap();
    std::fs::write(agents_dir.join("agent-a2.jsonl"), "\n").unwrap();
    std::fs::write(
        agents_dir.join("journal.jsonl"),
        concat!(
            r#"{"type":"started","agentId":"a1"}"#,
            "\n",
            r#"{"type":"started","agentId":"a3"}"#,
            "\n"
        ),
    )
    .unwrap();

    let (run, mut agents) = parse_run_from_dir("wf_dir", "sess", agents_dir);
    assert_eq!(run.status, WorkflowStatus::Running);
    assert_eq!(run.run_id, "wf_dir");
    // a1, a2 (from files) then a3 (journal-only).
    let mut ids: Vec<String> = agents.iter().map(|a| a.agent_id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["a1", "a2", "a3"]);
    assert_eq!(run.agent_count, 3);

    // Enrichment attaches the transcript path + tokens for a1.
    for agent in &mut agents {
        enrich_agent_from_transcript(agent, agents_dir);
    }
    let a1 = agents.iter().find(|a| a.agent_id == "a1").unwrap();
    assert_eq!(a1.tokens, 10);
    assert!(
        a1.transcript_path
            .as_deref()
            .unwrap()
            .ends_with("agent-a1.jsonl")
    );
    assert_eq!(a1.agent_session_id.as_deref(), Some("s"));
    // a3 has no file: no transcript path, zero tokens.
    let a3 = agents.iter().find(|a| a.agent_id == "a3").unwrap();
    assert!(a3.transcript_path.is_none());
    assert_eq!(a3.tokens, 0);
}

/// Write a `<slug>/<session_id>/` fixture with a parent transcript whose
/// `cwd` is `project_cwd`, one meta-backed run, and (optionally) one
/// dir-only run. Returns the `~/.claude/projects` root.
fn write_fixture(home: &Path, session_id: &str, project_cwd: &Path) -> PathBuf {
    let projects = home.join(".claude").join("projects");
    let slug = projects.join("dummy-slug");
    let session_dir = slug.join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();

    // Parent transcript records the owning session's cwd.
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

    // Meta-backed run + one agent transcript.
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

    // Dir-only (in-progress) run: no workflows/<id>.json.
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
    // `project_root` doubles as the recorded transcript cwd, so path-equality
    // scoping admits the run without needing a real git worktree.
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let projects = write_fixture(home.path(), "sess-1", &project_root);

    let store = workflow_test_store(&project_root).await;

    let stats = ingest_workflow_runs_from(
        store.database.as_ref(),
        &store.project_id,
        &project_root,
        &projects,
    )
    .await;
    // Both the meta run and the dir-only run land.
    assert_eq!(stats.runs_ingested, 2);
    // Meta run: 2 agents; orphan run: 1 agent.
    assert_eq!(stats.agents_ingested, 3);

    // The meta run is owned by sess-1 and reads as completed with its roster.
    let reader = RegisteredWorkflowIndexSnapshot::new(store.database.as_ref())
        .await
        .unwrap();
    let runs = reader.runs_for_session("sess-1", 10).await.unwrap();
    let ids: Vec<&str> = runs.iter().map(|r| r.run_id.as_str()).collect();
    assert!(ids.contains(&"wf_d0bf6fa4-48f")); // runId from meta, not dir name
    assert!(ids.contains(&"wf_orphan"));

    let meta_run = reader.run_for_id("wf_d0bf6fa4-48f").await.unwrap().unwrap();
    assert_eq!(meta_run.parent_session_id, "sess-1");
    assert_eq!(meta_run.status, WorkflowStatus::Completed);
    let agents = reader.agents_for_run("wf_d0bf6fa4-48f", 10).await.unwrap();
    assert_eq!(agents.len(), 2);
    // The first agent's transcript enriched tokens (100+40) and its path.
    let enriched = agents
        .iter()
        .find(|a| a.agent_id == "a17141dbe5a308242")
        .unwrap();
    assert_eq!(enriched.tokens, 140);
    assert!(
        enriched
            .transcript_path
            .as_deref()
            .unwrap()
            .ends_with("agent-a17141dbe5a308242.jsonl")
    );

    let orphan = reader.run_for_id("wf_orphan").await.unwrap().unwrap();
    assert_eq!(orphan.status, WorkflowStatus::Running);

    // Re-sweep with nothing changed: the watermark short-circuits every run,
    // so no rows are re-ingested.
    let again = ingest_workflow_runs_from(
        store.database.as_ref(),
        &store.project_id,
        &project_root,
        &projects,
    )
    .await;
    assert_eq!(again, WorkflowIngestStats::default());
}

#[tokio::test]
async fn sweep_skips_runs_owned_by_a_different_project() {
    let home = tempfile::tempdir().unwrap();
    // The fixture's owning session began in `/somewhere/else`, not the
    // project we sweep for, so its runs must not be ingested.
    let other = tempfile::tempdir().unwrap();
    let projects = write_fixture(home.path(), "sess-x", other.path());

    let target = tempfile::tempdir().unwrap();
    let target_root = target.path().canonicalize().unwrap();

    let store = workflow_test_store(&target_root).await;

    let stats = ingest_workflow_runs_from(
        store.database.as_ref(),
        &store.project_id,
        &target_root,
        &projects,
    )
    .await;
    assert_eq!(stats, WorkflowIngestStats::default());
    let reader = RegisteredWorkflowIndexSnapshot::new(store.database.as_ref())
        .await
        .unwrap();
    assert!(
        reader
            .runs_for_session("sess-x", 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Force `path`'s mtime to a fixed unix-second value, so a fixture's
/// `newest_mtime` is deterministic regardless of wall-clock creation time.
/// A read-only open covers both files and directories (a write open would
/// `EISDIR` on a directory).
fn set_mtime(path: &Path, unix_secs: u64) {
    // `filetime` sets a directory's mtime cross-platform; a read-only
    // `File::open` + `set_times` works on Unix but fails on Windows, where
    // adjusting a directory's timestamps needs backup-semantics access.
    filetime::set_file_mtime(
        path,
        filetime::FileTime::from_unix_time(i64::try_from(unix_secs).unwrap(), 0),
    )
    .unwrap();
}

/// Regression: a newer run belonging to a *different* project must not
/// advance this store's ingest watermark. `discover_runs` walks every
/// project slug on the machine, but the watermark is persisted per-store; if
/// an out-of-scope run could raise it, that watermark would leapfrog a
/// still-changing in-scope run and strand it (a Running run that later
/// completes would be skipped forever on subsequent sweeps). The watermark
/// after a sweep must therefore reflect only in-scope runs.
#[tokio::test]
async fn other_project_run_does_not_advance_watermark() {
    // Far-future mtime (year ~2100) for the out-of-scope run.
    const FUTURE: u64 = 4_102_444_800;

    let home = tempfile::tempdir().unwrap();

    // Target project: an in-scope owning session recorded at `target_root`.
    let target = tempfile::tempdir().unwrap();
    let target_root = target.path().canonicalize().unwrap();
    let projects = write_fixture(home.path(), "sess-target", &target_root);

    // A second project's session under the same `~/.claude/projects`, owned
    // by a different cwd so it is out of scope for this sweep.
    let other = tempfile::tempdir().unwrap();
    write_fixture(home.path(), "sess-other", other.path());

    // Give the out-of-scope run a far-future mtime. Since `newest_mtime`
    // maxes the meta file in, this run reads as the newest run on disk by a
    // wide margin — exactly the poison the watermark must resist.
    set_mtime(
        &projects
            .join("dummy-slug")
            .join("sess-other")
            .join("workflows")
            .join("wf_meta.json"),
        FUTURE,
    );

    let store = workflow_test_store(&target_root).await;

    // Sweep the target project only. The in-scope (target) runs are ingested;
    // the out-of-scope (other) runs are not.
    let stats = ingest_workflow_runs_from(
        store.database.as_ref(),
        &store.project_id,
        &target_root,
        &projects,
    )
    .await;
    assert_eq!(stats.runs_ingested, 2); // target's wf_meta + wf_orphan
    let reader = RegisteredWorkflowIndexSnapshot::new(store.database.as_ref())
        .await
        .unwrap();
    assert!(
        reader
            .runs_for_session("sess-other", 10)
            .await
            .unwrap()
            .is_empty()
    );

    // The persisted watermark must reflect only in-scope runs, so it stays
    // well below the out-of-project run's far-future mtime. On the buggy
    // path (watermark advanced before the scope filter) it would equal
    // FUTURE, and the next sweep would strand every target run.
    let snapshot = store.database.read_snapshot().await.unwrap();
    let watermark = read_ingest_watermark(&snapshot, INGEST_WATERMARK_KEY).await;
    assert!(
        watermark > 0 && watermark < i64::try_from(FUTURE).unwrap(),
        "out-of-project run advanced the watermark to {watermark} (>= {FUTURE})"
    );

    // Concretely, the target's still-Running dir-only run is not stranded: a
    // second sweep with an appended agent re-ingests it rather than skipping
    // it on a poisoned watermark.
    let orphan_dir = target_root_orphan_dir(&projects, "sess-target");
    std::fs::write(orphan_dir.join("agent-b2.jsonl"), "\n").unwrap();
    // Bump the run's mtime just past the (correct) watermark so the
    // incremental skip does not legitimately short-circuit it.
    set_mtime(
        &orphan_dir.join("agent-b2.jsonl"),
        u64::try_from(watermark).unwrap() + 60,
    );
    set_mtime(&orphan_dir, u64::try_from(watermark).unwrap() + 60);

    let again = ingest_workflow_runs_from(
        store.database.as_ref(),
        &store.project_id,
        &target_root,
        &projects,
    )
    .await;
    assert_eq!(
        again.runs_ingested, 1,
        "the still-Running target run must be re-ingested, not stranded"
    );
}

/// Path to a fixture session's dir-only (`wf_orphan`) run directory.
fn target_root_orphan_dir(projects: &Path, session_id: &str) -> PathBuf {
    projects
        .join("dummy-slug")
        .join(session_id)
        .join("subagents")
        .join("workflows")
        .join("wf_orphan")
}

use std::future::pending;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

#[derive(Default)]
struct RecordingWorkflowSink {
    watermark: AtomicI64,
    writes: Mutex<Vec<(WorkflowRun, Vec<WorkflowAgent>)>>,
}

impl RecordingWorkflowSink {
    fn writes(&self) -> Vec<(WorkflowRun, Vec<WorkflowAgent>)> {
        self.writes.lock().unwrap().clone()
    }
}

impl WorkflowIngestSink for RecordingWorkflowSink {
    fn matches_project_sessions_authority(&self, _project_id: &ProjectId) -> bool {
        true
    }

    async fn read_ingest_watermark(&self) -> Option<i64> {
        Some(self.watermark.load(Ordering::Acquire))
    }

    async fn bump_ingest_watermark(&self, value: i64) {
        self.watermark.fetch_max(value, Ordering::AcqRel);
    }

    async fn upsert_workflow_run(
        &self,
        run: &WorkflowRun,
        agents: &[WorkflowAgent],
    ) -> Result<(), crate::runtime::workflow_index::WorkflowIndexError> {
        self.writes
            .lock()
            .unwrap()
            .push((run.clone(), agents.to_vec()));
        Ok(())
    }
}

struct PendingWorkflowSink {
    attempts: AtomicUsize,
    started: tokio::sync::mpsc::UnboundedSender<()>,
}

impl WorkflowIngestSink for PendingWorkflowSink {
    fn matches_project_sessions_authority(&self, _project_id: &ProjectId) -> bool {
        true
    }

    async fn read_ingest_watermark(&self) -> Option<i64> {
        Some(0)
    }

    async fn bump_ingest_watermark(&self, _value: i64) {}

    async fn upsert_workflow_run(
        &self,
        _run: &WorkflowRun,
        _agents: &[WorkflowAgent],
    ) -> Result<(), crate::runtime::workflow_index::WorkflowIndexError> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        let _ = self.started.send(());
        pending().await
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

fn write_parent_transcript(slug: &Path, session_id: &str, project_root: &Path) {
    std::fs::write(
        slug.join(format!("{session_id}.jsonl")),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "user",
                "sessionId": session_id,
                "cwd": project_root,
                "timestamp": "2026-07-04T05:00:00.000Z"
            })
        ),
    )
    .unwrap();
}

fn write_large_agent_transcript(path: &Path, session_id: &str, records: usize) {
    let mut body = String::new();
    for ordinal in 0..records {
        body.push_str(
            &serde_json::json!({
                "sessionId": session_id,
                "timestamp": "2026-07-04T05:18:00.000Z",
                "ordinal": ordinal,
                "message": {
                    "usage": {
                        "input_tokens": 2,
                        "output_tokens": 1
                    }
                }
            })
            .to_string(),
        );
        body.push('\n');
    }
    std::fs::write(path, body).unwrap();
}

fn fixture_run(
    slug: &Path,
    session_id: &str,
    run_id: &str,
    project_root: &Path,
    meta: Option<Value>,
) -> DiscoveredRun {
    let session_dir = slug.join(session_id);
    let agents_dir = session_dir.join("subagents").join("workflows").join(run_id);
    std::fs::create_dir_all(&agents_dir).unwrap();
    write_parent_transcript(slug, session_id, project_root);

    let meta_path = meta.map(|meta| {
        let workflows_dir = session_dir.join("workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        let path = workflows_dir.join(format!("{run_id}.json"));
        std::fs::write(&path, serde_json::to_vec(&meta).unwrap()).unwrap();
        path
    });

    DiscoveredRun {
        run_id: run_id.to_string(),
        parent_session_id: session_id.to_string(),
        meta_path,
        agents_dir,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn workflow_discovery_keeps_the_only_tokio_worker_progressing() {
    let project = tempfile::tempdir().unwrap();
    let projects = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.workflow-heartbeat").unwrap();
    let sink = RecordingWorkflowSink::default();
    let handle = tokio::runtime::Handle::current();
    let started = Instant::now();
    let (heartbeat_tx, heartbeat_rx) = std::sync::mpsc::channel();
    let (sleep_end_tx, sleep_end_rx) = std::sync::mpsc::channel();

    ingest_workflow_runs_with_sink_and_discover(
        &sink,
        &project_id,
        project.path(),
        projects.path(),
        move |_| {
            handle.spawn(async move {
                let _ = heartbeat_tx.send(Instant::now());
            });
            std::thread::sleep(Duration::from_millis(80));
            let _ = sleep_end_tx.send(Instant::now());
            Vec::new()
        },
    )
    .await;

    let heartbeat = heartbeat_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("heartbeat must complete");
    let sleep_end = sleep_end_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("sleep-end mark must complete");
    assert!(
        heartbeat < sleep_end,
        "heartbeat after {:?} ran after blocked workflow discovery",
        heartbeat.saturating_duration_since(started)
    );
}

#[tokio::test]
async fn blocking_prepare_preserves_run_order_and_large_transcript_content() {
    const RECORDS: usize = 512;

    let project = tempfile::tempdir().unwrap();
    let projects = tempfile::tempdir().unwrap();
    let slug = projects.path().join("fixture-slug");
    std::fs::create_dir_all(&slug).unwrap();
    let project_root = project.path().canonicalize().unwrap();

    let second = fixture_run(&slug, "session-second", "wf-dir", &project_root, None);
    write_large_agent_transcript(
        &second.agents_dir.join("agent-dir-agent.jsonl"),
        "dir-agent-session",
        RECORDS,
    );
    std::fs::write(
        second.agents_dir.join("journal.jsonl"),
        "{\"type\":\"started\",\"agentId\":\"dir-agent\"}\n",
    )
    .unwrap();

    let mut meta = sample_meta();
    meta.as_object_mut().unwrap().insert(
        "runId".to_string(),
        Value::String("wf-meta-authority".to_string()),
    );
    meta.as_object_mut().unwrap().insert(
        "workflowProgress".to_string(),
        serde_json::json!([{
            "type": "workflow_agent",
            "label": "meta-agent-label",
            "agentId": "meta-agent",
            "state": "done"
        }]),
    );
    meta.as_object_mut()
        .unwrap()
        .insert("agentCount".to_string(), Value::from(1));
    let first = fixture_run(
        &slug,
        "session-first",
        "wf-meta-dir",
        &project_root,
        Some(meta),
    );
    write_large_agent_transcript(
        &first.agents_dir.join("agent-meta-agent.jsonl"),
        "meta-agent-session",
        RECORDS,
    );

    let sink = RecordingWorkflowSink::default();
    let project_id = ProjectId::new("project.workflow-identity").unwrap();
    let stats = ingest_workflow_runs_with_sink_and_discover(
        &sink,
        &project_id,
        &project_root,
        projects.path(),
        move |_| vec![second, first],
    )
    .await;

    assert_eq!(
        stats,
        WorkflowIngestStats {
            runs_ingested: 2,
            agents_ingested: 2,
        }
    );
    let writes = sink.writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].0.run_id, "wf-dir");
    assert_eq!(writes[0].0.parent_session_id, "session-second");
    assert_eq!(writes[0].0.status, WorkflowStatus::Running);
    assert_eq!(writes[0].1[0].agent_id, "dir-agent");
    assert_eq!(
        writes[0].1[0].agent_session_id.as_deref(),
        Some("dir-agent-session")
    );
    assert_eq!(writes[0].1[0].tokens, i64::try_from(RECORDS * 3).unwrap());
    assert_eq!(writes[1].0.run_id, "wf-meta-authority");
    assert_eq!(writes[1].0.parent_session_id, "session-first");
    assert_eq!(writes[1].0.status, WorkflowStatus::Completed);
    assert_eq!(writes[1].1[0].agent_id, "meta-agent");
    assert_eq!(writes[1].1[0].agent_label, "meta-agent-label");
    assert_eq!(
        writes[1].1[0].agent_session_id.as_deref(),
        Some("meta-agent-session")
    );
    assert_eq!(writes[1].1[0].tokens, i64::try_from(RECORDS * 3).unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn cancellation_at_sink_boundary_stops_before_the_next_run() {
    let project = tempfile::tempdir().unwrap();
    let projects = tempfile::tempdir().unwrap();
    let slug = projects.path().join("fixture-slug");
    std::fs::create_dir_all(&slug).unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let first = fixture_run(&slug, "session-first", "wf-first", &project_root, None);
    let second = fixture_run(&slug, "session-second", "wf-second", &project_root, None);
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let sink = Arc::new(PendingWorkflowSink {
        attempts: AtomicUsize::new(0),
        started: started_tx,
    });
    let task_sink = Arc::clone(&sink);
    let project_id = ProjectId::new("project.workflow-cancellation").unwrap();
    let task = tokio::spawn(async move {
        ingest_workflow_runs_with_sink_and_discover(
            task_sink.as_ref(),
            &project_id,
            &project_root,
            projects.path(),
            move |_| vec![first, second],
        )
        .await
    });

    started_rx.recv().await.expect("first sink call must start");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        sink.attempts.load(Ordering::Acquire),
        1,
        "cancellation must prevent the next run from reaching persistence"
    );
}

#[test]
fn transcript_summary_skips_malformed_and_oversized_jsonl_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-bounded.jsonl");
    let mut body = b"{malformed\n".to_vec();
    body.extend(std::iter::repeat_n(b'x', MAX_JSONL_RECORD_BYTES + 1));
    body.push(b'\n');
    body.extend_from_slice(
        br#"{"sessionId":"survivor","timestamp":"2026-07-04T05:18:00.000Z","message":{"usage":{"input_tokens":5,"output_tokens":2}}}"#,
    );
    body.push(b'\n');
    std::fs::write(&path, body).unwrap();

    let summary = summarize_transcript_file(&path);
    assert_eq!(summary.session_id.as_deref(), Some("survivor"));
    assert_eq!(summary.tokens, 7);
    assert_eq!(
        summary.first_ts,
        parse_timestamp("2026-07-04T05:18:00.000Z").map(|value| value as i64)
    );
    assert_eq!(summary.first_ts, summary.last_ts);
}

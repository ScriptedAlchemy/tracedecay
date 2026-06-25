mod automation_runner_support;

use automation_runner_support::*;

#[tokio::test]
async fn scheduler_memory_curator_respects_failure_cooldown() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let config = scheduler_config(Some(3600), Some(3600));
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record(
            "previous_failed_run",
            AutomationRunStatus::Failed,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_cooldown_active")
    );
}

#[tokio::test]
async fn scheduler_memory_curator_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record(
            "previous_successful_run",
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_session_reflector_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_session_reflector_run",
            AgentTaskKind::SessionReflector,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = SessionJsonBackend::new(json!({"facts": []}));

    let run = run_session_reflector_with_backend(
        &cg,
        &config,
        &backend,
        SessionReflectorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SessionReflectorAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_skill_writer_respects_interval_gate() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let config = scheduler_config(Some(3600), None);
    append_run_record(
        &cg.store_layout().dashboard_root,
        &scheduler_record_for(
            "previous_skill_writer_run",
            AgentTaskKind::SkillWriter,
            AutomationRunStatus::Succeeded,
            current_timestamp() - 60,
        ),
    )
    .await
    .unwrap();
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_interval_not_elapsed")
    );
}

#[tokio::test]
async fn scheduler_skill_writer_respects_idle_window_after_manual_run() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_session_evidence(&cg).await;
    let mut config = scheduler_config(Some(1), None);
    config.tasks.skill_writer.min_idle_secs = Some(3600);
    let mut record = scheduler_record_for(
        "recent_manual_skill_writer_run",
        AgentTaskKind::SkillWriter,
        AutomationRunStatus::Succeeded,
        current_timestamp() - 60,
    );
    record.trigger = AutomationTrigger::ManualCli;
    append_run_record(&cg.store_layout().dashboard_root, &record)
        .await
        .unwrap();
    let backend = SkillJsonBackend::new(json!({"skills": []}));

    let run = run_skill_writer_with_backend(
        &cg,
        &config,
        &backend,
        SkillWriterAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..SkillWriterAutomationOptions::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_idle_window_active")
    );
}

#[tokio::test]
async fn memory_curator_runner_cleans_up_lock_file() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert!(!cg
        .store_layout()
        .dashboard_root
        .join("automation_locks")
        .join("memory_curator.lock")
        .exists());
}

#[tokio::test]
async fn memory_curator_runner_recovers_stale_scheduler_lock_file() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(&lock_path, "pid=999999\ncreated_at=100\n").unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let mut config = scheduler_config(None, None);
    config.tasks.memory_curator.stale_lock_secs = Some(1);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_schedule_manual")
    );
    assert!(!lock_path.exists());
}

#[tokio::test]
async fn scheduler_memory_curator_ledgers_active_lock_skip() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(
        &lock_path,
        format!(
            "pid={}\ncreated_at={}\n",
            std::process::id(),
            current_timestamp()
        ),
    )
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 0);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Skipped);
    assert_eq!(
        run.ledger_record.error.as_deref(),
        Some("scheduler_lock_active")
    );
    assert!(lock_path.exists());
    let records = load_run_records(&cg.store_layout().dashboard_root, 10)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].error.as_deref(), Some("scheduler_lock_active"));
}

#[tokio::test]
async fn manual_memory_curator_run_ignores_scheduler_lock() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    seed_duplicate_facts(&cg).await;
    let lock_dir = cg.store_layout().dashboard_root.join("automation_locks");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("memory_curator.lock");
    fs::write(
        &lock_path,
        format!(
            "pid={}\ncreated_at={}\n",
            std::process::id(),
            current_timestamp()
        ),
    )
    .unwrap();
    let backend = JsonBackend::new(json!({"ops": []}));
    let config = scheduler_config(None, None);

    let run = run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::ManualCli,
            max_clusters: 4,
            min_confidence: 0.5,
            run_id: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(backend.calls(), 1);
    assert_eq!(run.ledger_record.status, AutomationRunStatus::Succeeded);
    assert!(lock_path.exists());
}

async fn init_project(project_root: &Path) -> TraceDecay {
    fs::create_dir_all(project_root.join("src")).unwrap();
    fs::write(project_root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    TraceDecay::init(project_root).await.unwrap()
}

async fn seed_session_evidence(cg: &TraceDecay) {
    let db = GlobalDb::open_at(&cg.store_layout().sessions_db_path)
        .await
        .expect("session db open");
    seed_session_message_in_db(
        &db,
        cg.project_root(),
        SeedSessionMessage {
            provider: "cursor",
            session_id: "session-reflect-1",
            message_id: "session-reflect-1-message-001",
            role: "user",
            timestamp: 1_715_000_001,
            text: "Remember durable session reflection facts must remain approval gated for automation workflows.",
            source: None,
        },
    )
    .await;
}

struct SeedSessionMessage<'a> {
    provider: &'a str,
    session_id: &'a str,
    message_id: &'a str,
    role: &'a str,
    timestamp: i64,
    text: &'a str,
    source: Option<&'a str>,
}

async fn seed_session_message_in_db(
    db: &GlobalDb,
    project_root: &Path,
    seed: SeedSessionMessage<'_>,
) {
    let session = SessionRecord {
        provider: seed.provider.to_string(),
        session_id: seed.session_id.to_string(),
        project_key: project_root.display().to_string(),
        project_path: project_root.display().to_string(),
        title: Some("Session reflection fixture".to_string()),
        started_at: Some(seed.timestamp.saturating_sub(1)),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(db.upsert_session(&session).await);
    let message = SessionMessageRecord {
        provider: seed.provider.to_string(),
        message_id: seed.message_id.to_string(),
        session_id: seed.session_id.to_string(),
        role: seed.role.to_string(),
        timestamp: Some(seed.timestamp),
        ordinal: 1,
        text: seed.text.to_string(),
        kind: Some("message".to_string()),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: seed
            .source
            .map(|source| json!({ "source": source }).to_string()),
    };
    assert!(db.upsert_session_message(&message).await);
}

async fn seed_duplicate_facts(cg: &TraceDecay) {
    let conn = cg.db().conn();
    let vec_a = HolographicEncoder::serialize(&[0.20, 0.35, 0.50]).unwrap();
    let vec_b = HolographicEncoder::serialize(&[0.21, 0.34, 0.49]).unwrap();
    for (fact_id, content, vector, trust_score) in [
        (
            101_i64,
            "Cache invalidation policy must be explicit",
            vec_a,
            0.97_f64,
        ),
        (
            102_i64,
            "Cache invalidation policy must stay explicit",
            vec_b,
            0.95_f64,
        ),
    ] {
        conn.execute(
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count,
                 created_at, updated_at, hrr_vector, hrr_algebra, hrr_dim, access_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            libsql::params![
                fact_id,
                content,
                "project",
                "[\"cache\",\"policy\"]",
                trust_score,
                0_i64,
                0_i64,
                1_700_000_000_i64 + fact_id,
                1_700_000_100_i64 + fact_id,
                libsql::Value::Blob(vector),
                "amari_fhrr",
                HolographicEncoder::DIMENSIONS as i64,
                0_i64,
            ],
        )
        .await
        .unwrap();
    }
}

fn scheduler_config(interval_secs: Option<u64>, cooldown_secs: Option<u64>) -> AutomationConfig {
    AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("interval".to_string()),
                interval_secs,
                cooldown_secs,
                ..AutomationTaskConfig::default()
            },
            session_reflector: AutomationTaskConfig {
                enabled: true,
                schedule: Some("interval".to_string()),
                interval_secs,
                cooldown_secs,
                ..AutomationTaskConfig::default()
            },
            skill_writer: AutomationTaskConfig {
                enabled: true,
                schedule: Some("interval".to_string()),
                interval_secs,
                cooldown_secs,
                ..AutomationTaskConfig::default()
            },
        },
        ..AutomationConfig::default()
    }
}

fn scheduler_record(
    run_id: &str,
    status: AutomationRunStatus,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    scheduler_record_for(run_id, AgentTaskKind::MemoryCurator, status, completed_at)
}

fn scheduler_record_for(
    run_id: &str,
    task: AgentTaskKind,
    status: AutomationRunStatus,
    completed_at: i64,
) -> AutomationRunLedgerRecord {
    AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: run_id.to_string(),
        trigger: AutomationTrigger::Scheduler,
        task,
        task_key: Some(test_task_key(task).to_string()),
        backend: "codex_app_server".to_string(),
        host_mode: Some("standalone".to_string()),
        prompt_version: Some(test_prompt_version(task).to_string()),
        response_schema: None,
        strict_json: None,
        model: None,
        status,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: None,
        applied_ops: None,
        rejected_ops: None,
        validation_report: None,
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: usize::from(status == AutomationRunStatus::Skipped),
        error: None,
        error_classification: None,
        error_retryable: None,
        fallback_status: None,
        report_ref: None,
        artifacts: Vec::new(),
        started_at: (completed_at - 1).to_string(),
        completed_at: completed_at.to_string(),
    }
}

fn test_task_key(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator",
        AgentTaskKind::SessionReflector => "session_reflector",
        AgentTaskKind::SkillWriter => "skill_writer",
    }
}

fn test_prompt_version(task: AgentTaskKind) -> &'static str {
    match task {
        AgentTaskKind::MemoryCurator => "memory_curator:v1",
        AgentTaskKind::SessionReflector => "session_reflector:v1",
        AgentTaskKind::SkillWriter => "skill_writer:v1",
    }
}

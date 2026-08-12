use std::{path::Path, sync::Arc, sync::atomic::Ordering};

use tracedecay_global_db::RegisteredGlobalDb;

use super::{
    AgentTaskRunContext, AutomationRunControl, NonEmptyAutomaticFactReceipts, RUN_ID_COUNTER,
    SchedulerGate, append_skipped_record, generated_run_id, task_run_gate,
};
use crate::automation::backend::AgentTaskKind;
use crate::automation::config::{
    AutomationBackend, AutomationConfig, AutomationHostMode, AutomationTaskConfig,
    AutomationTaskSet,
};
use crate::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationTrigger, load_run_records,
};

struct TestSessionsDb {
    db: Arc<RegisteredGlobalDb>,
    _runtime: tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
}

#[test]
fn committed_automatic_fact_receipts_cannot_be_empty() {
    assert!(NonEmptyAutomaticFactReceipts::from_vec(Vec::new()).is_none());
}

async fn test_sessions_db(root: &Path) -> TestSessionsDb {
    let nonce = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    let profile_root = root.join(format!("profile-{nonce}"));
    let runtime =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(&profile_root)
            .await
            .expect("registered profile test runtime");
    let db = runtime.profile_database_arc();
    TestSessionsDb {
        db,
        _runtime: runtime,
    }
}

#[test]
fn generated_run_ids_are_unique_for_same_prefix() {
    let first = generated_run_id("memory_curator");
    let second = generated_run_id("memory_curator");

    assert_ne!(first, second);
}

#[test]
fn run_control_read_observes_a_post_construction_interrupt() {
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&interrupted);
    let control =
        AutomationRunControl::from_interrupted(Arc::new(move || observed.load(Ordering::Acquire)));

    assert!(!control.read_control().interrupted());
    interrupted.store(true, Ordering::Release);
    assert!(control.read_control().interrupted());
}

#[test]
fn run_control_pre_interrupted_write_cannot_begin() {
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let observed = Arc::clone(&interrupted);
    let control =
        AutomationRunControl::from_interrupted(Arc::new(move || observed.load(Ordering::Acquire)));

    let write = control.write_control();
    assert!(write.interrupted());
    assert!(!write.try_begin_commit());
}

#[test]
fn run_control_gives_each_effect_an_independent_one_shot_commit_gate() {
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&interrupted);
    let control =
        AutomationRunControl::from_interrupted(Arc::new(move || observed.load(Ordering::Acquire)));

    let first_effect = control.write_control();
    assert!(first_effect.try_begin_commit());
    assert!(!first_effect.try_begin_commit());

    let second_effect = control.write_control();
    assert!(second_effect.try_begin_commit());
    assert!(!second_effect.try_begin_commit());
}

#[test]
fn run_control_commit_admission_stays_consumed_after_later_interrupt() {
    let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&interrupted);
    let control =
        AutomationRunControl::from_interrupted(Arc::new(move || observed.load(Ordering::Acquire)));
    let write = control.write_control();

    assert!(write.try_begin_commit());
    interrupted.store(true, Ordering::Release);
    assert!(write.interrupted());
    assert!(!write.try_begin_commit());
}

/// Runs the production skip path: evaluate the gate (which caches ledger
/// records for scheduler triggers) and then record the skip, exactly as a
/// gate-level skip does in the task runners.
async fn append_skip(
    dashboard_root: &Path,
    run_id: &str,
    trigger: AutomationTrigger,
    task: AgentTaskKind,
    reason: &str,
) -> AutomationRunLedgerRecord {
    let config = AutomationConfig::default();
    let sessions = test_sessions_db(dashboard_root).await;
    let mut run = AgentTaskRunContext::new(
        dashboard_root.to_path_buf(),
        Arc::clone(&sessions.db),
        Some(run_id.to_string()),
        "test",
        trigger,
        &config,
        task,
    );
    run.gate().await.expect("gate");
    let (_report, record) = run
        .skipped_parts(None, reason, None)
        .await
        .expect("append skipped record");
    record
}

#[tokio::test]
async fn consecutive_identical_scheduler_skips_persist_once() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let root = temp.path();
    let task = AgentTaskKind::MemoryCurator;

    append_skip(
        root,
        "run-1",
        AutomationTrigger::Scheduler,
        task,
        "scheduler_interval_not_elapsed",
    )
    .await;
    append_skip(
        root,
        "run-2",
        AutomationTrigger::Scheduler,
        task,
        "scheduler_interval_not_elapsed",
    )
    .await;

    let records = load_run_records(root, 50).await.expect("load records");
    assert_eq!(
        records.len(),
        1,
        "repeat scheduler skip must not append a second record"
    );
    assert_eq!(records[0].run_id, "run-1");
}

#[tokio::test]
async fn scheduler_skips_with_new_reason_or_task_still_persist() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let root = temp.path();

    append_skip(
        root,
        "run-1",
        AutomationTrigger::Scheduler,
        AgentTaskKind::MemoryCurator,
        "scheduler_interval_not_elapsed",
    )
    .await;
    append_skip(
        root,
        "run-2",
        AutomationTrigger::Scheduler,
        AgentTaskKind::MemoryCurator,
        "scheduler_cooldown_active",
    )
    .await;
    append_skip(
        root,
        "run-3",
        AutomationTrigger::Scheduler,
        AgentTaskKind::SessionReflector,
        "scheduler_interval_not_elapsed",
    )
    .await;

    let records = load_run_records(root, 50).await.expect("load records");
    assert_eq!(records.len(), 3, "distinct skip conditions must persist");
}

#[tokio::test]
async fn manual_trigger_skips_always_persist() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let root = temp.path();
    let task = AgentTaskKind::SkillWriter;

    append_skip(
        root,
        "run-1",
        AutomationTrigger::ManualCli,
        task,
        "skill_writer_disabled",
    )
    .await;
    append_skip(
        root,
        "run-2",
        AutomationTrigger::ManualCli,
        task,
        "skill_writer_disabled",
    )
    .await;

    let records = load_run_records(root, 50).await.expect("load records");
    assert_eq!(records.len(), 2, "manual skips must always be recorded");
}

fn scheduler_enabled_config() -> AutomationConfig {
    AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: true,
                schedule: Some("hourly".to_string()),
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    }
}

#[tokio::test]
async fn on_demand_triggers_bypass_only_scheduler_enablement() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let sessions = test_sessions_db(temp.path()).await;
    let disabled = AutomationConfig {
        enabled: false,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        tasks: AutomationTaskSet {
            memory_curator: AutomationTaskConfig {
                enabled: false,
                ..AutomationTaskConfig::default()
            },
            ..AutomationTaskSet::default()
        },
        ..AutomationConfig::default()
    };

    for trigger in [
        AutomationTrigger::ManualCli,
        AutomationTrigger::ManualMcp,
        AutomationTrigger::Dashboard,
    ] {
        let (gate, _) = task_run_gate(
            &disabled,
            temp.path(),
            sessions.db.as_ref(),
            AgentTaskKind::MemoryCurator,
            trigger,
        )
        .await
        .expect("on-demand gate");
        assert!(matches!(gate, SchedulerGate::Proceed(Some(_))));
    }
}

#[tokio::test]
async fn concurrent_on_demand_runs_share_the_canonical_task_lock() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let sessions = test_sessions_db(temp.path()).await;
    let config = scheduler_enabled_config();
    let (first, _) = task_run_gate(
        &config,
        temp.path(),
        sessions.db.as_ref(),
        AgentTaskKind::MemoryCurator,
        AutomationTrigger::ManualCli,
    )
    .await
    .expect("first on-demand gate");
    let SchedulerGate::Proceed(Some(first_lock)) = first else {
        panic!("first on-demand run must own the task lock");
    };

    let (concurrent, _) = task_run_gate(
        &config,
        temp.path(),
        sessions.db.as_ref(),
        AgentTaskKind::MemoryCurator,
        AutomationTrigger::Dashboard,
    )
    .await
    .expect("concurrent on-demand gate");
    assert!(matches!(
        concurrent,
        SchedulerGate::Skip("scheduler_lock_active")
    ));

    drop(first_lock);
    let (next, _) = task_run_gate(
        &config,
        temp.path(),
        sessions.db.as_ref(),
        AgentTaskKind::MemoryCurator,
        AutomationTrigger::ManualMcp,
    )
    .await
    .expect("next on-demand gate");
    assert!(matches!(next, SchedulerGate::Proceed(Some(_))));
}

#[tokio::test]
async fn scheduler_trigger_still_obeys_global_enablement() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let sessions = test_sessions_db(temp.path()).await;
    let disabled = AutomationConfig {
        enabled: false,
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };

    let (gate, _) = task_run_gate(
        &disabled,
        temp.path(),
        sessions.db.as_ref(),
        AgentTaskKind::MemoryCurator,
        AutomationTrigger::Scheduler,
    )
    .await
    .expect("scheduler gate");
    assert!(matches!(gate, SchedulerGate::Skip("automation_disabled")));
}

#[tokio::test]
async fn on_demand_trigger_does_not_bypass_backend_or_host_admission() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let sessions = test_sessions_db(temp.path()).await;
    let unavailable = AutomationConfig {
        enabled: false,
        backend: AutomationBackend::Disabled,
        host_mode: AutomationHostMode::Standalone,
        ..AutomationConfig::default()
    };
    let (gate, _) = task_run_gate(
        &unavailable,
        temp.path(),
        sessions.db.as_ref(),
        AgentTaskKind::MemoryCurator,
        AutomationTrigger::ManualMcp,
    )
    .await
    .expect("backend gate");
    assert!(matches!(gate, SchedulerGate::Skip("backend_disabled")));

    let delegated = AutomationConfig {
        backend: AutomationBackend::CodexAppServer,
        host_mode: AutomationHostMode::DelegatedHost,
        ..unavailable
    };
    let (gate, _) = task_run_gate(
        &delegated,
        temp.path(),
        sessions.db.as_ref(),
        AgentTaskKind::MemoryCurator,
        AutomationTrigger::Dashboard,
    )
    .await
    .expect("host gate");
    assert!(matches!(gate, SchedulerGate::Skip("delegated_host_mode")));
}

/// Runs the production post-gate skip path: the gate proceeds (caching
/// ledger records), and the task body later decides to skip.
async fn post_gate_scheduler_skip(dashboard_root: &Path, run_id: &str, reason: &str) {
    let config = scheduler_enabled_config();
    let sessions = test_sessions_db(dashboard_root).await;
    let mut run = AgentTaskRunContext::new(
        dashboard_root.to_path_buf(),
        Arc::clone(&sessions.db),
        Some(run_id.to_string()),
        "test",
        AutomationTrigger::Scheduler,
        &config,
        AgentTaskKind::MemoryCurator,
    );
    let SchedulerGate::Proceed(lock) = run.gate().await.expect("gate") else {
        panic!("gate must proceed so the skip is decided post-gate");
    };
    run.skipped_parts(None, reason, None)
        .await
        .expect("append post-gate skip");
    drop(lock);
}

#[tokio::test]
async fn consecutive_identical_post_gate_scheduler_skips_persist_once() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let root = temp.path();

    post_gate_scheduler_skip(root, "run-1", "nothing_to_review").await;
    post_gate_scheduler_skip(root, "run-2", "nothing_to_review").await;

    let records = load_run_records(root, 50).await.expect("load records");
    assert_eq!(
        records.len(),
        1,
        "repeat post-gate scheduler skip must not append a second record"
    );
    assert_eq!(records[0].run_id, "run-1");
}

#[tokio::test]
async fn page_cursor_transitions_bypass_reason_only_skip_deduplication() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let root = temp.path();
    let reason = "nothing_to_review";
    for (run_id, cursor) in [
        ("run-cursor-1", serde_json::json!("fact.cursor")),
        ("run-cursor-2", serde_json::Value::Null),
    ] {
        let config = scheduler_enabled_config();
        let sessions = test_sessions_db(root).await;
        let mut run = AgentTaskRunContext::new(
            root.to_path_buf(),
            Arc::clone(&sessions.db),
            Some(run_id.to_owned()),
            "test",
            AutomationTrigger::Scheduler,
            &config,
            AgentTaskKind::MemoryCurator,
        );
        let SchedulerGate::Proceed(lock) = run.gate().await.expect("gate") else {
            panic!("gate must proceed so the skip is decided post-gate");
        };
        run.skipped_parts_with_validation_report(
            None,
            reason,
            None,
            serde_json::json!({
                "pagination": {"resume_after_fact_id": cursor}
            }),
        )
        .await
        .expect("append cursor-bearing skip");
        drop(lock);
    }

    let records = load_run_records(root, 50).await.expect("load records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].run_id, "run-cursor-2");
    assert_eq!(
        records[0]
            .validation_report
            .as_ref()
            .and_then(|report| report.pointer("/pagination/resume_after_fact_id")),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(records[1].run_id, "run-cursor-1");
}

#[tokio::test]
async fn append_path_relies_solely_on_caller_computed_repeat_flag() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let root = temp.path();
    let config = AutomationConfig::default();
    let task = AgentTaskKind::MemoryCurator;
    let sessions = test_sessions_db(root).await;

    // Both identical scheduler skips persist when the caller reports
    // is_repeat=false, even though the second is a repeat on disk: the
    // append path must not perform its own ledger read to second-guess
    // the flag computed from the gate's cached records.
    for run_id in ["run-1", "run-2"] {
        let run = AgentTaskRunContext::new(
            root.to_path_buf(),
            Arc::clone(&sessions.db),
            Some(run_id.to_string()),
            "memory_curator",
            AutomationTrigger::Scheduler,
            &config,
            task,
        );
        append_skipped_record(&run, None, "scheduler_interval_not_elapsed", false)
            .await
            .expect("append skipped record");
    }
    let records = load_run_records(root, 50).await.expect("load records");
    assert_eq!(
        records.len(),
        2,
        "append path must trust the caller-computed repeat flag"
    );

    let run = AgentTaskRunContext::new(
        root.to_path_buf(),
        Arc::clone(&sessions.db),
        Some("run-3".to_string()),
        "memory_curator",
        AutomationTrigger::Scheduler,
        &config,
        task,
    );
    append_skipped_record(&run, None, "scheduler_interval_not_elapsed", true)
        .await
        .expect("append skipped record");
    let records = load_run_records(root, 50).await.expect("load records");
    assert_eq!(records.len(), 2, "is_repeat=true must suppress the append");
}

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};

use crate::client_identity::DaemonClientIdentity;
use crate::errors::{Result, TraceDecayError};

use super::{
    DAEMON_TASK_ABORT_DEADLINE, DaemonEngine, DaemonHandshake, ProjectServerKey, log_daemon_event,
    open_existing_project_with_options, spawn_lifecycle_automation_scheduler_activation,
};

pub(super) fn scheduler_task_log_fields(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    outcome: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            crate::automation::backend::task_key(task).to_string(),
        ),
        ("outcome", outcome.to_string()),
    ]
}

fn log_scheduler_task_start(project_path: &Path, task: crate::automation::backend::AgentTaskKind) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_task_log_fields(project_path, task, "start"),
    );
}

fn scheduler_task_error_log_fields(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    error: &TraceDecayError,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "task",
            crate::automation::backend::task_key(task).to_string(),
        ),
        ("error", error.to_string()),
    ]
}

fn log_scheduler_task_error(
    project_path: &Path,
    task: crate::automation::backend::AgentTaskKind,
    error: &TraceDecayError,
) {
    log_daemon_event(
        "scheduler_task_error",
        &scheduler_task_error_log_fields(project_path, task, error),
    );
}

fn scheduler_record_log_fields(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) -> Vec<(&'static str, String)> {
    use crate::automation::run_ledger::AutomationRunStatus;

    let outcome = match record.status {
        AutomationRunStatus::Succeeded => "complete",
        AutomationRunStatus::Failed => "error",
        AutomationRunStatus::Skipped => "skipped",
        AutomationRunStatus::Queued => "queued",
        AutomationRunStatus::Running => "running",
    };
    let task = record
        .task_key
        .as_deref()
        .unwrap_or_else(|| crate::automation::backend::task_key(record.task))
        .to_string();
    let mut fields = vec![
        ("project", project_path.display().to_string()),
        ("task", task),
        ("outcome", outcome.to_string()),
        ("run_id", record.run_id.clone()),
    ];
    if let Some(reason) = record.fallback_status.as_ref().or(record.error.as_ref()) {
        fields.push(("reason", reason.clone()));
    }
    fields
}

#[cfg(test)]
pub(super) fn daemon_scheduler_record_log_line(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) -> String {
    super::format_daemon_log_line(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    )
}

fn log_daemon_scheduler_record(
    project_path: &Path,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) {
    log_daemon_event(
        "scheduler_task",
        &scheduler_record_log_fields(project_path, record),
    );
}

pub(super) fn automation_staged_log_fields(
    project_path: &Path,
    counts: crate::automation::staged_notice::AutomationPendingCounts,
) -> Vec<(&'static str, String)> {
    vec![
        ("project", project_path.display().to_string()),
        (
            "pending_fact_proposals",
            counts.pending_fact_proposals.to_string(),
        ),
        ("pending_skills", counts.pending_skills.to_string()),
    ]
}

/// After a scheduler tick where at least one task completed, emit a stable
/// `event=automation_staged` line with managed-skill review counts plus fact
/// proposal telemetry.
/// Silent when nothing is pending or the profile root is unavailable.
async fn log_automation_staged_if_pending(project_path: &Path, dashboard_root: &Path) {
    let Ok(profile_root) = crate::storage::default_profile_root() else {
        return;
    };
    let counts = crate::automation::staged_notice::count_pending_automation_output(
        dashboard_root,
        &profile_root,
    )
    .await;
    if counts.total() == 0 {
        return;
    }
    log_daemon_event(
        "automation_staged",
        &automation_staged_log_fields(project_path, counts),
    );
}

pub(super) struct AutomationSchedulerHandle {
    pub(super) task: JoinHandle<()>,
    pub(super) wake: Arc<tokio::sync::Notify>,
}

impl DaemonEngine {
    pub(super) async fn ensure_automation_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
        cg: Arc<crate::tracedecay::TraceDecay>,
    ) {
        if !self.lifecycle.accepting() {
            return;
        }
        {
            let schedulers = self
                .store_administration
                .automation_schedulers()
                .lock()
                .await;
            if schedulers.contains_key(&key) {
                return;
            }
        }

        // Discover configuration from the project server that is already
        // registered for this owner. This keeps writable project opening and
        // identity repair under the project-server coordination path while
        // avoiding the daemon-wide writer gate for read-only discovery.
        let configured = match async {
            let config =
                effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
            automation_scheduler_has_work(&cg, &config).await
        }
        .await
        {
            Ok(configured) => configured,
            Err(e) => {
                log_daemon_event(
                    "scheduler_config",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "error".to_string()),
                        ("error", e.to_string()),
                    ],
                );
                false
            }
        };
        if !configured {
            log_daemon_event(
                "scheduler_config",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "skipped".to_string()),
                    ("reason", "not_configured".to_string()),
                ],
            );
            return;
        }

        self.store_administration
            .with_writer(|| async move {
                if !self.lifecycle.accepting() {
                    return;
                }
                let owner_is_current = self
                    .store_administration
                    .project_servers()
                    .lock()
                    .await
                    .get(&key)
                    .is_some();
                if !owner_is_current {
                    return;
                }
                {
                    let schedulers = self
                        .store_administration
                        .automation_schedulers()
                        .lock()
                        .await;
                    if schedulers.contains_key(&key) {
                        return;
                    }
                }
                self.start_automation_scheduler(key, project_path, handshake)
                    .await;
            })
            .await;
    }

    pub(super) fn automation_scheduler_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> crate::dashboard::AutomationSchedulerReconciler {
        let engine = self.clone();
        std::sync::Arc::new(move || {
            let engine = engine.clone();
            let current_key = Arc::clone(&current_key);
            let project_path = project_path.clone();
            let handshake = handshake.clone();
            let lifecycle = engine.lifecycle.clone();
            spawn_lifecycle_automation_scheduler_activation(lifecycle, async move {
                let key = current_key.lock().await.clone();
                let server = {
                    engine
                        .store_administration
                        .project_servers()
                        .lock()
                        .await
                        .get(&key)
                        .cloned()
                };
                let Some(server) = server else {
                    return;
                };
                let cg = server.cg().await;
                engine
                    .ensure_automation_scheduler(key.clone(), project_path, handshake, cg)
                    .await;
                if let Some(handle) = engine
                    .store_administration
                    .automation_schedulers()
                    .lock()
                    .await
                    .get(&key)
                {
                    handle.wake.notify_one();
                }
            });
        })
    }

    async fn start_automation_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) {
        if !self.lifecycle.accepting() {
            return;
        }
        let mut schedulers = self
            .store_administration
            .automation_schedulers()
            .lock()
            .await;
        if !self.lifecycle.accepting() || schedulers.contains_key(&key) {
            return;
        }
        let wake = Arc::new(tokio::sync::Notify::new());
        let loop_wake = Arc::clone(&wake);
        let task = tokio::spawn(async move {
            Box::pin(run_automation_scheduler_loop(
                project_path,
                handshake,
                loop_wake,
            ))
            .await;
        });
        schedulers.insert(key, AutomationSchedulerHandle { task, wake });
    }

    pub(super) async fn shutdown_automation_schedulers(&self) {
        self.shutdown_automation_schedulers_with_deadline(DAEMON_TASK_ABORT_DEADLINE)
            .await;
    }

    pub(super) async fn shutdown_automation_schedulers_with_deadline(&self, deadline: Duration) {
        let scheduler_handles: Vec<JoinHandle<()>> = match timeout(
            deadline,
            self.store_administration.with_writer(|| async {
                let mut schedulers = self
                    .store_administration
                    .automation_schedulers()
                    .lock()
                    .await;
                schedulers.drain().map(|(_, handle)| handle.task).collect()
            }),
        )
        .await
        {
            Ok(handles) => handles,
            Err(_) => {
                log_daemon_event("daemon_shutdown", &[("outcome", "timeout".to_string())]);
                return;
            }
        };
        let _child_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
        for handle in &scheduler_handles {
            handle.abort();
        }
        let _ = timeout(deadline, async {
            for handle in scheduler_handles {
                let _ = handle.await;
            }
        })
        .await;
    }
}

async fn run_automation_scheduler_loop(
    project_path: PathBuf,
    handshake: DaemonHandshake,
    wake: Arc<tokio::sync::Notify>,
) {
    loop {
        match Box::pin(automation_scheduler_has_work_for_project(
            &project_path,
            &handshake,
        ))
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                log_daemon_event(
                    "scheduler_exit",
                    &[
                        ("project", project_path.display().to_string()),
                        ("reason", "not_configured".to_string()),
                    ],
                );
                break;
            }
            Err(e) => {
                // A transient failure (e.g. a momentarily corrupt jobs file or
                // a project that cannot be opened this instant) must not
                // permanently kill the scheduler loop. Surface the cause and
                // retry on the next tick instead of exiting for good.
                log_daemon_event(
                    "scheduler_project_open",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "error".to_string()),
                        ("error", e.to_string()),
                    ],
                );
                tokio::time::sleep(Duration::from_secs(
                    crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS,
                ))
                .await;
                continue;
            }
        }
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "start".to_string()),
            ],
        );
        if let Err(e) = Box::pin(run_automation_scheduler_tick(&project_path, &handshake)).await {
            log_daemon_event(
                "scheduler_tick",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
        }
        if let Err(error) = Box::pin(run_host_receipt_review(&project_path, &handshake)).await {
            log_daemon_event(
                "host_receipt_review",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", error.to_string()),
                ],
            );
        }
        let tick_secs = Box::pin(automation_scheduler_tick_secs_for_project(
            &project_path,
            &handshake,
        ))
        .await;
        log_daemon_event(
            "scheduler_sleep",
            &[
                ("project", project_path.display().to_string()),
                ("next_tick_secs", tick_secs.to_string()),
            ],
        );
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(tick_secs)) => {}
            () = wake.notified() => {
                // Receipts arrive at tool cadence. Wait for a short quiet
                // period and reset it for every later receipt, producing one
                // review for the burst rather than one review per command.
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(5)) => break,
                        () = wake.notified() => {}
                    }
                }
                if let Err(error) = Box::pin(run_host_receipt_review(&project_path, &handshake)).await {
                    log_daemon_event(
                        "host_receipt_review",
                        &[
                            ("project", project_path.display().to_string()),
                            ("outcome", "error".to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                }
            }
        }
    }
}

async fn automation_scheduler_has_work_for_project(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<bool> {
    let cg = Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await?;
    let config = effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
    automation_scheduler_has_work(&cg, &config).await
}

pub(super) async fn automation_scheduler_tick_secs_for_project(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> u64 {
    match Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await
    {
        Ok(cg) => {
            match effective_automation_config_for_project(&cg, &handshake.client_identity).await {
                Ok(config) => config.scheduler_tick_secs,
                Err(e) => {
                    log_daemon_event(
                        "scheduler_config",
                        &[
                            ("project", project_path.display().to_string()),
                            ("outcome", "error".to_string()),
                            ("error", e.to_string()),
                        ],
                    );
                    crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS
                }
            }
        }
        Err(e) => {
            log_daemon_event(
                "scheduler_project_open",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
            crate::automation::config::DEFAULT_SCHEDULER_TICK_SECS
        }
    }
}

/// Minimum wall-clock interval between global-database retention passes,
/// shared across every project's scheduler loop so retention runs at most
/// this often no matter how many projects are active.
const RETENTION_MIN_INTERVAL_SECS: u64 = 6 * 60 * 60;

static LAST_GLOBAL_RETENTION: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// Returns whether a retention pass is due, recording `now` as the last run
/// when it is. The gate is process-global so N project loops do not each run
/// their own retention every tick.
fn global_retention_pass_due(now: std::time::Instant) -> bool {
    let mut guard = match LAST_GLOBAL_RETENTION.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let due = guard.is_none_or(|last| {
        now.duration_since(last) >= Duration::from_secs(RETENTION_MIN_INTERVAL_SECS)
    });
    if due {
        *guard = Some(now);
    }
    due
}

/// Applies the configured retention windows to the global telemetry tables,
/// at most once per [`RETENTION_MIN_INTERVAL_SECS`]. Best-effort: retention is
/// housekeeping, so failures are logged and never abort a scheduler tick.
async fn maybe_run_global_retention(
    project_path: &Path,
    config: &crate::automation::config::AutomationConfig,
) {
    if !global_retention_pass_due(std::time::Instant::now()) {
        return;
    }
    let db = match crate::global_db::GlobalDb::try_open().await {
        Ok(Some(db)) => db,
        Ok(None) => return,
        Err(error) => {
            log_daemon_event(
                "retention_prune",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "open_rejected".to_string()),
                    ("error", error.to_string()),
                ],
            );
            return;
        }
    };
    let now_secs = crate::tracedecay::current_timestamp();
    match db.prune_global_retention(&config.retention, now_secs).await {
        Ok(reports) => {
            for report in reports {
                if report.applied && report.rows > 0 {
                    log_daemon_event(
                        "retention_prune",
                        &[
                            ("project", project_path.display().to_string()),
                            ("table", report.table.to_string()),
                            ("rows", report.rows.to_string()),
                            (
                                "window_days",
                                report
                                    .window_days
                                    .map_or_else(|| "unlimited".to_string(), |d| d.to_string()),
                            ),
                        ],
                    );
                }
            }
        }
        Err(e) => {
            log_daemon_event(
                "retention_prune",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
        }
    }
}

pub(super) async fn run_automation_scheduler_tick(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<()> {
    use crate::automation::backend::{AgentTaskKind, CodexAppServerBackend};
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        CombinedReviewAutomationOptions, CombinedReviewDispatch, MemoryCuratorAutomationOptions,
        SessionReflectorAutomationOptions, SkillWriterAutomationOptions,
        run_combined_review_with_backend, run_memory_curator_with_backend,
        run_session_reflector_with_backend, run_skill_writer_with_backend,
    };

    let cg = Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await?;
    let control =
        crate::automation::scheduler::load_scheduler_control(&cg.store_layout().dashboard_root)
            .await?;
    if control.paused {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "paused".to_string()),
            ],
        );
        return Ok(());
    }
    let config = effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
    if !automation_scheduler_has_work(&cg, &config).await? {
        log_daemon_event(
            "scheduler_tick",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "skipped".to_string()),
                ("reason", "not_configured".to_string()),
            ],
        );
        return Ok(());
    }
    maybe_run_global_retention(project_path, &config).await;
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let mut first_error: Option<TraceDecayError> = None;
    let mut any_succeeded = false;

    log_scheduler_task_start(project_path, AgentTaskKind::MemoryCurator);
    match run_memory_curator_with_backend(
        &cg,
        &config,
        &backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Scheduler,
            ..MemoryCuratorAutomationOptions::default()
        },
    )
    .await
    {
        Ok(run) => {
            any_succeeded |= run.ledger_record.status
                == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
            log_daemon_scheduler_record(project_path, &run.ledger_record);
        }
        Err(e) => {
            log_scheduler_task_error(project_path, AgentTaskKind::MemoryCurator, &e);
            first_error.get_or_insert(e);
        }
    }
    // When both the reflector and the skill writer are due in this tick, the
    // combined path serves them with one backend call. Any other outcome
    // (combined mode disabled, only one task due, missing evidence) falls
    // back to the sequential per-task runs below.
    let mut combined_handled = false;
    if config.combine_due_tasks {
        log_scheduler_task_start(project_path, AgentTaskKind::CombinedReview);
        match run_combined_review_with_backend(
            &cg,
            &config,
            &backend,
            CombinedReviewAutomationOptions::default(),
        )
        .await
        {
            Ok(CombinedReviewDispatch::Ran(run)) => {
                any_succeeded |= run.session_reflector.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                any_succeeded |= run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
                log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
                combined_handled = true;
            }
            Ok(CombinedReviewDispatch::RecordedFailure { run, error }) => {
                any_succeeded |= run.session_reflector.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                any_succeeded |= run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
                log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
                log_scheduler_task_error(project_path, AgentTaskKind::CombinedReview, &error);
                first_error.get_or_insert(error);
                combined_handled = true;
            }
            Ok(CombinedReviewDispatch::NotCombined { reason }) => {
                log_daemon_event(
                    "scheduler_task",
                    &[
                        ("project", project_path.display().to_string()),
                        ("task", "combined_review".to_string()),
                        ("outcome", "not_combined".to_string()),
                        ("reason", reason.to_string()),
                    ],
                );
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::CombinedReview, &e);
            }
        }
    }
    if !combined_handled {
        log_scheduler_task_start(project_path, AgentTaskKind::SessionReflector);
        match run_session_reflector_with_backend(
            &cg,
            &config,
            &backend,
            SessionReflectorAutomationOptions {
                trigger: AutomationTrigger::Scheduler,
                ..SessionReflectorAutomationOptions::default()
            },
        )
        .await
        {
            Ok(run) => {
                any_succeeded |= run.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.ledger_record);
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::SessionReflector, &e);
                first_error.get_or_insert(e);
            }
        }
        log_scheduler_task_start(project_path, AgentTaskKind::SkillWriter);
        match run_skill_writer_with_backend(
            &cg,
            &config,
            &backend,
            SkillWriterAutomationOptions {
                trigger: AutomationTrigger::Scheduler,
                ..SkillWriterAutomationOptions::default()
            },
        )
        .await
        {
            Ok(run) => {
                any_succeeded |= run.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded;
                log_daemon_scheduler_record(project_path, &run.ledger_record);
            }
            Err(e) => {
                log_scheduler_task_error(project_path, AgentTaskKind::SkillWriter, &e);
                first_error.get_or_insert(e);
            }
        }
    }
    if any_succeeded {
        log_automation_staged_if_pending(project_path, &cg.store_layout().dashboard_root).await;
    }
    run_user_jobs_scheduler_pass(
        project_path,
        &handshake.client_identity.profile_root,
        &cg,
        &config,
        &backend,
        &mut first_error,
    )
    .await;
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

async fn run_host_receipt_review(project_path: &Path, handshake: &DaemonHandshake) -> Result<()> {
    use crate::automation::backend::CodexAppServerBackend;
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        CombinedReviewAutomationOptions, CombinedReviewDispatch, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, run_combined_review_with_backend,
    };

    let cg = Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await?;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let Some(ready) = crate::automation::host_receipts::oldest_ready(&dashboard_root).await? else {
        return Ok(());
    };
    let pending = ready.pending;
    if crate::automation::scheduler::load_scheduler_control(&dashboard_root)
        .await?
        .paused
    {
        return Ok(());
    }
    let config = effective_automation_config_for_project(&cg, &handshake.client_identity).await?;
    let session_id = pending
        .route
        .as_ref()
        .and_then(|route| route.session_id.clone());
    let Some(session_db) =
        crate::global_db::GlobalDb::open_read_only_at(&cg.store_layout().sessions_db_path).await
    else {
        return Ok(());
    };
    if session_db
        .lcm_load_raw_message("hermes", &ready.transcript_watermark)
        .await
        .is_none()
    {
        // Never review a terminal receipt until the exact completed-turn
        // watermark is durable in LCM.
        return Ok(());
    }
    let backend = CodexAppServerBackend::from_automation_config(&config);
    let result = run_combined_review_with_backend(
        &cg,
        &config,
        &backend,
        CombinedReviewAutomationOptions {
            run_id: Some(format!("host_receipt_{}", pending.generation)),
            session_reflector: SessionReflectorAutomationOptions {
                trigger: AutomationTrigger::HostReceipt,
                provider: "hermes".to_string(),
                session_id,
                ..SessionReflectorAutomationOptions::default()
            },
            skill_writer: SkillWriterAutomationOptions {
                trigger: AutomationTrigger::HostReceipt,
                provider: "hermes".to_string(),
                ..SkillWriterAutomationOptions::default()
            },
            trigger: AutomationTrigger::HostReceipt,
        },
    )
    .await?;
    match result {
        CombinedReviewDispatch::Ran(run) => {
            log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
            log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
            if run.session_reflector.ledger_record.status
                == crate::automation::run_ledger::AutomationRunStatus::Succeeded
                && run.skill_writer.ledger_record.status
                    == crate::automation::run_ledger::AutomationRunStatus::Succeeded
            {
                crate::automation::host_receipts::mark_consumed(
                    &dashboard_root,
                    &pending.session_key,
                    pending.generation,
                )
                .await?;
            }
        }
        CombinedReviewDispatch::RecordedFailure { run, error } => {
            log_daemon_scheduler_record(project_path, &run.session_reflector.ledger_record);
            log_daemon_scheduler_record(project_path, &run.skill_writer.ledger_record);
            return Err(error);
        }
        CombinedReviewDispatch::NotCombined { reason } => {
            log_daemon_event(
                "host_receipt_review",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "deferred".to_string()),
                    ("reason", reason.to_string()),
                ],
            );
        }
    }
    Ok(())
}

async fn effective_automation_config_for_project(
    cg: &crate::tracedecay::TraceDecay,
    client_identity: &DaemonClientIdentity,
) -> Result<crate::automation::config::AutomationConfig> {
    use crate::automation::config::{effective_config, load_project_config};

    let global = user_config_for_client(client_identity).automation;
    let project = load_project_config(&cg.store_layout().dashboard_root).await?;
    effective_config(&global, project.as_ref())
}

pub(super) fn user_config_for_client(
    client_identity: &DaemonClientIdentity,
) -> crate::user_config::UserConfig {
    let path = client_identity.profile_root.join("config.toml");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return crate::user_config::UserConfig::default();
    };
    crate::user_config::parse_or_warn_default(&path, &contents)
}

pub(super) fn automation_scheduler_configured(
    config: &crate::automation::config::AutomationConfig,
) -> bool {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};
    use crate::automation::scheduler::{AutomationSchedule, parse_schedule};

    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return false;
    }
    if config.combine_due_tasks
        && config.tasks.session_reflector.enabled
        && config.tasks.skill_writer.enabled
    {
        return true;
    }
    [
        &config.tasks.memory_curator,
        &config.tasks.session_reflector,
        &config.tasks.skill_writer,
    ]
    .into_iter()
    .any(|task| {
        if !task.enabled {
            return false;
        }
        match parse_schedule(task.schedule.as_deref()) {
            Ok(AutomationSchedule::Manual) | Err(_) => false,
            Ok(AutomationSchedule::ConfiguredInterval) => task.interval_secs.is_some(),
            Ok(AutomationSchedule::Interval { .. } | AutomationSchedule::Cron(_)) => true,
        }
    })
}

/// True when the scheduler loop has anything to do for this project: a
/// scheduled fixed task or a schedulable user-defined job.
async fn automation_scheduler_has_work(
    cg: &crate::tracedecay::TraceDecay,
    config: &crate::automation::config::AutomationConfig,
) -> Result<bool> {
    use crate::automation::config::{AutomationBackend, AutomationHostMode};

    if automation_scheduler_configured(config) {
        return Ok(true);
    }
    if !config.enabled
        || config.host_mode == AutomationHostMode::DelegatedHost
        || config.backend != AutomationBackend::CodexAppServer
    {
        return Ok(false);
    }
    crate::automation::jobs::jobs_configured_for_scheduler(&cg.store_layout().dashboard_root).await
}

/// Ticks every schedulable user-defined job with the same lock/cooldown
/// discipline as the fixed tasks (enforced inside the job runner).
async fn run_user_jobs_scheduler_pass(
    project_path: &Path,
    profile_root: &Path,
    cg: &crate::tracedecay::TraceDecay,
    config: &crate::automation::config::AutomationConfig,
    backend: &crate::automation::backend::CodexAppServerBackend,
    first_error: &mut Option<TraceDecayError>,
) {
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let jobs = match crate::automation::jobs::load_jobs(&dashboard_root).await {
        Ok(jobs) => jobs,
        Err(e) => {
            log_daemon_event(
                "scheduler_user_jobs",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "error".to_string()),
                    ("error", e.to_string()),
                ],
            );
            first_error.get_or_insert(e);
            return;
        }
    };
    for job in jobs
        .iter()
        .filter(|job| crate::automation::jobs::job_is_schedulable(job))
    {
        log_scheduler_task_start(
            project_path,
            crate::automation::backend::AgentTaskKind::UserJob,
        );
        match crate::automation::jobs::run_user_job_with_backend(
            &dashboard_root,
            config,
            backend,
            job,
            crate::automation::jobs::UserJobRunOptions {
                trigger: crate::automation::run_ledger::AutomationTrigger::Scheduler,
                profile_root: Some(profile_root.to_path_buf()),
                project_root: Some(project_path.to_path_buf()),
                ..crate::automation::jobs::UserJobRunOptions::default()
            },
        )
        .await
        {
            Ok(run) => log_daemon_scheduler_record(project_path, &run.ledger_record),
            Err(e) => {
                log_scheduler_task_error(
                    project_path,
                    crate::automation::backend::AgentTaskKind::UserJob,
                    &e,
                );
                first_error.get_or_insert(e);
            }
        }
    }
}

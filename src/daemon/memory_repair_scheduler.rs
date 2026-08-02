//! Daemon-owned resumable repair scheduler.
//!
//! One loop per project owner drains feedback-history repair and the legacy
//! memory cutover plus any checkpointed session-store repair; unlike
//! automation, repair is never configuration-gated.
//! The loop is driven by an explicit [`MemoryRepairPassDecision`] and retries
//! on the shared bounded `replay_backoff` curve rather than a fixed delay.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{
    future::Future,
    time::{Duration, Instant},
};

use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::errors::{Result, TraceDecayError};
use tracedecay_store::{
    CompatibilityFeedbackRepairProgressV1, CompatibilityLegacyMemoryCutoverProgressV1,
};

use super::branch_admin::MaintenanceReaperKind;
use super::scheduler::{MaintenanceTaskTermination, same_scheduler_owner};
use super::{
    DAEMON_TASK_ABORT_DEADLINE, DaemonEngine, DaemonHandshake, ProjectServerKey, log_daemon_event,
};

pub(super) struct MemoryRepairSchedulerHandle {
    pub(super) task: Option<JoinHandle<()>>,
    pub(super) completion: Arc<()>,
    pub(super) generation: Arc<std::sync::atomic::AtomicU64>,
    pub(super) lifecycle: MemoryRepairSchedulerLifecycle,
    termination: Arc<MaintenanceTaskTermination>,
}

#[cfg(test)]
impl MemoryRepairSchedulerHandle {
    pub(super) fn for_test(task: JoinHandle<()>) -> Self {
        Self {
            task: Some(task),
            completion: Arc::new(()),
            generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            lifecycle: MemoryRepairSchedulerLifecycle::Running,
            termination: Arc::new(MaintenanceTaskTermination::pending()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemoryRepairSchedulerLifecycle {
    Running,
    Finished,
    Retiring,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemoryRepairSchedulerReconcileOutcome {
    Started,
    Running,
    Retiring,
    LifecycleInactive,
}

pub(super) struct MemoryRepairSchedulerRetirement {
    termination: Arc<MaintenanceTaskTermination>,
}

impl MemoryRepairSchedulerRetirement {
    pub(super) async fn wait(self) {
        self.termination.wait().await;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemoryRepairTickOutcome {
    Incomplete,
    Complete,
    NotRequired,
}

/// How the repair loop proceeds after one tick, in the spirit of the
/// host-admission `ReplayPassDecision`: each variant gets distinct loop
/// handling instead of a collapsed retry bool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MemoryRepairPassDecision {
    /// Repair or cutover work remains — keep ticking on the backoff schedule.
    Advanced,
    /// Nothing left to repair — the loop stops until the next project open.
    Idle,
}

/// Per-worker shift cap for the shared `replay_backoff` curve: the retry
/// delay starts at 25ms and doubles per consecutive advanced tick until this
/// shift (or the curve's absolute ceiling) is reached.
const MEMORY_REPAIR_BACKOFF_SHIFT_CAP: u32 = 6;
const SESSION_TEMPORAL_MIN_PAGE_ROWS: usize = 256;
const SESSION_TEMPORAL_MAX_PAGE_ROWS: usize = 4_096;
const SESSION_TEMPORAL_TARGET_PAGE_LATENCY: Duration = Duration::from_millis(125);
const SESSION_TEMPORAL_SLOW_PAGE_LATENCY: Duration = Duration::from_millis(250);
const SESSION_TEMPORAL_MAX_PAGES_PER_TICK: usize = 16;
const SESSION_TEMPORAL_MAX_TICK_ELAPSED: Duration = Duration::from_secs(2);

fn next_session_temporal_page_rows(current_rows: usize, elapsed: Duration) -> usize {
    let current_rows = current_rows.clamp(
        SESSION_TEMPORAL_MIN_PAGE_ROWS,
        SESSION_TEMPORAL_MAX_PAGE_ROWS,
    );
    let ratio = if elapsed > SESSION_TEMPORAL_SLOW_PAGE_LATENCY {
        0.5
    } else {
        (SESSION_TEMPORAL_TARGET_PAGE_LATENCY.as_secs_f64()
            / elapsed.as_secs_f64().max(f64::EPSILON))
        .clamp(0.5, 2.0)
    };
    ((current_rows as f64 * ratio).round() as usize).clamp(
        SESSION_TEMPORAL_MIN_PAGE_ROWS,
        SESSION_TEMPORAL_MAX_PAGE_ROWS,
    )
}

async fn run_session_temporal_repair_pager_with<Tick, TickFuture>(
    mut tick: Tick,
) -> Result<MemoryRepairPassDecision>
where
    Tick: FnMut(usize) -> TickFuture,
    TickFuture: Future<Output = Result<crate::global_db::SessionTemporalRepairOutcome>>,
{
    let tick_started = Instant::now();
    let mut page_rows = SESSION_TEMPORAL_MIN_PAGE_ROWS;
    let mut previous_stage = None;
    for _ in 0..SESSION_TEMPORAL_MAX_PAGES_PER_TICK {
        let page_started = Instant::now();
        let outcome = tick(page_rows).await?;
        let page_elapsed = page_started.elapsed();
        match outcome {
            crate::global_db::SessionTemporalRepairOutcome::Pending { stage } => {
                page_rows = if previous_stage == Some(stage)
                    && matches!(
                        stage,
                        crate::global_db::SessionTemporalRepairStage::AuthorityEffects
                            | crate::global_db::SessionTemporalRepairStage::AuthorityReceipts
                    ) {
                    next_session_temporal_page_rows(page_rows, page_elapsed)
                } else {
                    SESSION_TEMPORAL_MIN_PAGE_ROWS
                };
                previous_stage = Some(stage);
            }
            crate::global_db::SessionTemporalRepairOutcome::Complete
            | crate::global_db::SessionTemporalRepairOutcome::NotRequired => {
                return Ok(MemoryRepairPassDecision::Idle);
            }
        }
        if tick_started.elapsed() >= SESSION_TEMPORAL_MAX_TICK_ELAPSED {
            break;
        }
    }
    Ok(MemoryRepairPassDecision::Advanced)
}

impl DaemonEngine {
    /// Starts one daemon-owned compatibility-memory repair loop for this exact
    /// project owner. Unlike automation, repair is never configuration-gated.
    #[cfg(test)]
    pub(super) async fn ensure_memory_repair_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> MemoryRepairSchedulerReconcileOutcome {
        let transition = self.maintenance_transition_gate(&key).await;
        let _transition = transition.lock().await;
        let scope = super::branch_admin::owner_writer_scope(&key);
        self.store_administration
            .with_writer_in(scope, || async move {
                self.reconcile_memory_repair_scheduler_locked(key, project_path, handshake)
                    .await
            })
            .await
    }

    pub(super) async fn start_memory_repair_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        _handshake: DaemonHandshake,
    ) -> MemoryRepairSchedulerReconcileOutcome {
        let server = {
            let servers = self.store_administration.project_servers().lock().await;
            let owner = servers
                .keys()
                .find(|candidate| same_scheduler_owner(candidate, &key));
            owner.and_then(|owner| servers.get(owner).cloned())
        };
        let Some(server) = server else {
            return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
        };
        let cg = server.cg().await;
        loop {
            if !self.lifecycle.accepting() {
                return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
            }
            let finished = {
                let mut schedulers = self
                    .store_administration
                    .memory_repair_schedulers()
                    .lock()
                    .await;
                if !self.lifecycle.accepting() {
                    return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
                }
                let owner = schedulers
                    .get_key_value(&key)
                    .map(|(owner, _)| owner.clone())
                    .or_else(|| {
                        schedulers
                            .keys()
                            .find(|candidate| same_scheduler_owner(candidate, &key))
                            .cloned()
                    });
                if let Some(owner) = owner {
                    let Some(handle) = schedulers.get_mut(&owner) else {
                        continue;
                    };
                    match observed_memory_repair_lifecycle(handle) {
                        MemoryRepairSchedulerLifecycle::Running => {
                            handle
                                .generation
                                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            return MemoryRepairSchedulerReconcileOutcome::Running;
                        }
                        MemoryRepairSchedulerLifecycle::Finished => schedulers.remove(&owner),
                        MemoryRepairSchedulerLifecycle::Retiring => {
                            return MemoryRepairSchedulerReconcileOutcome::Retiring;
                        }
                    }
                } else {
                    #[cfg(test)]
                    self.memory_repair_start_attempts
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let completion = Arc::new(());
                    let completed = Arc::clone(&completion);
                    let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
                    let termination = Arc::new(MaintenanceTaskTermination::pending());
                    let administration = self.store_administration.clone();
                    let scheduler_administration = administration.clone();
                    let cg = Arc::clone(&cg);
                    let (published, start) = tokio::sync::oneshot::channel();
                    let task = tokio::spawn(async move {
                        let _ = start.await;
                        Box::pin(run_memory_repair_scheduler_loop(
                            project_path,
                            cg,
                            scheduler_administration,
                        ))
                        .await;
                        administration
                            .memory_repair_schedulers()
                            .lock()
                            .await
                            .retain(|_, handle| {
                                !Arc::ptr_eq(&handle.completion, &completed)
                                    || handle.lifecycle == MemoryRepairSchedulerLifecycle::Retiring
                            });
                    });
                    schedulers.insert(
                        key,
                        MemoryRepairSchedulerHandle {
                            task: Some(task),
                            completion,
                            generation,
                            lifecycle: MemoryRepairSchedulerLifecycle::Running,
                            termination,
                        },
                    );
                    let _ = published.send(());
                    return MemoryRepairSchedulerReconcileOutcome::Started;
                }
            };
            if let Some(mut handle) = finished
                && let Some(task) = handle.task.take()
            {
                let _ = task.await;
            }
        }
    }

    pub(super) async fn reconcile_memory_repair_scheduler_locked(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) -> MemoryRepairSchedulerReconcileOutcome {
        if !self.lifecycle.accepting() {
            return MemoryRepairSchedulerReconcileOutcome::LifecycleInactive;
        }
        let finished = {
            let mut schedulers = self
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await;
            let owner = schedulers
                .get_key_value(&key)
                .map(|(owner, _)| owner.clone())
                .or_else(|| {
                    schedulers
                        .keys()
                        .find(|candidate| same_scheduler_owner(candidate, &key))
                        .cloned()
                });
            match owner {
                Some(owner) => {
                    let Some(handle) = schedulers.get_mut(&owner) else {
                        return MemoryRepairSchedulerReconcileOutcome::Running;
                    };
                    match observed_memory_repair_lifecycle(handle) {
                        MemoryRepairSchedulerLifecycle::Running => {
                            handle
                                .generation
                                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                            return MemoryRepairSchedulerReconcileOutcome::Running;
                        }
                        MemoryRepairSchedulerLifecycle::Finished => schedulers.remove(&owner),
                        MemoryRepairSchedulerLifecycle::Retiring => {
                            return MemoryRepairSchedulerReconcileOutcome::Retiring;
                        }
                    }
                }
                None => None,
            }
        };
        if let Some(mut handle) = finished
            && let Some(task) = handle.task.take()
        {
            let _ = task.await;
        }
        self.start_memory_repair_scheduler(key, project_path, handshake)
            .await
    }

    pub(super) async fn retire_memory_repair_scheduler_locked(
        &self,
        key: &ProjectServerKey,
    ) -> Option<MemoryRepairSchedulerRetirement> {
        let reservation = self.store_administration.reserve_retirement_reaper()?;
        let (task, completion, termination) = {
            let mut schedulers = self
                .store_administration
                .memory_repair_schedulers()
                .lock()
                .await;
            let owner = if schedulers.contains_key(key) {
                key.clone()
            } else {
                schedulers
                    .keys()
                    .find(|candidate| same_scheduler_owner(candidate, key))
                    .cloned()?
            };
            let handle = schedulers.get_mut(&owner)?;
            handle.lifecycle = MemoryRepairSchedulerLifecycle::Retiring;
            handle
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let task = handle.task.take();
            (
                task.map(|task| (owner, task)),
                Arc::clone(&handle.completion),
                Arc::clone(&handle.termination),
            )
        };
        if let Some((owner, task)) = task {
            let completed = Arc::clone(&completion);
            let reaper_administration = self.store_administration.clone();
            let reaper_owner = owner.clone();
            self.store_administration.spawn_retirement_reaper(
                reservation,
                MaintenanceReaperKind::MemoryRepair,
                owner,
                task,
                Arc::clone(&termination),
                async move {
                    {
                        reaper_administration
                            .memory_repair_schedulers()
                            .lock()
                            .await
                            .retain(|candidate, handle| {
                                candidate != &reaper_owner
                                    || !Arc::ptr_eq(&handle.completion, &completed)
                                    || handle.lifecycle != MemoryRepairSchedulerLifecycle::Retiring
                            });
                    }
                },
            );
        }
        Some(MemoryRepairSchedulerRetirement { termination })
    }

    pub(super) async fn shutdown_memory_repair_schedulers(&self) {
        // The daemon is already draining, so scheduler registration is closed.
        // Retire the owned tasks through their dedicated map instead of waiting
        // behind unrelated migration/branch administration.
        let owners: Vec<ProjectServerKey> = self
            .store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .keys()
            .cloned()
            .collect();
        let mut retirements = Vec::with_capacity(owners.len());
        for owner in owners {
            if let Some(retirement) = self.retire_memory_repair_scheduler_locked(&owner).await {
                retirements.push(retirement);
            }
        }
        self.store_administration
            .memory_repair_schedulers()
            .lock()
            .await
            .clear();
        let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            for retirement in retirements {
                retirement.wait().await;
            }
        })
        .await;
    }
}

fn observed_memory_repair_lifecycle(
    handle: &mut MemoryRepairSchedulerHandle,
) -> MemoryRepairSchedulerLifecycle {
    if handle.lifecycle == MemoryRepairSchedulerLifecycle::Running
        && handle.task.as_ref().is_none_or(JoinHandle::is_finished)
    {
        handle.lifecycle = MemoryRepairSchedulerLifecycle::Finished;
    }
    handle.lifecycle
}

async fn run_memory_repair_scheduler_loop(
    project_path: PathBuf,
    cg: Arc<crate::tracedecay::TraceDecay>,
    administration: super::branch_admin::StoreAdministration,
) {
    let tick_project = project_path.clone();
    run_memory_repair_scheduler_loop_with(
        &project_path,
        move || {
            let project_path = tick_project.clone();
            let cg = Arc::clone(&cg);
            let administration = administration.clone();
            async move {
                run_maintenance_repair_scheduler_tick(&project_path, &cg, &administration).await
            }
        },
        tokio::time::sleep,
    )
    .await;
}

async fn run_memory_repair_scheduler_loop_with<Tick, TickFuture, Wait, WaitFuture>(
    project_path: &Path,
    mut tick: Tick,
    mut wait: Wait,
) where
    Tick: FnMut() -> TickFuture,
    TickFuture: Future<Output = Result<MemoryRepairPassDecision>>,
    Wait: FnMut(Duration) -> WaitFuture,
    WaitFuture: Future<Output = ()>,
{
    let mut attempt = 0u32;
    loop {
        match tick().await {
            Ok(MemoryRepairPassDecision::Advanced) => {
                attempt = attempt.saturating_add(1);
                let delay = crate::application::host_admission::replay_backoff(
                    attempt,
                    MEMORY_REPAIR_BACKOFF_SHIFT_CAP,
                );
                log_daemon_event(
                    "memory_repair_scheduler",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "advanced".to_string()),
                        ("attempt", attempt.to_string()),
                        ("next_tick_secs", delay.as_secs().to_string()),
                    ],
                );
                wait(delay).await;
            }
            Ok(MemoryRepairPassDecision::Idle) => {
                log_daemon_event(
                    "memory_repair_scheduler",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "idle".to_string()),
                        ("attempt", attempt.to_string()),
                    ],
                );
                return;
            }
            Err(error) => {
                attempt = attempt.saturating_add(1);
                let delay = crate::application::host_admission::replay_backoff(
                    attempt,
                    MEMORY_REPAIR_BACKOFF_SHIFT_CAP,
                );
                log_daemon_event(
                    "memory_repair_scheduler",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "retry_scheduled".to_string()),
                        ("attempt", attempt.to_string()),
                        ("next_tick_ms", delay.as_millis().to_string()),
                        ("error", error.to_string()),
                    ],
                );
                wait(delay).await;
            }
        }
    }
}

async fn run_maintenance_repair_scheduler_tick(
    project_path: &Path,
    cg: &crate::tracedecay::TraceDecay,
    administration: &super::branch_admin::StoreAdministration,
) -> Result<MemoryRepairPassDecision> {
    let memory = run_memory_repair_scheduler_tick(project_path, cg).await?;
    let database = administration
        .registered_project_session_database(project_path, cg.store_layout())
        .await?;
    let session = run_session_temporal_repair_pager_with({
        let database = Arc::clone(&database);
        let project_path = project_path.to_path_buf();
        move |page_rows| {
            let database = Arc::clone(&database);
            let project_path = project_path.clone();
            async move {
                let page_rows = i64::try_from(page_rows).map_err(|_| TraceDecayError::Config {
                    message: "session temporal repair page size exceeds i64".to_owned(),
                })?;
                let outcome =
                    crate::global_db::advance_session_temporal_store_repair_with_page_rows(
                        database.as_ref(),
                        page_rows,
                    )
                    .await?;
                match outcome {
                    crate::global_db::SessionTemporalRepairOutcome::Pending { stage } => {
                        log_daemon_event(
                            "session_temporal_repair",
                            &[
                                ("project", project_path.display().to_string()),
                                ("outcome", "pending".to_string()),
                                ("stage", format!("{stage:?}")),
                                ("page_rows", page_rows.to_string()),
                            ],
                        );
                    }
                    crate::global_db::SessionTemporalRepairOutcome::Complete => {
                        log_daemon_event(
                            "session_temporal_repair",
                            &[
                                ("project", project_path.display().to_string()),
                                ("outcome", "complete".to_string()),
                            ],
                        );
                    }
                    crate::global_db::SessionTemporalRepairOutcome::NotRequired => {}
                }
                Ok(outcome)
            }
        }
    })
    .await?;
    let transcript = match crate::sessions::transcript_backfill::advance_transcript_facts_backfill(
        database.as_ref(),
    )
    .await?
    {
        crate::sessions::transcript_backfill::TranscriptFactsBackfillOutcome::Pending {
            cursor,
        } => {
            log_daemon_event(
                "transcript_facts_backfill",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "pending".to_string()),
                    ("cursor", cursor.to_string()),
                ],
            );
            MemoryRepairPassDecision::Advanced
        }
        crate::sessions::transcript_backfill::TranscriptFactsBackfillOutcome::Complete => {
            log_daemon_event(
                "transcript_facts_backfill",
                &[
                    ("project", project_path.display().to_string()),
                    ("outcome", "complete".to_string()),
                ],
            );
            MemoryRepairPassDecision::Idle
        }
        crate::sessions::transcript_backfill::TranscriptFactsBackfillOutcome::NotRequired => {
            MemoryRepairPassDecision::Idle
        }
    };
    if let Err(error) = database.release_connection_memory().await {
        log_daemon_event(
            "maintenance_repair_memory_release",
            &[
                ("project", project_path.display().to_string()),
                ("outcome", "degraded".to_string()),
                ("error", error.to_string()),
            ],
        );
    }
    crate::daemon::store_runtime::session_registry::release_process_allocator_memory();
    Ok(combine_repair_decisions(
        combine_repair_decisions(memory, session),
        transcript,
    ))
}

fn combine_repair_decisions(
    memory: MemoryRepairPassDecision,
    session: MemoryRepairPassDecision,
) -> MemoryRepairPassDecision {
    if memory == MemoryRepairPassDecision::Advanced || session == MemoryRepairPassDecision::Advanced
    {
        MemoryRepairPassDecision::Advanced
    } else {
        MemoryRepairPassDecision::Idle
    }
}

pub(super) async fn run_memory_repair_scheduler_tick(
    project_path: &Path,
    cg: &crate::tracedecay::TraceDecay,
) -> Result<MemoryRepairPassDecision> {
    let stats = cg.repair_project_memory_once().await?;
    let progress = stats.feedback_history_repair();
    let repair_outcome = memory_repair_tick_outcome(progress)?;
    // A pass that filled either repair batch may have more backlog behind the
    // cap; keep ticking instead of going idle mid-convergence. The store owns
    // the batch caps and reports saturation directly, so the scheduler no
    // longer compares counters against store-internal batch constants.
    let repair_advanced = memory_repair_pass_advanced(repair_outcome, stats.saturated());
    let repair_outcome = match repair_outcome {
        MemoryRepairTickOutcome::Incomplete => "incomplete",
        MemoryRepairTickOutcome::Complete => "complete",
        MemoryRepairTickOutcome::NotRequired => "not_required",
    };
    let cutover_progress = cg.advance_project_memory_cutover_once().await?;
    let cutover_advanced = legacy_memory_cutover_should_retry(cutover_progress);
    let cutover_outcome = if cutover_advanced {
        "incomplete"
    } else {
        "complete"
    };
    let advanced = repair_advanced || cutover_advanced;
    log_daemon_event(
        "memory_repair",
        &[
            ("project", project_path.display().to_string()),
            (
                "outcome",
                if advanced { "incomplete" } else { "complete" }.to_string(),
            ),
            ("repair_outcome", repair_outcome.to_string()),
            ("repair_processed", progress.processed().to_string()),
            ("cutover_outcome", cutover_outcome.to_string()),
            (
                "cutover_processed",
                cutover_progress.processed().to_string(),
            ),
        ],
    );
    Ok(if advanced {
        MemoryRepairPassDecision::Advanced
    } else {
        MemoryRepairPassDecision::Idle
    })
}

pub(super) fn memory_repair_tick_outcome(
    progress: CompatibilityFeedbackRepairProgressV1,
) -> Result<MemoryRepairTickOutcome> {
    match progress {
        CompatibilityFeedbackRepairProgressV1::Incomplete { .. } => {
            Ok(MemoryRepairTickOutcome::Incomplete)
        }
        CompatibilityFeedbackRepairProgressV1::Complete { .. } => {
            Ok(MemoryRepairTickOutcome::Complete)
        }
        CompatibilityFeedbackRepairProgressV1::NotRequired => {
            Ok(MemoryRepairTickOutcome::NotRequired)
        }
        CompatibilityFeedbackRepairProgressV1::Unknown => Err(TraceDecayError::Config {
            message: "daemon memory repair returned unknown feedback-history progress".to_string(),
        }),
    }
}

/// Whether the repair half of a tick still has work pending. Incomplete
/// feedback-history repair always advances; an otherwise-finished pass advances
/// only when the store reports it saturated a per-pass batch cap (so backlog
/// may remain behind the cap). The store computes saturation because it alone
/// owns the batch caps.
pub(super) fn memory_repair_pass_advanced(
    outcome: MemoryRepairTickOutcome,
    saturated: bool,
) -> bool {
    match outcome {
        MemoryRepairTickOutcome::Incomplete => true,
        MemoryRepairTickOutcome::Complete | MemoryRepairTickOutcome::NotRequired => saturated,
    }
}

pub(super) fn legacy_memory_cutover_should_retry(
    progress: CompatibilityLegacyMemoryCutoverProgressV1,
) -> bool {
    matches!(
        progress,
        CompatibilityLegacyMemoryCutoverProgressV1::Incomplete { .. }
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        MemoryRepairPassDecision, MemoryRepairTickOutcome, combine_repair_decisions,
        memory_repair_pass_advanced, next_session_temporal_page_rows,
        run_memory_repair_scheduler_loop_with, run_session_temporal_repair_pager_with,
    };
    use crate::errors::TraceDecayError;
    use crate::global_db::{SessionTemporalRepairOutcome, SessionTemporalRepairStage};

    #[test]
    fn dogfood_recovery_session_repair_keeps_shared_maintenance_loop_alive() {
        assert_eq!(
            combine_repair_decisions(
                MemoryRepairPassDecision::Idle,
                MemoryRepairPassDecision::Advanced
            ),
            MemoryRepairPassDecision::Advanced
        );
        assert_eq!(
            combine_repair_decisions(
                MemoryRepairPassDecision::Idle,
                MemoryRepairPassDecision::Idle
            ),
            MemoryRepairPassDecision::Idle
        );
    }

    #[test]
    fn saturation_flips_the_repair_decision_for_finished_passes() {
        // Incomplete feedback-history repair advances regardless of saturation.
        assert!(memory_repair_pass_advanced(
            MemoryRepairTickOutcome::Incomplete,
            false
        ));
        assert!(memory_repair_pass_advanced(
            MemoryRepairTickOutcome::Incomplete,
            true
        ));

        // A finished pass advances exactly when the store reports saturation.
        for outcome in [
            MemoryRepairTickOutcome::Complete,
            MemoryRepairTickOutcome::NotRequired,
        ] {
            assert!(
                !memory_repair_pass_advanced(outcome, false),
                "{outcome:?} must go idle when the store reports no saturation"
            );
            assert!(
                memory_repair_pass_advanced(outcome, true),
                "{outcome:?} must keep ticking when the store reports saturation"
            );
        }
    }

    #[test]
    fn session_temporal_page_rows_tracks_the_target_without_large_jumps() {
        assert_eq!(
            next_session_temporal_page_rows(256, Duration::from_millis(62)),
            512
        );
        assert_eq!(
            next_session_temporal_page_rows(1_024, Duration::from_millis(250)),
            512
        );
        assert_eq!(
            next_session_temporal_page_rows(2_048, Duration::from_millis(125)),
            2_048
        );
    }

    #[test]
    fn session_temporal_page_rows_stays_within_safe_bounds() {
        assert_eq!(
            next_session_temporal_page_rows(256, Duration::from_millis(1)),
            512
        );
        assert_eq!(
            next_session_temporal_page_rows(4_096, Duration::from_millis(1)),
            4_096
        );
        assert_eq!(
            next_session_temporal_page_rows(256, Duration::from_secs(2)),
            256
        );
    }

    #[tokio::test]
    async fn session_temporal_pager_binds_each_scheduler_tick() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let decision = run_session_temporal_repair_pager_with({
            let calls = Arc::clone(&calls);
            move |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                std::future::ready(Ok(SessionTemporalRepairOutcome::Pending {
                    stage: SessionTemporalRepairStage::AuthorityEffects,
                }))
            }
        })
        .await
        .unwrap();

        assert_eq!(decision, MemoryRepairPassDecision::Advanced);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            super::SESSION_TEMPORAL_MAX_PAGES_PER_TICK
        );
    }

    #[tokio::test]
    async fn session_temporal_pager_stops_on_completion() {
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            SessionTemporalRepairOutcome::Pending {
                stage: SessionTemporalRepairStage::AuthorityEffects,
            },
            SessionTemporalRepairOutcome::Complete,
        ])));
        let decision = run_session_temporal_repair_pager_with({
            let outcomes = Arc::clone(&outcomes);
            move |_| {
                let outcome = outcomes.lock().unwrap().pop_front().unwrap();
                std::future::ready(Ok(outcome))
            }
        })
        .await
        .unwrap();

        assert_eq!(decision, MemoryRepairPassDecision::Idle);
        assert!(outcomes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn session_temporal_pager_adapts_only_within_the_same_paged_stage() {
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            SessionTemporalRepairOutcome::Pending {
                stage: SessionTemporalRepairStage::PrepareSchema,
            },
            SessionTemporalRepairOutcome::Pending {
                stage: SessionTemporalRepairStage::AuthorityEffects,
            },
            SessionTemporalRepairOutcome::Pending {
                stage: SessionTemporalRepairStage::AuthorityEffects,
            },
            SessionTemporalRepairOutcome::Complete,
        ])));
        let requested_rows = Arc::new(Mutex::new(Vec::new()));
        run_session_temporal_repair_pager_with({
            let outcomes = Arc::clone(&outcomes);
            let requested_rows = Arc::clone(&requested_rows);
            move |page_rows| {
                requested_rows.lock().unwrap().push(page_rows);
                let outcome = outcomes.lock().unwrap().pop_front().unwrap();
                std::future::ready(Ok(outcome))
            }
        })
        .await
        .unwrap();

        assert_eq!(*requested_rows.lock().unwrap(), vec![256, 256, 256, 512]);
    }

    #[tokio::test]
    async fn transient_failure_retries_until_terminal_idle() {
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            Err(TraceDecayError::Config {
                message: "transient repair failure".to_string(),
            }),
            Ok(MemoryRepairPassDecision::Idle),
        ])));
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waits = Arc::new(Mutex::new(Vec::new()));

        run_memory_repair_scheduler_loop_with(
            Path::new("/project"),
            {
                let outcomes = Arc::clone(&outcomes);
                let attempts = Arc::clone(&attempts);
                move || {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let outcome = outcomes.lock().unwrap().pop_front().unwrap();
                    async move { outcome }
                }
            },
            {
                let waits = Arc::clone(&waits);
                move |delay| {
                    waits.lock().unwrap().push(delay);
                    std::future::ready(())
                }
            },
        )
        .await;

        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(waits.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn terminal_idle_stops_without_retry() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let waits = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        run_memory_repair_scheduler_loop_with(
            Path::new("/project"),
            {
                let attempts = Arc::clone(&attempts);
                move || {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    std::future::ready(Ok(MemoryRepairPassDecision::Idle))
                }
            },
            {
                let waits = Arc::clone(&waits);
                move |_| {
                    waits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    std::future::ready(())
                }
            },
        )
        .await;

        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(waits.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}

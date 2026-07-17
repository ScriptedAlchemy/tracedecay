//! Daemon-owned compatibility-memory repair scheduler.
//!
//! One loop per project owner drains feedback-history repair and the legacy
//! memory cutover; unlike automation, repair is never configuration-gated.
//! The loop is driven by an explicit [`MemoryRepairPassDecision`] and retries
//! on the shared bounded `replay_backoff` curve rather than a fixed delay.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::errors::{Result, TraceDecayError};
use tracedecay_store::{
    CompatibilityFeedbackRepairProgressV1, CompatibilityLegacyMemoryCutoverProgressV1,
};

use super::{
    DAEMON_TASK_ABORT_DEADLINE, DaemonEngine, DaemonHandshake, ProjectServerKey, log_daemon_event,
    open_existing_project_with_options,
};

pub(super) struct MemoryRepairSchedulerHandle {
    pub(super) task: JoinHandle<()>,
    pub(super) completion: Arc<()>,
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

impl DaemonEngine {
    /// Starts one daemon-owned compatibility-memory repair loop for this exact
    /// project owner. Unlike automation, repair is never configuration-gated.
    pub(super) async fn ensure_memory_repair_scheduler(
        &self,
        key: ProjectServerKey,
        project_path: PathBuf,
        handshake: DaemonHandshake,
    ) {
        self.store_administration
            .with_writer(|| async move {
                self.start_memory_repair_scheduler(key, project_path, handshake)
                    .await;
            })
            .await;
    }

    async fn start_memory_repair_scheduler(
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
            .memory_repair_schedulers()
            .lock()
            .await;
        if !self.lifecycle.accepting() || schedulers.contains_key(&key) {
            return;
        }

        let completion = Arc::new(());
        let completed = Arc::clone(&completion);
        let administration = self.store_administration.clone();
        let task = tokio::spawn(async move {
            Box::pin(run_memory_repair_scheduler_loop(project_path, handshake)).await;
            administration
                .memory_repair_schedulers()
                .lock()
                .await
                .retain(|_, handle| !Arc::ptr_eq(&handle.completion, &completed));
        });
        schedulers.insert(key, MemoryRepairSchedulerHandle { task, completion });
    }

    pub(super) async fn shutdown_memory_repair_schedulers(&self) {
        let scheduler_handles: Vec<JoinHandle<()>> = self
            .store_administration
            .with_writer(|| async {
                let mut schedulers = self
                    .store_administration
                    .memory_repair_schedulers()
                    .lock()
                    .await;
                schedulers.drain().map(|(_, handle)| handle.task).collect()
            })
            .await;
        for handle in &scheduler_handles {
            handle.abort();
        }
        let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
            for handle in scheduler_handles {
                let _ = handle.await;
            }
        })
        .await;
    }
}

async fn run_memory_repair_scheduler_loop(project_path: PathBuf, handshake: DaemonHandshake) {
    let mut attempt = 0u32;
    loop {
        match Box::pin(run_memory_repair_scheduler_tick(&project_path, &handshake)).await {
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
                        ("next_tick_secs", delay.as_secs().to_string()),
                    ],
                );
                tokio::time::sleep(delay).await;
            }
            Ok(MemoryRepairPassDecision::Idle) => return,
            Err(error) => {
                log_daemon_event(
                    "memory_repair_scheduler",
                    &[
                        ("project", project_path.display().to_string()),
                        ("outcome", "error".to_string()),
                        ("error", error.to_string()),
                    ],
                );
                return;
            }
        }
    }
}

pub(super) async fn run_memory_repair_scheduler_tick(
    project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<MemoryRepairPassDecision> {
    let cg = Box::pin(open_existing_project_with_options(
        project_path,
        handshake.open_options(),
    ))
    .await?;
    let stats = cg.repair_project_memory_once().await?;
    let progress = stats.feedback_history_repair();
    let repair_outcome = memory_repair_tick_outcome(progress)?;
    // A pass that filled either repair batch may have more backlog behind the
    // cap; keep ticking instead of going idle mid-convergence.
    let repair_batches_saturated = stats.missing_vectors_repaired()
        >= crate::store::memory::COMPATIBILITY_REPAIR_VECTOR_BATCH as u64
        || stats.banks_rebuilt() >= crate::store::memory::COMPATIBILITY_REPAIR_BANK_BATCH as u64;
    let (repair_outcome, repair_advanced) = match repair_outcome {
        MemoryRepairTickOutcome::Incomplete => ("incomplete", true),
        MemoryRepairTickOutcome::Complete => ("complete", repair_batches_saturated),
        MemoryRepairTickOutcome::NotRequired => ("not_required", repair_batches_saturated),
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

pub(super) fn legacy_memory_cutover_should_retry(
    progress: CompatibilityLegacyMemoryCutoverProgressV1,
) -> bool {
    matches!(
        progress,
        CompatibilityLegacyMemoryCutoverProgressV1::Incomplete { .. }
    )
}

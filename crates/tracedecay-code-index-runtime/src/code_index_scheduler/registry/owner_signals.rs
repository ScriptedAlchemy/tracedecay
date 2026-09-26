//! Change signals for one project root, and the readiness wait built on them.

use std::path::{Path, PathBuf};

use std::time::Duration;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

use tracedecay_contracts::code_index_freshness::{
    CodeIndexFreshnessReadFailureV1, CodeIndexReadinessTargetV1, CodeIndexReadinessV1,
    CodeIndexReadinessWaitReadV1,
};

use super::{CodeIndexCadenceTriggerV1, CodeIndexOwnerActivityV1, CodeIndexSchedulerRegistryV1};
use crate::code_index_scheduler::CodeIndexCadenceTelemetryV1;

/// Why the source could not be proven current before waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeIndexFreshSweepRefusedV1 {
    /// The publication authority is parked until an operator reset.
    PublicationParked,
    /// The blocking sweep did not complete.
    SweepFailed,
}

/// The registry that owns these channels is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeIndexOwnerSignalsClosedV1;

/// Every signal the registry publishes for one root: registry-wide serving
/// seats and mounts, the worktree's serving-generation changes, its owner
/// passes, pending wake and worker phase, and cadence receipts.
///
/// Subscribe before the first read. `watch::Sender::subscribe()` marks the
/// current value seen, so a change between subscribe and [`Self::changed`]
/// still wakes the waiter, and a read that misses a transition re-runs on
/// the next one instead of on a timer. The per-worktree channels exist only
/// while the worktree is mounted: `changed` returns as soon as one is
/// (re)subscribed, so a caller re-reads before depending on it.
pub struct CodeIndexOwnerSignalsV1 {
    registry: CodeIndexSchedulerRegistryV1,
    path: PathBuf,
    seats: tokio::sync::watch::Receiver<u64>,
    root_mounted: tokio::sync::watch::Receiver<u64>,
    receipts: tokio::sync::watch::Receiver<CodeIndexCadenceTelemetryV1>,
    serving: Option<tokio::sync::watch::Receiver<()>>,
    activity: Option<CodeIndexOwnerActivityV1>,
}

impl CodeIndexOwnerSignalsV1 {
    pub async fn subscribe(registry: &CodeIndexSchedulerRegistryV1, path: &Path) -> Self {
        Self {
            registry: registry.clone(),
            path: path.to_path_buf(),
            seats: registry.subscribe_serving_seats(),
            root_mounted: registry.subscribe_root_mounted(),
            receipts: registry.subscribe_cadence_receipts(),
            serving: registry.subscribe_serving_generation_changes(path).await,
            activity: registry.subscribe_owner_activity(path).await,
        }
    }

    /// Resolves once per quiescent burst of publications, so a waiter never
    /// re-reads, and contends with the worker, on every intra-pass update.
    pub async fn changed(&mut self) -> Result<(), CodeIndexOwnerSignalsClosedV1> {
        if self.serving.is_none() {
            self.serving = self
                .registry
                .subscribe_serving_generation_changes(&self.path)
                .await;
            if self.serving.is_some() {
                return Ok(());
            }
        }
        if self.activity.is_none() {
            self.activity = self.registry.subscribe_owner_activity(&self.path).await;
            if self.activity.is_some() {
                return Ok(());
            }
        }
        tokio::select! {
            changed = self.seats.changed() => changed.map_err(|_| CodeIndexOwnerSignalsClosedV1)?,
            changed = self.root_mounted.changed() => {
                changed.map_err(|_| CodeIndexOwnerSignalsClosedV1)?;
            }
            changed = self.receipts.changed() => {
                changed.map_err(|_| CodeIndexOwnerSignalsClosedV1)?;
            }
            changed = async {
                match self.serving.as_mut() {
                    Some(serving) => serving.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() {
                    self.serving = None;
                }
            }
            changed = async {
                match self.activity.as_mut() {
                    Some(activity) => activity.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() {
                    self.activity = None;
                }
            }
        }
        self.settle_burst().await;
        Ok(())
    }

    /// Consume publications until a scheduler turn passes without one.
    async fn settle_burst(&mut self) {
        loop {
            self.seats.borrow_and_update();
            self.root_mounted.borrow_and_update();
            self.receipts.borrow_and_update();
            if let Some(serving) = self.serving.as_mut() {
                serving.borrow_and_update();
            }
            tokio::task::yield_now().await;
            let pending = [
                self.seats.has_changed(),
                self.root_mounted.has_changed(),
                self.receipts.has_changed(),
            ]
            .into_iter()
            .any(|changed| changed.unwrap_or(false))
                || self
                    .serving
                    .as_ref()
                    .is_some_and(|serving| serving.has_changed().unwrap_or(false))
                || self
                    .activity
                    .as_ref()
                    .is_some_and(CodeIndexOwnerActivityV1::has_changed);
            if !pending {
                return;
            }
            if let Some(activity) = self.activity.as_mut()
                && activity.has_changed()
                && activity.changed().await.is_err()
            {
                self.activity = None;
            }
        }
    }
}

impl CodeIndexSchedulerRegistryV1 {
    /// Sweep the source witness now and post a wake for any proven change,
    /// so later freshness reads describe the source as of this call. An
    /// unmounted root has nothing to sweep; its mount reconciles.
    async fn request_fresh_now(
        &self,
        project_root: &Path,
    ) -> Result<(), CodeIndexFreshSweepRefusedV1> {
        let Ok(canonical) = canonical_existing_identity(project_root) else {
            return Ok(());
        };
        let (scheduler, pending_wake, wake) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&canonical) else {
                return Ok(());
            };
            if Self::publication_authority_reset(worktree).is_some() {
                return Err(CodeIndexFreshSweepRefusedV1::PublicationParked);
            }
            (
                std::sync::Arc::clone(&worktree.scheduler),
                std::sync::Arc::clone(&worktree.pending_wake),
                std::sync::Arc::clone(&worktree.wake),
            )
        };
        tokio::task::spawn_blocking(move || {
            let mut scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if scheduler.request_fresh_now_background() {
                Self::note_wake(
                    &pending_wake,
                    &wake,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                );
            }
        })
        .await
        .map_err(|_| CodeIndexFreshSweepRefusedV1::SweepFailed)
    }

    /// Wait until `project_root` reaches `target`, re-reading freshness only
    /// when the registry publishes a change, for at most `budget`.
    ///
    /// The wait first proves freshness against the source as it is now: the
    /// bounded Git/stat/content probe either refreshes the verified watermark
    /// or posts the wake for a proven change, so a reading taken after it
    /// cannot report an edit the scheduler has not yet seen as fresh. An
    /// unmounted root is waited through: a mount that lands inside the budget
    /// reconciles the source as of that mount. Dropping the future abandons
    /// the wait; a wake the probe posted is ordinary demand.
    pub async fn wait_for_readiness(
        &self,
        project_root: &Path,
        target: CodeIndexReadinessTargetV1,
        budget: Duration,
    ) -> Result<CodeIndexReadinessWaitReadV1, CodeIndexFreshnessReadFailureV1> {
        let deadline = tokio::time::Instant::now() + budget;
        let mut signals = CodeIndexOwnerSignalsV1::subscribe(self, project_root).await;
        // The probe can take the scheduler mutex; the caller's budget bounds
        // it, and an unproven source cannot be reported as reached.
        match tokio::time::timeout_at(deadline, self.request_fresh_now(project_root)).await {
            Ok(Err(CodeIndexFreshSweepRefusedV1::PublicationParked)) => {
                return Ok(CodeIndexReadinessWaitReadV1::Unreachable {
                    reason: "code_index_publication_parked".to_owned(),
                });
            }
            Ok(Err(CodeIndexFreshSweepRefusedV1::SweepFailed)) => {
                return Ok(CodeIndexReadinessWaitReadV1::Unreachable {
                    reason: "code_index_freshness_sweep_failed".to_owned(),
                });
            }
            Ok(Ok(())) => {}
            Err(_) => {
                return Ok(CodeIndexReadinessWaitReadV1::TimedOut {
                    last: self
                        .dashboard_freshness_read(project_root)
                        .await?
                        .map(Box::new),
                });
            }
        }
        loop {
            let last = self.dashboard_freshness_read(project_root).await?;
            if let Some(freshness) = last.as_ref() {
                match freshness.readiness(target) {
                    CodeIndexReadinessV1::Reached => {
                        return Ok(CodeIndexReadinessWaitReadV1::Reached);
                    }
                    CodeIndexReadinessV1::Unreachable { reason } => {
                        return Ok(CodeIndexReadinessWaitReadV1::Unreachable { reason });
                    }
                    CodeIndexReadinessV1::Pending => {}
                }
            }
            match tokio::time::timeout_at(deadline, signals.changed()).await {
                Err(_) => {
                    return Ok(CodeIndexReadinessWaitReadV1::TimedOut {
                        last: last.map(Box::new),
                    });
                }
                Ok(Err(CodeIndexOwnerSignalsClosedV1)) => {
                    return Ok(CodeIndexReadinessWaitReadV1::Unreachable {
                        reason: "code_index_scheduler_registry_closed".to_owned(),
                    });
                }
                Ok(Ok(())) => {}
            }
        }
    }
}

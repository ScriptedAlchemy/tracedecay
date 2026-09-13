//! Deterministic gates and observers the unit tests use to hold the registry
//! at a chosen step.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::super::{
    CodeIndexCadenceReadModelV1, CodeIndexEventToReadyReceiptV1, LatestCompleteCodeIndexV1,
};
use super::{
    CodeIndexSchedulerRegistryV1, ColdMountOpenEventV1, ColdMountOpenTestControlV1,
    ColdMountPostCheckTestControlV1, ExistingSemanticScheduleReplacementGateV1,
    PendingWakeDropGateTestV1, PendingWakeV1, PublishedTextProjectionGateV1,
    QueryAdmissionTestControlV1, ServingGenerationInstallationV1,
    ServingGenerationRollbackOutcomeV1, cold_mount_admission_barriers, cold_mount_open_controls,
    cold_mount_post_check_controls, existing_semantic_schedule_replacement_gate,
    published_text_projection_gate, query_admission_controls, unique_mounted_for_scope,
    wait_notified_if_unset,
};

impl CodeIndexSchedulerRegistryV1 {
    #[cfg(test)]
    pub async fn pause_next_published_text_projection(
        &self,
        project_root: PathBuf,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let (released, release) = tokio::sync::oneshot::channel();
        let mut gates = published_text_projection_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gates
                .insert(
                    project_root.clone(),
                    PublishedTextProjectionGateV1 { entered, release },
                )
                .is_none(),
            "one published text projection gate per worktree: {}",
            project_root.display()
        );
        (entered_observed, released)
    }

    #[cfg(test)]
    pub(super) async fn wait_for_published_text_projection_gate(project_root: &Path) {
        let gate = published_text_projection_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root);
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.await;
        }
    }

    #[cfg(test)]
    pub async fn observe_next_existing_semantic_schedule_replacement(
        &self,
        project_root: PathBuf,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let mut gate = existing_semantic_schedule_replacement_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gate.is_none(),
            "only one existing semantic schedule replacement gate may be armed at a time"
        );
        *gate = Some(ExistingSemanticScheduleReplacementGateV1 {
            project_root,
            entered,
        });
        entered_observed
    }

    #[cfg(test)]
    pub(super) fn observe_existing_semantic_schedule_replacement(project_root: &Path) {
        let gate = {
            let mut armed = existing_semantic_schedule_replacement_gate()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let matches_root = armed
                .as_ref()
                .is_some_and(|gate| gate.project_root == project_root);
            if matches_root { armed.take() } else { None }
        };
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
        }
    }

    /// Test-only observation of an exact mounted worktree's active owner pass.
    #[cfg(test)]
    pub async fn reconcile_in_progress_for_test(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let reconcile_in_progress = self
            .mounted
            .lock()
            .await
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.reconcile_in_progress));
        reconcile_in_progress
            .is_some_and(|reconcile_in_progress| reconcile_in_progress.load(Ordering::Acquire) != 0)
    }

    /// Test-only: hold an exact mounted worktree's owner-pass authority, as
    /// the worker does from claiming a wake through text projection and graph
    /// seating, without holding the scheduler mutex.
    #[cfg(test)]
    pub async fn hold_reconcile_pass_for_test(
        &self,
        project_root: &Path,
    ) -> Option<super::super::ReconcilePassGuard> {
        let project_root = project_root.canonicalize().ok()?;
        let reconcile_in_progress = self
            .mounted
            .lock()
            .await
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.reconcile_in_progress))?;
        Some(super::super::ReconcilePassGuard::enter(
            &reconcile_in_progress,
        ))
    }

    /// Serving slot only — no Git open, no freshness ladder, no wake.
    #[cfg(test)]
    pub async fn latest_complete_serving_for_test(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let serving = {
            let mounted = self.mounted.lock().await;
            Arc::clone(&mounted.get(&project_root)?.serving_generation)
        };
        serving
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub async fn expire_source_freshness_for_test(&self, project_root: &Path) {
        let project_root = project_root.canonicalize().expect("canonical test root");
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root).expect("mounted test worktree");
        worktree
            .source_freshness
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .staleness_threshold = Duration::ZERO;
        worktree
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .policy
            .staleness_threshold = Duration::ZERO;
    }

    #[cfg(test)]
    pub fn install_cold_mount_admission_barrier(&self, project_root: &Path, callers: usize) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let barrier = Arc::new(tokio::sync::Barrier::new(callers));
        let replaced = cold_mount_admission_barriers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root, barrier);
        assert!(replaced.is_none(), "cold-mount barrier already installed");
    }

    #[cfg(test)]
    pub fn install_cold_mount_post_check_gate(&self, project_root: &Path) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let replaced = cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root,
                Arc::new(ColdMountPostCheckTestControlV1 {
                    reached: AtomicBool::new(false),
                    entered: tokio::sync::Notify::new(),
                    release: tokio::sync::Notify::new(),
                }),
            );
        assert!(
            replaced.is_none(),
            "cold-mount post-check gate already installed"
        );
    }

    #[cfg(test)]
    pub async fn wait_for_cold_mount_post_check(&self, project_root: &Path) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let control = cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .cloned()
            .expect("cold-mount post-check gate");
        let entered = control.entered.notified();
        if !control.reached.load(Ordering::Acquire) {
            entered.await;
        }
    }

    #[cfg(test)]
    pub fn release_cold_mount_post_check(&self, project_root: &Path) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .expect("cold-mount post-check gate")
            .release
            .notify_one();
    }

    #[cfg(test)]
    pub fn install_cold_mount_open_gate(&self, project_root: &Path) {
        Self::install_cold_mount_open_control(project_root, true);
    }

    #[cfg(test)]
    pub fn install_cold_mount_open_observer(&self, project_root: &Path) {
        Self::install_cold_mount_open_control(project_root, false);
    }

    #[cfg(test)]
    fn install_cold_mount_open_control(project_root: &Path, blocks_open: bool) {
        let project_root = project_root
            .canonicalize()
            .expect("canonical test project root");
        let replaced = cold_mount_open_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project_root,
                Arc::new(ColdMountOpenTestControlV1::new(blocks_open)),
            );
        assert!(
            replaced.is_none(),
            "cold-mount open control already installed"
        );
    }

    #[cfg(test)]
    pub async fn wait_for_cold_mount_open_events(&self, project_root: &Path, events: usize) {
        let control =
            Self::cold_mount_open_control_for_test(project_root).expect("cold-mount open control");
        let mut changed = control.changed.subscribe();
        loop {
            let observed = control
                .events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len();
            if observed >= events {
                return;
            }
            let _ = changed.changed().await;
        }
    }

    #[cfg(test)]
    pub async fn wait_for_cold_mount_follower(&self, project_root: &Path) {
        let control =
            Self::cold_mount_open_control_for_test(project_root).expect("cold-mount open control");
        let mut changed = control.changed.subscribe();
        loop {
            if control.followers.load(Ordering::Acquire) != 0 {
                return;
            }
            let _ = changed.changed().await;
        }
    }

    #[cfg(test)]
    pub fn release_cold_mount_open_gate(&self, project_root: &Path) {
        let control =
            Self::cold_mount_open_control_for_test(project_root).expect("cold-mount open control");
        let mut released = control
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *released = true;
        control.release.notify_all();
    }

    #[cfg(test)]
    pub fn cold_mount_open_events(&self, project_root: &Path) -> Vec<ColdMountOpenEventV1> {
        Self::cold_mount_open_control_for_test(project_root)
            .expect("cold-mount open control")
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(test)]
    pub fn subscribe_cold_mount_cancellation(
        &self,
        project_root: &Path,
    ) -> Option<tokio::sync::watch::Receiver<()>> {
        let project_root = project_root.canonicalize().ok()?;
        self.cold_mount_reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .map(|slot| slot.cancellation.subscribe())
    }

    #[cfg(test)]
    pub fn install_query_admission_barrier(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
        callers: usize,
    ) {
        let control = Arc::new(QueryAdmissionTestControlV1 {
            lookup_gate: tokio::sync::Mutex::new(()),
            rendezvous: tokio::sync::Barrier::new(callers),
            pauses_after_claim: AtomicBool::new(false),
            claim_reached: AtomicBool::new(false),
            claim_entered: tokio::sync::Notify::new(),
            claim_release: tokio::sync::Notify::new(),
        });
        let replaced = query_admission_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.worktree_id.clone(), control);
        assert!(
            replaced.is_none(),
            "query-admission barrier already installed"
        );
    }

    #[cfg(test)]
    pub fn install_query_claim_gate(&self, scope: &tracedecay_contracts::ResolvedScope) {
        let control = Arc::new(QueryAdmissionTestControlV1 {
            lookup_gate: tokio::sync::Mutex::new(()),
            rendezvous: tokio::sync::Barrier::new(1),
            pauses_after_claim: AtomicBool::new(true),
            claim_reached: AtomicBool::new(false),
            claim_entered: tokio::sync::Notify::new(),
            claim_release: tokio::sync::Notify::new(),
        });
        let replaced = query_admission_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(scope.worktree_id.clone(), control);
        assert!(replaced.is_none(), "query-claim gate already installed");
    }

    #[cfg(test)]
    pub async fn wait_for_query_claim(&self, scope: &tracedecay_contracts::ResolvedScope) {
        let control = Self::query_admission_control_for_test(scope).expect("query-claim gate");
        wait_notified_if_unset(&control.claim_reached, &control.claim_entered).await;
    }

    #[cfg(test)]
    pub fn release_query_claim(&self, scope: &tracedecay_contracts::ResolvedScope) {
        Self::query_admission_control_for_test(scope)
            .expect("query-claim gate")
            .claim_release
            .notify_one();
    }

    #[cfg(test)]
    pub(super) async fn pause_cold_mount_admission_for_test(project_root: &Path) {
        let barrier = cold_mount_admission_barriers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
    }

    #[cfg(test)]
    pub(super) async fn pause_cold_mount_after_outer_check_for_test(project_root: &Path) {
        let control = cold_mount_post_check_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned();
        let Some(control) = control else {
            return;
        };
        control.reached.store(true, Ordering::Release);
        control.entered.notify_waiters();
        control.release.notified().await;
    }

    #[cfg(test)]
    fn cold_mount_open_control_for_test(
        project_root: &Path,
    ) -> Option<Arc<ColdMountOpenTestControlV1>> {
        let project_root = project_root.canonicalize().ok()?;
        cold_mount_open_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_root)
            .cloned()
    }

    #[cfg(test)]
    pub(super) fn pause_cold_mount_open_for_test(project_root: &Path) {
        let Some(control) = Self::cold_mount_open_control_for_test(project_root) else {
            return;
        };
        control.record(ColdMountOpenEventV1::Started);
        if control.blocks_open {
            let mut released = control
                .released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*released {
                released = control
                    .release
                    .wait(released)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn finish_cold_mount_open_for_test(project_root: &Path) {
        if let Some(control) = Self::cold_mount_open_control_for_test(project_root) {
            control.record(ColdMountOpenEventV1::Finished);
        }
    }

    #[cfg(test)]
    pub(super) fn note_cold_mount_follower_for_test(project_root: &Path) {
        if let Some(control) = Self::cold_mount_open_control_for_test(project_root) {
            control.record_follower();
        }
    }

    #[cfg(test)]
    pub(super) fn query_admission_control_for_test(
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<Arc<QueryAdmissionTestControlV1>> {
        query_admission_controls()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&scope.worktree_id)
            .cloned()
    }

    #[cfg(test)]
    async fn pending_wake_for_scope_for_test(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<Arc<PendingWakeV1>> {
        let mounted = self.mounted.lock().await;
        unique_mounted_for_scope(&mounted, scope)
            .unique()
            .map(|(_, worktree)| Arc::clone(&worktree.pending_wake))
    }

    #[cfg(test)]
    pub async fn install_pending_wake_drop_gate(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) {
        let mounted = self.mounted.lock().await;
        let pending_wake = mounted
            .values()
            .find(|worktree| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            })
            .map(|worktree| Arc::clone(&worktree.pending_wake))
            .expect("mounted worktree");
        let replaced = pending_wake
            .drop_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(Arc::new(PendingWakeDropGateTestV1::new()));
        assert!(
            replaced.is_none(),
            "pending-wake drop gate already installed"
        );
    }

    #[cfg(test)]
    async fn pending_wake_drop_gate_for_test(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Arc<PendingWakeDropGateTestV1> {
        let pending_wake = self
            .pending_wake_for_scope_for_test(scope)
            .await
            .expect("mounted worktree");
        pending_wake
            .drop_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("pending-wake drop gate")
    }

    #[cfg(test)]
    pub async fn wait_for_pending_wake_claim_drop(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) {
        let gate = self.pending_wake_drop_gate_for_test(scope).await;
        wait_notified_if_unset(&gate.drop_reached, &gate.drop_entered).await;
    }

    #[cfg(test)]
    pub async fn wait_for_foreign_pending_wake_attempt(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) {
        let gate = self.pending_wake_drop_gate_for_test(scope).await;
        wait_notified_if_unset(&gate.foreign_attempted, &gate.foreign_entered).await;
    }

    #[cfg(test)]
    pub async fn release_pending_wake_claim_drop(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) {
        let gate = self.pending_wake_drop_gate_for_test(scope).await;
        let mut released = gate
            .drop_released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *released = true;
        gate.drop_release.notify_all();
    }

    /// The pending-wake slot for one exact scope's worktree, in unix micros;
    /// `0` means no wake is outstanding.
    #[cfg(test)]
    pub async fn pending_wake_micros_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<u64> {
        let mounted = self.mounted.lock().await;
        mounted
            .values()
            .find(|worktree| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            })
            .map(|worktree| {
                worktree
                    .pending_wake
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .micros
            })
    }

    /// The exact-source currency witness for one mounted root, so tests can
    /// stage the unproven-seat state a restart restore leaves behind.
    #[cfg(test)]
    pub(crate) async fn serving_source_witness_for_root(
        &self,
        project_root: &Path,
    ) -> Option<Arc<RwLock<Option<super::super::ServingSourceWitnessV1>>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.serving_source_witness))
    }

    /// The shared source-freshness fence for one mounted root, so tests can
    /// age its bounded proof instead of waiting the bound out in wall clock.
    #[cfg(test)]
    pub(crate) async fn source_freshness_for_root(
        &self,
        project_root: &Path,
    ) -> Option<super::super::SourceFreshnessFenceV1> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| worktree.source_freshness.clone())
    }

    /// Drop the retained serving generation, reproducing a mount whose restore
    /// produced nothing servable.
    #[cfg(test)]
    pub async fn clear_serving_generation_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) {
        let mounted = self.mounted.lock().await;
        for worktree in mounted.values() {
            if worktree.repository_id == scope.repository_id
                && worktree.worktree_id == scope.worktree_id
            {
                let mut serving = worktree
                    .serving_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *serving = None;
                *worktree
                    .text_generation
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                *worktree
                    .serving_source_witness
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                worktree
                    .serving_generation_epoch
                    .fetch_add(1, Ordering::AcqRel);
                worktree.serving_generation_changed.send_replace(());
            }
        }
    }

    /// Retires the serving generation only when this operation's metadata
    /// rollback succeeded and its exact installation token is still current.
    #[cfg(test)]
    pub async fn retire_owned_serving_generation(
        &self,
        project_root: &Path,
        installation: ServingGenerationInstallationV1,
    ) -> ServingGenerationRollbackOutcomeV1 {
        self.resolve_serving_generation_installation(project_root, &installation.claim, true)
            .await
    }

    /// Latest completed event-to-ready receipt for this registry, if any.
    #[cfg(test)]
    pub fn latest_event_to_ready_receipt(&self) -> Option<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest()
            .cloned()
    }

    /// Every retained event-to-ready receipt, oldest first.
    #[cfg(test)]
    pub fn event_to_ready_receipts(&self) -> Vec<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receipts()
            .cloned()
            .collect()
    }

    /// Bounded truthful cadence read model over the retained receipts.
    ///
    /// Percentiles are withheld until the retained population reaches the floor
    /// each one declares, and receipts with an unobservable arrival are reported
    /// as unavailable rather than counted as zero-latency samples.
    #[cfg(test)]
    pub fn cadence_read_model(&self) -> CodeIndexCadenceReadModelV1 {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .read_model()
    }

    /// Test support for proving the explicit same-store build/publication
    /// invariant independently of the scheduler metadata mutex.
    #[cfg(test)]
    pub async fn build_publication_lock_handle(
        &self,
        project_root: &Path,
    ) -> Option<Arc<tokio::sync::Mutex<()>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.build_publication_lock))
    }

    #[cfg(test)]
    pub async fn retiring_owner_count(&self) -> usize {
        self.retiring.lock().await.len()
    }
}

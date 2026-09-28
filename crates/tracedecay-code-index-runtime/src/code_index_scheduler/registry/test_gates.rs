//! Deterministic gates and observers the unit tests use to hold the registry
//! at a chosen step.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use super::super::{
    CodeIndexCadenceReadModelV1, CodeIndexEventToReadyReceiptV1, LatestCompleteCodeIndexV1,
};
use super::{
    CodeIndexSchedulerRegistryV1, ColdMountOpenEventV1, ColdMountOpenTestControlV1,
    ColdMountPostCheckTestControlV1, PendingWakeDropGateTestV1, PendingWakeV1,
    QueryAdmissionTestControlV1, ServingGenerationInstallationV1,
    ServingGenerationRollbackOutcomeV1, WorkerStepGateV1, cold_mount_admission_barriers,
    cold_mount_open_controls, cold_mount_post_check_controls, published_text_projection_gate,
    query_admission_controls, serving_swap_gate, unique_mounted_for_scope, wait_notified_if_unset,
};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

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
                .insert(project_root.clone(), WorkerStepGateV1 { entered, release })
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

    /// Hold the next graph tail of the worker for `project_root` right before
    /// it seats the decoded generation. The first receiver resolves once the
    /// worker waits there; sending on the returned sender releases it.
    #[cfg(test)]
    pub fn pause_next_serving_swap(
        &self,
        project_root: PathBuf,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let (released, release) = tokio::sync::oneshot::channel();
        let replaced = serving_swap_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root, WorkerStepGateV1 { entered, release });
        assert!(replaced.is_none(), "one serving swap gate per worktree");
        (entered_observed, released)
    }

    #[cfg(test)]
    pub(super) async fn wait_for_serving_swap_gate(project_root: &Path) {
        let gate = serving_swap_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root);
        if let Some(gate) = gate {
            let _ = gate.entered.send(());
            let _ = gate.release.await;
        }
    }

    /// Test-only observation of an exact mounted worktree's active owner pass.
    #[cfg(test)]
    pub async fn reconcile_in_progress_for_test(&self, project_root: &Path) -> bool {
        let Ok(project_root) = canonical_existing_identity(project_root) else {
            return false;
        };
        let reconcile_in_progress = self
            .mounted
            .lock()
            .await
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.reconcile_in_progress));
        reconcile_in_progress.is_some_and(|reconcile_in_progress| reconcile_in_progress.running())
    }

    /// Test-only: hold an exact mounted worktree's owner-pass authority, as
    /// the worker does from claiming a wake through text projection and graph
    /// seating, without holding the scheduler mutex.
    #[cfg(test)]
    pub async fn hold_reconcile_pass_for_test(
        &self,
        project_root: &Path,
    ) -> Option<super::super::ReconcilePassGuard> {
        let project_root = canonical_existing_identity(project_root).ok()?;
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

    /// Serving slot only, no Git open, no freshness ladder, no wake.
    #[cfg(test)]
    pub async fn latest_complete_serving_for_test(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = canonical_existing_identity(project_root).ok()?;
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
    pub fn install_cold_mount_admission_barrier(&self, project_root: &Path, callers: usize) {
        let project_root =
            canonical_existing_identity(project_root).expect("canonical test project root");
        let barrier = Arc::new(tokio::sync::Barrier::new(callers));
        let replaced = cold_mount_admission_barriers()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root, barrier);
        assert!(replaced.is_none(), "cold-mount barrier already installed");
    }

    #[cfg(test)]
    pub fn install_cold_mount_post_check_gate(&self, project_root: &Path) {
        let project_root =
            canonical_existing_identity(project_root).expect("canonical test project root");
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
        let project_root =
            canonical_existing_identity(project_root).expect("canonical test project root");
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
        let project_root =
            canonical_existing_identity(project_root).expect("canonical test project root");
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
        let project_root =
            canonical_existing_identity(project_root).expect("canonical test project root");
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
        let project_root = canonical_existing_identity(project_root).ok()?;
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
        let project_root = canonical_existing_identity(project_root).ok()?;
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

    /// The mounted root that owns one exact scope's worktree.
    #[cfg(test)]
    pub async fn mounted_root_for_scope_for_test(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<std::path::PathBuf> {
        let mounted = self.mounted.lock().await;
        mounted
            .iter()
            .find(|(_, worktree)| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            })
            .map(|(root, _)| root.clone())
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
            .map(|worktree| worktree.pending_wake.lock().micros)
    }

    /// The pending-wake slot for one exact mounted root, in unix micros; `0`
    /// means no wake is outstanding. A pass that ends while a wake is already
    /// pending re-arms a busy follow-up whose receipt lands later, so a test
    /// pinning wake or receipt accounting needs this as well as
    /// `reconcile_in_progress_for_test`.
    #[cfg(test)]
    pub(crate) async fn pending_wake_micros_for_root(&self, project_root: &Path) -> Option<u64> {
        let project_root = canonical_existing_identity(project_root).ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| worktree.pending_wake.lock().micros)
    }

    /// The exact-source currency witness for one mounted root, so tests can
    /// stage the unproven-seat state a restart restore leaves behind.
    #[cfg(test)]
    pub(crate) async fn serving_source_witness_for_root(
        &self,
        project_root: &Path,
    ) -> Option<Arc<RwLock<Option<super::super::ServingSourceWitnessV1>>>> {
        let project_root = canonical_existing_identity(project_root).ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.serving_source_witness))
    }

    /// One mounted root's scheduler mutex, the lock every step that renews the
    /// source proof must hold, so a test can age that proof and read it back
    /// without a pass tail re-proving it in between.
    #[cfg(test)]
    pub(crate) async fn scheduler_for_root(
        &self,
        project_root: &Path,
    ) -> Option<Arc<std::sync::Mutex<super::super::CodeIndexWorktreeSchedulerV1>>> {
        let project_root = canonical_existing_identity(project_root).ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.scheduler))
    }

    /// The shared source-freshness fence for one mounted root, so tests can
    /// age its bounded proof instead of waiting the bound out in wall clock.
    #[cfg(test)]
    pub(crate) async fn source_freshness_for_root(
        &self,
        project_root: &Path,
    ) -> Option<super::super::SourceFreshnessFenceV1> {
        let project_root = canonical_existing_identity(project_root).ok()?;
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
        self.cadence_telemetry.borrow().latest().cloned()
    }

    /// Every retained event-to-ready receipt, oldest first.
    #[cfg(test)]
    pub fn event_to_ready_receipts(&self) -> Vec<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .borrow()
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
        self.cadence_telemetry.borrow().read_model()
    }

    /// Test support for proving the explicit same-store build/publication
    /// invariant independently of the scheduler metadata mutex.
    #[cfg(test)]
    pub async fn build_publication_lock_handle(
        &self,
        project_root: &Path,
    ) -> Option<Arc<tokio::sync::Mutex<()>>> {
        let project_root = canonical_existing_identity(project_root).ok()?;
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

/// Holds the publication's text projection after its build reservation opens,
/// with a resident buffer the projection task owns until the test releases it.
#[cfg(test)]
struct PublishedTextOverlapHoldV1 {
    entered: tokio::sync::oneshot::Sender<()>,
    allocate: tokio::sync::oneshot::Receiver<()>,
    held: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
    hold_bytes: usize,
}

#[cfg(test)]
fn published_text_overlap_hold() -> &'static Mutex<BTreeMap<PathBuf, PublishedTextOverlapHoldV1>> {
    static GATE: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, PublishedTextOverlapHoldV1>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Two-phase hold around the sealed graph growth sampler: the sample phase
/// opens after the sampler starts, and the recorded phase opens after the
/// growth has been stored.
#[cfg(test)]
struct GraphGrowthWindowGateV1 {
    sampled: Option<tokio::sync::oneshot::Sender<()>>,
    release_sample: Option<tokio::sync::oneshot::Receiver<()>>,
    recorded: Option<tokio::sync::oneshot::Sender<()>>,
    release_recorded: Option<tokio::sync::oneshot::Receiver<()>>,
}

#[cfg(test)]
fn graph_growth_window_gate() -> &'static Mutex<BTreeMap<PathBuf, GraphGrowthWindowGateV1>> {
    static GATE: std::sync::OnceLock<Mutex<BTreeMap<PathBuf, GraphGrowthWindowGateV1>>> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
fn graph_measurement_publish_gate() -> &'static Mutex<BTreeSet<PathBuf>> {
    static GATE: std::sync::OnceLock<Mutex<BTreeSet<PathBuf>>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| Mutex::new(BTreeSet::new()))
}

impl CodeIndexSchedulerRegistryV1 {
    /// Hold the next publication text projection after it opens its build
    /// reservation. The projection allocates `hold_bytes` of resident memory
    /// only once `allocate` is sent, which the caller does after the graph
    /// growth sampler has started, and drops it when `release` is sent.
    #[cfg(test)]
    pub fn pause_published_text_overlap_hold(
        &self,
        project_root: PathBuf,
        hold_bytes: usize,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, entered_observed) = tokio::sync::oneshot::channel();
        let (allocate_sender, allocate) = tokio::sync::oneshot::channel();
        let (held, held_observed) = tokio::sync::oneshot::channel();
        let (release_sender, release) = tokio::sync::oneshot::channel();
        let mut gates = published_text_overlap_hold()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gates
                .insert(
                    project_root,
                    PublishedTextOverlapHoldV1 {
                        entered,
                        allocate,
                        held,
                        release,
                        hold_bytes,
                    },
                )
                .is_none(),
            "one published text overlap hold per worktree"
        );
        (
            entered_observed,
            allocate_sender,
            held_observed,
            release_sender,
        )
    }

    #[cfg(test)]
    pub(super) async fn wait_for_published_text_overlap_hold(project_root: &Path) {
        let gate = published_text_overlap_hold()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root);
        let Some(gate) = gate else {
            return;
        };
        let _ = gate.entered.send(());
        let _ = gate.allocate.await;
        let hold = vec![1_u8; gate.hold_bytes];
        let _ = gate.held.send(());
        let _ = gate.release.await;
        drop(hold);
    }

    /// Hold the next sealed-graph growth window: `sampled` resolves after the
    /// resident sampler starts, and `recorded` resolves after that growth is
    /// stored. Releasing each sender lets the worker continue.
    #[cfg(test)]
    pub fn pause_graph_growth_window(
        &self,
        project_root: PathBuf,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (sampled, sampled_observed) = tokio::sync::oneshot::channel();
        let (release_sample_sender, release_sample) = tokio::sync::oneshot::channel();
        let (recorded, recorded_observed) = tokio::sync::oneshot::channel();
        let (release_recorded_sender, release_recorded) = tokio::sync::oneshot::channel();
        let mut gates = graph_growth_window_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gates
                .insert(
                    project_root,
                    GraphGrowthWindowGateV1 {
                        sampled: Some(sampled),
                        release_sample: Some(release_sample),
                        recorded: Some(recorded),
                        release_recorded: Some(release_recorded),
                    },
                )
                .is_none(),
            "one graph growth window per worktree"
        );
        (
            sampled_observed,
            release_sample_sender,
            recorded_observed,
            release_recorded_sender,
        )
    }

    #[cfg(test)]
    pub(super) async fn wait_for_graph_growth_sample(project_root: &Path) {
        let (sampled, release_sample) = {
            let mut gates = graph_growth_window_gate()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(gate) = gates.get_mut(project_root) else {
                return;
            };
            (gate.sampled.take(), gate.release_sample.take())
        };
        if let Some(sampled) = sampled {
            let _ = sampled.send(());
        }
        if let Some(release_sample) = release_sample {
            let _ = release_sample.await;
        }
    }

    #[cfg(test)]
    pub(super) async fn wait_for_graph_growth_recorded(project_root: &Path) {
        let gate = graph_growth_window_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root);
        let Some(gate) = gate else {
            return;
        };
        if let Some(recorded) = gate.recorded {
            let _ = recorded.send(());
        }
        if let Some(release_recorded) = gate.release_recorded {
            let _ = release_recorded.await;
        }
    }

    /// The next memory-authority graph publish for `project_root` runs the
    /// sealed row build inside the growth window and is recorded as published.
    #[cfg(test)]
    pub fn arm_graph_measurement_publish(&self, project_root: PathBuf) {
        let mut gates = graph_measurement_publish_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            gates.insert(project_root),
            "one graph measurement publish per worktree"
        );
    }

    #[cfg(test)]
    pub(super) fn take_graph_measurement_publish(project_root: &Path) -> bool {
        graph_measurement_publish_gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root)
    }

    /// Build the sealed generation's code-graph rows the way publication does,
    /// from the on-disk segments, and drop them. The caller is inside the
    /// graph growth sampler window.
    #[cfg(test)]
    pub(super) fn build_overlapping_sealed_graph(
        scheduler: &Arc<Mutex<super::super::reconcile::CodeIndexWorktreeSchedulerV1>>,
        generation_id: &tracedecay_domain::CodeGenerationId,
    ) -> Result<(), String> {
        let binding = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .code_graph_replay_binding(generation_id)
            .map_err(|error| error.to_string())?;
        let digest = tracedecay_domain::sha256_hex_suffix(binding.sealed_state_digest.as_str())
            .ok_or_else(|| "sealed replay digest is not sha256".to_owned())?;
        let sealed_manifest = std::fs::read(
            binding
                .generations_root
                .join(format!("generation-{digest}.json")),
        )
        .map_err(|error| error.to_string())?;
        let segments_root =
            tracedecay_code_index_retention::code_index_generations::code_generation_segments_root(
                binding
                    .generations_root
                    .parent()
                    .ok_or_else(|| "generation root has no store root".to_owned())?,
            );
        let source =
            crate::code_index::production::SealedGenerationFileWindowsV1::open(&sealed_manifest)
                .map_err(|error| error.to_string())?;
        let projector_revision = tracedecay_graph_db::GraphProjectorRevision::try_from(
            crate::code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
        )
        .map_err(|error| error.to_string())?;
        let projection = crate::code_index::graph_projection::code_graph_projection_identity(
            tracedecay_graph_db::GraphNamespace::new("code-graph")
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let scratch = tempfile::TempDir::new().map_err(|error| error.to_string())?;
        let spilled = crate::code_index::graph_projection::build_sealed_code_graph_rows(
            projection.clone(),
            &source,
            &mut |request, buffer| {
                let crate::code_index::production::SealedGenerationSegmentReadV1::Whole {
                    digest,
                    ..
                } = request
                else {
                    panic!("overlapping graph measurement reads whole file segments");
                };
                let segment_digest =
                    tracedecay_domain::sha256_hex_suffix(digest.as_str()).expect("segment digest");
                *buffer =
                    std::fs::read(segments_root.join(format!("segment-{segment_digest}.json")))
                        .expect("sealed segment");
                Ok(())
            },
            &projector_revision,
            tracedecay_graph_db::GraphGenerationRowSpill::create(
                scratch.path().join("rows"),
                projection,
            )
            .map_err(|error| error.to_string())?,
            &|| Ok(()),
        )
        .map_err(|error| error.to_string())?;
        let _manifest = spilled
            .materialize(&|| Ok(()))
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

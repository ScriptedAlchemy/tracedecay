//! Worktree mount: cold-mount admission through the first serving generation.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use tracedecay_code_index_retention::code_index_generations::wait_for_code_generation_store_release;
use tracedecay_contracts::code_index_freshness::{
    CodeIndexBuildBlockedReasonV1, CodeIndexConvergenceParkedV1,
};
use tracedecay_domain::{IndexPathPolicyV1, ProjectId};

use super::super::{
    CodeIndexCadenceTriggerV1, CodeIndexHintPolicyV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, LatestCodeTextGenerationV1,
    graph_activation::{CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1},
    reconcile_panic_guard::ReconcileCapacityRetryV1,
};
use super::{
    CONVERGENCE_PARK_PUBLICATION_CORRUPTION_REMEDIATION_V1,
    CONVERGENCE_PARK_PUBLICATION_RESET_FAILED_REMEDIATION_V1,
    CONVERGENCE_PARK_STORE_RELEASE_WAIT_REMEDIATION_V1, CodeIndexSchedulerRegistryV1,
    ColdMountAdmissionV1, ColdMountReservationV1, MountedCodeIndexWorktreeV1, PendingWakeV1,
    ServingGenerationSlot, park_convergence, park_panicked_worker,
};
use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};

use crate::code_index_task_support::spawn_detached;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

mod worker;

/// What the worker does with a reconcile pass that found the durable
/// publication corrupt.
enum PublicationAuthorityResetV1 {
    /// The derived store was deleted; the next pass rebuilds it from source.
    Rebuilding,
    /// Another owner holds the store lock; the reset is retried when it is
    /// released and does not spend the one-shot budget.
    StoreBusy,
    /// The mount is parked until the operator acts.
    Terminal {
        reason: String,
        remediation: &'static str,
    },
}

/// Why a wait for the code-generation store lock ended without a release.
pub(super) enum StoreReleaseWaitStopV1 {
    /// Shutdown or retirement of this worker ended the wait.
    Shutdown,
    /// The kernel wait itself failed. The worktree parks until the next wake.
    Unavailable(String),
}

/// Why graph prepare produced no generation to seat.
enum GraphPrepareStopV1 {
    /// The build or decode does not fit resident memory yet.
    ResidentMemory(String),
    /// Another holder has the code-generation store lock; it releases on its own.
    StoreBusy(String),
    /// The active generation could not be decoded; a retry needs a new wake.
    DecodeFailed(String),
}

/// Why a reconcile pass's blocking step returned nothing. A panic is input
/// that reproduces on every pass, which the panic guard bounds.
#[derive(Debug, thiserror::Error)]
enum ReconcileStepFailureV1 {
    #[error("reconcile step panicked: {0}")]
    Panicked(String),
    #[error("{0}")]
    Unavailable(String),
}

impl ReconcileStepFailureV1 {
    fn is_panic(&self) -> bool {
        matches!(self, Self::Panicked(_))
    }
}

/// Runs a reconcile step on the native blocking pool, keeping a panic apart
/// from a runtime failure.
async fn run_reconcile_step<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ReconcileStepFailureV1> {
    let step = native_runtime::spawn_blocking(move || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
    });
    match step.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(payload)) => Err(ReconcileStepFailureV1::Panicked(
            payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default(),
        )),
        Err(error) => Err(ReconcileStepFailureV1::Unavailable(error)),
    }
}

impl CodeIndexSchedulerRegistryV1 {
    /// Arm one delayed wake for capacity another holder releases without
    /// waking this worktree; `false` once the bound is spent.
    fn arm_capacity_retry(
        capacity_retry: &mut ReconcileCapacityRetryV1,
        wake: &Arc<tokio::sync::Notify>,
    ) -> bool {
        let Some(delay) = capacity_retry.record_capacity_failure() else {
            return false;
        };
        Self::wake_after(delay, wake);
        true
    }

    /// Wake the worker once `delay` has passed. A delay no native timer can
    /// time is logged and dropped: the worktree waits for its next real wake.
    fn wake_after(delay: std::time::Duration, wake: &Arc<tokio::sync::Notify>) {
        let wake = Arc::clone(wake);
        let deadline = Instant::now() + delay;
        spawn_detached("code_index_delayed_wake", async move {
            match native_runtime::sleep_until(deadline).await {
                Ok(()) => wake.notify_one(),
                Err(error) => tracing::warn!(
                    event = "code_index_delayed_wake_unavailable",
                    %error,
                    "a delayed code-index wake has no native timer"
                ),
            }
        });
    }

    /// Block this worker until the holder of the store lock that refused the
    /// previous pass releases it. A follow-up wake banked during the wait must
    /// not start another failing pass: the worker does not sit on `Notify`
    /// here. The holder may be another process, so the kernel lock is the
    /// signal. Shutdown ends the wait; a wait that cannot complete parks the
    /// worktree typed.
    pub(super) async fn wait_for_worker_store_release(
        store_root: &Path,
        shutting_down: &AtomicBool,
        serving_generation_changed: &tokio::sync::watch::Sender<()>,
    ) -> Result<(), StoreReleaseWaitStopV1> {
        if shutting_down.load(Ordering::Acquire) {
            return Err(StoreReleaseWaitStopV1::Shutdown);
        }
        let mut shutdown_observed = serving_generation_changed.subscribe();
        let (released_tx, mut released) = tokio::sync::oneshot::channel();
        let root = store_root.to_path_buf();
        if let Err(error) = std::thread::Builder::new()
            .name("code-index-store-release".to_owned())
            .spawn(move || {
                let _ = released_tx.send(wait_for_code_generation_store_release(&root));
            })
        {
            return Err(StoreReleaseWaitStopV1::Unavailable(error.to_string()));
        }
        loop {
            if shutting_down.load(Ordering::Acquire) {
                return Err(StoreReleaseWaitStopV1::Shutdown);
            }
            tokio::select! {
                released = &mut released => {
                    return match released {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => {
                            Err(StoreReleaseWaitStopV1::Unavailable(error.to_string()))
                        }
                        Err(_) => Err(StoreReleaseWaitStopV1::Unavailable(
                            "code-index store release wait ended without a result".to_owned(),
                        )),
                    };
                }
                changed = shutdown_observed.changed() => {
                    if changed.is_err() || shutting_down.load(Ordering::Acquire) {
                        return Err(StoreReleaseWaitStopV1::Shutdown);
                    }
                }
            }
        }
    }

    fn park_store_release_unavailable(
        convergence_park: &RwLock<Option<CodeIndexConvergenceParkedV1>>,
        reason: String,
    ) {
        tracing::warn!(
            event = "code_index_store_release_wait_failed",
            error = %reason,
            "the worktree cannot wait for the code-generation store lock; parked until the next wake"
        );
        park_convergence(
            convergence_park,
            reason,
            CONVERGENCE_PARK_STORE_RELEASE_WAIT_REMEDIATION_V1,
            Some(CodeIndexBuildBlockedReasonV1::ArtifactStoreUnavailable),
            true,
        );
    }

    /// Wait until the holder of the store lock that refused a cold open lets
    /// go of it. The holder may be another process, so the kernel lock wait
    /// is the signal; shutdown or retirement of this reservation ends the wait.
    async fn wait_for_cold_open_store_release(
        store_root: &Path,
        reservation: &ColdMountReservationV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let mut cancellation = reservation.slot.cancellation.subscribe();
        let cancelled = || {
            CodeIndexSchedulerErrorV1::Identity(if reservation.slot.is_retired() {
                "code-index scheduler owner is still retiring".to_owned()
            } else {
                "code-index scheduler is shutting down".to_owned()
            })
        };
        if reservation.slot.is_cancelled() {
            return Err(cancelled());
        }
        let (released_tx, released) = tokio::sync::oneshot::channel();
        let root = store_root.to_path_buf();
        std::thread::Builder::new()
            .name("code-index-store-release".to_owned())
            .spawn(move || {
                let _ = released_tx.send(wait_for_code_generation_store_release(&root));
            })?;
        tokio::select! {
            released = released => match released {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(std::io::Error::other(error.to_string()).into()),
                Err(_) => Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index store release wait ended without a result".to_owned(),
                )),
            },
            _ = cancellation.changed() => Err(cancelled()),
        }
    }

    /// Spend this mount's single automatic reset on a corrupt publication.
    ///
    /// The reset is attempted once per mount so a store that is corrupt again
    /// after its own rebuild cannot cycle full re-indexes; a daemon restart
    /// (or retire/remount) grants exactly one more attempt.
    fn reset_corrupt_publication_authority(
        scheduler: &Mutex<CodeIndexWorktreeSchedulerV1>,
        reset_attempted: &mut bool,
        corruption: &CodeIndexSchedulerErrorV1,
    ) -> PublicationAuthorityResetV1 {
        if *reset_attempted {
            tracing::warn!(
                event = "code_index_publication_authority_corrupt_after_reset",
                path = "background_worker",
                error = %corruption,
                "code-index publication is corrupt again after its rebuild; parked until the daemon restarts"
            );
            return PublicationAuthorityResetV1::Terminal {
                reason: corruption.to_string(),
                remediation: CONVERGENCE_PARK_PUBLICATION_CORRUPTION_REMEDIATION_V1,
            };
        }
        let reset = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reset_corrupt_publication_authority();
        match reset {
            Ok(receipt) => {
                *reset_attempted = true;
                tracing::warn!(
                    event = "code_index_publication_authority_reset",
                    path = "background_worker",
                    removed_entries = receipt.removed_entries,
                    removed_bytes = receipt.removed_bytes,
                    error = %corruption,
                    "corrupt derived code-index publication deleted; rebuilding from source"
                );
                PublicationAuthorityResetV1::Rebuilding
            }
            Err(busy) if busy.is_store_lock_contended() => PublicationAuthorityResetV1::StoreBusy,
            Err(failure) => {
                *reset_attempted = true;
                tracing::warn!(
                    event = "code_index_publication_authority_reset_failed",
                    path = "background_worker",
                    error = %corruption,
                    reset_error = %failure,
                    "corrupt derived code-index publication could not be deleted; parked until the operator acts"
                );
                PublicationAuthorityResetV1::Terminal {
                    reason: format!("{corruption}; reset failed: {failure}"),
                    remediation: CONVERGENCE_PARK_PUBLICATION_RESET_FAILED_REMEDIATION_V1,
                }
            }
        }
    }

    #[cfg(test)]
    pub fn open_worktree(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
    ) -> Result<CodeIndexWorktreeSchedulerV1, CodeIndexSchedulerErrorV1> {
        if self.max_worktrees == 0 {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is zero".to_owned(),
            ));
        }
        let producer_incarnation = self.mint_progress_producer_incarnation()?;
        CodeIndexWorktreeSchedulerV1::open(
            project_id,
            project_root,
            store_root,
            Arc::clone(&self.byte_pool),
        )
        .map(|mut scheduler| {
            scheduler.bind_resident_memory(Arc::clone(&self.resident_memory));
            scheduler.bind_resident_owners(Arc::clone(&self.resident_owners));
            scheduler
                .bind_progress_incarnations(self.progress_daemon_incarnation, producer_incarnation);
            scheduler
        })
    }

    pub async fn mount_worktree_with_graph_runtime(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        graph_runtime: Arc<dyn crate::code_graph_seat::CodeGraphSeatRuntimePortV1>,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
        graph_activation_policy: CodeGraphActivationPolicyV1,
        path_policy: IndexPathPolicyV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            CodeGraphActivationAuthorityV1::Persistent {
                runtime: graph_runtime,
                project_database,
                policy: Arc::new(AtomicBool::new(graph_activation_policy.is_enabled())),
                seated: Arc::default(),
            },
            path_policy,
        )
        .await
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn mount_worktree(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            CodeGraphActivationAuthorityV1::Memory {
                policy: Arc::new(AtomicBool::new(true)),
            },
            crate::config::registry_default_index_path_policy(),
        )
        .await
    }

    #[cfg(test)]
    pub async fn mount_worktree_with_graph_policy(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        policy: CodeGraphActivationPolicyV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            CodeGraphActivationAuthorityV1::Memory {
                policy: Arc::new(AtomicBool::new(policy.is_enabled())),
            },
            crate::config::registry_default_index_path_policy(),
        )
        .await
    }

    /// Seat the graph head a publication just wrote on its text owner, the
    /// way a restart recovers it, so graph reads serve from the mapped store
    /// without the whole-generation decode. `false` leaves the decode and its
    /// activation to seat the graph.
    async fn recover_published_graph_head(
        scheduler: &Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
        shutting_down: &Arc<AtomicBool>,
        passes: &Arc<super::super::ReconcilePassesV1>,
        graph_activation: &CodeGraphActivationAuthorityV1,
        (project_id, repository_id, worktree_id): (
            &ProjectId,
            &tracedecay_domain::RepositoryId,
            &tracedecay_domain::WorktreeId,
        ),
        text: &LatestCodeTextGenerationV1,
    ) -> bool {
        let generation_id = text.metadata().manifest().generation_id.clone();
        let binding_scheduler = Arc::clone(scheduler);
        let binding_shutting_down = Arc::clone(shutting_down);
        let binding_passes = Arc::clone(passes);
        let binding = native_runtime::spawn_blocking(move || {
            Self::lock_scheduler_for_graph_step(
                &binding_scheduler,
                &binding_shutting_down,
                &binding_passes,
            )?
            .1
            .code_graph_replay_binding(&generation_id)
        })
        .await;
        let binding = match binding {
            Ok(Ok(binding)) => binding,
            Ok(Err(error)) => {
                tracing::warn!(
                    event = "code_index_published_graph_head_binding_unavailable",
                    error = %error,
                    "the published graph head has no replay binding; the serving decode seats it"
                );
                return false;
            }
            Err(error) => {
                tracing::warn!(
                    event = "code_index_published_graph_head_binding_task_failed",
                    error = %error,
                    "the published graph head binding task failed; the serving decode seats it"
                );
                return false;
            }
        };
        match graph_activation
            .recover_verified_head(
                project_id,
                repository_id,
                worktree_id,
                text.clone(),
                binding,
                Arc::clone(shutting_down),
            )
            .await
        {
            Ok(recovered) => recovered,
            Err(error) => {
                tracing::warn!(
                    event = "code_index_published_graph_head_recovery_failed",
                    error = %error,
                    "the published graph head did not seat from the text owner; the serving \
                     decode seats it"
                );
                false
            }
        }
    }

    /// Drop a seat that no longer names the advertised generation. Search
    /// serves the text owner while the seat is empty, and holding the
    /// predecessor's decode would only keep a corpus-sized generation alive.
    pub(super) fn release_superseded_serving_seat(
        serving_generation: &ServingGenerationSlot,
        serving_generation_epoch: &AtomicU64,
        serving_source_witness: &RwLock<Option<super::super::ServingSourceWitnessV1>>,
        serving_seats: &tokio::sync::watch::Sender<u64>,
        serving_generation_changed: &tokio::sync::watch::Sender<()>,
    ) {
        let displaced = serving_generation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if displaced.is_none() {
            return;
        }
        serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
        *serving_source_witness
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        drop(displaced);
        Self::record_serving_seat(serving_seats);
        serving_generation_changed.send_replace(());
    }

    /// A same-root remount keeps the incumbent owner: it may only refresh the
    /// graph activation policy, and it wakes the worker so a policy change or
    /// an edit that raced the remount is picked up by the next pass.
    fn refresh_existing_mount(
        existing: &MountedCodeIndexWorktreeV1,
        project_id: &ProjectId,
        graph_activation: &CodeGraphActivationAuthorityV1,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        if existing.project_id != *project_id {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "mounted worktree belongs to a different project identity".to_owned(),
            ));
        }
        existing
            .graph_activation
            .update_policy(graph_activation.policy());
        Self::note_wake(
            existing.pending_wake.as_ref(),
            existing.wake.as_ref(),
            CodeIndexCadenceTriggerV1::BusyFollowUp,
        );
        Ok(())
    }

    async fn mount_worktree_inner(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        graph_activation: CodeGraphActivationAuthorityV1,
        path_policy: IndexPathPolicyV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = canonical_existing_identity(project_root)?;
        #[cfg(test)]
        Self::pause_cold_mount_admission_for_test(&project_root).await;
        let cold_mount_reservation = loop {
            if self.background_reconcile_admission.is_closed() {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index scheduler is shutting down".to_owned(),
                ));
            }
            #[cfg(test)]
            Self::pause_cold_mount_after_outer_check_for_test(&project_root).await;
            // A retiring owner still holds the store: admitting a fresh mount
            // here would race the dying reconcile task over the same physical
            // shard.
            let mut retiring = self.retiring.lock().await;
            // A deadline can leave a worker owned by retirement after its
            // caller has returned. Join only a completed exact-root worker
            // before admitting its replacement; a live worker stays fenced.
            if retiring
                .get(&project_root)
                .is_some_and(|worktree| worktree.task.is_finished())
                && let Some(worktree) = retiring.remove(&project_root)
            {
                // However the worker ended, it no longer holds the store, so
                // its replacement starts clean. A panic was already parked on
                // the retired owner by its supervisor.
                let outcome = worktree.task.wait().await;
                if !matches!(outcome, NativeTaskOutcome::Completed(())) {
                    tracing::warn!(
                        event = "code_index_retired_worker_failed",
                        outcome = ?outcome,
                        "retired code-index worker ended without completing; remounting"
                    );
                }
            }
            if retiring.contains_key(&project_root) {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index scheduler owner is still retiring".to_owned(),
                ));
            }
            let mounted = self.mounted.lock().await;
            if let Some(existing) = mounted.get(&project_root) {
                Self::refresh_existing_mount(existing, &project_id, &graph_activation)?;
                return Ok(false);
            }
            let admission = self.admit_cold_mount(&project_root, mounted.len())?;
            drop(mounted);
            drop(retiring);
            match admission {
                ColdMountAdmissionV1::Owner(reservation) => break reservation,
                ColdMountAdmissionV1::Follower(mut completion) => {
                    #[cfg(test)]
                    Self::note_cold_mount_follower_for_test(&project_root);
                    let _ = completion.changed().await;
                }
            }
        };
        // Keep CPU-bound cold-open identity setup off runtime workers.
        let scoped_store_root =
            super::super::scoped_code_index_store_root(&store_root, &project_root);
        let progress_daemon_incarnation = self.progress_daemon_incarnation;
        let progress_producer_incarnation = self.mint_progress_producer_incarnation()?;
        let mounted_path_policy = path_policy.clone();
        let mut cold_mount_reservation = cold_mount_reservation;
        let opened = loop {
            let open_project_id = project_id.clone();
            let open_project_root = project_root.clone();
            let open_store_root = scoped_store_root.clone();
            let open_path_policy = path_policy.clone();
            let open_byte_pool = Arc::clone(&self.byte_pool);
            let open_resident_memory = Arc::clone(&self.resident_memory);
            let open_resident_owners = Arc::clone(&self.resident_owners);
            let (opened, reservation) = native_runtime::spawn_blocking(move || {
                #[cfg(test)]
                Self::pause_cold_mount_open_for_test(&open_project_root);
                let opened = CodeIndexWorktreeSchedulerV1::open_with_policy(
                    open_project_id,
                    &open_project_root,
                    open_store_root,
                    open_byte_pool,
                    CodeIndexHintPolicyV1::default(),
                    open_path_policy,
                );
                #[cfg(test)]
                Self::finish_cold_mount_open_for_test(&open_project_root);
                let opened = opened.map(|mut opened| {
                    opened.bind_resident_memory(open_resident_memory);
                    opened.bind_resident_owners(open_resident_owners);
                    opened.bind_progress_incarnations(
                        progress_daemon_incarnation,
                        progress_producer_incarnation,
                    );
                    opened
                });
                (opened, cold_mount_reservation)
            })
            .await
            .map_err(|error| {
                CodeIndexSchedulerErrorV1::Identity(format!(
                    "code-index mount task failed: {error}"
                ))
            })?;
            cold_mount_reservation = reservation;
            match opened {
                Err(error) if error.is_store_lock_contended() => {
                    Self::wait_for_cold_open_store_release(
                        &scoped_store_root,
                        &cold_mount_reservation,
                    )
                    .await?;
                }
                opened => break opened?,
            }
        };
        let repository_id = opened.identity().repository_id().clone();
        let worktree_id = opened.identity().worktree_id().clone();
        let reconcile_in_progress = opened.reconcile_in_progress();
        let generation_recovery = opened.generation_recovery();
        let active_generation_encoded_bytes = opened.active_generation_encoded_bytes();
        let build_progress = opened.build_progress_slot();
        let historical_generation_owner = opened.historical_generation_owner();
        let source_freshness = opened.freshness_fence();
        let last_reconciled_at_micros = opened.last_reconciled_at_micros_slot();
        // Cold mount publishes only the exact route. The worker may seat a
        // complete identity-valid generation as stale serving before refresh
        // claims freshness; missing Git authority still leaves this empty.
        let serving_generation: Arc<ServingGenerationSlot> = Arc::new(RwLock::new(None));
        let complete_generation_requested = Arc::new(AtomicBool::new(false));
        let (complete_generation_requested_changed, worker_complete_generation_requested_changed) =
            tokio::sync::watch::channel(false);
        let text_generation: Arc<RwLock<Option<LatestCodeTextGenerationV1>>> =
            Arc::new(RwLock::new(None));
        let convergence_park: Arc<RwLock<Option<CodeIndexConvergenceParkedV1>>> =
            Arc::new(RwLock::new(None));
        let serving_source_witness: Arc<RwLock<Option<super::super::ServingSourceWitnessV1>>> =
            Arc::new(RwLock::new(None));
        let serving_generation_epoch = Arc::new(AtomicU64::new(0));
        let serving_generation_changed = Arc::new(tokio::sync::watch::channel(()).0);
        let serving_generation_installation = Arc::new(Mutex::new(None));
        let hints = Arc::clone(&opened.hints);
        let wake = Arc::clone(&opened.wake);
        let epoch = Arc::clone(&opened.epoch);
        let shutting_down = Arc::clone(&opened.shutting_down);
        let residency_publication = opened.publication.clone();
        let residency_retained_parses = opened.owner.retained_parse_pool();
        let scheduler = Arc::new(Mutex::new(opened));
        let build_publication_lock = Arc::new(tokio::sync::Mutex::new(()));
        let ignored_dependency_admissions = Arc::new(Mutex::new(BTreeMap::new()));
        let pending_wake = Arc::new(PendingWakeV1::default());
        let worker_phase = Arc::new(tokio::sync::watch::Sender::new(
            super::CodeIndexWorkerPhaseV1::default(),
        ));
        let index_observability = Arc::new(OnceLock::<
            super::super::observability::CodeIndexObservabilityV1,
        >::new());
        let memory_retry = Arc::new(super::MemoryRefusalRetryV1::default());
        let worker_project_root = project_root.clone();
        #[cfg(test)]
        let exit_root = project_root.clone();
        #[cfg(any(test, feature = "test-helpers"))]
        Self::wait_for_cold_mount_final_commit_gate(&project_root).await;
        // Reacquire the lifecycle fences before publication. Once acquired,
        // worker spawn and insertion contain no await point, so cancellation
        // cannot leave a detached worker that was never made canonical.
        let retiring = self.retiring.lock().await;
        let mut mounted = self.mounted.lock().await;
        if retiring.contains_key(&project_root) || cold_mount_reservation.slot.is_retired() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler owner is still retiring".to_owned(),
            ));
        }
        if self.background_reconcile_admission.is_closed()
            || cold_mount_reservation.slot.is_cancelled()
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler is shutting down".to_owned(),
            ));
        }
        if let Some(existing) = mounted.get(&project_root) {
            Self::refresh_existing_mount(existing, &project_id, &graph_activation)?;
            return Ok(false);
        }
        let at_capacity = mounted.len() >= self.max_worktrees;
        let entry = match mounted.entry(project_root) {
            std::collections::btree_map::Entry::Occupied(_) => {
                return Err(CodeIndexSchedulerErrorV1::Identity(
                    "code-index scheduler owner changed before final mount commit; remount must retry"
                        .to_owned(),
                ));
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                if at_capacity {
                    return Err(CodeIndexSchedulerErrorV1::Identity(
                        "code-index scheduler capacity is exhausted".to_owned(),
                    ));
                }
                entry
            }
        };
        let residency = Arc::new(super::super::residency::WorktreeResidencyV1::new(
            super::super::residency::WorktreeResidencyPartsV1 {
                serving_generation: Arc::clone(&serving_generation),
                serving_generation_epoch: Arc::clone(&serving_generation_epoch),
                serving_generation_changed: Arc::clone(&serving_generation_changed),
                serving_seats: Arc::clone(&self.serving_seats),
                complete_generation_requested: Arc::clone(&complete_generation_requested),
                reconcile_in_progress: Arc::clone(&reconcile_in_progress),
                publication: residency_publication,
                retained_parses: residency_retained_parses,
                text_generation: Arc::clone(&text_generation),
            },
        ));
        let mut worker_owner_headroom = self.resident_owners.subscribe_headroom();
        let worker_owner_headroom_seen = residency
            .park_headroom
            .released_by_others(&mut worker_owner_headroom);
        let worker_state = worker::WorktreeWorkerLoopStateV1::new(
            worker_owner_headroom,
            worker_owner_headroom_seen,
            self.resident_memory.pressure().subscribe_headroom(),
        );
        let worker = worker::WorktreeWorkerV1 {
            phase: Arc::clone(&worker_phase),
            index_observability: Arc::clone(&index_observability),
            scheduler: Arc::clone(&scheduler),
            reconcile_in_progress: Arc::clone(&reconcile_in_progress),
            build_progress: Arc::clone(&build_progress),
            serving_generation: Arc::clone(&serving_generation),
            complete_generation_requested: Arc::clone(&complete_generation_requested),
            complete_generation_requested_changed: worker_complete_generation_requested_changed,
            text_generation: Arc::clone(&text_generation),
            convergence_park: Arc::clone(&convergence_park),
            serving_source_witness: Arc::clone(&serving_source_witness),
            serving_generation_epoch: Arc::clone(&serving_generation_epoch),
            serving_generation_changed: Arc::clone(&serving_generation_changed),
            source_freshness: source_freshness.clone(),
            wake: Arc::clone(&wake),
            control_epoch: Arc::clone(&epoch),
            pending_wake: Arc::clone(&pending_wake),
            memory_retry: Arc::clone(&memory_retry),
            cadence_telemetry: Arc::clone(&self.cadence_telemetry),
            shutting_down: Arc::clone(&shutting_down),
            build_publication_lock: Arc::clone(&build_publication_lock),
            background_reconcile_admission: Arc::clone(&self.background_reconcile_admission),
            generation_publications: self.generation_publications.clone(),
            serving_seats: Arc::clone(&self.serving_seats),
            project_root: worker_project_root,
            project_id: project_id.clone(),
            repository_id: repository_id.clone(),
            worktree_id: worktree_id.clone(),
            graph_activation: graph_activation.clone(),
            scope_store_root: scoped_store_root,
            residency: Arc::clone(&residency),
            resident_owners: Arc::clone(&self.resident_owners),
            byte_pool: Arc::clone(&self.byte_pool),
        };
        // The worker boxes each pass, so its own future stays small; see
        // `WorktreeWorkerV1::run`.
        let worker_loop = Box::pin(worker.run(worker_state));
        #[cfg(test)]
        let worker_loop = Box::pin(async move {
            worker_loop.await;
            Self::panic_worker_pass_if_armed(&exit_root);
        });
        let task = NativeTask::spawn(tracing::Instrument::instrument(
            worker_loop,
            tracing::trace_span!("daemon.code_index.scheduler_worker"),
        ))
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!(
                "code-index scheduler worker did not start: {error}"
            ))
        })?;
        let task = Arc::new(task);
        // A panic outside the per-step guard, or a poll the debug long-poll
        // guard fails, ends the worker. Its outcome is the one place both land.
        let supervised = Arc::clone(&task);
        let supervised_park = Arc::clone(&convergence_park);
        let supervised_phase = Arc::clone(&worker_phase);
        let supervised_serving_changed = Arc::clone(&serving_generation_changed);
        spawn_detached("code_index_worker_supervisor", async move {
            if let NativeTaskOutcome::Panicked(panic) = supervised.wait().await {
                park_panicked_worker(
                    &supervised_park,
                    &supervised_phase,
                    &supervised_serving_changed,
                    &panic.to_string(),
                );
            }
        });
        self.register_worker_shutdown_signal(&shutting_down, &wake, &serving_generation_changed);
        let residency_registration = Arc::clone(&residency).register(
            &self.resident_owners,
            tracedecay_runtime_core::resident_memory::ResidentOwnerScopeV1 {
                project_id: project_id.clone(),
                worktree_id: worktree_id.clone(),
            },
        );
        // Memory given back is the retry for work refused for it: another
        // owner released, or the resident-memory authority finding a refused
        // request would now fit. This worktree's own park releases are not:
        // the retry would decode that same memory back (issue #3588). It
        // wakes the worker even over an arrival a refused pass restored
        // without a permit. The watcher holds no strong reference, so it
        // ends with the worktree.
        let mut owner_headroom = self.resident_owners.subscribe_headroom();
        let mut owner_headroom_seen = residency
            .park_headroom
            .released_by_others(&mut owner_headroom);
        let mut admission_headroom = self.resident_memory.pressure().subscribe_headroom();
        let headroom_pending_wake = Arc::downgrade(&pending_wake);
        let headroom_wake = Arc::downgrade(&wake);
        let headroom_text = Arc::downgrade(&text_generation);
        let headroom_park = Arc::downgrade(&convergence_park);
        let headroom_residency = Arc::downgrade(&residency);
        spawn_detached("code_index_headroom_watcher", async move {
            let Some(cx) = tracedecay_runtime_core::native_runtime::cx() else {
                tracing::error!(
                    "native runtime unavailable; memory headroom no longer wakes this worktree"
                );
                return;
            };
            loop {
                let (changed, owner_released) = tokio::select! {
                    changed = owner_headroom.changed(&cx) => (changed, true),
                    changed = admission_headroom.changed(&cx) => (changed, false),
                };
                let (
                    Ok(()),
                    Some(pending_wake),
                    Some(wake),
                    Some(text),
                    Some(park),
                    Some(residency),
                ) = (
                    changed,
                    headroom_pending_wake.upgrade(),
                    headroom_wake.upgrade(),
                    headroom_text.upgrade(),
                    headroom_park.upgrade(),
                    headroom_residency.upgrade(),
                )
                else {
                    return;
                };
                if owner_released {
                    let by_others = residency
                        .park_headroom
                        .released_by_others(&mut owner_headroom);
                    if by_others <= owner_headroom_seen {
                        continue;
                    }
                    owner_headroom_seen = by_others;
                }
                if worktree_waits_for_memory(&text, &park, &residency) {
                    Self::note_wake(
                        &pending_wake,
                        &wake,
                        CodeIndexCadenceTriggerV1::MemoryHeadroom,
                    );
                }
            }
        });
        entry.insert(MountedCodeIndexWorktreeV1 {
            residency,
            _residency_registration: residency_registration,
            project_id,
            repository_id,
            worktree_id,
            query_authority: None,
            scheduler,
            path_policy: mounted_path_policy,
            build_publication_lock,
            historical_generation_owner,
            serving_generation,
            complete_generation_requested,
            complete_generation_requested_changed,
            memory_retry,
            source_freshness,
            last_reconciled_at_micros,
            text_generation,
            convergence_park,
            generation_recovery,
            serving_source_witness,
            build_progress,
            serving_generation_epoch,
            serving_generation_changed,
            serving_generation_installation,
            graph_activation,
            graph_cursor_retention: Arc::default(),
            ignored_dependency_admissions,
            hints,
            wake: Arc::clone(&wake),
            epoch,
            pending_wake: Arc::clone(&pending_wake),
            index_observability,
            shutting_down,
            reconcile_in_progress,
            worker_phase,
            _active_generation_encoded_bytes: active_generation_encoded_bytes,
            task,
        });
        Self::record_root_mounted(&self.root_mounted);
        // Until retained decode/truth verification completes, reads see warming
        // instead of serving unproven bytes.
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::Mount);
        Ok(true)
    }
}

/// Whether memory given back can advance this worktree: its text owner is
/// missing or unfinished, its graph is parked on resident memory, or its
/// last refresh was refused for memory. A pass on a finished worktree would
/// only re-seat the decode a release just gave back.
fn worktree_waits_for_memory(
    text: &RwLock<Option<LatestCodeTextGenerationV1>>,
    park: &RwLock<Option<CodeIndexConvergenceParkedV1>>,
    residency: &super::super::residency::WorktreeResidencyV1,
) -> bool {
    let text_unfinished = text
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_none_or(LatestCodeTextGenerationV1::text_projection_needs_work);
    let graph_refused_for_memory = park
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|parked| {
            parked.blocked_reason == Some(CodeIndexBuildBlockedReasonV1::ResidentMemory)
        });
    text_unfinished || graph_refused_for_memory || residency.refresh_waits_for_memory()
}

/// Sole exact/lexical-ready bit for the published graph seat gate and the
/// full sealed-generation replay skip. Delegates to
/// [`LatestCodeTextGenerationV1::query_owners_are_ready`] so those two sites
/// cannot fork.
#[inline]
fn exact_and_lexical_ready_for_graph(text: Option<&LatestCodeTextGenerationV1>) -> bool {
    text.is_some_and(LatestCodeTextGenerationV1::query_owners_are_ready)
}

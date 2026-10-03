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

use tracedecay_code_index::parallelism::collect_installed_worker_heaps_when_idle;
use tracedecay_code_index::production::{
    CodeIndexInterruptionV1, CodeIndexPublicationStoreErrorV1,
};
use tracedecay_code_index_retention::code_index_generations::wait_for_code_generation_store_release;
use tracedecay_contracts::code_index_freshness::{
    CodeIndexBuildBlockedReasonV1, CodeIndexConvergenceParkedV1,
};
use tracedecay_domain::{IndexPathPolicyV1, ProjectId};

use super::super::{
    CodeIndexCadenceTriggerV1, CodeIndexHintPolicyV1, CodeIndexNoopEvidenceV1,
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    DaemonCodeIndexPublicationStoreV1, LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1,
    RetainedTextGenerationRestoreV1,
    graph_activation::{CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1},
    now_micros,
    reconcile_panic_guard::{
        ReconcileCapacityRetryV1, ReconcilePanicDecisionV1, ReconcilePanicGuardV1,
    },
};
use super::{
    ACTIVATION_RETRY_BACKOFF_CEILING, ACTIVATION_RETRY_BACKOFF_FLOOR,
    CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1,
    CONVERGENCE_PARK_GRAPH_RESIDENT_MEMORY_REMEDIATION_V1,
    CONVERGENCE_PARK_PUBLICATION_CORRUPTION_REMEDIATION_V1,
    CONVERGENCE_PARK_PUBLICATION_RESET_FAILED_REMEDIATION_V1,
    CONVERGENCE_PARK_RECONCILE_FAILURE_REMEDIATION_V1,
    CONVERGENCE_PARK_REFRESH_RESIDENT_MEMORY_REMEDIATION_V1,
    CONVERGENCE_PARK_STORE_RELEASE_WAIT_REMEDIATION_V1,
    CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1, CodeIndexSchedulerRegistryV1,
    ColdMountAdmissionV1, ColdMountReservationV1, GraphActivationGateV1, GraphSeatGateV1,
    MountedCodeIndexWorktreeV1, PendingWakeV1, PublishedTextProjectionOutcomeV1,
    ServingGenerationSlot, ServingSwapOutcomeV1, TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1,
    clear_convergence_park, clear_graph_resident_memory_park, convergence_park_retries_on_wake,
    graph_head_belongs_to_another_generation, is_repeated_conflict_verdict, park_convergence,
    publication_authority_is_terminal, retained_noop_requires_follow_up_wake,
};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

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
enum StoreReleaseWaitStopV1 {
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
        let retry_wake = Arc::clone(wake);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            retry_wake.notify_one();
        });
        true
    }

    /// Block this worker until the holder of the store lock that refused the
    /// previous pass releases it. A follow-up wake banked during the wait must
    /// not start another failing pass: the worker does not sit on `Notify`
    /// here. The holder may be another process, so the kernel lock is the
    /// signal. Shutdown ends the wait; a wait that cannot complete parks the
    /// worktree typed.
    async fn wait_for_worker_store_release(
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
        let binding = tokio::task::spawn_blocking(move || {
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
    fn release_superseded_serving_seat(
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
            let retiring = self.retiring.lock().await;
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
        let worker_scope_store_root = scoped_store_root.clone();
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
            let (opened, reservation) = tokio::task::spawn_blocking(move || {
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
        let serving_generation: Arc<ServingGenerationSlot> = Arc::new(hotpath::rw_lock!(
            RwLock::new(None),
            label = "daemon.code_index.serving_generation"
        ));
        let complete_generation_requested = Arc::new(AtomicBool::new(false));
        let (
            complete_generation_requested_changed,
            mut worker_complete_generation_requested_changed,
        ) = tokio::sync::watch::channel(false);
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
        let worker_phase_signal = Arc::clone(&worker_phase);
        let index_observability = Arc::new(OnceLock::<
            super::super::observability::CodeIndexObservabilityV1,
        >::new());
        let worker_index_observability = Arc::clone(&index_observability);
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_reconcile_in_progress = Arc::clone(&reconcile_in_progress);
        let worker_build_progress = Arc::clone(&build_progress);
        let worker_serving_generation = Arc::clone(&serving_generation);
        let worker_complete_generation_requested = Arc::clone(&complete_generation_requested);
        let worker_text_generation = Arc::clone(&text_generation);
        let worker_convergence_park = Arc::clone(&convergence_park);
        let worker_serving_source_witness = Arc::clone(&serving_source_witness);
        let worker_serving_generation_epoch = Arc::clone(&serving_generation_epoch);
        let worker_serving_generation_changed = serving_generation_changed.clone();
        let worker_source_freshness = source_freshness.clone();
        let worker_wake = Arc::clone(&wake);
        // The code-index control epoch. It advances exactly when new input is
        // announced (hook hints, watch paths, overflow), so it is the signal a
        // quarantined worker uses to decide that the bytes which panicked it
        // are no longer the bytes it is being asked to index.
        let worker_control_epoch = Arc::clone(&epoch);
        let worker_pending_wake = Arc::clone(&pending_wake);
        let memory_retry = Arc::new(super::MemoryRefusalRetryV1::default());
        let worker_memory_retry = Arc::clone(&memory_retry);
        let worker_cadence_telemetry = Arc::clone(&self.cadence_telemetry);
        let worker_shutting_down = Arc::clone(&shutting_down);
        let worker_build_publication_lock = Arc::clone(&build_publication_lock);
        let worker_background_reconcile_admission =
            Arc::clone(&self.background_reconcile_admission);
        let worker_generation_publications = self.generation_publications.clone();
        let worker_serving_seats = Arc::clone(&self.serving_seats);
        let worker_project_root = project_root.clone();
        let worker_project_id = project_id.clone();
        let worker_repository_id = repository_id.clone();
        let worker_worktree_id = worktree_id.clone();
        let worker_graph_activation = graph_activation.clone();
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
                complete_generation_requested: Arc::clone(&complete_generation_requested),
                reconcile_in_progress: Arc::clone(&reconcile_in_progress),
                publication: residency_publication,
                retained_parses: residency_retained_parses,
                text_generation: Arc::clone(&text_generation),
            },
        ));
        let worker_residency = Arc::clone(&residency);
        let worker_resident_owners = Arc::clone(&self.resident_owners);
        let worker_byte_pool = Arc::clone(&self.byte_pool);
        let mut worker_owner_headroom = self.resident_owners.subscribe_headroom();
        let mut worker_admission_headroom = self.resident_memory.pressure().subscribe_headroom();
        // Boxed at definition on purpose: this worker's state machine is the
        // largest future in the daemon (reconcile + text advance + decode +
        // activation + swap inline), and an unboxed `let` materializes the
        // whole machine on the spawning runtime worker's stack before
        // `tokio::spawn` can move it to the heap - measured tonight as an
        // instant stack overflow at project mount. `Box::pin` around the
        // async block constructs it into the allocation instead (the same
        // pattern as the `_inner` boxed fns from the 37MB-future fix).
        let worker_loop = Box::pin(async move {
            // Bounded retry state for activating an already-sealed complete
            // generation. The sealed artifact is immutable and retryable, so a
            // retryable activation failure must not fall through into a
            // rebuild+reseal of an equivalent generation.
            let mut seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
            let mut next_seat_attempt_at: Option<Instant> = None;
            // The generation this worker last offered to graph activation
            // outside the replace gate. It bounds the recovery attempt for a
            // generation that serves text without a native graph to one per
            // generation, so a permanently unactivatable seal cannot spin.
            let mut graph_seat_attempted: Option<tracedecay_domain::CodeGenerationId> = None;
            // A retained revision-7 graph gets one verified-head attempt
            // before ordinary source reconciliation owns any repair. A failed
            // verification falls through to one canonical replay of the same
            // sealed generation rather than repeating the head read forever.
            let mut retained_graph_head_recovery_attempted = false;
            // A retained manifest whose verified head belonged to another
            // generation. Replaying it would discard the generation that owns
            // the head. Later retained passes of this same generation skip
            // that replay; a different generation clears the memory.
            let mut graph_head_conflict_generation: Option<tracedecay_domain::CodeGenerationId> =
                None;
            // The conflict verdict of the previous failed seat attempt. A
            // Conflict can be a race (a concurrent publisher advanced the
            // head) and is retried once like any transient failure, but the
            // same guard site refusing with identical compared evidence for
            // the same sealed generation on the very next attempt is a
            // deterministic verdict that no backoff can outwait. One repeat
            // converts the retry into the terminal typed refusal below, so
            // the backoff ceiling can never become an infinite conflict loop
            // (issue #765).
            let mut last_seat_conflict: Option<(
                tracedecay_domain::CodeGenerationId,
                tracedecay_graph_db::GraphConflictContextV1,
            )> = None;
            // Bounded retry state for a reconcile whose blocking task unwound.
            // Arbitrary user source runs through the indexing pool, so a panic
            // there is an input fault, not a programmer-contract break: it
            // reproduces on every pass over the same bytes. Without this the
            // loop re-dispatched the identical unit on every wake forever.
            let mut panic_guard = ReconcilePanicGuardV1::new();
            // Bounded retry state for a reconcile refused because a graph
            // operation budget was momentarily held by a sibling worktree.
            // Releasing that budget emits no wake, so this worker schedules
            // its own. Memory refusals wait for the headroom wake instead.
            let mut capacity_retry = ReconcileCapacityRetryV1::new();
            // The previous pass was refused because another owner holds this
            // scope's store lock. The next iteration waits for that release
            // before it runs, so a follow-up wake cannot start another
            // failing pass while the holder is still inside the lock.
            let mut pass_waits_for_store_release = false;
            // Whether this worker already deleted and rebuilt a corrupt derived
            // publication. One reset per mount bounds the work: a store that
            // is corrupt again after its own rebuild parks instead of looping
            // through full re-indexes.
            let mut publication_authority_reset_attempted = false;
            // The last arrival this worker restored for a nothing-seated
            // warming outcome. A quiet remount's seat pass restores its
            // arrival exactly once so the next pass can restore and warm the
            // retained text owner; a worktree with nothing restorable (for
            // example no extractable sources and no publication) reproduces
            // the identical warming Noop on every pass, and restoring the
            // same arrival again would spin this worker forever.
            let mut warming_restore_arrival: Option<i64> = None;
            // Whether the optional graph prepare already yielded to a pending
            // arrival since it last actually ran. One yield lets a landed
            // arrival's exact/lexical pass go first; a second consecutive
            // yield would make "no arrival pending" a seat precondition, which
            // on a busy checkout is the quiet-tree starvation the seat gate
            // rework removed.
            let mut graph_prepare_yielded_to_arrival = false;
            // A retained owner's projection may outlive the pass that seats
            // its verified graph. Keep the task owned by this worker while a
            // late complete-generation request starts a successor pass; that
            // successor must neither detach nor duplicate the text owner.
            let mut retained_text_projection = None;
            // Whether the retained projection in flight started with its
            // query owners already serving, so its finish changes no owner the
            // seat reads and owes the worker no successor pass.
            let mut retained_projection_successor_only = false;
            loop {
                // Memory given back while the last pass was being refused
                // reached the watcher before that refusal was visible.
                let headroom_moved = worker_owner_headroom.has_changed().unwrap_or(false)
                    || worker_admission_headroom.has_changed().unwrap_or(false);
                if headroom_moved
                    && worktree_waits_for_memory(
                        &worker_text_generation,
                        &worker_convergence_park,
                        &worker_residency,
                    )
                {
                    Self::note_wake(
                        &worker_pending_wake,
                        &worker_wake,
                        CodeIndexCadenceTriggerV1::MemoryHeadroom,
                    );
                }
                // A banked follow-up permit must not skip this wait. The
                // previous pass already restored its arrival; this iteration
                // runs that pass once the holder lets go.
                let mut entered_after_store_release = false;
                if pass_waits_for_store_release {
                    pass_waits_for_store_release = false;
                    super::CodeIndexWorkerPhaseV1::enter(
                        &worker_phase_signal,
                        super::CodeIndexWorkerPhaseV1::Working,
                    );
                    match Self::wait_for_worker_store_release(
                        &worker_scope_store_root,
                        &worker_shutting_down,
                        &worker_serving_generation_changed,
                    )
                    .await
                    {
                        Ok(()) => entered_after_store_release = true,
                        Err(StoreReleaseWaitStopV1::Shutdown) => {
                            tracing::info!(
                                event = "code_index_worker_shutdown_observed",
                                phase = "store_release_wait",
                                "code-index worker observed shutdown and stopped its pass"
                            );
                            Self::join_retained_text_projection_on_worker_exit(
                                &mut retained_text_projection,
                            )
                            .await;
                            return;
                        }
                        Err(StoreReleaseWaitStopV1::Unavailable(reason)) => {
                            Self::park_store_release_unavailable(&worker_convergence_park, reason);
                        }
                    }
                }
                if !entered_after_store_release {
                    let notified = worker_wake.notified();
                    tokio::pin!(notified);
                    // Parked only while registered with no banked permit: a
                    // banked permit resolves the wait at once, so the worker was
                    // never idle.
                    if !notified.as_mut().enable() {
                        super::CodeIndexWorkerPhaseV1::enter(
                            &worker_phase_signal,
                            super::CodeIndexWorkerPhaseV1::Parked,
                        );
                    }
                    hotpath::future!(notified, label = "daemon.code_index.wake_wait").await;
                }
                worker_owner_headroom.mark_unchanged();
                worker_admission_headroom.mark_unchanged();
                super::CodeIndexWorkerPhaseV1::enter(
                    &worker_phase_signal,
                    super::CodeIndexWorkerPhaseV1::Working,
                );
                if worker_shutting_down.load(Ordering::Acquire) {
                    tracing::info!(
                        event = "code_index_worker_shutdown_observed",
                        phase = "wake",
                        "code-index worker observed shutdown and stopped its pass"
                    );
                    Self::join_retained_text_projection_on_worker_exit(
                        &mut retained_text_projection,
                    )
                    .await;
                    return;
                }
                // The shared park slot is the authority admission already
                // consults. A task-local bool would ignore a park planted by
                // another actor and would reset if this loop state were lost
                // while the slot remained.
                if publication_authority_is_terminal(&worker_convergence_park) {
                    let _ = Self::take_pending_arrival(
                        &worker_pending_wake,
                        &worker_wake,
                        CodeIndexCadenceTriggerV1::Mount,
                    );
                    tracing::debug!(
                        event = "code_index_reconcile_terminal_suppressed",
                        path = "background_worker",
                        "code-index reconcile is blocked until the publication authority is reset"
                    );
                    continue;
                }
                // This aggregate starts when the wake is observed and ends
                // when the pass holds every admission/publication gate it
                // needs. It deliberately does not attribute the span to the
                // SQL writer alone.
                let pass_wake_observed_at = Instant::now();
                let pass_control_epoch = worker_control_epoch.load(Ordering::Acquire);
                // A quarantined or backing-off unit must not consume the
                // pending arrival: the wake stays outstanding so a later
                // eligible pass still measures its full queue wait.
                if panic_guard.suppresses_pass(tokio::time::Instant::now(), pass_control_epoch) {
                    tracing::debug!(
                        event = "code_index_reconcile_panic_suppressed",
                        path = "background_worker",
                        consecutive_panics = panic_guard.consecutive_panics(),
                        "code-index reconcile is suppressed over unchanged input after a panic or a reproducing failure"
                    );
                    continue;
                }
                super::CodeIndexWorkerPhaseV1::enter(
                    &worker_phase_signal,
                    super::CodeIndexWorkerPhaseV1::AwaitingAdmission,
                );
                let Ok(_background_reconcile_admission) = hotpath::future!(
                    Arc::clone(&worker_background_reconcile_admission).acquire_owned(),
                    label = "daemon.code_index.admission_wait"
                )
                .await
                else {
                    Self::join_retained_text_projection_on_worker_exit(
                        &mut retained_text_projection,
                    )
                    .await;
                    return;
                };
                // Retirement and daemon shutdown set the flag, then fire this
                // watch, so the gate wait below wakes on them.
                let mut shutdown_observed = worker_serving_generation_changed.subscribe();
                if worker_shutting_down.load(Ordering::Acquire) {
                    tracing::info!(
                        event = "code_index_worker_shutdown_observed",
                        phase = "gates_held",
                        "code-index worker observed shutdown and stopped its pass"
                    );
                    Self::join_retained_text_projection_on_worker_exit(
                        &mut retained_text_projection,
                    )
                    .await;
                    return;
                }
                super::CodeIndexWorkerPhaseV1::enter(
                    &worker_phase_signal,
                    super::CodeIndexWorkerPhaseV1::AwaitingPublicationGate,
                );
                let mut build_publication =
                    std::pin::pin!(Arc::clone(&worker_build_publication_lock).lock_owned());
                let _build_publication = loop {
                    tokio::select! {
                        guard = &mut build_publication => break guard,
                        changed = shutdown_observed.changed() => {
                            if changed.is_err() || worker_shutting_down.load(Ordering::Acquire) {
                                tracing::info!(
                                    event = "code_index_worker_shutdown_observed",
                                    phase = "build_publication_lock",
                                    "code-index worker observed shutdown and stopped its pass"
                                );
                                Self::join_retained_text_projection_on_worker_exit(
                                    &mut retained_text_projection,
                                )
                                .await;
                                return;
                            }
                        }
                    }
                };
                super::CodeIndexWorkerPhaseV1::enter(
                    &worker_phase_signal,
                    super::CodeIndexWorkerPhaseV1::Working,
                );
                let wake_to_gates_held_micros =
                    u64::try_from(pass_wake_observed_at.elapsed().as_micros()).unwrap_or(u64::MAX);
                let scheduler = Arc::clone(&worker_scheduler);
                let graph_activation_enabled = worker_graph_activation.policy().is_enabled();
                // A coalesced text-slice wake can outlive the graph-off pass
                // that drained its final overflow. Do not project that bare
                // Notify permit as a second refresh: no arrival and no hint
                // means there is no source work left, while a scheduler-owned
                // raw overflow still reaches the worker through `None` here.
                let graph_off_text_is_settled = !graph_activation_enabled
                    && !worker_pending_wake.has_pending_arrival()
                    && worker_text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .is_some_and(LatestCodeTextGenerationV1::query_owners_are_ready)
                    && match scheduler.try_lock() {
                        Ok(scheduler) => scheduler.pending_hint_count() == Some(0),
                        Err(std::sync::TryLockError::Poisoned(error)) => {
                            error.into_inner().pending_hint_count() == Some(0)
                        }
                        Err(std::sync::TryLockError::WouldBlock) => false,
                    };
                if graph_off_text_is_settled {
                    continue;
                }
                // Cover wake claim through failed-arrival restoration so admission
                // never misreads in-flight owner work as plain unavailability.
                let mut reconcile_pass = Some(super::super::ReconcilePassGuard::enter(
                    &worker_reconcile_in_progress,
                ));
                let mut retained_work_waiting_for_source = false;
                let mut text_generation = worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                // A query-driven advance can finish the build between worker
                // passes; a park observed earlier must not outlive the
                // violation it named.
                if text_generation
                    .as_ref()
                    .is_some_and(super::super::LatestCodeTextGenerationV1::query_owners_are_ready)
                {
                    clear_convergence_park(&worker_convergence_park);
                } else if convergence_park_retries_on_wake(&worker_convergence_park)
                    && text_generation
                        .as_ref()
                        .is_some_and(|latest| !latest.text_projection_needs_work())
                {
                    // A deterministic contract violation latched this text
                    // handle failed, and a latched handle never advances
                    // again. Withdraw it so the ordinary restore below
                    // re-creates a fresh handle from the durable pointer:
                    // every external wake re-checks the parked violation, and
                    // an operator fix (for example chmod 700 on the artifacts
                    // root) is picked up without a remount. The park itself
                    // never self-schedules a wake, so this stays one bounded
                    // retry per ordinary wake, not a hot loop.
                    if let Some(withdrawn) = text_generation.take() {
                        let mut current = worker_text_generation
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if current
                            .as_ref()
                            .is_some_and(|current| current.same_text_owner(&withdrawn))
                        {
                            *current = None;
                        }
                    }
                }
                let mut text_slice_incomplete = false;
                // The retained owner's bounded projection, driven to
                // completion concurrently with this pass's graph seat and
                // joined before the pass ends.
                if retained_text_projection.is_none()
                    && let Some(latest) = text_generation.clone()
                    && latest.text_projection_needs_work()
                    && graph_activation_enabled
                {
                    // Warming a missing owner always runs here. Work left behind
                    // an owner that already serves runs only once the seated
                    // generation matches this text owner and the source is
                    // current: starting it while the serving slot is empty,
                    // mismatched, or the checkout is dirty races the
                    // publish/seat path that still owns the receipt bound.
                    let owners_ready = latest.query_owners_are_ready();
                    let serving_matches_text = worker_serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .is_some_and(|serving| {
                            serving.generation().manifest().generation_id
                                == latest.metadata().manifest().generation_id
                        });
                    let source_current = worker_source_freshness
                        .ready_without_stat(&worker_project_root, &worker_shutting_down);
                    // This flag schedules the successor pass; that pass
                    // re-checks `serving_matches_text` before driving it.
                    retained_work_waiting_for_source = owners_ready && !source_current;
                    let drive_retained = !owners_ready || (serving_matches_text && source_current);
                    if drive_retained {
                        // The retained owner projects on its own task, exactly as
                        // a publication's replacement owner does, and this pass
                        // joins it after the graph seat. Awaiting a slice here
                        // instead put the whole projection ahead of the seat, and
                        // one slice is not divisible below its finalization: a
                        // restart that resumed an unfinished ngram index spent
                        // that entire build -- 377 s measured on a 5,181-file
                        // corpus -- before the pass even reached the gate that
                        // would have recovered the verified head in 8 s.
                        let shutting_down = Arc::clone(&worker_shutting_down);
                        let park = Arc::clone(&worker_convergence_park);
                        let installed = Arc::clone(&worker_text_generation);
                        #[cfg(any(test, feature = "test-helpers"))]
                        let gated_root = worker_project_root.clone();
                        // The pass guard is what `rebuild_in_flight`, the
                        // `verifying` freshness state and a read's busy fence
                        // consult: it means query serving is still being
                        // produced. Holding it for work behind owners that
                        // already serve reported a complete current generation
                        // as verifying. The worker still owns and joins
                        // the task, so shutdown sees the work.
                        retained_projection_successor_only = owners_ready;
                        let projection_pass = (!retained_projection_successor_only).then(|| {
                            super::super::ReconcilePassGuard::enter(&worker_reconcile_in_progress)
                        });
                        let projection_pending_wake = Arc::clone(&worker_pending_wake);
                        let projection_wake = Arc::clone(&worker_wake);
                        let projection_serving_changed = worker_serving_generation_changed.clone();
                        retained_text_projection = Some(tokio::spawn(async move {
                            let _projection_pass = projection_pass;
                            #[cfg(any(test, feature = "test-helpers"))]
                            Self::wait_for_retained_text_projection_gate(&gated_root).await;
                            let outcome = Self::drive_text_projection(
                                latest,
                                shutting_down,
                                park,
                                Some(installed),
                                None,
                                #[cfg(test)]
                                gated_root,
                            )
                            .await;
                            match outcome {
                                // Exact and lexical serve from here on, while
                                // this pass may still be recovering the graph:
                                // search waiters must wake now, not at the seat.
                                PublishedTextProjectionOutcomeV1::Finished => {
                                    projection_serving_changed.send_replace(());
                                }
                                PublishedTextProjectionOutcomeV1::Unfinished => {
                                    Self::note_worker_continuation(
                                        &projection_pending_wake,
                                        &projection_wake,
                                    );
                                }
                                PublishedTextProjectionOutcomeV1::WaitingForMemory
                                | PublishedTextProjectionOutcomeV1::WaitingForStore
                                | PublishedTextProjectionOutcomeV1::Shutdown => {}
                            }
                            outcome
                        }));
                    }
                } else if let Some(latest) = text_generation
                    && latest.text_projection_needs_work()
                {
                    // Graph activation is off for this worktree: there is no
                    // seat to unblock, so the slice stays inline and the pass
                    // keeps yielding back to the loop between slices.
                    let failed_latest = latest.clone();
                    let build = hotpath::future!(
                        tokio::task::spawn_blocking(move || {
                            latest.advance_text_serving(TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1)
                        }),
                        label = "daemon.code_index.text_projection"
                    )
                    .await;
                    match build {
                        Ok(Ok(true)) => {
                            clear_convergence_park(&worker_convergence_park);
                        }
                        Ok(Ok(false)) => {
                            clear_convergence_park(&worker_convergence_park);
                            worker_wake.notify_one();
                            text_slice_incomplete = true;
                        }
                        Ok(Err(error)) => {
                            if matches!(
                                &error,
                                tracedecay_query::retrieval::RetrievalPortError::Cancelled
                            ) {
                                let mut current = worker_text_generation
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if current
                                    .as_ref()
                                    .is_some_and(|current| current.same_text_owner(&failed_latest))
                                {
                                    *current = None;
                                }
                            }
                            if error.is_deterministic_contract() {
                                // Unchanged input reproduces this on every
                                // wake, so an unparked WARN would mask it as
                                // warming forever. Park it typed; the wake
                                // cadence still re-checks, so an operator fix
                                // is picked up without a restart.
                                park_convergence(
                                    &worker_convergence_park,
                                    error.to_string(),
                                    CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1,
                                    None,
                                    true,
                                );
                                tracing::warn!(
                                    event = "code_index_convergence_parked",
                                    path = "background_worker",
                                    error = %error,
                                    "code-index text projection parked on a deterministic \
                                     contract violation; status reports it typed and every \
                                     wake re-checks"
                                );
                            } else if error.is_generation_store_lock_contended() {
                                pass_waits_for_store_release = true;
                                tracing::info!(
                                    event = "code_index_text_projection_waiting_for_store",
                                    "code-index background text projection waits for the \
                                     code-generation store lock"
                                );
                            } else {
                                tracing::warn!(
                                    event = "code_index_text_projection_failed",
                                    error = %error,
                                    "code-index background text projection failed"
                                );
                            }
                        }
                        Err(error) => {
                            failed_latest.mark_text_serving_failed();
                            park_convergence(
                                &worker_convergence_park,
                                format!("code text projection task failed abnormally: {error}"),
                                CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                                None,
                                false,
                            );
                            tracing::warn!(
                                event = "code_index_text_projection_task_failed",
                                error = %error,
                                "code-index background text projection task failed"
                            );
                        }
                    }
                }
                if worker_shutting_down.load(Ordering::Acquire) {
                    tracing::info!(
                        event = "code_index_worker_shutdown_observed",
                        phase = "text_projection",
                        "code-index worker observed shutdown and stopped its pass"
                    );
                    Self::join_retained_text_projection_on_worker_exit(
                        &mut retained_text_projection,
                    )
                    .await;
                    return;
                }
                if text_slice_incomplete {
                    if !graph_activation_enabled {
                        #[cfg(feature = "hotpath")]
                        hotpath::gauge!("daemon.code_index.artifact.slice.continue_total")
                            .inc(1_u64);
                        continue;
                    }
                    if Self::incomplete_text_slice_may_continue(&worker_pending_wake) {
                        #[cfg(feature = "hotpath")]
                        hotpath::gauge!("daemon.code_index.artifact.slice.continue_total")
                            .inc(1_u64);
                        continue;
                    }
                    #[cfg(feature = "hotpath")]
                    hotpath::gauge!("daemon.code_index.artifact.slice.yield_to_reconcile_total")
                        .inc(1_u64);
                }
                let refused_retained_text_metadata = if worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none()
                {
                    let text_scheduler = Arc::clone(&scheduler);
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let retained_text = hotpath::future!(
                        tokio::task::spawn_blocking(move || {
                            Self::lock_scheduler_unless_shutting_down(
                                &text_scheduler,
                                &shutting_down,
                            )
                            .map(|mut scheduler| scheduler.restore_retained_text_generation())
                        }),
                        label = "daemon.code_index.text_restore"
                    )
                    .await;
                    if worker_shutting_down.load(Ordering::Acquire) {
                        tracing::info!(
                            event = "code_index_worker_shutdown_observed",
                            phase = "text_restore",
                            "code-index worker observed shutdown and stopped its pass"
                        );
                        Self::join_retained_text_projection_on_worker_exit(
                            &mut retained_text_projection,
                        )
                        .await;
                        return;
                    }
                    match retained_text {
                        Ok(Ok(Some(RetainedTextGenerationRestoreV1::Servable(retained_text)))) => {
                            *worker_text_generation
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(retained_text);
                            // A restored text owner is a serving change with
                            // no publication: waiters subscribed before the
                            // restore must wake now, not after the next pass.
                            worker_serving_generation_changed.send_replace(());
                            worker_wake.notify_one();
                            continue;
                        }
                        Ok(Ok(Some(RetainedTextGenerationRestoreV1::Refused(metadata)))) => {
                            Some(metadata)
                        }
                        Ok(Ok(Some(RetainedTextGenerationRestoreV1::Failed(error)))) => {
                            tracing::error!(
                                event = "code_index_retained_text_restore_failed",
                                error = %error,
                                "retained text restore failed at decoded-cache release"
                            );
                            None
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let text_serving_ready = worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(LatestCodeTextGenerationV1::query_owners_are_ready);
                // Admission is held: queue wait ends and service time begins.
                let started_micros = now_micros().0;
                let (arrival, trigger) = Self::take_pending_arrival(
                    &worker_pending_wake,
                    &worker_wake,
                    CodeIndexCadenceTriggerV1::Mount,
                );
                if text_slice_incomplete {
                    worker_wake.notify_one();
                }
                // A retryable native-graph failure defers only graph
                // activation. Reconcile and finish the lightweight text owner
                // before opening the full generation: a large graph replay
                // must never become exact/lexical time-to-ready. During
                // backoff, the scheduled retry is the only pass that may open
                // the immutable full generation again.
                let graph_activation_deferred =
                    next_seat_attempt_at.is_some_and(|at| Instant::now() < at);
                let retained_text = worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let memory_pass = trigger == CodeIndexCadenceTriggerV1::MemoryHeadroom;
                if memory_pass {
                    worker_memory_retry.reset();
                }
                // Memory given back is the retry for a graph refused at the
                // watermark: its generation activates again on this pass.
                if memory_pass
                    && retained_text.as_ref().is_some_and(
                        LatestCodeTextGenerationV1::retry_resident_memory_graph_refusal,
                    )
                {
                    graph_seat_attempted = None;
                    tracing::info!(
                        event = "code_index_graph_activation_memory_retry",
                        trigger = trigger.label(),
                        "memory was given back; the refused native graph activates again"
                    );
                }
                let serving_empty = worker_serving_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none();
                let retained_text_uses_partitioned_manifest = retained_text
                    .as_ref()
                    .is_some_and(LatestCodeTextGenerationV1::uses_partitioned_manifest);
                // A revision-7 owner can restore its retained persistent graph
                // directly from the verified head. Give that recovery exactly
                // one empty-slot pass before the dirty source capture creates
                // its successor. Once the retained graph is Ready, this guard
                // falls through to the ordinary reconciliation path instead of
                // repeatedly consuming the successor's wake as a Noop.
                let retained_partitioned_graph_recovery_pending = graph_activation_enabled
                    && !graph_activation_deferred
                    && serving_empty
                    && !retained_graph_head_recovery_attempted
                    && retained_text.as_ref().is_some_and(|text| {
                        text.uses_partitioned_manifest() && text.interactive_graph_store().is_err()
                    });
                let retained_text_metadata = retained_text
                    .as_ref()
                    .map(|text| text.metadata().clone())
                    .or(refused_retained_text_metadata);
                // Phase boundary: source reconciliation begins. Together with
                // `code_index_generation_published` / `_interrupted` and
                // `code_index_serving_generation_seated` this lets a status
                // reader attribute a `progress: null` warming window to the
                // uninstrumented capture+seal instead of to text or graph work.
                tracing::info!(
                    event = "code_index_reconcile_pass_started",
                    path = "background_worker",
                    trigger = trigger.label(),
                    arrival = arrival.label(),
                    queue_delay_micros = ?arrival
                        .wake_micros()
                        .map(|wake_micros| started_micros.saturating_sub(wake_micros).max(0)),
                    wake_to_gates_held_micros,
                    retained_text = retained_text_metadata
                        .as_ref()
                        .map(|metadata| metadata.manifest().generation_id.as_str()),
                    text_serving_ready,
                    serving_empty,
                    graph_activation_deferred,
                    "code-index background reconcile pass started"
                );
                let shutting_down = Arc::clone(&worker_shutting_down);
                let bind_serving_generation = Arc::clone(&worker_serving_generation);
                let bind_serving_source_witness = Arc::clone(&worker_serving_source_witness);
                let bind_source_freshness = worker_source_freshness.clone();
                let source_result = hotpath::future!(
                    tokio::task::spawn_blocking(move || {
                        let mut scheduler =
                            Self::lock_scheduler_unless_shutting_down(&scheduler, &shutting_down)?;
                        // One arrival per attempted pass, before the branch: the
                        // three reconcile entry points below are alternatives, so
                        // hooking them individually would under- or double-count.
                        #[cfg(test)]
                        scheduler.arrive_reconcile_fault_for_test()?;
                        // A prior pass may have built the successor and lost
                        // only the durable write. Republish before choosing a
                        // reconcile branch: graph-off with no text owner goes
                        // to reconcile_now and would otherwise extract again.
                        if let Some(outcome) =
                            scheduler.republish_unpublished_retained_generation()?
                        {
                            return Ok(outcome);
                        }
                        // Legacy graph recovery requires a decoded complete
                        // generation in the serving slot before a dirty-tree
                        // rebuild. Revision 7 deliberately leaves that slot
                        // empty: the retained manifest owner validates and
                        // seats Grafeo below without opening partition bytes.
                        // Graph-off and deferred passes also fall through to
                        // retained text reconciliation.
                        if graph_activation_enabled
                            && !graph_activation_deferred
                            && serving_empty
                            && text_serving_ready
                            && !retained_text_uses_partitioned_manifest
                            && let Some(outcome) =
                                scheduler.seat_retained_generation_on_empty_serving()?
                        {
                            return Ok(outcome);
                        }
                        if retained_partitioned_graph_recovery_pending
                            && let Some(metadata) = retained_text_metadata.as_ref()
                        {
                            // Reserve this pass for verified-head recovery.
                            // `Noop` here means no successor was published;
                            // it never claims source currency. The successor
                            // pass below captures the source state itself so
                            // a quiet remount is not fabricated as dirty.
                            return Ok(CodeIndexReconcileOutcomeV1::Noop(
                                CodeIndexNoopEvidenceV1 {
                                    snapshot_content_identity: metadata
                                        .snapshot()
                                        .content_identity
                                        .clone(),
                                    overflow_reconciled: false,
                                },
                            ));
                        }
                        let outcome = if let Some(metadata) = retained_text_metadata {
                            match scheduler.reconcile_retained_text_generation_with(
                                &metadata,
                                !graph_activation_enabled,
                            ) {
                                Ok(Some(outcome)) => Ok(outcome),
                                Ok(None) if graph_activation_enabled => {
                                    scheduler.activate_or_reconcile()
                                }
                                Ok(None) => scheduler.reconcile_now(),
                                Err(error) => Err(error),
                            }
                        } else if graph_activation_enabled {
                            scheduler.activate_or_reconcile()
                        } else {
                            scheduler.reconcile_now()
                        }?;
                        // A seat whose publishing pass could not prove its
                        // source (`code_index_post_projection_source_unverified`)
                        // installs without a currency witness. The swap arm
                        // re-proves such a seat as `Offered`, but a retained
                        // native graph that already serves skips the graph
                        // prepare and with it the swap, so no later pass ever
                        // reached that arm. This unchanged pass verified
                        // exactly the snapshot the seat was sealed from, so
                        // bind that proof here, while this pass still holds
                        // the scheduler: a reader that holds the scheduler to
                        // keep a seat unproven must not see the proof land
                        // after the pass has already let go.
                        if let CodeIndexReconcileOutcomeV1::Noop(evidence) = &outcome {
                            Self::bind_unproven_seat_to_verified_source(
                                &bind_serving_generation,
                                &bind_serving_source_witness,
                                &bind_source_freshness,
                                &evidence.snapshot_content_identity,
                            );
                        }
                        Ok(outcome)
                    }),
                    // Sealing moved inside this blocking reconcile pipeline.
                    // Keep the outer future labeled so default reports retain
                    // the end-to-end seal path even when short synchronous
                    // inner spans fall below the functions-timing row limit.
                    label = "daemon.code_index.reconcile_or_seal"
                )
                .await;
                if worker_shutting_down.load(Ordering::Acquire) {
                    tracing::info!(
                        event = "code_index_worker_shutdown_observed",
                        phase = "reconcile_or_seal",
                        "code-index worker observed shutdown and stopped its pass"
                    );
                    Self::join_retained_text_projection_on_worker_exit(
                        &mut retained_text_projection,
                    )
                    .await;
                    return;
                }
                if let Ok(Ok(CodeIndexReconcileOutcomeV1::Published(evidence))) = &source_result {
                    // Announce only. This pass has not seated anything yet:
                    // the replacement text owner reopens below and the serving
                    // swap runs after graph work, so recording the id here
                    // made `latest_generation_id` name a generation every
                    // serving arm still answered the *previous* id for.
                    Self::broadcast_generation_publication(
                        &worker_generation_publications,
                        worker_project_root.clone(),
                        evidence,
                    );
                }
                // A retained-generation Noop on an empty serving slot consumed
                // the mount wake, so the dirty-checkout successor rebuild never
                // started. Follow-up notify starts that pass, with two
                // bounds. Not during graph-activation backoff: the scheduled
                // retry is the only self-wake then, while external source
                // hints still wake normally. And only for a consumed external
                // arrival: a self-woken pass with no arrival reproduces the
                // identical Noop, and re-notifying it spins a generation-less
                // worktree forever.
                let retained_noop_follow_up = retained_noop_requires_follow_up_wake(
                    serving_empty,
                    graph_activation_deferred,
                    arrival.wake_micros().is_some(),
                    matches!(&source_result, Ok(Ok(CodeIndexReconcileOutcomeV1::Noop(_)))),
                );
                if let Ok(Ok(outcome)) = &source_result {
                    Self::record_source_reconcile_observation(
                        worker_index_observability.get(),
                        &worker_pending_wake,
                        outcome,
                        started_micros,
                    );
                }
                // Source reconciliation is complete: release the background
                // admission permit before HeadOpening / graph work so sibling
                // stores can start. The permit is never re-acquired inside
                // this pass: `_build_publication` is held for the rest of the
                // iteration, and `run_ignored_dependency_admission` takes the
                // admission *before* that same gate, so waiting on admission
                // here would invert that order (see
                // `background_worker_waits_for_global_admission_before_publication_gate`).
                // Keep `reconcile_pass` through text seating, dropping it
                // made `reconcile_in_progress` lie while this worker still
                // owned graph try_lock, which deadlocked tests that hold the
                // scheduler mutex and wait for that flag.
                drop(_background_reconcile_admission);
                // A publication reopens its own lightweight text owner and
                // swaps it in for the prior one in a single write, so status
                // always names a committed generation. A failed
                // reopen withdraws the prior owner instead: the durable
                // pointer has moved, and the next pass restores from it.
                let published_pass = matches!(
                    &source_result,
                    Ok(Ok(CodeIndexReconcileOutcomeV1::Published(_)))
                );
                let mut graph_text = retained_text.clone();
                // The replacement owner's bounded projection runs on its own
                // task while this pass prepares and activates the graph. Both
                // consume the same sealed generation and are corpus-sized, so
                // graph starts only after text's first advance has opened the
                // build and taken its reservation: graph replay can then no
                // longer hold the source or the RSS headroom text needs to
                // open. The serving swap still waits for the text outcome.
                let mut published_text_projection_outcome = None;
                let mut published_text_projection = None;
                let mut published_text_opened = None;
                if published_pass {
                    let text_scheduler = Arc::clone(&worker_scheduler);
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let published_text = tokio::task::spawn_blocking(move || {
                        Self::lock_scheduler_unless_shutting_down(&text_scheduler, &shutting_down)
                            .map(|mut scheduler| scheduler.servable_retained_text_generation())
                    })
                    .await;
                    if worker_shutting_down.load(Ordering::Acquire) {
                        tracing::info!(
                            event = "code_index_worker_shutdown_observed",
                            phase = "published_text_reopen",
                            "code-index worker observed shutdown and stopped its pass"
                        );
                        Self::join_retained_text_projection_on_worker_exit(
                            &mut retained_text_projection,
                        )
                        .await;
                        return;
                    }
                    graph_text = match published_text {
                        Ok(Ok(Ok(Some(published_text)))) => {
                            *worker_text_generation
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(published_text.clone());
                            // The publication broadcast went out before the
                            // successor's owner was installed; waiters that
                            // probed in between must wake now.
                            worker_serving_generation_changed.send_replace(());
                            Some(published_text)
                        }
                        failed => {
                            if let Ok(Ok(Err(error))) = &failed {
                                tracing::error!(
                                    event = "code_index_published_text_reopen_failed",
                                    error = %error,
                                    "published text restore failed at decoded-cache release"
                                );
                            }
                            *worker_text_generation
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                            worker_serving_generation_changed.send_replace(());
                            None
                        }
                    };
                    // Drive the replacement text owner in this pass. Exact and
                    // lexical are the required fresh-index product; graph
                    // activation is an optional projection that must not take
                    // the source or resident-memory headroom text needs to
                    // open. Yielding back to the loop instead would hand the
                    // next pass a checkout that has already moved, and on a
                    // shared repository that pass publishes again - which is
                    // exactly how a sealed generation stayed unseated forever.
                    if graph_activation_enabled
                        && !graph_activation_deferred
                        && let Some(text) = graph_text.clone()
                    {
                        // `reconcile_pass` covers this projection, so the
                        // pointer rename is inside the pass a reader samples.
                        // The task releases it when text stops, after stamping
                        // the continuation the projection still owes: graph
                        // work that outlives text is not query serving. Taking
                        // the admission permit back here instead would
                        // deadlock against an ignored-dependency owner that
                        // already holds it and is waiting for
                        // `_build_publication`.
                        let projection_pass = if retained_text_projection.is_none()
                            || retained_projection_successor_only
                        {
                            reconcile_pass.take()
                        } else {
                            None
                        };
                        let (opened, text_opened) = tokio::sync::oneshot::channel();
                        published_text_opened = Some(text_opened);
                        let shutting_down = Arc::clone(&worker_shutting_down);
                        let park = Arc::clone(&worker_convergence_park);
                        let projection_pending_wake = Arc::clone(&worker_pending_wake);
                        let projection_wake = Arc::clone(&worker_wake);
                        #[cfg(test)]
                        let project_root = worker_project_root.clone();
                        published_text_projection = Some(tokio::spawn(async move {
                            let _projection_pass = projection_pass;
                            let outcome = Self::drive_text_projection(
                                text.clone(),
                                shutting_down,
                                park,
                                None,
                                Some(opened),
                                #[cfg(test)]
                                project_root,
                            )
                            .await;
                            let schedule_continuation = match outcome {
                                PublishedTextProjectionOutcomeV1::Finished => {
                                    text.text_projection_needs_work()
                                }
                                PublishedTextProjectionOutcomeV1::Unfinished => true,
                                PublishedTextProjectionOutcomeV1::WaitingForMemory
                                | PublishedTextProjectionOutcomeV1::WaitingForStore
                                | PublishedTextProjectionOutcomeV1::Shutdown => false,
                            };
                            if schedule_continuation {
                                Self::note_worker_continuation(
                                    &projection_pending_wake,
                                    &projection_wake,
                                );
                            }
                            outcome
                        }));
                    } else if graph_text
                        .as_ref()
                        .is_none_or(LatestCodeTextGenerationV1::text_projection_needs_work)
                    {
                        Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
                    }
                }
                // Graph seating must not wait for the checkout to hold still.
                // Requiring a `Noop` outcome made tree quiescence the seat
                // condition, and a shared checkout with peers editing never
                // offers that window: every pass published a new generation, so
                // three full seals produced zero seat attempts and every exact
                // graph read answered "not ready" while a complete sealed
                // generation sat on disk. A publication now seats on its own
                // pass, once its lightweight text owner has reopened and its
                // projection has finished; it is stale by
                // construction and superseded by the next publication. An
                // unchanged pass still seats the retained Ready text owner's
                // generation. Source reconciliation is complete either way:
                // release its public freshness guard before the optional
                // O(store) full decode and native graph activation begin; a
                // publication's projection task holds its own share until text
                // stops. Optional graph must not hold it: each graph step that
                // must take the scheduler re-enters the pass around that
                // acquisition (see `lock_scheduler_for_graph_step`); only the
                // unlocked decode and native activation run outside it.
                let gate = GraphSeatGateV1::decide(
                    graph_activation_enabled,
                    graph_activation_deferred,
                    matches!(&source_result, Ok(Ok(_))),
                    published_pass,
                    if published_pass {
                        published_text_projection.is_some()
                            || exact_and_lexical_ready_for_graph(graph_text.as_ref())
                    } else {
                        graph_text.is_some()
                    },
                );
                // An arrival that landed during this retained pass is
                // exact/lexical work waiting for the worker. The optional
                // graph prepare parks this worker on an O(store) sealed
                // decode, tens of seconds on a cold large repository, and
                // serving that arrival must never queue behind it. The
                // arrival's own notify re-runs this worker and the follow-up
                // pass re-gates Prepare from its own terminal outcome, so
                // seating is deferred, never lost. Two bounds keep this a
                // deferral rather than a quiet-wake seat precondition: a
                // published pass never yields (its seat is the pass's own
                // product, and a continuously edited tree has an arrival
                // pending on nearly every pass), and a retained pass yields at
                // most once per prepare so an arrival storm cannot starve
                // seating indefinitely.
                let arrival_pending_before_graph_prepare = gate == GraphSeatGateV1::Prepare
                    && !published_pass
                    && !graph_prepare_yielded_to_arrival
                    && worker_pending_wake.has_pending_arrival();
                if arrival_pending_before_graph_prepare {
                    graph_prepare_yielded_to_arrival = true;
                    tracing::debug!(
                        event = "code_index_graph_seat_skipped",
                        reason = "arrival_pending",
                        published_pass,
                        "an arrival is pending; the optional graph decode yields this pass"
                    );
                }
                // Stamped after the yield sample, which the worker's own
                // follow-up must not trigger, and before the guard drops, so
                // freshness never reads Fresh while the follow-up is owed.
                if retained_noop_follow_up {
                    Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
                }
                // A successor-only retained projection holds no pass guard of
                // its own; keeping the worker's guard through graph seat would
                // report rebuild_in_flight for work that is not query serving.
                if retained_text_projection.is_none() || retained_projection_successor_only {
                    drop(reconcile_pass.take());
                }
                let mut prepare_graph =
                    gate == GraphSeatGateV1::Prepare && !arrival_pending_before_graph_prepare;
                let mut retained_head_recovered_without_complete_replay = false;
                if prepare_graph {
                    graph_prepare_yielded_to_arrival = false;
                }
                if gate == GraphSeatGateV1::RetainedGenerationUnavailable && !published_pass {
                    let text_empty = worker_text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    let serving_empty = worker_serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    if text_empty
                        && serving_empty
                        && arrival.wake_micros().is_some()
                        && warming_restore_arrival != arrival.wake_micros()
                    {
                        // Restore the arrival so warming is not terminal, then
                        // fall through to record this Noop. `continue` skipped
                        // the receipt and left latest_generation_id observers
                        // without event-to-ready evidence. One restore per
                        // arrival: a second identical warming pass proves
                        // nothing became restorable, so the arrival drains
                        // instead of respinning this worker.
                        warming_restore_arrival = arrival.wake_micros();
                        Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
                        worker_wake.notify_one();
                    }
                }
                if let Some(reason) = gate.skip_reason() {
                    tracing::debug!(
                        event = "code_index_graph_seat_skipped",
                        reason,
                        published_pass,
                        "graph seating skipped this pass; the sealed generation stays unseated"
                    );
                }
                if !prepare_graph {
                    let serving_empty = worker_serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    let text_empty = worker_text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_none();
                    // A warming terminal source outcome with nothing seated is
                    // not done: restore the arrival so the next pass can finish
                    // text instead of sleeping until an unrelated hint. Failed
                    // source outcomes already restore in the error arm; waking
                    // those here would spin while Git is unavailable. The same
                    // one-restore-per-arrival bound applies: when the follow-up
                    // pass restored nothing and reconciled to the identical
                    // outcome, the arrival drains rather than respinning.
                    if serving_empty
                        && text_empty
                        && arrival.wake_micros().is_some()
                        && warming_restore_arrival != arrival.wake_micros()
                        && matches!(&source_result, Ok(Ok(_)))
                    {
                        warming_restore_arrival = arrival.wake_micros();
                        Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
                        worker_wake.notify_one();
                    }
                }
                let graph_already_serves = graph_text
                    .as_ref()
                    .is_some_and(|retained| retained.interactive_graph_store().is_ok());
                if prepare_graph && graph_already_serves {
                    // A retained native graph serves without a full decode.
                    // Explicit complete-generation demand still admits binding
                    // and seating, independently of redundant activation. A
                    // predecessor in the serving slot does not satisfy that
                    // demand for the text owner's generation.
                    let text_generation_is_unseated = graph_text.as_ref().is_some_and(|text| {
                        let generation_id = &text.metadata().manifest().generation_id;
                        worker_serving_generation
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_ref()
                            .is_none_or(|seat| {
                                &seat.generation().manifest().generation_id != generation_id
                            })
                    });
                    prepare_graph = text_generation_is_unseated
                        && worker_complete_generation_requested.load(Ordering::Acquire);
                }
                let retained_generation = graph_text
                    .as_ref()
                    .map(|text| text.metadata().manifest().generation_id.clone());
                if graph_head_conflict_generation
                    .as_ref()
                    .is_some_and(|conflicted| {
                        retained_generation
                            .as_ref()
                            .is_some_and(|current| current != conflicted)
                    })
                {
                    graph_head_conflict_generation = None;
                } else if !published_pass
                    && prepare_graph
                    && graph_head_conflict_generation.is_some()
                    && retained_generation.is_some()
                {
                    prepare_graph = false;
                    tracing::debug!(
                        event = "code_index_graph_seat_skipped",
                        reason = "graph_head_belongs_to_another_generation",
                        "this retained manifest already lost the verified graph head to another \
                         generation; it is not replayed"
                    );
                }
                // Recovery installs a fresh graph store on the owner; an owner
                // that already serves one (a publication seated it) keeps it
                // and its warm catalog.
                if prepare_graph
                    && !published_pass
                    && !graph_already_serves
                    && !retained_graph_head_recovery_attempted
                    && let Some(retained) = graph_text
                        .as_ref()
                        .filter(|retained| retained.uses_partitioned_manifest())
                        .cloned()
                {
                    // Every outcome of this attempt schedules one successor.
                    // Stamp it before the recovery await, while the pass is
                    // visible, so the wait cannot be sampled as an idle slot.
                    Self::note_visible_worker_continuation(
                        &worker_reconcile_in_progress,
                        &worker_pending_wake,
                        &worker_wake,
                    );
                    retained_graph_head_recovery_attempted = true;
                    #[cfg(any(test, feature = "test-helpers"))]
                    Self::wait_for_retained_graph_recovery_gate(
                        &worker_project_root,
                        super::RetainedGraphRecoveryPauseV1::BeforeHeadRecovery,
                    )
                    .await;
                    let generation_id = retained.metadata().manifest().generation_id.clone();
                    let recovered_generation = generation_id.clone();
                    let replay_scheduler = Arc::clone(&worker_scheduler);
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let replay_passes = Arc::clone(&worker_reconcile_in_progress);
                    let replay_binding = tokio::task::spawn_blocking(move || {
                        let scheduler = Self::lock_scheduler_for_graph_step(
                            &replay_scheduler,
                            &shutting_down,
                            &replay_passes,
                        )?
                        .1;
                        Ok::<_, CodeIndexSchedulerErrorV1>((
                            scheduler.code_graph_replay_binding(&generation_id)?,
                            scheduler.names_active_publication(&generation_id)?,
                        ))
                    })
                    .await;
                    match replay_binding {
                        Ok(Ok((replay_binding, retained_names_active_publication))) => {
                            match worker_graph_activation
                                .recover_verified_head(
                                    &worker_project_id,
                                    &worker_repository_id,
                                    &worker_worktree_id,
                                    retained,
                                    replay_binding,
                                    Arc::clone(&worker_shutting_down),
                                )
                                .await
                            {
                                Ok(true) => {
                                    // Verified-head recovery is sufficient for
                                    // graph-only readers, but an explicit
                                    // complete-generation consumer still needs
                                    // the decoded serving owner. Continue that
                                    // replay in this pass: the retained text
                                    // projection is joined at the end of the
                                    // pass and can take minutes on a cold large
                                    // store, so deferring the replay to the
                                    // successor strands branch publication
                                    // behind unrelated lexical work.
                                    let complete_generation_requested =
                                        worker_complete_generation_requested
                                            .load(Ordering::Acquire);
                                    prepare_graph = complete_generation_requested;
                                    retained_head_recovered_without_complete_replay =
                                        !complete_generation_requested;
                                    tracing::info!(
                                        event = "code_index_graph_head_recovered",
                                        complete_generation_requested,
                                        "partitioned manifest matched the durable verified graph \
                                         head; startup seated graph reads without replay"
                                    );
                                }
                                Ok(false) => {}
                                Err(error)
                                    if graph_head_belongs_to_another_generation(&error)
                                        && retained_names_active_publication =>
                                {
                                    // The durable pointer names this manifest, so the head is
                                    // older and this generation's graph publication stopped
                                    // before seating; resume it from its sealed segments.
                                    tracing::info!(
                                        event = "code_index_graph_head_recovery_resumes_publication",
                                        error = %error,
                                        "durable publication names the retained manifest; resume \
                                         its interrupted graph publication"
                                    );
                                }
                                Err(error) if graph_head_belongs_to_another_generation(&error) => {
                                    // A newer generation owns the durable pointer. Falling
                                    // through into a cold replay discards the in-flight successor
                                    // that owns the head; do not retry this manifest.
                                    prepare_graph = false;
                                    graph_head_conflict_generation = Some(recovered_generation);
                                    pass_waits_for_store_release = true;
                                    tracing::warn!(
                                        event = "code_index_graph_head_recovery_generation_conflict",
                                        error = %error,
                                        "verified graph head belongs to another generation; this \
                                         retained manifest is not replayed"
                                    );
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        event = "code_index_graph_head_recovery_degraded",
                                        error = %error,
                                        "verified graph head did not match the partitioned \
                                         manifest; replay the exact sealed generation to \
                                         repair its quarantined graph projection"
                                    );
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(
                                event = "code_index_graph_head_recovery_binding_unavailable",
                                error = %error,
                                "partitioned replay binding is unavailable; graph coverage stays \
                                 pending while the admitted worker repairs the generation"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                event = "code_index_graph_head_recovery_task_failed",
                                error = %error,
                                "partitioned replay binding task failed; graph coverage stays \
                                 pending while the admitted worker repairs the generation"
                            );
                        }
                    }
                    // Hold a test-installed successor gate after every reserved
                    // recovery attempt, including Memory authorities that
                    // abstain (`Ok(false)`) and degraded recoveries. Quiet
                    // Persistent recoveries already pause here so observers can
                    // see the retained text owner before the dirty successor
                    // publishes; Memory remounts used to skip the gate and race
                    // the rebuild past that observation window.
                    #[cfg(any(test, feature = "test-helpers"))]
                    if !worker_complete_generation_requested.load(Ordering::Acquire) {
                        Self::wait_for_retained_graph_recovery_gate(
                            &worker_project_root,
                            super::RetainedGraphRecoveryPauseV1::BeforeSuccessor,
                        )
                        .await;
                    }
                    // The reserved pass deliberately did not capture the
                    // checkout, and it consumed whatever wake ran it. Schedule
                    // the successor for every outcome of the attempt, not only
                    // the recovered one: an abstaining or degraded attempt
                    // leaves exactly the same uncaptured source behind, and
                    // the reservation is armed once per worker, so a quiet
                    // reserved pass that answered `Noop` for a checkout with
                    // hook hints already pending stranded them with no wake at
                    // all and never published the successor generation. The
                    // `retained_graph_head_recovery_attempted` guard above is
                    // now false for every later pass, so this cannot spin
                    // another retained-recovery Noop. The successor was
                    // stamped before this await.
                }
                // A recovered revision-7 verified head already serves its
                // native graph from the retained text owner, and that owner
                // is the authority every graph route reads. Preparing the
                // same generation again buys nothing but the O(store)
                // partition replay the verified-head recovery exists to
                // avoid: the decoder reloads the active generation, and the
                // seat that follows is a second copy of what already serves.
                // Restarts of a partitioned manifest therefore leave the
                // sealed slot unseated until an explicit complete-generation
                // consumer asks for it, exactly as graph-only
                // `sealed_decode_count() == 0` requires. A publication seats
                // its own product as usual, and a legacy (non-partitioned)
                // owner still takes the seat.
                if prepare_graph
                    && !published_pass
                    && graph_already_serves
                    && graph_text
                        .as_ref()
                        .is_some_and(LatestCodeTextGenerationV1::uses_partitioned_manifest)
                    && !worker_complete_generation_requested.load(Ordering::Acquire)
                {
                    prepare_graph = false;
                    tracing::debug!(
                        event = "code_index_graph_seat_skipped",
                        reason = "verified_head_already_serves",
                        "the recovered partitioned head already serves; the sealed generation \
                         is not replayed to seat a second copy of it"
                    );
                }
                // Retained passes may race text only far enough to recover a
                // verified graph head, which reads the durable graph store
                // instead of replaying the sealed source. If recovery did not
                // satisfy this pass, wait for text before the corpus-sized
                // decode and replay below. Otherwise a failed fresh text pass
                // becomes a retained pass on its next wake and recreates the
                // same source and resident-memory contention we avoid above.
                // A publication's own projection is the exception: it already
                // holds its build reservation once `opened` fires, and the
                // swap joins it before seating.
                if prepare_graph
                    && published_text_projection.is_none()
                    && !graph_already_serves
                    && !exact_and_lexical_ready_for_graph(graph_text.as_ref())
                {
                    prepare_graph = false;
                    tracing::debug!(
                        event = "code_index_graph_seat_skipped",
                        reason = "text_projection_unfinished",
                        published_pass,
                        "full graph replay waits for exact and lexical projection"
                    );
                }
                // A projection that stopped before opening its build (parked,
                // failed, or already ready) admits graph only on ready owners.
                if prepare_graph
                    && let Some(opened) = published_text_opened.take()
                    && opened.await.is_err()
                {
                    prepare_graph = exact_and_lexical_ready_for_graph(graph_text.as_ref());
                    if !prepare_graph {
                        tracing::debug!(
                            event = "code_index_graph_seat_skipped",
                            reason = "text_projection_unfinished",
                            published_pass,
                            "fresh graph publication waits for a text projection that did not open"
                        );
                    }
                }
                // A publication's graph build refused while its own text
                // projection still holds the build memory is sequencing, not
                // a stall: it runs again as soon as that projection joins.
                let mut graph_waits_for_text = false;
                let mut graph_publication_budget_spent = false;
                let text_projection_running = published_text_projection.is_some();
                let mut result = match source_result {
                    Ok(mut outcome) if prepare_graph => {
                        // Publish the graph head from the sealed segments
                        // before the serving decode below. The graph build and
                        // the decoded generation are this step's two
                        // corpus-sized working sets and must not be resident
                        // together; the activation after the decode recovers
                        // the head published here.
                        let mut graph_publish_refusal = None;
                        let mut graph_head_published = false;
                        // An owner that already serves this generation's graph
                        // needs no second build: only the decode is demanded.
                        // Neither does one whose build already spent its budget.
                        if let Some(text) = graph_text.as_ref().filter(|text| {
                            !graph_already_serves && !text.graph_publication_budget_spent()
                        }) {
                            let generation_id = text.metadata().manifest().generation_id.clone();
                            let binding_scheduler = Arc::clone(&worker_scheduler);
                            let shutting_down = Arc::clone(&worker_shutting_down);
                            let binding_passes = Arc::clone(&worker_reconcile_in_progress);
                            // The retained seat selects a cold or changed-page
                            // plan, then this authority admits that exact plan
                            // before its first page is materialized.
                            let admitted_binding = tokio::task::spawn_blocking(move || {
                                let (_step, scheduler) = Self::lock_scheduler_for_graph_step(
                                    &binding_scheduler,
                                    &shutting_down,
                                    &binding_passes,
                                )?;
                                let binding =
                                    scheduler.code_graph_replay_binding(&generation_id)?;
                                let admission = scheduler.active_generation_decoder().as_ref().map(
                                    DaemonCodeIndexPublicationStoreV1::sealed_graph_build_admission,
                                );
                                Ok::<_, CodeIndexSchedulerErrorV1>((binding, admission))
                            })
                            .await;
                            match admitted_binding {
                                Ok(Ok((replay_binding, admission))) => {
                                    super::CodeIndexWorkerPhaseV1::enter(
                                        &worker_phase_signal,
                                        super::CodeIndexWorkerPhaseV1::PublishingGraph,
                                    );
                                    let published = worker_graph_activation
                                        .publish_sealed_graph(
                                            &worker_project_id,
                                            &worker_repository_id,
                                            &worker_worktree_id,
                                            text,
                                            replay_binding,
                                            admission,
                                            Arc::clone(&worker_shutting_down),
                                        )
                                        .await;
                                    super::CodeIndexWorkerPhaseV1::enter(
                                        &worker_phase_signal,
                                        super::CodeIndexWorkerPhaseV1::Working,
                                    );
                                    match published {
                                        Ok(published) => graph_head_published = published,
                                        Err(error) if error.is_resident_memory_graph_refusal() => {
                                            graph_publish_refusal = Some(error.to_string());
                                        }
                                        Err(error) if error.is_graph_publication_deadline() => {
                                            graph_publication_budget_spent = true;
                                            tracing::warn!(
                                                event = "code_index_graph_publication_budget_spent",
                                                error = %error,
                                                "sealed graph publication spent its background \
                                                 budget; the generation serves exact and lexical \
                                                 with a typed graph refusal and is not replayed"
                                            );
                                        }
                                        Err(error) => tracing::warn!(
                                            event = "code_index_graph_publish_before_decode_failed",
                                            error = %error,
                                            "sealed graph publication failed before the serving \
                                             decode; activation retries it after the decode"
                                        ),
                                    }
                                }
                                Ok(Err(error)) => tracing::warn!(
                                    event = "code_index_graph_publish_binding_unavailable",
                                    error = %error,
                                    "sealed replay binding is unavailable; activation publishes \
                                     the graph after the serving decode"
                                ),
                                Err(error) => tracing::warn!(
                                    event = "code_index_graph_publish_binding_task_failed",
                                    error = %error,
                                    "sealed replay binding task failed; activation publishes the \
                                     graph after the serving decode"
                                ),
                            }
                        }
                        // The head just published serves graph reads from the
                        // text owner's mapped store, exactly as a restart's
                        // verified-head recovery does, so the whole-generation
                        // decode below runs only for a reader that demanded it.
                        let graph_serves_from_text = match graph_text.as_ref() {
                            Some(text) if graph_head_published => {
                                Self::recover_published_graph_head(
                                    &worker_scheduler,
                                    &worker_shutting_down,
                                    &worker_reconcile_in_progress,
                                    &worker_graph_activation,
                                    (
                                        &worker_project_id,
                                        &worker_repository_id,
                                        &worker_worktree_id,
                                    ),
                                    text,
                                )
                                .await
                            }
                            _ => false,
                        };
                        let defer_serving_decode = graph_serves_from_text
                            && !worker_complete_generation_requested.load(Ordering::Acquire);
                        if defer_serving_decode {
                            clear_graph_resident_memory_park(&worker_convergence_park);
                            worker_memory_retry.reset();
                            Self::release_superseded_serving_seat(
                                &worker_serving_generation,
                                &worker_serving_generation_epoch,
                                &worker_serving_source_witness,
                                &worker_serving_seats,
                                &worker_serving_generation_changed,
                            );
                            tracing::info!(
                                event = "code_index_serving_decode_deferred",
                                "the published graph head serves from the text owner; the \
                                 whole-generation decode waits for a reader that needs it"
                            );
                        }
                        let graph_scheduler = Arc::clone(&worker_scheduler);
                        let graph_text = graph_text.clone();
                        let shutting_down = Arc::clone(&worker_shutting_down);
                        let prepare_passes = Arc::clone(&worker_reconcile_in_progress);
                        let prepare_pending_wake = Arc::clone(&worker_pending_wake);
                        let prepare_wake = Arc::clone(&worker_wake);
                        #[cfg(test)]
                        let after_decode_gate =
                            Self::enter_graph_decode_gate(&worker_project_root).await;
                        let prepared = hotpath::future!(
                            tokio::task::spawn_blocking(move || {
                                // A graph build the memory watermark stopped
                                // parks exactly like a decode that does not fit.
                                if let Some(detail) = graph_publish_refusal {
                                    return Ok((
                                        None,
                                        None,
                                        false,
                                        Some(GraphPrepareStopV1::ResidentMemory(detail)),
                                    ));
                                }
                                if defer_serving_decode {
                                    return Ok((None, None, false, None));
                                }
                                let decoder = Self::lock_scheduler_for_graph_step(
                                    &graph_scheduler,
                                    &shutting_down,
                                    &prepare_passes,
                                )?
                                .1
                                .active_generation_decoder();
                                if decoder.is_none() {
                                    tracing::warn!(
                                        event = "code_index_graph_prepare_no_decoder",
                                        "graph prepare found no active generation decoder; \
                                         the sealed generation cannot seat"
                                    );
                                }
                                // The seal released the decoded generation for
                                // the text build. Decoding it again is charged
                                // against the process budget, and a decode that
                                // does not fit parks until memory is given back.
                                let generation = match decoder
                                    .as_ref()
                                    .map(DaemonCodeIndexPublicationStoreV1::load_active_shared)
                                {
                                    None => None,
                                    Some(Ok(generation)) => generation,
                                    Some(Err(error)) => {
                                        let stop = match error {
                                            CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(
                                                detail,
                                            ) => GraphPrepareStopV1::ResidentMemory(detail),
                                            error @ CodeIndexPublicationStoreErrorV1::StoreLockContended => {
                                                GraphPrepareStopV1::StoreBusy(error.to_string())
                                            }
                                            error => {
                                                tracing::warn!(
                                                    event = "code_index_graph_prepare_load_failed",
                                                    error = %error,
                                                    "active generation decode failed; \
                                                     the sealed generation cannot seat"
                                                );
                                                GraphPrepareStopV1::DecodeFailed(error.to_string())
                                            }
                                        };
                                        return Ok((None, None, false, Some(stop)));
                                    }
                                };
                                let latest = match generation {
                                    Some(generation) => Self::lock_scheduler_for_graph_step(
                                        &graph_scheduler,
                                        &shutting_down,
                                        &prepare_passes,
                                    )?
                                    .1
                                    .servable_decoded_retained_generation(
                                        generation,
                                        graph_text.as_ref(),
                                    ),
                                    None => None,
                                };
                                // A refused ignored-source roster clears
                                // itself, so the very next pass can publish
                                // the successor, but this pass consumed the
                                // wake that would have run it.
                                let roster_refusal_rebuild = latest.is_none()
                                    && Self::lock_scheduler_for_graph_step(
                                        &graph_scheduler,
                                        &shutting_down,
                                        &prepare_passes,
                                    )?
                                    .1
                                    .take_ignored_roster_refusal_rebuild();
                                if roster_refusal_rebuild {
                                    // One pass, claimed from the scheduler, so
                                    // a refusal that keeps reproducing cannot
                                    // spin this worker. Stamp before this
                                    // closure drops the step guard: the result
                                    // is observed only after the slot is set.
                                    Self::note_visible_worker_continuation(
                                        &prepare_passes,
                                        &prepare_pending_wake,
                                        &prepare_wake,
                                    );
                                }
                                let replay_binding = match latest.as_ref() {
                                    Some(latest) => Some(
                                        Self::lock_scheduler_for_graph_step(
                                            &graph_scheduler,
                                            &shutting_down,
                                            &prepare_passes,
                                        )?
                                        .1
                                        .code_graph_replay_binding(
                                            &latest.generation().manifest().generation_id,
                                        ),
                                    ),
                                    None => None,
                                };
                                replay_binding
                                    .transpose()
                                    .map(|binding| (latest, binding, roster_refusal_rebuild, None))
                            }),
                            label = "daemon.code_index.graph_prepare"
                        )
                        .await;
                        #[cfg(test)]
                        Self::pass_worker_step_gate(after_decode_gate).await;
                        match prepared {
                            Ok(Ok((_, _, _, Some(stop)))) => {
                                // Once the text owner serves the graph, the
                                // generation has converged: only the reader
                                // that demanded the whole decode waits for
                                // memory, and freshness is not parked for it.
                                let converged = graph_serves_from_text || graph_already_serves;
                                // A build that waits for this pass's own text
                                // projection is rescheduled at its join below.
                                // That projection is also the store-lock holder
                                // a publication's decode meets: its artifact
                                // attachment holds the lock exclusively, and
                                // the join is its release.
                                graph_waits_for_text = published_pass && text_projection_running;
                                match &stop {
                                    GraphPrepareStopV1::ResidentMemory(detail) => {
                                        if !graph_waits_for_text {
                                            if !converged {
                                                park_convergence(
                                                    &worker_convergence_park,
                                                    detail.clone(),
                                                    CONVERGENCE_PARK_GRAPH_RESIDENT_MEMORY_REMEDIATION_V1,
                                                    Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
                                                    true,
                                                );
                                            }
                                            worker_memory_retry.wait();
                                        }
                                        tracing::warn!(
                                            event = "code_index_graph_prepare_decode_refused",
                                            published_pass,
                                            converged,
                                            graph_waits_for_text,
                                            detail = %detail,
                                            "the sealed generation waits to decode until memory \
                                             is given back; text serving is unaffected"
                                        );
                                    }
                                    GraphPrepareStopV1::StoreBusy(detail) => {
                                        if !graph_waits_for_text {
                                            pass_waits_for_store_release = true;
                                        }
                                        tracing::warn!(
                                            event = "code_index_graph_prepare_store_busy",
                                            published_pass,
                                            converged,
                                            graph_waits_for_text,
                                            detail = %detail,
                                            "the sealed generation waits to decode until the \
                                             code-generation store lock is released"
                                        );
                                    }
                                    GraphPrepareStopV1::DecodeFailed(detail) => {
                                        if !graph_waits_for_text && !converged {
                                            park_convergence(
                                                &worker_convergence_park,
                                                detail.clone(),
                                                CONVERGENCE_PARK_RECONCILE_FAILURE_REMEDIATION_V1,
                                                Some(
                                                    CodeIndexBuildBlockedReasonV1::ArtifactStoreUnavailable,
                                                ),
                                                true,
                                            );
                                        }
                                    }
                                }
                                Ok((outcome, None, None))
                            }
                            Ok(Ok((latest, replay_binding, roster_refusal_rebuild, None))) => {
                                if latest.is_none() && !defer_serving_decode {
                                    tracing::warn!(
                                        event = "code_index_graph_prepare_no_servable_generation",
                                        published_pass,
                                        roster_refusal_rebuild,
                                        "graph prepare produced no servable generation; \
                                         the sealed generation cannot seat"
                                    );
                                }
                                Ok((outcome, latest, replay_binding))
                            }
                            Ok(Err(error)) => {
                                outcome = Err(error);
                                Ok((outcome, None, None))
                            }
                            Err(error) => Err(error),
                        }
                    }
                    Ok(outcome) => Ok((outcome, None, None)),
                    Err(error) => Err(error),
                };
                let replace_serving_generation = match &result {
                    Ok((Ok(CodeIndexReconcileOutcomeV1::Noop(_)), Some(latest), Some(_))) => {
                        let serving = worker_serving_generation
                            .read()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        serving.as_ref().is_none_or(|serving| {
                            serving.generation().manifest().generation_id
                                != latest.generation().manifest().generation_id
                        })
                    }
                    Ok((Ok(_), Some(_), Some(_))) => true,
                    _ => false,
                };
                // Activation is decided independently of the seat: every arm
                // below refuses only the native graph call, and the serving
                // swap still installs the prepared generation.
                let activate_graph = match &result {
                    Ok((Ok(_), Some(latest), Some(_)))
                        if graph_publication_budget_spent
                            || latest.graph_publication_budget_spent() =>
                    {
                        false
                    }
                    Ok((Ok(_), Some(latest), Some(_))) => GraphActivationGateV1::decide(
                        graph_already_serves
                            || latest.code_graph_serving_readiness().is_activated(),
                        replace_serving_generation,
                        latest.graph_activation_is_pending(),
                        graph_seat_attempted.as_ref()
                            == Some(&latest.generation().manifest().generation_id),
                    )
                    .activates(),
                    _ => false,
                };
                if activate_graph && let Ok((Ok(_), Some(latest), Some(replay_binding))) = &result {
                    graph_seat_attempted =
                        Some(latest.generation().manifest().generation_id.clone());
                    super::CodeIndexWorkerPhaseV1::enter(
                        &worker_phase_signal,
                        super::CodeIndexWorkerPhaseV1::PublishingGraph,
                    );
                    // The serving generation's graph store is the catalog a
                    // layered successor carries from.
                    let predecessor = worker_serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        .and_then(|serving| serving.interactive_graph_store().ok());
                    let activation = worker_graph_activation
                        .activate(
                            &worker_project_id,
                            &worker_repository_id,
                            &worker_worktree_id,
                            latest.clone(),
                            predecessor,
                            replay_binding.clone(),
                            Arc::clone(&worker_shutting_down),
                        )
                        .await;
                    super::CodeIndexWorkerPhaseV1::enter(
                        &worker_phase_signal,
                        super::CodeIndexWorkerPhaseV1::Working,
                    );
                    match activation {
                        Ok(()) => {
                            next_seat_attempt_at = None;
                            seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                            last_seat_conflict = None;
                            clear_graph_resident_memory_park(&worker_convergence_park);
                            worker_memory_retry.reset();
                        }
                        Err(error) if error.is_graph_activation_refusal() => {
                            next_seat_attempt_at = None;
                            seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                            last_seat_conflict = None;
                            // A configuration refusal never re-attempts, so an
                            // unparked refusal read as an indefinite `indexing`.
                            // A memory refusal parks typed until memory is
                            // given back.
                            if error.is_resident_memory_graph_refusal() {
                                park_convergence(
                                    &worker_convergence_park,
                                    error.to_string(),
                                    CONVERGENCE_PARK_GRAPH_RESIDENT_MEMORY_REMEDIATION_V1,
                                    Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
                                    true,
                                );
                                worker_memory_retry.wait();
                            }
                            tracing::warn!(
                                event = "code_index_graph_activation_refused",
                                error = %error,
                                "code-index generation remains text-serving without native graph"
                            );
                        }
                        Err(error) => {
                            let seat_generation_id =
                                latest.generation().manifest().generation_id.clone();
                            let repeated_conflict = is_repeated_conflict_verdict(
                                &error,
                                &seat_generation_id,
                                last_seat_conflict.as_ref(),
                            );
                            // The generation just sealed is complete; a retryable
                            // activation failure arms the same seat backoff so the
                            // next passes retry this artifact instead of resealing.
                            // A conflict verdict identical to the previous
                            // attempt's for this same generation is deterministic
                            // and falls through to the terminal arm instead.
                            //
                            // The prepared text candidate stays. Wiping it to
                            // `Ok((Err, None, None))` skipped the serving swap,
                            // so search kept the predecessor while graph backoff
                            // ran.
                            if error.is_retryable_activation() && !repeated_conflict {
                                last_seat_conflict = error
                                    .activation_conflict_context()
                                    .map(|context| (seat_generation_id, context.clone()));
                                next_seat_attempt_at = Some(Instant::now() + seat_retry_backoff);
                                let retry_wake = Arc::clone(&worker_wake);
                                let retry_delay = seat_retry_backoff;
                                tokio::spawn(async move {
                                    tokio::time::sleep(retry_delay).await;
                                    retry_wake.notify_one();
                                });
                                tracing::warn!(
                                    event = "code_index_graph_activation_retry_scheduled",
                                    retry_delay_micros = retry_delay.as_micros() as u64,
                                    error = %error,
                                    "graph activation failed retryably; the sealed generation \
                                     still seats and the next pass retries native graph"
                                );
                                hotpath::gauge!("daemon.code_index.graph_seat.retry_total")
                                    .inc(1_u64);
                                hotpath::gauge!(
                                    "daemon.code_index.graph_seat.retry_backoff_micros"
                                )
                                .set(retry_delay.as_micros() as u64);
                                seat_retry_backoff = seat_retry_backoff
                                    .saturating_mul(2)
                                    .min(ACTIVATION_RETRY_BACKOFF_CEILING);
                                // The scheduled retry is the seat attempt, so it
                                // must not be turned away as already attempted.
                                graph_seat_attempted = None;
                                // The prepared candidate stays. Rewriting the
                                // pass result to `Ok((Err, None, None))` here
                                // failed the serving swap's own guard, so an
                                // activation that kept failing retryably never
                                // let any pass seat: search held its predecessor
                                // while a complete generation sat on disk. The
                                // terminal arm below already keeps the seat and
                                // marks graph unavailable; a retry is a weaker
                                // verdict than terminal and must not seat less.
                            } else {
                                next_seat_attempt_at = None;
                                seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                                last_seat_conflict = None;
                                latest.mark_graph_activation_unavailable(error.to_string());
                                if repeated_conflict {
                                    tracing::warn!(
                                        event = "code_index_graph_activation_conflict_terminal",
                                        error = %error,
                                        "graph activation repeated an identical conflict \
                                         verdict for the same sealed generation; retrying \
                                         cannot succeed, so the generation serves exact and \
                                         lexical with typed graph unavailability"
                                    );
                                } else {
                                    tracing::warn!(
                                        event = "code_index_graph_activation_failed",
                                        error = %error,
                                        "graph activation failed terminally; exact and lexical \
                                         serving remain available with typed graph unavailability"
                                    );
                                }
                            }
                        }
                    }
                }
                // Join the publication's projection, then process its outcome
                // at the existing source-proof and serving-swap boundary. Graph
                // work above overlapped it; nothing seats before text is done.
                if let Some(projection) = published_text_projection.take() {
                    published_text_projection_outcome = Some(match projection.await {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            if let Some(text) = graph_text.as_ref() {
                                text.mark_text_serving_failed();
                            }
                            park_convergence(
                                &worker_convergence_park,
                                format!("code text projection task failed abnormally: {error}"),
                                CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                                None,
                                false,
                            );
                            tracing::warn!(
                                event = "code_index_text_projection_task_failed",
                                error = %error,
                                "published text projection task failed before graph seating"
                            );
                            PublishedTextProjectionOutcomeV1::Unfinished
                        }
                    });
                }
                if graph_waits_for_text {
                    Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
                }
                if let Some(outcome) = published_text_projection_outcome.take() {
                    // A clone-fingerprint successor is still `Unfinished` work
                    // after exact and lexical owners are ready. That must not
                    // clear the prepared generation the way a missing owner does.
                    let owners_ready = exact_and_lexical_ready_for_graph(graph_text.as_ref());
                    let outcome = match outcome {
                        PublishedTextProjectionOutcomeV1::Unfinished
                            if !super::text_projection_unfinished_withholds_seat(owners_ready) =>
                        {
                            PublishedTextProjectionOutcomeV1::Finished
                        }
                        other => other,
                    };
                    if matches!(outcome, PublishedTextProjectionOutcomeV1::Finished) {
                        worker_memory_retry.reset();
                    }
                    match outcome {
                        PublishedTextProjectionOutcomeV1::Finished => {
                            // The seat needs only the ready owners. Text work
                            // left in the slot is retained-owner work on the
                            // next pass (no pass guard once owners already
                            // serve); schedule that pass here, since there is
                            // no periodic cadence timer.
                            if graph_text
                                .as_ref()
                                .is_some_and(LatestCodeTextGenerationV1::text_projection_needs_work)
                            {
                                // Already stamped before optional graph. Re-enter
                                // the pass so a reader that cleared the slot
                                // during graph still cannot sample the stamp.
                                Self::note_visible_worker_continuation(
                                    &worker_reconcile_in_progress,
                                    &worker_pending_wake,
                                    &worker_wake,
                                );
                            }
                            // Source evidence (a hint, an observed change, or a
                            // Git metadata move) can land during a large text
                            // projection, after the proof established before
                            // publication. The serving swap must bind to source
                            // truth observed after that work, otherwise an
                            // exact current generation seats without a witness
                            // and every readiness read schedules another
                            // identical Noop.
                            //
                            // A plain write moves neither the source epoch nor
                            // Git metadata, so an unmoved proof does not mean
                            // an unchanged tree. Whatever the proof says, the
                            // sealed digests are swept again here: a write that
                            // landed during the projection is observed now,
                            // leaves the seat stale, and wakes its successor.
                            if let Some(text) = graph_text.as_ref() {
                                let proof_unmoved = worker_source_freshness.serves_verified_source(
                                    &text.metadata().snapshot().content_identity,
                                    &worker_project_root,
                                    &worker_shutting_down,
                                );
                                let scheduler = Arc::clone(&worker_scheduler);
                                let shutting_down = Arc::clone(&worker_shutting_down);
                                let pending_wake = Arc::clone(&worker_pending_wake);
                                let wake = Arc::clone(&worker_wake);
                                let source_freshness = worker_source_freshness.clone();
                                let metadata = text.metadata().clone();
                                let source_current = tokio::task::spawn_blocking(move || {
                                    let mut scheduler = Self::lock_scheduler_unless_shutting_down(
                                        &scheduler,
                                        &shutting_down,
                                    )?;
                                    if !proof_unmoved
                                        && matches!(
                                            scheduler.reconcile_retained_text_generation_with(
                                                &metadata, false,
                                            )?,
                                            Some(CodeIndexReconcileOutcomeV1::Noop(_))
                                        )
                                    {
                                        return Ok(true);
                                    }
                                    // A pending hint or observed change already
                                    // carries its own wake; sweeping on top of
                                    // it would turn that targeted pass into an
                                    // overflow rescan.
                                    if source_freshness.source_change_pending() {
                                        return Ok(false);
                                    }
                                    let moved = scheduler.request_fresh_now_background();
                                    if moved {
                                        Self::note_wake(
                                            &pending_wake,
                                            &wake,
                                            CodeIndexCadenceTriggerV1::Overflow,
                                        );
                                    }
                                    Ok::<_, CodeIndexSchedulerErrorV1>(!moved)
                                })
                                .await;
                                match source_current {
                                    Ok(Ok(true)) => {}
                                    Ok(Ok(false)) => tracing::info!(
                                        event = "code_index_post_projection_source_unverified",
                                        "source moved while text projection ran; the completed generation may only take a stale seat"
                                    ),
                                    Ok(Err(error)) => tracing::warn!(
                                        event = "code_index_post_projection_source_verification_failed",
                                        error = %error,
                                        "source verification after text projection failed; the completed generation may only take a stale seat"
                                    ),
                                    Err(error) => tracing::warn!(
                                        event = "code_index_post_projection_source_verification_task_failed",
                                        error = %error,
                                        "source verification after text projection did not complete; the completed generation may only take a stale seat"
                                    ),
                                }
                            }
                            // The ready text owner now serves this generation,
                            // with or without a decoded seat after it.
                            worker_serving_generation_changed.send_replace(());
                        }
                        PublishedTextProjectionOutcomeV1::Shutdown => {
                            tracing::info!(
                                event = "code_index_worker_shutdown_observed",
                                phase = "text_projection_before_seat",
                                "code-index worker observed shutdown and stopped its pass"
                            );
                            Self::join_retained_text_projection_on_worker_exit(
                                &mut retained_text_projection,
                            )
                            .await;
                            return;
                        }
                        PublishedTextProjectionOutcomeV1::WaitingForMemory => {
                            if let Ok((_, latest, replay_binding)) = &mut result {
                                *latest = None;
                                *replay_binding = None;
                            }
                            worker_memory_retry.wait();
                        }
                        PublishedTextProjectionOutcomeV1::WaitingForStore => {
                            if let Ok((_, latest, replay_binding)) = &mut result {
                                *latest = None;
                                *replay_binding = None;
                            }
                            pass_waits_for_store_release = true;
                        }
                        PublishedTextProjectionOutcomeV1::Unfinished => {
                            if let Ok((_, latest, replay_binding)) = &mut result {
                                *latest = None;
                                *replay_binding = None;
                            }
                            tracing::debug!(
                                event = "code_index_graph_seat_skipped",
                                reason = "published_text_owner_unfinished",
                                published_pass,
                                "the publication's text owner did not finish its projection; \
                                 the sealed generation stays unseated until it does"
                            );
                            Self::note_visible_worker_continuation(
                                &worker_reconcile_in_progress,
                                &worker_pending_wake,
                                &worker_wake,
                            );
                        }
                    }
                    // Keep the pass lifetime around the post-projection source
                    // proof so concurrent reads attribute their single wake as
                    // a follow-up to this owner. The swap below takes its own
                    // nested guard and publishes the witness before either
                    // lifetime becomes idle.
                }
                #[cfg(test)]
                if matches!(&result, Ok((Ok(_), Some(_), _))) {
                    Self::wait_for_serving_swap_gate(&worker_project_root).await;
                }
                if let Ok((Ok(_), Some(latest), _)) = &result {
                    let scheduler = Arc::clone(&worker_scheduler);
                    let serving_generation = Arc::clone(&worker_serving_generation);
                    let serving_generation_epoch = Arc::clone(&worker_serving_generation_epoch);
                    let serving_source_witness = Arc::clone(&worker_serving_source_witness);
                    let text_generation = Arc::clone(&worker_text_generation);
                    let serving_seats = Arc::clone(&worker_serving_seats);
                    let serving_generation_changed = worker_serving_generation_changed.clone();
                    let text_latest = latest.clone();
                    let latest = latest.clone();
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let swap_passes = Arc::clone(&worker_reconcile_in_progress);
                    let serving_swap = hotpath::future!(
                        tokio::task::spawn_blocking(move || {
                            let (_swap_pass, scheduler) = Self::lock_scheduler_for_graph_step(
                                &scheduler,
                                &shutting_down,
                                &swap_passes,
                            )?;
                            // A generation that sealed while the checkout kept
                            // moving is stale the moment it completes, and the
                            // durable pointer may already name its successor.
                            // Refusing the swap outright then left the route
                            // serving nothing at all, so an empty serving slot
                            // takes the stale seat and the next publication
                            // supersedes it.
                            //
                            // Only the *active* publication may refuse that
                            // stale seat. Asking merely whether something was
                            // seated let an incumbent the canonical store had
                            // already superseded keep the slot forever: both
                            // candidate and incumbent were then non-active, so
                            // every later pass refused too and serving never
                            // converged on the durable head.
                            let publication_matches =
                                scheduler.active_publication_matches(&latest)?;
                            // The freshness fence is the single source-currency
                            // authority; the witness only binds one of its
                            // proofs to the seat. Asking the fence whether it
                            // has verified *this* sealed snapshot is what makes
                            // the binding truthful for a seat this pass did not
                            // publish. A git-index sample this seal moved is
                            // not a different snapshot.
                            // Dropping the witness here cleared the newer
                            // generation. The lexical full-copy is not decided
                            // on this swap.
                            let sealed_currency = scheduler.currency_witness_for_sealed_snapshot(
                                &latest.generation().manifest().generation_id,
                                &latest.generation().snapshot().content_identity,
                            );
                            // A store that cannot answer keeps the incumbent
                            // rather than displacing it.
                            let incumbent_is_active =
                                |incumbent: &Option<LatestCompleteCodeIndexV1>| {
                                    incumbent.as_ref().is_some_and(|incumbent| {
                                        scheduler
                                            .active_publication_matches(incumbent)
                                            .unwrap_or(true)
                                    })
                                };
                            // The publication comparison runs outside the
                            // write guard, so readers never queue behind it.
                            // Every slot writer bumps the epoch, so an
                            // unchanged epoch under the guard proves the
                            // incumbent it judged is still seated.
                            let observed_epoch = serving_generation_epoch.load(Ordering::Acquire);
                            let observed_incumbent = serving_generation
                                .read()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .clone();
                            let observed_active = incumbent_is_active(&observed_incumbent);
                            drop(observed_incumbent);
                            let mut serving = serving_generation
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let incumbent_is_active = if serving_generation_epoch
                                .load(Ordering::Acquire)
                                == observed_epoch
                            {
                                observed_active
                            } else {
                                incumbent_is_active(&serving)
                            };
                            let outcome = ServingSwapOutcomeV1::decide(
                                publication_matches,
                                incumbent_is_active,
                                replace_serving_generation,
                            );
                            // The displaced seats can hold the last reference
                            // to a whole generation; they drop after the
                            // guards, not inside the swap.
                            let mut displaced = (None, None);
                            if outcome.installs() {
                                displaced.0 = serving.replace(latest.clone());
                                serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
                                displaced.1 = text_generation
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .replace(latest.text_generation_handle());
                            }
                            // A pass is the only thing that verifies source
                            // against the sealed digests, so it is also the
                            // only thing that can re-prove a seat. `Offered`
                            // is that case: the active publication already
                            // serves and this pass re-observed the checkout it
                            // was sealed from. Arming only on a publication
                            // left a restored, retired, or withdrawn seat
                            // permanently unproven, busy verified reads then
                            // refused a generation whose source was current.
                            match outcome {
                                ServingSwapOutcomeV1::Seated | ServingSwapOutcomeV1::Offered => {
                                    *serving_source_witness
                                        .write()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        sealed_currency;
                                }
                                // The durable pointer names a successor, so no
                                // proof of this seat's currency exists to bind.
                                ServingSwapOutcomeV1::SeatedStale => {
                                    *serving_source_witness
                                        .write()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                                }
                                // The slot kept a foreign incumbent; its
                                // witness belongs to that generation.
                                ServingSwapOutcomeV1::Superseded => {}
                            }
                            drop(serving);
                            drop(displaced);
                            // The serving slot is now fully published, including
                            // its exact-source witness, so dependent readers
                            // may wake.
                            if outcome.installs() {
                                Self::record_serving_seat(&serving_seats);
                                serving_generation_changed.send_replace(());
                            }
                            Ok::<_, CodeIndexSchedulerErrorV1>(outcome)
                        }),
                        label = "daemon.code_index.serving_swap"
                    )
                    .await;
                    match serving_swap {
                        Ok(Ok(outcome)) => {
                            let generation_id = text_latest
                                .generation()
                                .manifest()
                                .generation_id
                                .as_str()
                                .to_owned();
                            match outcome {
                                // Graph seating is the last strict-readiness
                                // boundary after text publication, so it is
                                // `info` like `code_index_generation_published`:
                                // a dogfood or operator log must be able to
                                // tell "text current, graph pending" from
                                // "graph seated" without a debug filter.
                                ServingSwapOutcomeV1::Seated => tracing::info!(
                                    event = "code_index_serving_generation_seated",
                                    generation_id,
                                    "the sealed generation now serves"
                                ),
                                ServingSwapOutcomeV1::SeatedStale => tracing::info!(
                                    event = "code_index_serving_generation_seated_stale",
                                    generation_id,
                                    "the sealed generation seats stale: the durable pointer \
                                     already names its successor"
                                ),
                                ServingSwapOutcomeV1::Superseded => tracing::warn!(
                                    event = "code_index_serving_swap_superseded",
                                    generation_id,
                                    "the reconciled generation is no longer the active durable \
                                     publication; the seated generation keeps serving"
                                ),
                                ServingSwapOutcomeV1::Offered => {}
                            }
                            // Text work a seated owner still owes is this
                            // worker's slice: the worker has no cadence timer,
                            // so stamp what the two sibling publication sites
                            // above already stamp.
                            if text_latest.text_projection_needs_work() {
                                Self::note_visible_worker_continuation(
                                    &worker_reconcile_in_progress,
                                    &worker_pending_wake,
                                    &worker_wake,
                                );
                            }
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(
                                event = "code_index_serving_swap_failed",
                                error = %error,
                                "the serving swap refused the reconciled generation"
                            );
                            result = Ok((Err(error), None, None));
                        }
                        Err(error) => {
                            tracing::warn!(
                                event = "code_index_serving_swap_task_failed",
                                error = %error,
                                "the serving-swap task failed; the sealed generation stays unseated"
                            );
                            result = Ok((
                                Err(CodeIndexSchedulerErrorV1::Identity(format!(
                                    "serving-swap task failed: {error}"
                                ))),
                                None,
                                None,
                            ));
                        }
                    }
                }
                // The source proof and serving witness are now published as
                // one lifecycle. Optional receipts do not keep source
                // verification in flight. A retained-work continuation this
                // pass already knows about is stamped first, so the drop is
                // not an empty slot.
                if retained_work_waiting_for_source
                    && matches!(
                        &result,
                        Ok((Ok(CodeIndexReconcileOutcomeV1::Noop(_)), _, _))
                    )
                    && worker_text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some()
                    && worker_source_freshness
                        .ready_without_stat(&worker_project_root, &worker_shutting_down)
                {
                    Self::note_visible_worker_continuation(
                        &worker_reconcile_in_progress,
                        &worker_pending_wake,
                        &worker_wake,
                    );
                }
                drop(reconcile_pass.take());
                worker_byte_pool.release_dead_entries();
                collect_installed_worker_heaps_when_idle();
                let refused_for_memory = matches!(
                    &result,
                    Ok((Err(error), _, _)) if error.is_resident_memory_refusal()
                );
                let waited_for_memory = worker_residency.refresh_waits_for_memory();
                worker_residency.set_refresh_waits_for_memory(refused_for_memory);
                if let Ok((Err(error), _, _)) = &result
                    && refused_for_memory
                {
                    park_convergence(
                        &worker_convergence_park,
                        error.to_string(),
                        CONVERGENCE_PARK_REFRESH_RESIDENT_MEMORY_REMEDIATION_V1,
                        Some(CodeIndexBuildBlockedReasonV1::ResidentMemory),
                        true,
                    );
                    worker_residency.yield_serving_graph_to_refresh(&worker_resident_owners);
                } else if waited_for_memory {
                    clear_graph_resident_memory_park(&worker_convergence_park);
                }
                if let Ok((Ok(outcome), _, _)) = &result {
                    // A pass that ran to a terminal outcome proves neither the
                    // panicking input nor the capacity contention is still
                    // reproducing, so both bounded retry states restart.
                    panic_guard.record_progress();
                    capacity_retry.record_progress();
                    let _service_micros = Self::record_reconcile_receipt(
                        &worker_cadence_telemetry,
                        worker_project_root.clone(),
                        arrival,
                        trigger,
                        started_micros,
                        outcome,
                    );
                    // An unchanged-source reconcile can restore admission for
                    // retained text authority without publishing or replacing
                    // a decoded seat. Partitioned verified-head recovery
                    // intentionally leaves that seat empty, so the canonical
                    // text owner and current source proof are the wake
                    // authority. Readers still validate scope and freshness.
                    let retained_text_serves =
                        matches!(outcome, CodeIndexReconcileOutcomeV1::Noop(_))
                            && worker_text_generation
                                .read()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .is_some();
                    if retained_text_serves
                        && worker_source_freshness
                            .ready_without_stat(&worker_project_root, &worker_shutting_down)
                    {
                        // The pass bound the seat's source proof under the
                        // scheduler lock; announce the change now that the
                        // proof is public.
                        worker_serving_generation_changed.send_replace(());
                        // The retained-work continuation was stamped before
                        // this pass dropped `reconcile_in_progress`.
                    }
                } else {
                    // Surface bounded non-terminal failure without new project-path data.
                    match &result {
                        Ok((Err(error), _, _)) if error.reconcile_interruption().is_some() => {
                            // An interrupted pass ran to a typed stop, not a
                            // failure: shutdown, or a newer source observation
                            // advanced the cancellation epoch. Every epoch
                            // advance is paired with a wake, so the restored
                            // arrival below is picked up by the pass that
                            // observation already scheduled. Attribute the
                            // origin instead of reporting the served
                            // generation stale.
                            panic_guard.record_progress();
                            capacity_retry.record_progress();
                            let origin = if worker_shutting_down.load(Ordering::Acquire) {
                                "shutdown"
                            } else {
                                match error.reconcile_interruption() {
                                    Some(CodeIndexInterruptionV1::DeadlineExceeded) => "deadline",
                                    _ => "superseded_by_source_observation",
                                }
                            };
                            tracing::info!(
                                event = "code_index_reconcile_interrupted",
                                path = "background_worker",
                                origin,
                                trigger = trigger.label(),
                                "code-index background reconcile was interrupted; the pending wake re-runs it"
                            );
                        }
                        Ok((Err(error), _, _)) => {
                            // The pass completed; whatever refused it was not an
                            // unwind, so panic accounting restarts.
                            panic_guard.record_progress();
                            let publication_corruption =
                                error.is_publication_authority_corruption();
                            let mut store_lock_held = error.is_store_lock_contended();
                            let transient_capacity =
                                !store_lock_held && error.is_transient_capacity_failure();
                            // A corrupt derived publication is deleted and
                            // rebuilt from source, once per mount. A store
                            // that is corrupt again after its own rebuild, or
                            // that cannot be deleted, is the terminal park;
                            // a store another owner holds is retried when it
                            // is released. `None` here means the failure was
                            // not corruption or the reset is being retried.
                            let publication_reset = if publication_corruption {
                                match Self::reset_corrupt_publication_authority(
                                    &worker_scheduler,
                                    &mut publication_authority_reset_attempted,
                                    error,
                                ) {
                                    PublicationAuthorityResetV1::Rebuilding => {
                                        clear_convergence_park(&worker_convergence_park);
                                        worker_serving_generation_changed.send_replace(());
                                        worker_wake.notify_one();
                                        None
                                    }
                                    PublicationAuthorityResetV1::StoreBusy => {
                                        store_lock_held = true;
                                        None
                                    }
                                    PublicationAuthorityResetV1::Terminal {
                                        reason,
                                        remediation,
                                    } => Some((reason, remediation)),
                                }
                            } else if refused_for_memory {
                                if !waited_for_memory {
                                    tracing::warn!(
                                        event = "code_index_refresh_waiting_for_memory",
                                        path = "background_worker",
                                        trigger = trigger.label(),
                                        error = %error,
                                        "code-index refresh refused for resident memory; the served generation stays stale until memory is given back"
                                    );
                                }
                                None
                            } else {
                                tracing::warn!(
                                    event = "code_index_reconcile_failed",
                                    path = "background_worker",
                                    transient_capacity,
                                    store_lock_held,
                                    trigger = trigger.label(),
                                    error = %error,
                                    "code-index background reconcile failed; the served generation stays stale"
                                );
                                None
                            };
                            if let Some((reason, remediation)) = publication_reset {
                                capacity_retry.record_progress();
                                park_convergence(
                                    &worker_convergence_park,
                                    reason,
                                    remediation,
                                    Some(
                                        CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt,
                                    ),
                                    false,
                                );
                                worker_build_progress
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .block_current(
                                        CodeIndexBuildBlockedReasonV1::PublicationAuthorityCorrupt,
                                    );
                                // Mid-wait branch publication rechecks the park on
                                // serving-generation notifications. The next
                                // wake reads that same slot; no local flag.
                                worker_serving_generation_changed.send_replace(());
                            } else if store_lock_held {
                                capacity_retry.record_progress();
                                pass_waits_for_store_release = true;
                            } else if refused_for_memory {
                                // The refusal registered with the resident-memory
                                // authority; the headroom wake is its retry.
                                capacity_retry.record_progress();
                            } else if transient_capacity {
                                // A graph operation budget another holder
                                // releases without waking this worktree gets a
                                // bounded retry. Permanent refusals never reach
                                // here: retrying those forever is the failure
                                // this loop already had.
                                if !Self::arm_capacity_retry(&mut capacity_retry, &worker_wake) {
                                    tracing::warn!(
                                        event = "code_index_reconcile_capacity_retry_exhausted",
                                        path = "background_worker",
                                        consecutive = capacity_retry.consecutive(),
                                        "code-index reconcile stopped retrying a capacity refusal; the next hint retries"
                                    );
                                }
                            } else if error.reproduces_on_unchanged_input() {
                                // The restored arrival alone read as an
                                // indefinite `indexing` while every wake
                                // rebuilt the whole worktree into the same
                                // refusal. Park it typed and hold passes
                                // until the input changes; a daemon restart
                                // remounts with an empty park and retries.
                                capacity_retry.record_progress();
                                panic_guard.quarantine_unchanged_input(pass_control_epoch);
                                park_convergence(
                                    &worker_convergence_park,
                                    error.to_string(),
                                    CONVERGENCE_PARK_RECONCILE_FAILURE_REMEDIATION_V1,
                                    None,
                                    false,
                                );
                            } else {
                                capacity_retry.record_progress();
                            }
                        }
                        Err(error) if error.is_panic() => {
                            // Arbitrary user source runs through the indexing
                            // pool, so an unwind here is malformed input that
                            // reproduces byte-for-byte on every later pass.
                            // Bound it instead of re-dispatching the identical
                            // unit on every wake.
                            capacity_retry.record_progress();
                            let decision = panic_guard.record_panic(
                                tokio::time::Instant::now(),
                                worker_control_epoch.load(Ordering::Acquire),
                            );
                            match decision {
                                ReconcilePanicDecisionV1::RetryAfter(delay) => {
                                    tracing::warn!(
                                        event = "code_index_reconcile_panicked",
                                        path = "background_worker",
                                        consecutive_panics = panic_guard.consecutive_panics(),
                                        error = %error,
                                        "code-index background reconcile panicked; retrying the same input with backoff"
                                    );
                                    let retry_wake = Arc::clone(&worker_wake);
                                    tokio::spawn(async move {
                                        tokio::time::sleep(delay).await;
                                        retry_wake.notify_one();
                                    });
                                }
                                ReconcilePanicDecisionV1::Quarantine => tracing::warn!(
                                    event = "code_index_reconcile_quarantined",
                                    path = "background_worker",
                                    consecutive_panics = panic_guard.consecutive_panics(),
                                    error = %error,
                                    "code-index background reconcile is quarantined after repeated panics; changed input or a progressing pass resumes it"
                                ),
                            }
                        }
                        Err(error) => tracing::warn!(
                            event = "code_index_reconcile_failed",
                            path = "background_worker",
                            error = %error,
                            "code-index background reconcile task did not complete"
                        ),
                        Ok((Ok(_), _, _)) => {}
                    }
                    if !publication_authority_is_terminal(&worker_convergence_park) {
                        // Restore arrival so the next pass measures this wake's full queue wait.
                        Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
                    }
                }
                // The retained owner's projection is joined after the seat, not
                // before it: the graph this pass recovered already serves, and
                // an unfinished projection withholds only exact and lexical
                // serving. `reconcile_in_progress` stays truthful until here so
                // admission does not misread in-flight text work as
                // unavailability.
                if let Some(projection) = retained_text_projection.as_mut() {
                    let projection_result = if retained_head_recovered_without_complete_replay {
                        let mut preserve_worker_wake = false;
                        let result = loop {
                            tokio::select! {
                                outcome = &mut *projection => break Some(outcome),
                                () = async {
                                    match worker_complete_generation_requested_changed
                                        .wait_for(|requested| *requested)
                                        .await
                                    {
                                        Ok(_) | Err(_) => {}
                                    }
                                } => {
                                    // Recovery already satisfied graph-only reads, but a complete
                                    // consumer arrived after that decision while text projection
                                    // was still running. Retain the projection handle across the
                                    // successor pass so it remains uniquely owned and joined.
                                    break None;
                                }
                                () = worker_wake.notified() => {
                                    if worker_shutting_down.load(Ordering::Acquire) {
                                        break Some((&mut *projection).await);
                                    }
                                    // The pending arrival remains authoritative while this pass
                                    // owns the projection. Preserve one permit after the join;
                                    // unlike complete demand, an unrelated wake cannot end it.
                                    preserve_worker_wake = true;
                                }
                            }
                        };
                        if preserve_worker_wake {
                            worker_wake.notify_one();
                        }
                        result
                    } else {
                        Some(projection.await)
                    };
                    let Some(projection_result) = projection_result else {
                        drop(reconcile_pass.take());
                        continue;
                    };
                    // The task completed in the branch above. Remove that
                    // completed handle before processing its outcome so a
                    // later pass may start a new owner only when needed.
                    retained_text_projection.take();
                    let outcome = match projection_result {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            if let Some(text) = retained_text.as_ref() {
                                text.mark_text_serving_failed();
                            }
                            park_convergence(
                                &worker_convergence_park,
                                format!("code text projection task failed abnormally: {error}"),
                                CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                                None,
                                false,
                            );
                            tracing::warn!(
                                event = "code_index_text_projection_task_failed",
                                error = %error,
                                "retained text projection task failed"
                            );
                            PublishedTextProjectionOutcomeV1::Unfinished
                        }
                    };
                    if matches!(outcome, PublishedTextProjectionOutcomeV1::Finished) {
                        worker_memory_retry.reset();
                        worker_serving_generation_changed.send_replace(());
                    }
                    match outcome {
                        PublishedTextProjectionOutcomeV1::Finished
                            if !retained_head_recovered_without_complete_replay
                                && !retained_projection_successor_only =>
                        {
                            // Graph head recovery may have abstained while the
                            // retained text task was still running. Give the
                            // now-ready owner one bounded successor pass so a
                            // full replay can proceed without overlapping it.
                            // Work behind owners that already served changed
                            // no owner the seat reads, so it owes no such pass.
                            Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
                        }
                        PublishedTextProjectionOutcomeV1::Finished => {
                            let installed_owner_still_needs_work = worker_text_generation
                                .read()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .as_ref()
                                .is_some_and(
                                    LatestCodeTextGenerationV1::text_projection_needs_work,
                                );
                            if installed_owner_still_needs_work {
                                Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
                            }
                        }
                        PublishedTextProjectionOutcomeV1::Shutdown => {
                            tracing::info!(
                                event = "code_index_worker_shutdown_observed",
                                phase = "retained_text_projection",
                                "code-index worker observed shutdown and stopped its pass"
                            );
                            Self::join_retained_text_projection_on_worker_exit(
                                &mut retained_text_projection,
                            )
                            .await;
                            return;
                        }
                        PublishedTextProjectionOutcomeV1::Unfinished => {
                            // The owner carries the typed state; a later pass
                            // re-checks it. Without this wake a projection that
                            // stopped short would sleep until an unrelated
                            // arrival, exactly as the inline slice's own
                            // follow-up notify prevented.
                            Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
                        }
                        PublishedTextProjectionOutcomeV1::WaitingForStore => {
                            pass_waits_for_store_release = true;
                        }
                        PublishedTextProjectionOutcomeV1::WaitingForMemory => {
                            worker_memory_retry.wait();
                        }
                    }
                    // The continuation is already in the slot. Dropping here
                    // is the first moment this pass looks idle.
                    drop(reconcile_pass.take());
                }
                if worker_shutting_down.load(Ordering::Acquire) {
                    tracing::info!(
                        event = "code_index_worker_shutdown_observed",
                        phase = "pass_end",
                        "code-index worker observed shutdown and stopped its pass"
                    );
                    Self::join_retained_text_projection_on_worker_exit(
                        &mut retained_text_projection,
                    )
                    .await;
                    return;
                }
                let _ = result;
            }
        });
        let task = tokio::spawn(hotpath::future!(
            worker_loop,
            label = "daemon.code_index.scheduler_worker"
        ));
        self.register_worker_shutdown_signal(&shutting_down, &wake, &serving_generation_changed);
        let residency_registration = Arc::clone(&residency).register(
            &self.resident_owners,
            tracedecay_runtime_core::resident_memory::ResidentOwnerScopeV1 {
                project_id: project_id.clone(),
                worktree_id: worktree_id.clone(),
            },
        );
        // Memory given back anywhere in the process is the retry for work
        // refused for it: an owner released, or the resident-memory authority
        // finding a refused request would now fit. It wakes the worker even
        // over an arrival a refused pass restored without a permit. The
        // watcher holds no strong reference, so it ends with the worktree.
        let mut owner_headroom = self.resident_owners.subscribe_headroom();
        let mut admission_headroom = self.resident_memory.pressure().subscribe_headroom();
        let headroom_pending_wake = Arc::downgrade(&pending_wake);
        let headroom_wake = Arc::downgrade(&wake);
        let headroom_text = Arc::downgrade(&text_generation);
        let headroom_park = Arc::downgrade(&convergence_park);
        let headroom_residency = Arc::downgrade(&residency);
        tokio::spawn(async move {
            loop {
                let changed = tokio::select! {
                    changed = owner_headroom.changed() => changed,
                    changed = admission_headroom.changed() => changed,
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

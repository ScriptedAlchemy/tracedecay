//! Worktree mount: cold-mount admission through the first serving generation.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tracedecay_application::semantic_runtime::SavedGenerationScheduleOutcomeV1;
use tracedecay_code_index::production::CodeIndexInterruptionV1;
use tracedecay_contracts::code_index_freshness::{
    CodeGraphServingReadinessV1, CodeIndexConvergenceParkedV1,
};
use tracedecay_domain::ProjectId;

use super::super::{
    CodeIndexCadenceTriggerV1, CodeIndexNoopEvidenceV1, CodeIndexReconcileOutcomeV1,
    CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1, LatestCodeTextGenerationV1,
    LatestCompleteCodeIndexV1, RetainedTextGenerationRestoreV1,
    graph_activation::{CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1},
    now_micros,
    reconcile_panic_guard::{
        ReconcileCapacityRetryV1, ReconcilePanicDecisionV1, ReconcilePanicGuardV1,
    },
};
use super::{
    ACTIVATION_RETRY_BACKOFF_CEILING, ACTIVATION_RETRY_BACKOFF_FLOOR,
    CONVERGENCE_PARK_CONTRACT_REMEDIATION_V1, CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
    CodeIndexSchedulerRegistryV1, ColdMountAdmissionV1, GraphActivationGateV1, GraphSeatGateV1,
    MountedCodeIndexWorktreeV1, PendingWakeV1, PublishedTextProjectionOutcomeV1,
    ServingSwapOutcomeV1, TEXT_PROJECTION_DOCUMENTS_PER_PASS_V1, clear_convergence_park,
    convergence_park_retries_on_wake, is_repeated_conflict_verdict, park_convergence,
    retained_noop_requires_follow_up_wake, semantic_handoff_has_exact_witness,
};

impl CodeIndexSchedulerRegistryV1 {
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
        semantic_schedule: Option<
            tracedecay_application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
        graph_runtime: Arc<dyn crate::code_graph_seat::CodeGraphSeatRuntimePortV1>,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
        graph_activation_policy: CodeGraphActivationPolicyV1,
        semantic_lifecycle_owner: Option<Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>>,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            semantic_schedule,
            CodeGraphActivationAuthorityV1::Persistent {
                runtime: graph_runtime,
                project_database,
                policy: Arc::new(AtomicBool::new(graph_activation_policy.is_enabled())),
            },
            semantic_lifecycle_owner,
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
        semantic_schedule: Option<
            tracedecay_application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            semantic_schedule,
            CodeGraphActivationAuthorityV1::Memory {
                policy: Arc::new(AtomicBool::new(true)),
            },
            None,
        )
        .await
    }

    #[cfg(test)]
    pub async fn mount_worktree_with_graph_policy(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            tracedecay_application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
        policy: CodeGraphActivationPolicyV1,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.mount_worktree_inner(
            project_id,
            project_root,
            store_root,
            semantic_schedule,
            CodeGraphActivationAuthorityV1::Memory {
                policy: Arc::new(AtomicBool::new(policy.is_enabled())),
            },
            None,
        )
        .await
    }

    async fn replace_existing_semantic_schedule(
        &self,
        project_root: &Path,
        scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
        serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
        pending_wake: Arc<PendingWakeV1>,
        shutting_down: Arc<AtomicBool>,
        project_id: ProjectId,
        semantic_schedule: Option<
            tracedecay_application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        // Reconcile and tests may hold this mutex. `lock()` would park remount
        // behind that holder and lose the retiring identity: retirement now
        // cancels the worker via try_lock, drains, and leaves remount seeing
        // only "owner changed". Poll try_lock and abort as soon as the owner
        // is shutting down so remount observes "retired while … waited".
        let incumbent = Arc::clone(&scheduler);
        tokio::task::spawn_blocking(move || {
            loop {
                if shutting_down.load(Ordering::Acquire) {
                    return Err(CodeIndexSchedulerErrorV1::Identity(
                        "code-index scheduler owner was retired while semantic schedule update waited; remount must retry"
                            .to_owned(),
                    ));
                }
                let mut scheduler = match scheduler.try_lock() {
                    Ok(guard) => guard,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                };
                if scheduler.project_id() != &project_id {
                    return Err(CodeIndexSchedulerErrorV1::Identity(
                        "mounted worktree belongs to a different project identity".to_owned(),
                    ));
                }
                scheduler.replace_semantic_schedule_hook(semantic_schedule);
                if let Some(latest) = serving_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                {
                    let _ = scheduler.schedule_semantic_generation(latest.generation_handle());
                }
                // A replaced hook must not leave the worker parked: the next
                // reconcile (including an edit that raced the remount) needs a
                // wake even when this pass already finished text.
                Self::note_wake(
                    pending_wake.as_ref(),
                    scheduler.wake.as_ref(),
                    CodeIndexCadenceTriggerV1::BusyFollowUp,
                );
                return Ok(());
            }
        })
        .await
        .map_err(|_error| {
            CodeIndexSchedulerErrorV1::SemanticSchedule("hook task failed".to_owned())
        })??;

        let retiring = self.retiring.lock().await;
        if retiring.contains_key(project_root) {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler owner was retired while semantic schedule update waited; remount must retry"
                    .to_owned(),
            ));
        }
        let mounted = self.mounted.lock().await;
        if !mounted
            .get(project_root)
            .is_some_and(|current| Arc::ptr_eq(&current.scheduler, &incumbent))
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler owner changed while semantic schedule update waited; remount must retry"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    async fn mount_worktree_inner(
        &self,
        project_id: ProjectId,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            tracedecay_application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
        graph_activation: CodeGraphActivationAuthorityV1,
        semantic_lifecycle_owner: Option<Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>>,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let validate_lifecycle_owner = |existing: &MountedCodeIndexWorktreeV1| {
            let same_owner = match (
                existing.semantic_lifecycle_owner.as_ref(),
                semantic_lifecycle_owner.as_ref(),
            ) {
                (Some(incumbent), Some(incoming)) => Arc::ptr_eq(incumbent, incoming),
                (None, None) => true,
                _ => false,
            };
            if same_owner {
                Ok(())
            } else {
                Err(CodeIndexSchedulerErrorV1::Identity(
                    "mounted worktree belongs to a different semantic lifecycle owner".to_owned(),
                ))
            }
        };
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
                validate_lifecycle_owner(existing)?;
                existing
                    .graph_activation
                    .update_policy(graph_activation.policy());
                let scheduler = Arc::clone(&existing.scheduler);
                let serving_generation = Arc::clone(&existing.serving_generation);
                let pending_wake = Arc::clone(&existing.pending_wake);
                let shutting_down = Arc::clone(&existing.shutting_down);
                drop(mounted);
                drop(retiring);
                #[cfg(test)]
                Self::observe_existing_semantic_schedule_replacement(&project_root);
                self.replace_existing_semantic_schedule(
                    &project_root,
                    scheduler,
                    serving_generation,
                    pending_wake,
                    shutting_down,
                    project_id,
                    semantic_schedule,
                )
                .await?;
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
        let open_project_id = project_id.clone();
        let open_project_root = project_root.clone();
        let open_byte_pool = Arc::clone(&self.byte_pool);
        let open_semantic_schedule = semantic_schedule.clone();
        let open_resident_memory = Arc::clone(&self.resident_memory);
        let progress_daemon_incarnation = self.progress_daemon_incarnation;
        let progress_producer_incarnation = self.mint_progress_producer_incarnation()?;
        let (opened, cold_mount_reservation) = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            Self::pause_cold_mount_open_for_test(&open_project_root);
            let opened = CodeIndexWorktreeSchedulerV1::open(
                open_project_id,
                &open_project_root,
                scoped_store_root,
                open_byte_pool,
            );
            #[cfg(test)]
            Self::finish_cold_mount_open_for_test(&open_project_root);
            let mut opened = opened?;
            opened.replace_semantic_schedule_hook(open_semantic_schedule);
            opened.bind_resident_memory(open_resident_memory);
            opened.bind_progress_incarnations(
                progress_daemon_incarnation,
                progress_producer_incarnation,
            );
            Ok::<_, CodeIndexSchedulerErrorV1>((opened, cold_mount_reservation))
        })
        .await
        .map_err(|error| {
            CodeIndexSchedulerErrorV1::Identity(format!("code-index mount task failed: {error}"))
        })??;
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
        let serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>> =
            Arc::new(RwLock::new(None));
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
        let (serving_generation_changed, _) = tokio::sync::watch::channel(());
        let serving_generation_installation = Arc::new(Mutex::new(None));
        let hints = Arc::clone(&opened.hints);
        let wake = Arc::clone(&opened.wake);
        let epoch = Arc::clone(&opened.epoch);
        let shutting_down = Arc::clone(&opened.shutting_down);
        let scheduler = Arc::new(Mutex::new(opened));
        let build_publication_lock = Arc::new(tokio::sync::Mutex::new(()));
        let semantic_evaluation_publication_gate = Arc::new(tokio::sync::Mutex::new(()));
        let ignored_dependency_admissions = Arc::new(Mutex::new(BTreeMap::new()));
        let pending_wake = Arc::new(PendingWakeV1::default());
        let index_observability = Arc::new(OnceLock::<
            super::super::observability::CodeIndexObservabilityV1,
        >::new());
        let worker_index_observability = Arc::clone(&index_observability);
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_reconcile_in_progress = Arc::clone(&reconcile_in_progress);
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
        let worker_cadence_telemetry = Arc::clone(&self.cadence_telemetry);
        let worker_shutting_down = Arc::clone(&shutting_down);
        let worker_build_publication_lock = Arc::clone(&build_publication_lock);
        let worker_semantic_evaluation_publication_gate =
            Arc::clone(&semantic_evaluation_publication_gate);
        let worker_background_reconcile_admission =
            Arc::clone(&self.background_reconcile_admission);
        let worker_generation_publications = self.generation_publications.clone();
        let worker_serving_seats = Arc::clone(&self.serving_seats);
        let worker_project_root = project_root.clone();
        let worker_project_id = project_id;
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
            validate_lifecycle_owner(existing)?;
            // The scheduler Arc is the mounted owner's exact identity. It is
            // rechecked after the asynchronous update so a retirement or
            // replacement cannot turn this remount into a success for a
            // detached worker.
            existing
                .graph_activation
                .update_policy(graph_activation.policy());
            let scheduler = Arc::clone(&existing.scheduler);
            let serving_generation = Arc::clone(&existing.serving_generation);
            let pending_wake = Arc::clone(&existing.pending_wake);
            let shutting_down = Arc::clone(&existing.shutting_down);
            drop(mounted);
            drop(retiring);
            #[cfg(test)]
            Self::observe_existing_semantic_schedule_replacement(&project_root);
            self.replace_existing_semantic_schedule(
                &project_root,
                scheduler,
                serving_generation,
                pending_wake,
                shutting_down,
                worker_project_id,
                semantic_schedule,
            )
            .await?;
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
            // Bounded retry state for a reconcile refused because shared
            // process capacity was momentarily held by a sibling worktree or
            // artifact build. Releasing that capacity emits no wake, so this
            // worker must schedule its own.
            let mut capacity_retry = ReconcileCapacityRetryV1::new();
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
            loop {
                hotpath::future!(
                    worker_wake.notified(),
                    label = "daemon.code_index.wake_wait"
                )
                .await;
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
                // This aggregate starts when the wake is observed and ends
                // when the pass holds every admission/publication gate it
                // needs. It deliberately does not attribute the span to the
                // SQL writer alone.
                let pass_wake_observed_at = Instant::now();
                // A quarantined or backing-off panic unit must not consume the
                // pending arrival: the wake stays outstanding so a later
                // eligible pass still measures its full queue wait.
                if panic_guard.suppresses_pass(
                    tokio::time::Instant::now(),
                    worker_control_epoch.load(Ordering::Acquire),
                ) {
                    tracing::debug!(
                        event = "code_index_reconcile_panic_suppressed",
                        path = "background_worker",
                        consecutive_panics = panic_guard.consecutive_panics(),
                        "code-index reconcile is suppressed after repeated panics over unchanged input"
                    );
                    continue;
                }
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
                let _semantic_evaluation_publication =
                    worker_semantic_evaluation_publication_gate.lock().await;
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
                let mut build_publication =
                    std::pin::pin!(Arc::clone(&worker_build_publication_lock).lock_owned());
                let _build_publication = loop {
                    tokio::select! {
                        guard = &mut build_publication => break guard,
                        () = tokio::time::sleep(Duration::from_millis(5)) => {
                            if worker_shutting_down.load(Ordering::Acquire) {
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
                        .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready)
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
                let mut text_generation = worker_text_generation
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                // A query-driven advance can finish the build between worker
                // passes; a park observed earlier must not outlive the
                // violation it named.
                if text_generation
                    .as_ref()
                    .is_some_and(super::super::LatestCodeTextGenerationV1::text_serving_is_ready)
                {
                    clear_convergence_park(&worker_convergence_park);
                } else if convergence_park_retries_on_wake(&worker_convergence_park)
                    && text_generation
                        .as_ref()
                        .is_some_and(|latest| !latest.text_serving_needs_work())
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
                    && latest.text_serving_needs_work()
                    && graph_activation_enabled
                {
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
                    let projection_pass =
                        super::super::ReconcilePassGuard::enter(&worker_reconcile_in_progress);
                    let projection_pending_wake = Arc::clone(&worker_pending_wake);
                    let projection_wake = Arc::clone(&worker_wake);
                    retained_text_projection = Some(tokio::spawn(async move {
                        let _projection_pass = projection_pass;
                        #[cfg(any(test, feature = "test-helpers"))]
                        Self::wait_for_retained_text_projection_gate(&gated_root).await;
                        let outcome = Self::drive_text_projection(
                            latest,
                            shutting_down,
                            park,
                            Some(installed),
                            #[cfg(test)]
                            gated_root,
                        )
                        .await;
                        if matches!(outcome, PublishedTextProjectionOutcomeV1::Unfinished) {
                            Self::note_worker_continuation(
                                &projection_pending_wake,
                                &projection_wake,
                            );
                        }
                        outcome
                    }));
                } else if let Some(latest) = text_generation
                    && latest.text_serving_needs_work()
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
                            worker_wake.notify_one();
                            continue;
                        }
                        Ok(Ok(Some(RetainedTextGenerationRestoreV1::Refused(metadata)))) => {
                            Some(metadata)
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
                    .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready);
                // Admission is held: queue wait ends and service time begins.
                let started_micros = now_micros().0;
                let (arrival, trigger) = Self::take_pending_arrival(
                    &worker_pending_wake,
                    CodeIndexCadenceTriggerV1::Mount,
                );
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
                        if let Some(metadata) = retained_text_metadata {
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
                        }
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
                if retained_noop_requires_follow_up_wake(
                    serving_empty,
                    graph_activation_deferred,
                    arrival.wake_micros().is_some(),
                    matches!(&source_result, Ok(Ok(CodeIndexReconcileOutcomeV1::Noop(_)))),
                ) {
                    // This is the one bounded second look for the arrival the
                    // Noop just consumed. Keep it unattributed: publishing a
                    // new pending arrival here would satisfy this same
                    // predicate on every quiet successor and self-requeue
                    // forever.
                    worker_wake.notify_one();
                }
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
                // stores can start. Keep `reconcile_pass` through text
                // seating — dropping it made `reconcile_in_progress` lie while
                // this worker still owned graph try_lock, which deadlocked
                // tests that hold the scheduler mutex and wait for that flag.
                drop(_background_reconcile_admission);
                // A publication must first reopen its own lightweight text
                // owner: publication moved the durable pointer, so the prior
                // owner is no longer authoritative even while the new
                // lightweight handle is opening. Withdraw it first - a failed
                // or delayed reopen must report warming, never keep serving
                // the superseded generation indefinitely.
                let published_pass = matches!(
                    &source_result,
                    Ok(Ok(CodeIndexReconcileOutcomeV1::Published(_)))
                );
                let mut graph_text = retained_text.clone();
                // The replacement owner's bounded projection, driven to
                // completion concurrently with this pass's graph prepare and
                // activation and joined before the seat.
                let mut published_text_projection = None;
                if published_pass {
                    *worker_text_generation
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
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
                    graph_text = if let Ok(Ok(Some(published_text))) = published_text {
                        *worker_text_generation
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(published_text.clone());
                        Some(published_text)
                    } else {
                        None
                    };
                    // The replacement text owner finishes its bounded
                    // projection on its own task, concurrently with the
                    // optional O(store) graph decode and native activation
                    // below: both consume the sealed generation and neither
                    // depends on the other until the seat, which joins the
                    // projection first. Exact and lexical serving therefore
                    // never inherit graph activation latency, and graph
                    // readiness no longer waits behind the whole text build.
                    // Yielding back to the loop instead would hand the next
                    // pass a checkout that has already moved, and on a shared
                    // repository that pass publishes again - which is exactly
                    // how a sealed generation stayed unseated forever.
                    if graph_activation_enabled
                        && !graph_activation_deferred
                        && let Some(text) = graph_text.clone()
                    {
                        published_text_projection =
                            Some(tokio::spawn(Self::drive_text_projection(
                                text,
                                Arc::clone(&worker_shutting_down),
                                Arc::clone(&worker_convergence_park),
                                None,
                                #[cfg(test)]
                                worker_project_root.clone(),
                            )));
                    } else if graph_text
                        .as_ref()
                        .is_none_or(LatestCodeTextGenerationV1::text_serving_needs_work)
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
                // concurrent projection has finished; it is stale by
                // construction and superseded by the next publication. An
                // unchanged pass still seats the retained Ready text owner's
                // generation. Source reconciliation is complete either way:
                // release its public freshness guard before the optional
                // O(store) full decode and native graph activation begin,
                // unless this pass still drives the publication's text
                // projection — `reconcile_in_progress` stays truthful while
                // this worker owns source/text work, and that guard falls
                // when the projection is joined below. Optional graph must
                // not hold it: each graph step that must take the scheduler
                // re-enters the pass around that acquisition (see
                // `lock_scheduler_for_graph_step`); only the unlocked decode
                // and native activation run outside it.
                if published_text_projection.is_none() && retained_text_projection.is_none() {
                    drop(reconcile_pass.take());
                }
                let gate = GraphSeatGateV1::decide(
                    graph_activation_enabled,
                    graph_activation_deferred,
                    matches!(&source_result, Ok(Ok(_))),
                    published_pass,
                    graph_text.is_some(),
                );
                // An arrival that landed during this retained pass is
                // exact/lexical work waiting for the worker. The optional
                // graph prepare parks this worker on an O(store) sealed
                // decode — tens of seconds on a cold large repository — and
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
                    // and seating, independently of redundant activation.
                    prepare_graph = serving_empty
                        && worker_complete_generation_requested.load(Ordering::Acquire);
                }
                if prepare_graph
                    && !published_pass
                    && !retained_graph_head_recovery_attempted
                    && let Some(retained) = graph_text
                        .as_ref()
                        .filter(|retained| retained.uses_partitioned_manifest())
                        .cloned()
                {
                    retained_graph_head_recovery_attempted = true;
                    let generation_id = retained.metadata().manifest().generation_id.clone();
                    let replay_scheduler = Arc::clone(&worker_scheduler);
                    let shutting_down = Arc::clone(&worker_shutting_down);
                    let replay_passes = Arc::clone(&worker_reconcile_in_progress);
                    let replay_binding = tokio::task::spawn_blocking(move || {
                        Self::lock_scheduler_for_graph_step(
                            &replay_scheduler,
                            &shutting_down,
                            &replay_passes,
                        )?
                        .1
                        .code_graph_replay_binding(&generation_id)
                    })
                    .await;
                    match replay_binding {
                        Ok(Ok(replay_binding)) => {
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
                                        "revision-7 manifest matched the durable verified graph \
                                         head; startup seated graph reads without replay"
                                    );
                                    #[cfg(any(test, feature = "test-helpers"))]
                                    if !complete_generation_requested {
                                        Self::wait_for_retained_graph_recovery_successor_gate(
                                            &worker_project_root,
                                        )
                                        .await;
                                    }
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    tracing::warn!(
                                        event = "code_index_graph_head_recovery_degraded",
                                        error = %error,
                                        "verified graph head did not match the revision-7 \
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
                                "revision-7 replay binding is unavailable; graph coverage stays \
                                 pending while the admitted worker repairs the generation"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                event = "code_index_graph_head_recovery_task_failed",
                                error = %error,
                                "revision-7 replay binding task failed; graph coverage stays \
                                 pending while the admitted worker repairs the generation"
                            );
                        }
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
                    // another retained-recovery Noop.
                    Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
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
                        "the recovered revision-7 head already serves; the sealed generation \
                         is not replayed to seat a second copy of it"
                    );
                }
                let mut result = match source_result {
                    Ok(mut outcome) if prepare_graph => {
                        let graph_scheduler = Arc::clone(&worker_scheduler);
                        let graph_text = graph_text.clone();
                        let shutting_down = Arc::clone(&worker_shutting_down);
                        let prepare_passes = Arc::clone(&worker_reconcile_in_progress);
                        match hotpath::future!(
                            tokio::task::spawn_blocking(move || {
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
                                let generation = decoder.and_then(|decoder| {
                                    match decoder.load_active_shared() {
                                        Ok(generation) => generation,
                                        Err(error) => {
                                            tracing::warn!(
                                                event = "code_index_graph_prepare_load_failed",
                                                error = %error,
                                                "active generation decode failed; \
                                                 the sealed generation cannot seat"
                                            );
                                            None
                                        }
                                    }
                                });
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
                                // the successor — but this pass consumed the
                                // wake that would have run it.
                                let roster_refusal_rebuild = latest.is_none()
                                    && Self::lock_scheduler_for_graph_step(
                                        &graph_scheduler,
                                        &shutting_down,
                                        &prepare_passes,
                                    )?
                                    .1
                                    .take_ignored_roster_refusal_rebuild();
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
                                    .map(|binding| (latest, binding, roster_refusal_rebuild))
                            }),
                            label = "daemon.code_index.graph_prepare"
                        )
                        .await
                        {
                            Ok(Ok((latest, replay_binding, roster_refusal_rebuild))) => {
                                if latest.is_none() {
                                    tracing::warn!(
                                        event = "code_index_graph_prepare_no_servable_generation",
                                        published_pass,
                                        roster_refusal_rebuild,
                                        "graph prepare produced no servable generation; \
                                         the sealed generation cannot seat"
                                    );
                                }
                                if roster_refusal_rebuild {
                                    // One pass, claimed from the scheduler, so
                                    // a refusal that keeps reproducing cannot
                                    // spin this worker.
                                    Self::note_worker_continuation(
                                        &worker_pending_wake,
                                        &worker_wake,
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
                    Ok((Ok(_), Some(latest), Some(_))) => GraphActivationGateV1::decide(
                        graph_already_serves
                            || latest.code_graph_serving_readiness()
                                == CodeGraphServingReadinessV1::Ready,
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
                    let activation = worker_graph_activation
                        .activate(
                            &worker_project_id,
                            &worker_repository_id,
                            &worker_worktree_id,
                            latest.clone(),
                            replay_binding.clone(),
                            Arc::clone(&worker_shutting_down),
                        )
                        .await;
                    match activation {
                        Ok(()) => {
                            next_seat_attempt_at = None;
                            seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                            last_seat_conflict = None;
                        }
                        Err(error) if error.is_graph_activation_refusal() => {
                            next_seat_attempt_at = None;
                            seat_retry_backoff = ACTIVATION_RETRY_BACKOFF_FLOOR;
                            last_seat_conflict = None;
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
                                     stays unseated until the scheduled retry"
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
                                result = Ok((Err(error), None, None));
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
                // The seat requires the publication's own text owner to be
                // ready: join the concurrent projection here, after graph
                // activation, so an unfinished owner refuses only the swap.
                // Activation is durable and idempotent, so the pass that later
                // finds the owner ready seats without repeating it.
                if let Some(projection) = published_text_projection.take() {
                    let outcome = match projection.await {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            if let Some(text) = graph_text.as_ref() {
                                text.mark_text_serving_failed();
                            }
                            park_convergence(
                                &worker_convergence_park,
                                format!("code text projection task failed abnormally: {error}"),
                                CONVERGENCE_PARK_TASK_FAILURE_REMEDIATION_V1,
                                false,
                            );
                            tracing::warn!(
                                event = "code_index_text_projection_task_failed",
                                error = %error,
                                "published text projection task failed before graph seating"
                            );
                            PublishedTextProjectionOutcomeV1::Unfinished
                        }
                    };
                    match outcome {
                        PublishedTextProjectionOutcomeV1::Finished => {
                            // Large text projections can outlive the bounded
                            // source proof established before publication. The
                            // serving swap must bind to source truth observed
                            // after that work, otherwise an exact current
                            // generation seats without a witness and every
                            // readiness read schedules another identical Noop.
                            let proof_is_current = graph_text.as_ref().is_some_and(|text| {
                                worker_source_freshness.serves_recently_verified_source(
                                    &text.metadata().snapshot().content_identity,
                                    &worker_project_root,
                                    &worker_shutting_down,
                                )
                            });
                            if !proof_is_current && let Some(text) = graph_text.as_ref() {
                                let scheduler = Arc::clone(&worker_scheduler);
                                let shutting_down = Arc::clone(&worker_shutting_down);
                                let metadata = text.metadata().clone();
                                let renewed = tokio::task::spawn_blocking(move || {
                                    Self::lock_scheduler_unless_shutting_down(
                                        &scheduler,
                                        &shutting_down,
                                    )?
                                    .reconcile_retained_text_generation_with(&metadata, false)
                                })
                                .await;
                                match renewed {
                                    Ok(Ok(Some(CodeIndexReconcileOutcomeV1::Noop(_)))) => {}
                                    Ok(Ok(Some(_) | None)) => tracing::info!(
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
                            Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
                        }
                    }
                    // Keep the pass lifetime around the post-projection source
                    // proof so concurrent reads attribute their single wake as
                    // a follow-up to this owner. The swap below takes its own
                    // nested guard and publishes the witness before either
                    // lifetime becomes idle.
                }
                if let Ok((Ok(_), Some(latest), _)) = &result {
                    let scheduler = Arc::clone(&worker_scheduler);
                    let serving_generation = Arc::clone(&worker_serving_generation);
                    let serving_generation_epoch = Arc::clone(&worker_serving_generation_epoch);
                    let serving_source_witness = Arc::clone(&worker_serving_source_witness);
                    let text_generation = Arc::clone(&worker_text_generation);
                    let serving_seats = Arc::clone(&worker_serving_seats);
                    let serving_generation_changed = worker_serving_generation_changed.clone();
                    let source_freshness = worker_source_freshness.clone();
                    let project_root = worker_project_root.clone();
                    let control_epoch = Arc::clone(&worker_control_epoch);
                    let semantic_observed_epoch = control_epoch.load(Ordering::Acquire);
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
                            // publish.
                            let pass_proves_latest = source_freshness
                                .serves_recently_verified_source(
                                    &latest.generation().snapshot().content_identity,
                                    &project_root,
                                    &shutting_down,
                                );
                            let mut serving = serving_generation
                                .write()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let incumbent_is_active = serving.as_ref().is_some_and(|incumbent| {
                                // The active generation is already loaded
                                // and cached by the check above, so this
                                // is a second comparison, not a second
                                // decode. A store that cannot answer keeps
                                // the incumbent rather than displacing it.
                                scheduler
                                    .active_publication_matches(incumbent)
                                    .unwrap_or(true)
                            });
                            let outcome = ServingSwapOutcomeV1::decide(
                                publication_matches,
                                incumbent_is_active,
                                replace_serving_generation,
                            );
                            if outcome.installs() {
                                *serving = Some(latest.clone());
                                serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
                                *text_generation
                                    .write()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                    Some(latest.text_generation_handle());
                            }
                            // A pass is the only thing that verifies source
                            // against the sealed digests, so it is also the
                            // only thing that can re-prove a seat. `Offered`
                            // is that case: the active publication already
                            // serves and this pass re-observed the checkout it
                            // was sealed from. Arming only on a publication
                            // left a restored, retired, or withdrawn seat
                            // permanently unproven — busy verified reads then
                            // refused a generation whose source was current.
                            match outcome {
                                ServingSwapOutcomeV1::Seated | ServingSwapOutcomeV1::Offered => {
                                    *serving_source_witness
                                        .write()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        pass_proves_latest
                                            .then(|| {
                                                source_freshness.source_currency_witness_for(
                                                    &latest.generation().manifest().generation_id,
                                                    &latest
                                                        .generation()
                                                        .snapshot()
                                                        .content_identity,
                                                )
                                            })
                                            .flatten();
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
                            // The serving slot is now fully published, including
                            // its exact-source witness. Wake dependent readers
                            // before the optional semantic handoff: that hook is
                            // independently retryable and must not hold serving
                            // readiness hostage if it blocks or loses capacity.
                            if outcome.installs() {
                                Self::record_serving_seat(&serving_seats);
                                serving_generation_changed.send_replace(());
                            }
                            // Only the exact-source witness authorizes this
                            // decoded-seat handoff. A retained stale seat is
                            // allowed to keep reads available while refresh
                            // runs, but must not enter semantic projection. Its
                            // later current Noop uses the retained-text handoff
                            // below. A witnessed `Offered` generation remains
                            // eligible for the existing retry semantics.
                            let semantic_source_is_current = semantic_handoff_has_exact_witness(
                                publication_matches,
                                serving_source_witness
                                    .read()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .as_ref(),
                                &latest.generation().manifest().generation_id,
                            ) && control_epoch
                                .load(Ordering::Acquire)
                                == semantic_observed_epoch
                                && source_freshness
                                    .ready_without_stat(&project_root, &shutting_down);
                            if semantic_source_is_current {
                                let _ = scheduler
                                    .schedule_semantic_generation(latest.generation_handle());
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
                            if text_latest.text_serving_needs_work() {
                                Self::note_worker_continuation(&worker_pending_wake, &worker_wake);
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
                                Err(CodeIndexSchedulerErrorV1::SemanticSchedule(format!(
                                    "serving-swap task failed: {error}"
                                ))),
                                None,
                                None,
                            ));
                        }
                    }
                }
                // The source proof and serving witness are now published as
                // one lifecycle. Optional receipts and semantic scheduling do
                // not keep source verification in flight.
                drop(reconcile_pass.take());
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
                    let retained_generation =
                        matches!(outcome, CodeIndexReconcileOutcomeV1::Noop(_))
                            .then(|| {
                                worker_text_generation
                                    .read()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .as_ref()
                                    .map(|latest| {
                                        latest.metadata().manifest().generation_id.clone()
                                    })
                            })
                            .flatten();
                    let source_is_current = retained_generation.is_some()
                        && worker_source_freshness
                            .ready_without_stat(&worker_project_root, &worker_shutting_down);
                    if source_is_current {
                        worker_serving_generation_changed.send_replace(());
                    }
                    if source_is_current && let Some(expected_generation) = retained_generation {
                        // Semantic projection consumes the canonical immutable
                        // generation, not the text/graph serving adapters. A
                        // partitioned retained head intentionally has no decoded
                        // seat, so load its shared publication only after the
                        // quiet source proof. Publication caching makes repeated
                        // Noops reuse this Arc; the semantic scheduler retains
                        // its existing at-least-once deduplication and retry.
                        let scheduler = Arc::clone(&worker_scheduler);
                        let serving_generation = Arc::clone(&worker_serving_generation);
                        let shutting_down = Arc::clone(&worker_shutting_down);
                        let source_freshness = worker_source_freshness.clone();
                        let project_root = worker_project_root.clone();
                        let control_epoch = Arc::clone(&worker_control_epoch);
                        let observed_epoch = control_epoch.load(Ordering::Acquire);
                        let handoff = tokio::task::spawn_blocking(move || -> Result<
                            Option<SavedGenerationScheduleOutcomeV1>,
                            CodeIndexSchedulerErrorV1,
                        > {
                            let scheduler = Self::lock_scheduler_unless_shutting_down(
                                &scheduler,
                                &shutting_down,
                            )?;
                            if scheduler.semantic_schedule.is_none()
                                || control_epoch.load(Ordering::Acquire) != observed_epoch
                            {
                                return Ok(None);
                            }
                            let generation = serving_generation
                                .read()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .as_ref()
                                .filter(|latest| {
                                    latest.generation().manifest().generation_id
                                        == expected_generation
                                })
                                .map(LatestCompleteCodeIndexV1::generation_handle)
                                .or_else(|| {
                                    scheduler.latest_complete().and_then(|latest| {
                                        (latest.generation().manifest().generation_id
                                            == expected_generation)
                                            .then(|| latest.generation_handle())
                                        })
                                });
                            if control_epoch.load(Ordering::Acquire) != observed_epoch
                                || !source_freshness
                                    .ready_without_stat(&project_root, &shutting_down)
                            {
                                return Ok(None);
                            }
                            let Some(generation) = generation else {
                                tracing::warn!(
                                    event = "code_index_semantic_schedule_declined",
                                    outcome = SavedGenerationScheduleOutcomeV1::NoServingGeneration
                                        .as_str(),
                                    generation = %expected_generation,
                                    "current retained generation could not be loaded for semantic projection"
                                );
                                return Ok(None);
                            };
                            Ok(Some(scheduler.schedule_semantic_generation(generation)))
                        })
                        .await;
                        match handoff {
                            Ok(Ok(Some(_) | None)) => {}
                            Ok(Err(error)) => tracing::warn!(
                                event = "code_index_semantic_retained_handoff_failed",
                                error = %error,
                                "current retained generation could not reach semantic projection"
                            ),
                            Err(error) => tracing::warn!(
                                event = "code_index_semantic_retained_handoff_task_failed",
                                error = %error,
                                "retained semantic handoff task failed"
                            ),
                        }
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
                            let transient_capacity = error.is_transient_capacity_failure();
                            tracing::warn!(
                                event = "code_index_reconcile_failed",
                                path = "background_worker",
                                transient_capacity,
                                trigger = trigger.label(),
                                error = %error,
                                "code-index background reconcile failed; the served generation stays stale"
                            );
                            if transient_capacity {
                                // Shared process capacity was held by another
                                // holder when this pass asked for it. Releasing
                                // it emits no wake, so without a self-scheduled
                                // retry this worktree stayed stale until some
                                // unrelated query or edit happened to wake it.
                                // Permanent refusals deliberately never reach
                                // here: retrying those forever is the failure
                                // this loop already had.
                                match capacity_retry.record_capacity_failure() {
                                    Some(delay) => {
                                        let retry_wake = Arc::clone(&worker_wake);
                                        tokio::spawn(async move {
                                            tokio::time::sleep(delay).await;
                                            retry_wake.notify_one();
                                        });
                                    }
                                    None => tracing::warn!(
                                        event = "code_index_reconcile_capacity_retry_exhausted",
                                        path = "background_worker",
                                        consecutive = capacity_retry.consecutive(),
                                        "code-index reconcile stopped retrying a capacity refusal; the next hint retries"
                                    ),
                                }
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
                    // Restore arrival so the next pass measures this wake's full queue wait.
                    Self::restore_pending_arrival(&worker_pending_wake, arrival, trigger);
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
                    drop(reconcile_pass.take());
                    match outcome {
                        PublishedTextProjectionOutcomeV1::Finished => {}
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
                    }
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
        entry.insert(MountedCodeIndexWorktreeV1 {
            repository_id,
            worktree_id,
            query_authority: None,
            semantic_query_authority: None,
            semantic_lifecycle_owner,
            query_activation_revision: None,
            query_activation_epoch: None,
            query_activation_transition_digest: None,
            query_activation_attempt: 0,
            query_activation_redundancy: None,
            semantic_vector_graph_provider: None,
            scheduler,
            build_publication_lock,
            historical_generation_owner,
            serving_generation,
            complete_generation_requested,
            complete_generation_requested_changed,
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
            ignored_dependency_admissions,
            hints,
            wake: Arc::clone(&wake),
            epoch,
            pending_wake: Arc::clone(&pending_wake),
            index_observability,
            shutting_down,
            reconcile_in_progress,
            _active_generation_encoded_bytes: active_generation_encoded_bytes,
            semantic_evaluation_publication_gate,
            task,
        });
        // Until retained decode/truth verification completes, reads see warming
        // instead of serving unproven bytes.
        Self::note_wake(&pending_wake, &wake, CodeIndexCadenceTriggerV1::Mount);
        Ok(true)
    }
}

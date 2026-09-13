//! Read models over mounted worktrees: freshness, serving generations, and
//! text owners.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1;
use tracedecay_domain::CodeGenerationId;

#[cfg(feature = "hotpath")]
use super::super::now_micros;
use super::super::{
    CodeIndexCadenceTriggerV1, CodeIndexSchedulerErrorV1, GenerationDecodeAdmissionV1,
    LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1,
};
use super::scope_identity::{latest_matches_scope_identity, text_matches_scope_identity};
use super::{
    CodeIndexMountedScopeV1, CodeIndexSchedulerRegistryV1,
    CodeIndexSemanticEvaluationPublicationLeaseV1, CodeIndexServingScopeV1,
    MountedCodeIndexWorktreeV1, PendingWakeClaimV1, ReadyProbeServingPartsV1,
    SemanticEvaluationGenerationRefusalV1, dashboard_code_graph_serving,
    dashboard_freshness_identity, dashboard_generation_is_ready, dashboard_text_freshness_identity,
    record_semantic_candidate_refusal, unique_mounted_for_scope,
};

impl CodeIndexSchedulerRegistryV1 {
    /// Return the mounted scheduler's canonical worktree-change generation.
    ///
    /// The generation is exactly as fresh as the index used by search: hook
    /// hints and Git metadata changes are observed immediately, while other
    /// out-of-band edits are observed by the 30-second source-witness ladder
    /// (stat signature, then sealed file digests).
    /// Until that ladder runs, callers intentionally receive the preceding
    /// generation and must not derive a parallel workspace fingerprint.
    pub async fn diagnostics_change_generation(&self, project_root: &Path) -> Option<u64> {
        let Ok(project_root) = project_root.canonicalize() else {
            return None;
        };
        let (scheduler, source_freshness, hints, wake, pending_wake, reconcile_in_progress, epoch) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.epoch),
            )
        };
        // A neutral verification or publication pass must not hide a real Git
        // move from diagnostics caches. Sample the fixed-cost Git authority
        // before the busy shortcut and record the movement through the same
        // hint/epoch authority the scheduler uses. `source_change_pending`
        // keeps repeated reads from superseding the same in-flight remedy.
        if source_freshness.verified_git_metadata_moved(&project_root) {
            let mut hints = hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Re-prove both predicates while holding the hint authority. This
            // serializes concurrent diagnostics and closes the window where a
            // reconciler could finish after the first sample but before this
            // read recorded what would then be a phantom second transition.
            if source_freshness.verified_git_metadata_moved(&project_root)
                && !source_freshness.source_change_pending()
            {
                super::super::CodeIndexWorktreeSchedulerV1::record_background_reconcile_hint(
                    &mut hints, &epoch, true,
                );
            }
            drop(hints);
            Self::note_wake_if_idle(
                &pending_wake,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
            return Some(epoch.load(Ordering::Acquire));
        }
        if pending_wake.has_pending_arrival() || reconcile_in_progress.load(Ordering::Acquire) != 0
        {
            return Some(epoch.load(Ordering::Acquire));
        }
        tokio::task::spawn_blocking(move || {
            let mut scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if scheduler.request_fresh_for_query_background() {
                Self::note_wake(
                    &pending_wake,
                    &wake,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                );
            }
            epoch.load(Ordering::Acquire)
        })
        .await
        .ok()
    }

    /// Mounted scope identity plus the currently serving generation for one
    /// project. Daemon authorities that must retain this scope's code-graph
    /// runtime (semantic vectors, generation retention) resolve through this
    /// read instead of re-deriving repository/worktree identity themselves.
    pub async fn serving_code_scope(&self, project_root: &Path) -> Option<CodeIndexServingScopeV1> {
        let project_root = project_root.canonicalize().ok()?;
        let (repository_id, worktree_id, shutting_down, serving) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                worktree.repository_id.clone(),
                worktree.worktree_id.clone(),
                Arc::clone(&worktree.shutting_down),
                Arc::clone(&worktree.serving_generation),
            )
        };
        let serving_generation = serving
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|latest| Arc::clone(&latest.generation));
        Some(CodeIndexServingScopeV1 {
            repository_id,
            worktree_id,
            shutting_down,
            serving_generation,
        })
    }

    pub async fn mounted_code_scope(&self, project_root: &Path) -> Option<CodeIndexMountedScopeV1> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root)?;
        Some(CodeIndexMountedScopeV1 {
            repository_id: worktree.repository_id.clone(),
            worktree_id: worktree.worktree_id.clone(),
            shutting_down: Arc::clone(&worktree.shutting_down),
        })
    }

    /// Resolve one sealed generation's replay binding without joining the
    /// scheduler mutex. A background reconcile owns that mutex for its whole
    /// pass — sealing a production-scale corpus holds it for minutes — and
    /// the binding is an immutable publication read the retained historical
    /// owner answers directly, so blocking here parked the caller (and its
    /// runtime worker thread) behind work the read never needed.
    #[hotpath::measure(label = "daemon.code_index.registry.replay_binding", future = true)]
    pub async fn code_graph_replay_binding(
        &self,
        project_root: &Path,
        generation: &CodeGenerationId,
    ) -> Option<Result<super::super::CodeGraphReplayBindingV1, CodeIndexSchedulerErrorV1>> {
        let project_root = project_root.canonicalize().ok()?;
        let historical = {
            let mounted = self.mounted.lock().await;
            mounted
                .get(&project_root)?
                .historical_generation_owner
                .clone()
        };
        let generation = generation.clone();
        Some(
            tokio::task::spawn_blocking(move || historical.sealed_replay_binding(&generation))
                .await
                .unwrap_or_else(|error| {
                    Err(CodeIndexSchedulerErrorV1::Identity(format!(
                        "sealed replay-binding read task failed: {error}"
                    )))
                }),
        )
    }

    /// Load one exact code generation through its durable publication store,
    /// including a source generation superseded in process-local retention.
    pub async fn published_generation(
        &self,
        project_root: &Path,
        generation_id: &CodeGenerationId,
    ) -> Option<Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexSchedulerErrorV1>>
    {
        let project_root = project_root.canonicalize().ok()?;
        let owner = {
            let mounted = self.mounted.lock().await;
            mounted
                .get(&project_root)?
                .historical_generation_owner
                .clone()
        };
        let generation_id = generation_id.clone();
        Some(
            tokio::task::spawn_blocking(move || owner.published_generation(&generation_id))
                .await
                .unwrap_or_else(|error| {
                    Err(CodeIndexSchedulerErrorV1::Identity(format!(
                        "published-generation read task failed: {error}"
                    )))
                }),
        )
    }

    pub async fn latest_generation_id(&self, project_root: &Path) -> Option<CodeGenerationId> {
        let project_root = project_root.canonicalize().ok()?;
        // Read the O(1) serving slot instead of the scheduler mutex. This used
        // to take `scheduler.lock()` — a blocking std mutex held by any
        // in-flight reconcile — while still holding the `mounted` async mutex,
        // so one warmup/dashboard call during a rebuild parked a runtime worker
        // for the reconcile's whole duration AND serialized every code-index
        // query behind it: a silent, daemon-wide code-index outage.
        let (serving, text) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.text_generation),
            )
        };
        // Only a seat answers here. A durable publication moves the pointer
        // long before either swap installs the generation it sealed, and a
        // slot fed from that broadcast named a generation every serving arm
        // still answered the *previous* id for: a caller that polled for a
        // changed id and then asked for the generation was handed the one it
        // had already seen. The complete serving slot is the swap's own
        // witness, so it answers first; the text slot covers a graph-off or
        // still-activating mount that deliberately leaves the complete slot
        // empty.
        let serving_id = serving
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|latest| latest.generation.manifest().generation_id.clone());
        if serving_id.is_some() {
            return serving_id;
        }
        text.read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|latest| latest.metadata().manifest().generation_id.clone())
    }

    /// Re-offer the exact serving generation to its installed semantic hook.
    ///
    /// Model selection is deliberately background work and can settle after
    /// text/graph publication first offered this generation. That first offer
    /// truthfully refuses while the artifact is unavailable; selection
    /// completion calls this bounded retry so an unchanged repository does
    /// not need another source reconciliation before vector indexing starts.
    #[hotpath::measure(
        label = "daemon.code_index.semantic_generation_reschedule",
        future = true
    )]
    /// Exact bounded dashboard projection for one mounted worktree.
    ///
    /// This is a status read, not a query-admission boundary: it reports the
    /// last scheduler execution state and never runs a freshness probe, opens
    /// Git, scans the worktree, publishes a generation, or posts a wake.
    /// Generation and scope fields are copied from the last sealed generation,
    /// never reconstructed from the dashboard's display path.
    pub async fn dashboard_freshness(
        &self,
        project_root: &Path,
    ) -> Option<tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1> {
        let canonical_root = project_root.canonicalize().ok()?;
        let (
            scheduler,
            reconcile_in_progress,
            serving_generation,
            last_reconciled_at_micros,
            text_generation,
            convergence_park,
            generation_recovery,
            build_progress,
            hints,
            pending_wake,
            source_freshness,
            graph_activation_enabled,
        ) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&canonical_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.last_reconciled_at_micros),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.convergence_park),
                Arc::clone(&worktree.generation_recovery),
                Arc::clone(&worktree.build_progress),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.pending_wake),
                worktree.source_freshness.clone(),
                worktree.graph_activation.policy().is_enabled(),
            )
        };
        tokio::task::spawn_blocking(move || {
            let progress = hotpath::measure_block!("daemon.code_index.dashboard.progress", {
                let progress = build_progress
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .snapshot()
                    .map(|snapshot| snapshot.as_ref().clone());
                #[cfg(feature = "hotpath")]
                if let Some(progress) = progress.as_ref() {
                    let age_micros = now_micros()
                        .0
                        .saturating_sub(progress.last_progress_micros)
                        .max(0);
                    hotpath::gauge!("daemon.code_index.dashboard.progress_age_micros")
                        .set(u64::try_from(age_micros).unwrap_or(u64::MAX));
                }
                progress
            });
            let refresh_in_flight = reconcile_in_progress.load(Ordering::Acquire) != 0
                || pending_wake
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .micros
                    != 0;
            let source_change_pending = source_freshness.source_change_pending();
            let parked = convergence_park
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let generation_recovery = generation_recovery
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    let latest = serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let text = text_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let text_ready = text
                        .as_ref()
                        .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready);
                    let identity = if text.is_some() {
                        dashboard_text_freshness_identity(text.as_ref())
                    } else {
                        dashboard_freshness_identity(latest.as_ref())
                    };
                    let hook_hint_count = hints
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .count();
                    let code_graph_serving = dashboard_code_graph_serving(
                        latest.as_ref(),
                        text.as_ref(),
                        graph_activation_enabled,
                    );
                    let ready = dashboard_generation_is_ready(
                        latest.as_ref(),
                        text_ready,
                        graph_activation_enabled,
                        &code_graph_serving,
                    );
                    let verifying = ready && refresh_in_flight && !source_change_pending;
                    let refreshing = refresh_in_flight && !verifying;
                    let rebuild_in_flight = refreshing;
                    let stale = hook_hint_count != Some(0);
                    let last_reconcile_micros = match last_reconciled_at_micros
                        .load(Ordering::Acquire)
                    {
                        0 => None,
                        micros => Some(micros),
                    };
                    return tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
                        worktree_root: canonical_root.display().to_string(),
                        code_graph_serving,
                        last_reconcile_micros,
                        rebuild_in_flight,
                        staleness_state: Some(
                            if parked.is_some() && !ready {
                                "parked"
                            } else if verifying {
                                "verifying"
                            } else if refreshing {
                                if ready {
                                    "refreshing"
                                } else {
                                    "indexing"
                                }
                            } else if stale && ready {
                                "stale"
                            } else if ready {
                                "fresh"
                            } else {
                                "indexing"
                            }
                            .to_owned(),
                        ),
                        hook_hint_count,
                        coverage: if refreshing {
                            "partial_refresh_in_progress"
                        } else if verifying {
                            "partial_source_verification"
                        } else if hook_hint_count.is_some() {
                            "complete"
                        } else {
                            "partial_hook_hint_overflow"
                        }
                        .to_owned(),
                        progress,
                        parked,
                        generation_recovery,
                        ..identity
                    };
                }
            };
            let verified = scheduler.verified_against_source();
            let stale = !verified;
            let latest = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let text = text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let text_ready = text
                .as_ref()
                .is_some_and(LatestCodeTextGenerationV1::text_serving_is_ready);
            let hook_hint_count = scheduler.pending_hint_count();
            let code_graph_serving = dashboard_code_graph_serving(
                latest.as_ref(),
                text.as_ref(),
                graph_activation_enabled,
            );
            let ready = dashboard_generation_is_ready(
                latest.as_ref(),
                text_ready,
                graph_activation_enabled,
                &code_graph_serving,
            );
            let verifying = ready && refresh_in_flight && !source_change_pending;
            let refreshing = refresh_in_flight && !verifying;
            let rebuild_in_flight = refreshing;
            let staleness_state = if parked.is_some() && !ready {
                "parked"
            } else if verifying {
                "verifying"
            } else if refreshing {
                if ready {
                    "refreshing"
                } else {
                    "indexing"
                }
            } else if stale || hook_hint_count != Some(0) {
                if ready {
                    "stale"
                } else {
                    "indexing"
                }
            } else if ready {
                "fresh"
            } else {
                "indexing"
            };
            let identity = if text.is_some() {
                dashboard_text_freshness_identity(text.as_ref())
            } else {
                dashboard_freshness_identity(latest.as_ref())
            };
            tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
                worktree_root: canonical_root.display().to_string(),
                code_graph_serving,
                last_reconcile_micros: scheduler.last_reconciled_at_micros(),
                rebuild_in_flight,
                staleness_state: Some(staleness_state.to_owned()),
                hook_hint_count,
                coverage: if refreshing {
                    "partial_refresh_in_progress"
                } else if verifying {
                    "partial_source_verification"
                } else if !verified {
                    "partial_unverified_restore"
                } else if hook_hint_count.is_some() {
                    "complete"
                } else {
                    "partial_hook_hint_overflow"
                }
                .to_owned(),
                progress,
                parked,
                generation_recovery,
                ..identity
            }
        })
        .await
        .ok()
    }

    /// The deterministic contract violation currently parking background
    /// convergence for one mounted worktree, when the worker has observed one.
    /// A status read for doctor/status projections, never an admission
    /// boundary: it takes no scheduler lock and runs no probe.
    pub async fn convergence_park(
        &self,
        project_root: &Path,
    ) -> Option<CodeIndexConvergenceParkedV1> {
        let canonical_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&canonical_root)?;
        worktree
            .convergence_park
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Query-admission entry point: serve only an already-decoded generation
    /// whose exact identity authority still resolves. Freshness verification and
    /// any rebuild remain retained background work.
    #[hotpath::measure(label = "daemon.code_index.query.latest_fresh", future = true)]
    pub async fn latest_complete_fresh(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // Clone the per-worktree handle under a short map lock, then drop the
        // registry guard before checking the mounted route.
        let (
            scheduler,
            serving_generation,
            text_generation,
            hints,
            wake,
            pending_wake,
            source_freshness,
            shutting_down,
            first_complete_demand,
        ) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            let first_complete_demand = !worktree
                .complete_generation_requested
                .swap(true, Ordering::AcqRel);
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.shutting_down),
                first_complete_demand,
            )
        };
        // When the background worker already owns the scheduler, preserve the
        // last complete immutable generation instead of joining its work.
        let authority_root = project_root.clone();
        let freshness_root = project_root.clone();
        let latest = crate::ports::park_admission(tokio::task::spawn_blocking(move || {
            let scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    let serving = serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    // A still-current proof needs no follow-up. If it expired
                    // after this pass began, leave one coalesced wake so the
                    // worker re-observes source after releasing its ownership.
                    if serving.is_some()
                        && !source_freshness.ready_without_stat(&freshness_root, &shutting_down)
                    {
                        Self::note_wake_if_idle(
                            &pending_wake,
                            &wake,
                            CodeIndexCadenceTriggerV1::BusyFollowUp,
                        );
                    }
                    return serving;
                }
            };
            // Serve-old-first, continued: winning the scheduler lock must not
            // mean paying for the rebuild. `ensure_fresh_for_query` reconciles
            // inline, and that reconcile is O(store) with no bound of its own —
            // a live `tracedecay_context` call sat on this exact line for 900
            // seconds while the daemon ground a failing semantic publish loop,
            // and only the client's own timeout ended it. The ladder's checks
            // are cheap; its remedy belongs to the background worker.
            //
            // The git authority is still proven inline, because serving
            // retained bytes under an identity nothing can confirm is the one
            // thing the old inline reconcile fail-closed on.
            if !scheduler.git_authority_available() {
                return None;
            }
            let servable = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(latest) = servable {
                // The bounded proof is the only source-currentness work a
                // read performs. An expired proof leaves the immutable owner
                // servable and hands exact verification to the retained worker.
                if !source_freshness.ready_without_stat(&freshness_root, &shutting_down) {
                    Self::note_wake_if_idle(
                        &pending_wake,
                        &wake,
                        CodeIndexCadenceTriggerV1::QueryAdmission,
                    );
                }
                return Some(latest);
            }
            // A graph-off mount can already own authenticated text serving
            // while its graph-bearing generation deliberately remains
            // unseated. That owner is a real remedy for lexical/exact reads;
            // do not misclassify it as a cold open and inject an overflow that
            // would supersede its bounded projection.
            if text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
            {
                // A retained text owner needs only complete-owner demand. A
                // truly cold owner below must attach its authoritative scan
                // before recording the wake, so demand cannot suppress it.
                if first_complete_demand {
                    Self::note_wake(
                        &pending_wake,
                        &wake,
                        CodeIndexCadenceTriggerV1::QueryAdmission,
                    );
                }
                return None;
            }
            // Cold open has no servable generation. Verification and any
            // rebuild stay with the retained owner; reads only request the
            // wake and return typed unavailable/unverified.
            if pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .micros
                == 0
            {
                // A cold read carries no source-change evidence. Preserve any
                // snapshot the retained owner is already reconstructing and
                // keep one follow-up authoritative scan pending instead.
                hints
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .overflow();
                Self::note_wake(
                    &pending_wake,
                    &wake,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                );
            }
            None
        }))
        .await
        .ok()
        .flatten()?;
        self.install_test_attribution_authority(&authority_root, &latest)
            .await;
        Some(latest)
    }

    /// Query-admission entry point for latency-sensitive application paths.
    /// It serves only a generation whose freshness is already proven; stale,
    /// restored-unverified, or busy schedulers abstain after scheduling the
    /// background worker instead of reconciling on the caller.
    pub async fn latest_complete_ready(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_ready_with(project_root, GenerationDecodeAdmissionV1::AwaitDecode)
            .await
    }

    /// [`Self::latest_complete_ready`] under an explicit decode admission.
    #[hotpath::measure(label = "daemon.code_index.query.latest_ready", future = true)]
    async fn latest_complete_ready_with(
        &self,
        project_root: &Path,
        admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let (
            source_freshness,
            serving_generation,
            serving_source_witness,
            shutting_down,
            graph_enabled,
            wake,
            pending_wake,
        ) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            if admission == GenerationDecodeAdmissionV1::AwaitDecode
                && !worktree
                    .complete_generation_requested
                    .swap(true, Ordering::AcqRel)
            {
                Self::note_wake(
                    &worktree.pending_wake,
                    &worktree.wake,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                );
            }
            (
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.serving_source_witness),
                Arc::clone(&worktree.shutting_down),
                worktree.graph_activation.policy().is_enabled(),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
            )
        };
        let freshness_root = project_root.clone();
        let (request_reconcile, latest) = tokio::task::spawn_blocking(move || {
            let serving = serving_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let witness_matches_seat = serving.as_ref().is_some_and(|serving| {
                serving_source_witness
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_some_and(|witness| {
                        witness.generation_id == serving.generation().manifest().generation_id
                    })
            });
            let source_ready = witness_matches_seat
                && source_freshness.ready_without_stat(&freshness_root, &shutting_down);
            let latest = source_ready.then(|| serving.clone()).flatten();
            if latest.is_none() {
                tracing::info!(
                    event = "code_index_complete_generation_unavailable",
                    project = %freshness_root.display(),
                    graph_enabled,
                    source_ready,
                    serving_present = serving.is_some(),
                    shutting_down = shutting_down.load(Ordering::Acquire),
                    "complete-generation read declined before scope validation"
                );
            }
            drop(serving);
            (
                !source_ready && !shutting_down.load(Ordering::Acquire),
                latest,
            )
        })
        .await
        .ok()?;
        if request_reconcile && admission == GenerationDecodeAdmissionV1::AwaitDecode {
            Self::note_wake_if_idle(
                &pending_wake,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
        }
        let latest = latest?;
        self.install_test_attribution_authority(&project_root, &latest)
            .await;
        Some(latest)
    }

    /// Resolve one mounted root by the exact admitted repository/worktree/ref
    /// scope, then run that root's freshness ladder. A request never inherits
    /// whichever mounted worktree sorts first.
    pub async fn latest_complete_fresh_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let root = {
            let mounted = self.mounted.lock().await;
            unique_mounted_for_scope(&mounted, scope)
                .unique()?
                .0
                .clone()
        };
        let latest = self.latest_complete_fresh(&root).await?;
        // Relaxed identity gate, not the exact one. `latest_complete_fresh` is
        // itself a serve-old-first ladder: it returns whatever complete
        // generation is retained and only *requests* the reconcile. Post-checking
        // the exact reference here discarded that retained generation the moment
        // HEAD moved, so grep/context/callers went `Unavailable` after every
        // restart-following-a-commit even though a complete generation was in
        // hand. Attribution is generation-bound (see
        // [`latest_matches_scope_identity`]), and the ladder has already
        // scheduled the rebuild that will replace this generation.
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Resolve one exact scope and admit only an already-current generation.
    pub async fn latest_complete_ready_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.latest_complete_ready_for_scope_with(scope, GenerationDecodeAdmissionV1::AwaitDecode)
            .await
    }

    /// Resolve one exact scope and report whether its mounted scheduler has
    /// verified the live source and that verified source publishes no code
    /// generation at all (no extractable files). This is the typed
    /// generation-empty state a caller awaiting first publication must accept
    /// instead of timing out against a generation that can never exist.
    pub async fn reconciled_without_generation_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> bool {
        let freshness = {
            let mounted = self.mounted.lock().await;
            let Some((_, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
                return false;
            };
            worktree.source_freshness.clone()
        };
        freshness.reconciled_without_generation()
    }

    /// [`Self::latest_complete_ready_for_scope`] restricted to an
    /// already-decoded generation.
    ///
    /// This is the freshness probe for a caller that *already* has a complete
    /// seated generation it can serve. Validate that immutable serving
    /// authority directly rather than consulting the publication decoder cache:
    /// unrelated activation work may own that cache while the seated generation
    /// remains fully decoded and current. When a background reconcile owns the
    /// scheduler mutex, the recorded exact-source witness answers for the
    /// seated generation instead of refusing for the whole pass (see
    /// [`MountedCodeIndexWorktreeV1::serving_source_witness`]).
    pub async fn latest_complete_ready_decoded_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        self.activate_for_scope(scope);
        let root = {
            let mounted = self.mounted.lock().await;
            unique_mounted_for_scope(&mounted, scope)
                .unique()?
                .0
                .clone()
        };
        self.latest_complete_ready_decoded_for_root_scope(&root, scope)
            .await
    }

    fn current_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // A synchronous census abstains under map contention; the verified
        // read path awaits the map instead (see
        // [`Self::latest_complete_ready_decoded_for_root_scope`]).
        let parts = {
            let mounted = self.mounted.try_lock().ok()?;
            Self::serving_parts_for_root_scope(&mounted, &project_root, scope)?
        };
        Self::ready_decoded_from_serving_parts(parts, &project_root, scope)
    }

    /// Extract the seat handles the ready probe needs from one mounted route.
    fn serving_parts_for_root_scope(
        mounted: &BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<ReadyProbeServingPartsV1> {
        let worktree = mounted.get(project_root)?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return None;
        }
        Some((
            worktree.source_freshness.clone(),
            worktree.historical_generation_owner.clone(),
            Arc::clone(&worktree.serving_generation),
            Arc::clone(&worktree.serving_source_witness),
            Arc::clone(&worktree.shutting_down),
            Arc::clone(&worktree.wake),
            Arc::clone(&worktree.pending_wake),
            Arc::clone(&worktree.reconcile_in_progress),
        ))
    }

    fn ready_decoded_from_serving_parts(
        (
            source_freshness,
            historical_generation_owner,
            serving_generation,
            serving_source_witness,
            shutting_down,
            wake,
            pending_wake,
            reconcile_in_progress,
        ): ReadyProbeServingPartsV1,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        // The census asks only whether a fully decoded generation is already
        // seated. A graph-off mount deliberately leaves this slot empty while
        // its authenticated text owner is warming. Return that known answer
        // before entering the exact freshness probe: probing an unseated slot
        // cannot produce a decoded owner, and on an initial lightweight mount
        // it would turn `freshness_unknown` into a fabricated overflow wake.
        let serving = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let witness_matches_seat = serving_source_witness
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|witness| {
                witness.generation_id == serving.generation().manifest().generation_id
            });
        let wake_trigger = if reconcile_in_progress.load(Ordering::Acquire) == 0 {
            CodeIndexCadenceTriggerV1::QueryAdmission
        } else {
            CodeIndexCadenceTriggerV1::BusyFollowUp
        };
        if !witness_matches_seat {
            Self::note_wake_if_idle(&pending_wake, &wake, wake_trigger);
            return None;
        }
        if !source_freshness.ready_without_stat(project_root, &shutting_down) {
            Self::note_wake_if_idle(&pending_wake, &wake, wake_trigger);
            return None;
        }
        if !historical_generation_owner
            .active_publication_covers(serving.generation())
            .ok()?
        {
            *serving_source_witness
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            return None;
        }
        // Checkout-identity gate: the ready probe (or its recorded witness)
        // proved the generation current against the live worktree, and the
        // sealed reference label is attribution, not identity (see
        // [`latest_matches_scope_identity`]).
        if !latest_matches_scope_identity(&serving, scope) {
            return None;
        }
        Some(serving)
    }

    /// Report an already-decoded current generation for one exact mounted root
    /// and scope without mounting, decoding, or reconciling.
    pub fn has_current_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> bool {
        self.current_ready_decoded_for_root_scope(project_root, scope)
            .is_some()
    }

    /// Return the exact ready generation without blocking the async executor
    /// on the bounded synchronous freshness probe.
    #[hotpath::measure(label = "daemon.code_index.query.latest_ready_decoded", future = true)]
    pub async fn latest_complete_ready_decoded_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // Await the map mutex rather than try-locking it: its critical
        // sections are brief map reads, while an abstention under contention
        // here falsely demotes a proven-current answer to the stale serving
        // arm for that read.
        let parts = hotpath::measure_block!(
            "daemon.code_index.query.latest_ready_decoded.mounted_wait",
            {
                let mounted = self.mounted.lock().await;
                Self::serving_parts_for_root_scope(&mounted, &project_root, scope)?
            }
        );
        let scope = scope.clone();
        let probe_root = project_root.clone();
        let probe = tokio::task::spawn_blocking(move || {
            hotpath::measure_block!(
                "daemon.code_index.query.latest_ready_decoded.execution",
                Self::ready_decoded_from_serving_parts(parts, &probe_root, &scope)
            )
        });
        let latest = hotpath::measure_block!(
            "daemon.code_index.query.latest_ready_decoded.offload_join",
            probe.await
        )
        .ok()
        .flatten()?;
        self.install_test_attribution_authority(&project_root, &latest)
            .await;
        Some(latest)
    }

    async fn latest_complete_ready_for_scope_with(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
        admission: GenerationDecodeAdmissionV1,
    ) -> Option<LatestCompleteCodeIndexV1> {
        // MCP search resolves its generation before it asks for query authority,
        // so this is the first authenticated demand boundary on that path.
        self.activate_for_scope(scope);
        let (root, graph_activation_enabled, text_generation) = {
            let mounted = self.mounted.lock().await;
            let Some((root, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
                tracing::info!(
                    event = "code_index_complete_generation_unavailable",
                    project_id = %scope.project_id.as_str(),
                    repository_id = %scope.repository_id.as_str(),
                    worktree_id = %scope.worktree_id.as_str(),
                    reason = "no_unique_mounted_scope",
                    "complete-generation read declined at mounted scope"
                );
                return None;
            };
            (
                root.clone(),
                worktree.graph_activation.policy().is_enabled(),
                Arc::clone(&worktree.text_generation),
            )
        };
        // Graph-off mounts authenticate the sealed text source before its
        // bounded projection becomes ready. During that window the text owner
        // is the canonical warming authority; falling through to AwaitDecode
        // would reconstruct the graph-bearing generation solely to report the
        // same typed unavailability, defeating the lightweight cutover.
        if !graph_activation_enabled
            && text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
        {
            tracing::info!(
                event = "code_index_complete_generation_unavailable",
                project = %root.display(),
                reason = "graph_policy_disabled_with_text_owner",
                "complete-generation read retains the text-only authority"
            );
            return None;
        }
        let latest = self.latest_complete_ready_with(&root, admission).await?;
        // Checkout-identity gate: the ready ladder verified currency against
        // the live worktree, so a scope whose branch label was resolved on
        // the other side of a `git switch` must still be served its own
        // checkout's generation (see [`latest_matches_scope_identity`]).
        let scope_matches = latest_matches_scope_identity(&latest, scope);
        if !scope_matches {
            tracing::info!(
                event = "code_index_complete_generation_unavailable",
                project = %root.display(),
                scope_valid = scope.validate().is_ok(),
                project_matches = latest.generation().manifest().project_id == scope.project_id,
                repository_matches = latest.generation().snapshot().repository == scope.repository_id,
                worktree_matches = latest.generation().snapshot().worktree.as_ref() == Some(&scope.worktree_id),
                reason = "serving_scope_mismatch",
                "complete-generation read declined exact checkout identity"
            );
        }
        scope_matches.then_some(latest)
    }

    /// Resolve one exact scope and serve the last complete generation already
    /// held for that worktree, without running the freshness ladder.
    ///
    /// This is the stale-while-revalidate arm of query admission. The
    /// per-worktree `serving_generation` is seeded at mount from the restored
    /// generation and rewritten by every publication, so the read is O(1) and
    /// never blocks on reconcile, gix status, or the scheduler mutex. A caller
    /// that takes this arm is serving an older complete generation and must
    /// mark its lanes stale; it must never present the result as current.
    pub async fn latest_text_serving_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCodeTextGenerationV1> {
        let text_generation = {
            let mounted = self.mounted.lock().await;
            Arc::clone(
                &unique_mounted_for_scope(&mounted, scope)
                    .unique()?
                    .1
                    .text_generation,
            )
        };
        let latest = text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        (text_matches_scope_identity(&latest, scope) && latest.text_serving_is_ready())
            .then_some(latest)
    }

    /// Recover one retained persistent graph generation for an authenticated
    /// immutable query. The retained publication metadata and verified graph
    /// head are generation-addressed, so this does not admit the generation's
    /// lexical artifact or alias the currently serving graph.
    pub(crate) async fn retained_graph_generation_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
        generation_id: &CodeGenerationId,
    ) -> Result<Option<LatestCodeTextGenerationV1>, CodeIndexSchedulerErrorV1> {
        let (
            historical_generation_owner,
            graph_activation,
            project_id,
            repository_id,
            worktree_id,
            shutting_down,
        ) = {
            let mounted = self.mounted.lock().await;
            let Some((_, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
                return Ok(None);
            };
            (
                worktree.historical_generation_owner.clone(),
                worktree.graph_activation.clone(),
                worktree.historical_generation_owner.project_id.clone(),
                worktree.repository_id.clone(),
                worktree.worktree_id.clone(),
                Arc::clone(&worktree.shutting_down),
            )
        };
        let Some(latest) = historical_generation_owner.published_text_generation(generation_id)?
        else {
            return Ok(None);
        };
        let replay_binding = historical_generation_owner.sealed_replay_binding(generation_id)?;
        if !graph_activation
            .recover_verified_generation(
                &project_id,
                &repository_id,
                &worktree_id,
                latest.clone(),
                replay_binding,
                shutting_down,
            )
            .await?
        {
            return Ok(None);
        }
        Ok(Some(latest))
    }

    /// The queryable exact/lexical owner for one mounted root, if the
    /// lightweight text slot has finished seating. Distinct from
    /// [`Self::latest_generation_id`], which prefers the graph-bearing serving
    /// slot and therefore stays on the previous generation until optional
    /// graph activation finishes.
    pub async fn latest_text_serving_for_root(
        &self,
        project_root: &Path,
    ) -> Option<LatestCodeTextGenerationV1> {
        self.text_owner_for_root(project_root, true).await
    }

    /// The retained owner of one mounted root whatever its lexical artifact is
    /// doing: the manifest, snapshot, file list, and native graph store, none
    /// of which that artifact contributes to.
    ///
    /// Reads that answer from the artifact keep the serving accessor above.
    /// Symbol-graph identity and graph projection reads took it too, so a
    /// restart that resumed an unfinished ngram index refused
    /// `code_symbol_search` with `lsp-code-index-generation-unavailable` for
    /// the whole build while the recovered graph was already serving
    /// (issue #1244).
    pub async fn retained_text_owner_for_root(
        &self,
        project_root: &Path,
    ) -> Option<LatestCodeTextGenerationV1> {
        self.text_owner_for_root(project_root, false).await
    }

    async fn text_owner_for_root(
        &self,
        project_root: &Path,
        require_serving_ready: bool,
    ) -> Option<LatestCodeTextGenerationV1> {
        let project_root = project_root.canonicalize().ok()?;
        let text_generation = {
            let mounted = self.mounted.lock().await;
            Arc::clone(&mounted.get(&project_root)?.text_generation)
        };
        text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .filter(|latest| !require_serving_ready || latest.text_serving_is_ready())
    }

    /// Resolve graph-independent exact/lexical serving through the same cheap
    /// freshness ladder as complete-generation queries. The immutable text
    /// owner remains servable while a real edit is reconciled in the retained
    /// background worker; a quiet repository only refreshes its source witness
    /// clock and posts no wake.
    pub async fn latest_text_fresh_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCodeTextGenerationV1> {
        self.latest_text_serving_freshness_for_scope(scope)
            .await
            .map(|(latest, _)| latest)
    }

    /// Resolve the graph-independent text owner together with the freshness
    /// decision made by the same scheduler observation. A ready text artifact
    /// is not inherently stale merely because native graph activation is off.
    ///
    /// Currency is judged from the shared fence's bounded proof of the exact
    /// sealed source. Once that proof expires, the immutable owner remains
    /// available as stale while one coalesced wake asks the retained worker to
    /// run the exact stat/content proof. The read never performs that work or
    /// waits for the scheduler mutex — the pass counter is read only to
    /// attribute the wake, never to decide currency.
    pub async fn latest_text_serving_freshness_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<(LatestCodeTextGenerationV1, bool)> {
        self.text_owner_freshness_for_scope(scope, true).await
    }

    /// The same freshness ladder without the lexical-readiness requirement.
    /// See [`Self::retained_text_owner_for_root`].
    pub async fn retained_text_owner_freshness_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<(LatestCodeTextGenerationV1, bool)> {
        self.text_owner_freshness_for_scope(scope, false).await
    }

    async fn text_owner_freshness_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
        require_serving_ready: bool,
    ) -> Option<(LatestCodeTextGenerationV1, bool)> {
        let (
            root,
            source_freshness,
            text_generation,
            wake,
            pending_wake,
            reconcile_in_progress,
            shutting_down,
        ) = {
            let mounted = self.mounted.lock().await;
            let (root, worktree) = unique_mounted_for_scope(&mounted, scope).unique()?;
            (
                root.clone(),
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.shutting_down),
            )
        };
        let scope = scope.clone();
        let latest = text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .filter(|latest| {
                (!require_serving_ready || latest.text_serving_is_ready())
                    && text_matches_scope_identity(latest, &scope)
            })?;
        let current = source_freshness.serves_recently_verified_source(
            &latest.metadata().snapshot().content_identity,
            &root,
            &shutting_down,
        );
        if !current {
            // Attribution, not admission. A read that cannot verify the owner
            // while a pass already owns the worktree is a follow-up to that
            // pass: its wake is served after the in-flight pass ends, so its
            // event-to-ready latency measures the busy window, not the query
            // ladder's. Reporting both as `QueryAdmission` buried an unrelated
            // pass inside the query-admission cadence sample.
            let trigger = if reconcile_in_progress.load(Ordering::Acquire) == 0 {
                CodeIndexCadenceTriggerV1::QueryAdmission
            } else {
                CodeIndexCadenceTriggerV1::BusyFollowUp
            };
            Self::note_wake_if_idle(&pending_wake, &wake, trigger);
        }
        Some((latest, current))
    }

    pub async fn latest_complete_serving_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let serving_generation = {
            let mounted = self.mounted.lock().await;
            Arc::clone(
                &unique_mounted_for_scope(&mounted, scope)
                    .unique()?
                    .1
                    .serving_generation,
            )
        };
        let latest = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        // Relaxed identity gate: this arm is stale by construction, so a moved
        // reference is exactly the condition it exists to survive.
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// [`Self::latest_complete_serving_for_scope`] keyed by the exact mounted
    /// root, mirroring [`Self::latest_complete_ready_decoded_for_root_scope`]:
    /// the stale-while-revalidate arm behind the daemon's exact-scope graph
    /// reads. The seat is O(1), never joins the scheduler mutex, and holds the
    /// last complete generation for the whole rebuild window; a caller that
    /// serves it must type the answer as the last complete generation rather
    /// than current.
    pub async fn latest_complete_serving_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let serving_generation = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                return None;
            }
            Arc::clone(&worktree.serving_generation)
        };
        let latest = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        // Relaxed identity gate: this arm is stale by construction, so a moved
        // reference is exactly the condition it exists to survive.
        latest_matches_scope_identity(&latest, scope).then_some(latest)
    }

    /// Whether a source-moving rebuild remedy is actually in motion for the
    /// exact mounted root. An expired source proof can own the same worker
    /// without any evidence that the checkout moved; that is verification,
    /// not a replacement build.
    #[hotpath::measure(
        label = "daemon.code_index.query.rebuild_pass_in_flight",
        future = true
    )]
    pub async fn rebuild_pass_in_flight_for_root_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let mounted = self.mounted.lock().await;
        let Some(worktree) = mounted.get(&project_root) else {
            return false;
        };
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return false;
        }
        let refresh_in_flight = worktree.reconcile_in_progress.load(Ordering::Acquire) != 0
            || worktree
                .pending_wake
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .micros
                != 0;
        refresh_in_flight && worktree.source_freshness.source_change_pending()
    }

    /// Whether an exact mounted route has no admissible generation because its
    /// retained owner is still verifying or rebuilding it.
    pub async fn generation_is_unverified_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> bool {
        let mounted = self.mounted.lock().await;
        let Some((_, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
            return false;
        };
        worktree
            .serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
            && (worktree.reconcile_in_progress.load(Ordering::Acquire) != 0
                || worktree
                    .pending_wake
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .micros
                    != 0)
    }

    /// Ask the background worker for a reconcile on behalf of a query admission
    /// that found nothing servable, then return whether a wake was posted.
    ///
    /// This never reconciles inline and never parks: it checks only the bounded
    /// source proof and hands exact verification or rebuild to the worker. It
    /// exists because the search path had no remedy at
    /// all — the freshness ladder lives in `latest_complete_fresh`, which search
    /// deliberately does not call, so a search that resolved to nothing returned
    /// its typed failure forever without ever asking anyone to rebuild.
    ///
    /// A quiet repository must not turn every read into a wake, so two
    /// suppressions apply. First, an already-pending, unclaimed wake *is* the
    /// remedy this admission would ask for, so it is reused rather than
    /// duplicated — that is what keeps a rebuild window's worth of failing
    /// searches from becoming a wake storm and from each fabricating its own
    /// cadence arrival. Second, when a generation's immutable text owners are
    /// ready, the shared source fence suppresses a wake while its proof is
    /// current. Authenticated metadata without those owners is still warming
    /// and always needs the worker's next bounded slice.
    ///
    /// An in-flight pass is deliberately *not* a third suppression. That pass
    /// observed the checkout when it started, which may predate the state this
    /// admission found unservable, so declining here strands the remedy until
    /// an unrelated hint arrives. The claim above already coalesces the only
    /// duplicate worth suppressing — a wake nobody has dequeued yet.
    pub async fn request_query_background_reconcile(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> bool {
        #[cfg(test)]
        let test_control = Self::query_admission_control_for_test(scope);
        #[cfg(test)]
        let lookup_guard = if let Some(test_control) = test_control.as_ref() {
            Some(test_control.lookup_gate.lock().await)
        } else {
            None
        };
        let (
            root,
            source_freshness,
            shutting_down,
            serving_generation,
            text_generation,
            hints,
            wake,
            pending_wake,
        ) = {
            let mounted = self.mounted.lock().await;
            let Some((root, worktree)) = unique_mounted_for_scope(&mounted, scope).unique() else {
                return false;
            };
            (
                root.clone(),
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.shutting_down),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.text_generation),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
            )
        };
        #[cfg(test)]
        drop(lookup_guard);
        #[cfg(test)]
        if let Some(test_control) = test_control.as_ref() {
            test_control.rendezvous.wait().await;
        }
        // Atomically reserve the existing pending-wake slot before scheduling
        // the blocking freshness probe. A pending or concurrently claimed wake
        // already supplies this query's remedy.
        let Some(wake_claim) = PendingWakeClaimV1::claim(Arc::clone(&pending_wake)) else {
            return false;
        };
        #[cfg(test)]
        if let Some(test_control) = test_control.as_ref()
            && test_control.pauses_after_claim.load(Ordering::Acquire)
        {
            let released = test_control.claim_release.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            test_control.claim_reached.store(true, Ordering::Release);
            test_control.claim_entered.notify_waiters();
            released.await;
        }
        let nothing_servable = serving_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
            && text_generation
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none();
        let text_owners_are_warming = text_generation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(LatestCodeTextGenerationV1::text_serving_needs_work);
        let proof_expired = !source_freshness.ready_without_stat(&root, &shutting_down);
        if !nothing_servable && !text_owners_are_warming && !proof_expired {
            return false;
        }
        if nothing_servable {
            // This admission observed no source mutation, so it may not
            // supersede an in-flight authoritative snapshot. The retained
            // overflow plus pending wake guarantees a follow-up pass.
            hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .overflow();
        }
        // `claim` only proves the slot was free at that instant. `note_wake`
        // coalesces a foreign arrival into a live claim — it keeps the claimed
        // `micros` and takes the owner — so a hook hint, overflow, or watcher
        // probe can land in the window between the claim and here. That
        // arrival is the remedy this admission would ask for, and stamping
        // `QueryAdmission` over it is exactly the fabricated cadence arrival
        // the pending-wake suppression exists to prevent. The remedy above is
        // already recorded; leave the claim unsettled so its drop releases
        // this admission's owner without erasing the foreign marker. The
        // claim is also lost when the worker consumed the marker through
        // `take_pending_arrival` (owner reset to zero); that only happens
        // inside a reconcile pass, which is itself the remedy.
        if !wake_claim.still_owns() {
            return false;
        }
        Self::note_wake(
            &pending_wake,
            &wake,
            CodeIndexCadenceTriggerV1::QueryAdmission,
        );
        wake_claim.settle();
        true
    }

    /// Resolve the current canonical generation for semantic evaluation.
    ///
    /// A partitioned restart deliberately restores text and graph through
    /// lightweight owners without installing the decoded serving seat. Native
    /// evaluation still needs the immutable full generation, so it opens the
    /// active publication through the scheduler's shared decode cache after
    /// proving the exact mounted scope and source-freshness witness. This never
    /// seats graph serving or promotes a retained generation on its own.
    pub async fn semantic_evaluation_generation_for_scope(
        &self,
        project_root: &Path,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Result<
        (
            super::super::SemanticEvaluationCodeSnapshotV1,
            Arc<CodeIndexPublishedGenerationV1>,
        ),
        SemanticEvaluationGenerationRefusalV1,
    > {
        let requested_root = project_root;
        let project_root = project_root
            .canonicalize()
            .map_err(|_| SemanticEvaluationGenerationRefusalV1::ProjectRootCanonicalizationFailed);
        let project_root = match project_root {
            Ok(project_root) => project_root,
            Err(reason) => {
                record_semantic_candidate_refusal(requested_root, reason);
                return Err(reason);
            }
        };
        let (scheduler, source_freshness, shutting_down, wake, pending_wake) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                let reason = SemanticEvaluationGenerationRefusalV1::ProjectRootNotMounted;
                record_semantic_candidate_refusal(&project_root, reason);
                return Err(reason);
            };
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                let reason = SemanticEvaluationGenerationRefusalV1::ScopeIdentityMismatch;
                record_semantic_candidate_refusal(&project_root, reason);
                return Err(reason);
            }
            (
                Arc::clone(&worktree.scheduler),
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.shutting_down),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
            )
        };
        let scope = scope.clone();
        let task_shutting_down = Arc::clone(&shutting_down);
        let result = tokio::task::spawn_blocking(move || {
            if task_shutting_down.load(Ordering::Acquire) {
                return Err(SemanticEvaluationGenerationRefusalV1::SchedulerUnavailable);
            }
            if source_freshness.source_change_pending() {
                return Err(SemanticEvaluationGenerationRefusalV1::SourceChanged);
            }
            let mut scheduler =
                Self::lock_scheduler_unless_shutting_down(&scheduler, &task_shutting_down)
                    .map_err(|_| SemanticEvaluationGenerationRefusalV1::SchedulerUnavailable)?;
            if !scheduler.git_authority_available() {
                return Err(SemanticEvaluationGenerationRefusalV1::GitAuthorityUnavailable);
            }
            match scheduler.freshness_probe_verdict() {
                super::super::FreshnessProbeVerdictV1::Current => {}
                super::super::FreshnessProbeVerdictV1::Unverified => {
                    return Err(SemanticEvaluationGenerationRefusalV1::SourceUnverified);
                }
                super::super::FreshnessProbeVerdictV1::Moved => {
                    return Err(SemanticEvaluationGenerationRefusalV1::SourceChanged);
                }
            }
            let latest = scheduler
                .latest_complete()
                .ok_or(SemanticEvaluationGenerationRefusalV1::GenerationUnavailable)?;
            if source_freshness.source_change_pending() {
                return Err(SemanticEvaluationGenerationRefusalV1::SourceChanged);
            }
            match scheduler.freshness_probe_verdict() {
                super::super::FreshnessProbeVerdictV1::Current => {}
                super::super::FreshnessProbeVerdictV1::Unverified => {
                    return Err(SemanticEvaluationGenerationRefusalV1::SourceUnverified);
                }
                super::super::FreshnessProbeVerdictV1::Moved => {
                    return Err(SemanticEvaluationGenerationRefusalV1::SourceChanged);
                }
            }
            if !latest_matches_scope_identity(&latest, &scope) {
                return Err(SemanticEvaluationGenerationRefusalV1::GenerationScopeMismatch);
            }
            Ok((
                latest.semantic_evaluation_snapshot(),
                latest.generation_handle(),
            ))
        })
        .await
        .map_err(|_| SemanticEvaluationGenerationRefusalV1::WorkerJoinFailed)
        .and_then(|result| result);
        if result.is_err() && !shutting_down.load(Ordering::Acquire) {
            Self::note_wake_if_idle(
                &pending_wake,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
        }
        if let Err(reason) = result.as_ref() {
            record_semantic_candidate_refusal(&project_root, *reason);
        }
        result
    }

    pub async fn semantic_evaluation_snapshot_for_scope(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
    ) -> Option<super::super::SemanticEvaluationCodeSnapshotV1> {
        let root = {
            let mounted = self.mounted.lock().await;
            unique_mounted_for_scope(&mounted, scope)
                .unique()?
                .0
                .clone()
        };
        self.semantic_evaluation_generation_for_scope(&root, scope)
            .await
            .ok()
            .map(|(snapshot, _)| snapshot)
    }

    pub async fn acquire_semantic_evaluation_publication_lease(
        &self,
        scope: &tracedecay_contracts::ResolvedScope,
        expected: &super::super::SemanticEvaluationCodeSnapshotV1,
    ) -> Option<CodeIndexSemanticEvaluationPublicationLeaseV1> {
        let gate = {
            let mounted = self.mounted.lock().await;
            Arc::clone(
                &unique_mounted_for_scope(&mounted, scope)
                    .unique()?
                    .1
                    .semantic_evaluation_publication_gate,
            )
        };
        let guard = gate.lock_owned().await;
        if self
            .semantic_evaluation_snapshot_for_scope(scope)
            .await
            .as_ref()
            != Some(expected)
        {
            return None;
        }
        Some(CodeIndexSemanticEvaluationPublicationLeaseV1 { _guard: guard })
    }
}

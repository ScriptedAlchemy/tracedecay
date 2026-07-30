//! Daemon-owned registry of mounted per-worktree code-index schedulers.
//!
//! Owns the map of live worktree schedulers, their reconciliation worker tasks,
//! and the shared content-addressed byte pool. The registry is the async-facing
//! surface: hook-hint delivery, query-admission freshness, and lifecycle
//! (mount/shutdown). The synchronous per-worktree indexing logic lives on
//! [`CodeIndexWorktreeSchedulerV1`]; this module never runs it while holding the
//! registry map lock.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tracedecay_domain::{CodeGenerationId, ManifestDigest, RepositoryId, WorktreeId};
use tracedecay_lsp::{LspRuntimeFailure, LspRuntimeFuture};

use super::{
    CodeIndexBytePoolStatsV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexEventToReadyReceiptV1, CodeIndexNoopEvidenceV1,
    CodeIndexPublishEvidenceV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, LatestCompleteCodeIndexV1, SharedCodeIndexBytePoolV1, now_micros,
};

const MAX_CONCURRENT_BACKGROUND_RECONCILES: usize = 1;
const GENERATION_PUBLICATION_CHANNEL_CAPACITY: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeIndexGenerationPublishedV1 {
    pub project_root: PathBuf,
    pub repository_id: RepositoryId,
    pub generation_id: CodeGenerationId,
    pub snapshot_content_identity: tracedecay_domain::ContentDigest,
    pub observation_time_micros: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CodeIndexSchedulerMemoryStatsV1 {
    pub mounted_worktrees: u64,
    pub reconciling_worktrees: u64,
    pub retained_generation_encoded_bytes: u64,
}

pub(super) struct MountedCodeIndexWorktreeV1 {
    pub(super) repository_id: RepositoryId,
    pub(super) worktree_id: WorktreeId,
    pub(super) pr9_query_authority: Option<(
        ManifestDigest,
        Arc<tracedecay_query::retrieval::Pr9QueryAuthorityV1>,
    )>,
    pub(super) semantic_query_authority: Option<(
        ManifestDigest,
        Arc<super::semantic_query_runtime::SemanticQueryAuthorityV1>,
    )>,
    pub(super) scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    pub(super) serving_generation: Arc<RwLock<Option<LatestCompleteCodeIndexV1>>>,
    wake: Arc<tokio::sync::Notify>,
    /// Unix micros of the earliest pending wake not yet consumed by a receipt.
    pending_wake_micros: Arc<AtomicU64>,
    /// Packed [`CodeIndexCadenceTriggerV1`] for the pending wake.
    pending_wake_trigger: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    reconcile_in_progress: Arc<AtomicBool>,
    active_generation_encoded_bytes: Arc<AtomicU64>,
    pub(super) semantic_evaluation_publication_gate: Arc<tokio::sync::Mutex<()>>,
    pub(super) task: tokio::task::JoinHandle<()>,
}

pub(in crate::daemon) struct CodeIndexSemanticEvaluationPublicationLeaseV1 {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

#[derive(Clone)]
pub(crate) struct CodeIndexSchedulerRegistryV1 {
    pub(super) max_worktrees: usize,
    pub(super) byte_pool: Arc<SharedCodeIndexBytePoolV1>,
    pub(super) mounted: Arc<tokio::sync::Mutex<BTreeMap<PathBuf, MountedCodeIndexWorktreeV1>>>,
    mount_admission: Arc<tokio::sync::Mutex<()>>,
    background_reconcile_admission: Arc<tokio::sync::Semaphore>,
    generation_publications: tokio::sync::broadcast::Sender<CodeIndexGenerationPublishedV1>,
    cadence_telemetry: Arc<Mutex<CodeIndexCadenceTelemetryV1>>,
    test_attribution_authorities: Arc<
        RwLock<
            BTreeMap<
                PathBuf,
                (
                    CodeGenerationId,
                    crate::code_index::production::PublishedGenerationTestAttributionAuthorityV1,
                ),
            >,
        >,
    >,
}

impl CodeIndexSchedulerRegistryV1 {
    pub fn new(max_worktrees: usize) -> Self {
        let (generation_publications, _) =
            tokio::sync::broadcast::channel(GENERATION_PUBLICATION_CHANNEL_CAPACITY);
        Self {
            max_worktrees,
            byte_pool: Arc::new(SharedCodeIndexBytePoolV1::default()),
            mounted: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
            mount_admission: Arc::new(tokio::sync::Mutex::new(())),
            background_reconcile_admission: Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_BACKGROUND_RECONCILES,
            )),
            generation_publications,
            cadence_telemetry: Arc::new(Mutex::new(CodeIndexCadenceTelemetryV1::default())),
            test_attribution_authorities: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    fn pack_trigger(trigger: CodeIndexCadenceTriggerV1) -> u64 {
        match trigger {
            CodeIndexCadenceTriggerV1::Mount => 1,
            CodeIndexCadenceTriggerV1::HookHint => 2,
            CodeIndexCadenceTriggerV1::Overflow => 3,
            CodeIndexCadenceTriggerV1::QueryAdmission => 4,
            CodeIndexCadenceTriggerV1::BusyFollowUp => 5,
        }
    }

    fn unpack_trigger(packed: u64) -> CodeIndexCadenceTriggerV1 {
        match packed {
            2 => CodeIndexCadenceTriggerV1::HookHint,
            3 => CodeIndexCadenceTriggerV1::Overflow,
            4 => CodeIndexCadenceTriggerV1::QueryAdmission,
            5 => CodeIndexCadenceTriggerV1::BusyFollowUp,
            _ => CodeIndexCadenceTriggerV1::Mount,
        }
    }

    fn note_wake(
        pending_wake_micros: &AtomicU64,
        pending_wake_trigger: &AtomicU64,
        wake: &tokio::sync::Notify,
        trigger: CodeIndexCadenceTriggerV1,
    ) {
        let wake_micros = u64::try_from(now_micros().0).unwrap_or(u64::MAX);
        let _ = pending_wake_micros.compare_exchange(
            0,
            wake_micros,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        pending_wake_trigger.store(Self::pack_trigger(trigger), Ordering::Release);
        wake.notify_one();
    }

    fn record_reconcile_receipt(
        telemetry: &Mutex<CodeIndexCadenceTelemetryV1>,
        project_root: PathBuf,
        pending_wake_micros: &AtomicU64,
        pending_wake_trigger: &AtomicU64,
        default_trigger: CodeIndexCadenceTriggerV1,
        outcome: &CodeIndexReconcileOutcomeV1,
    ) {
        let ready_micros = now_micros().0;
        let wake_micros = pending_wake_micros.swap(0, Ordering::AcqRel);
        let trigger = if wake_micros == 0 {
            default_trigger
        } else {
            Self::unpack_trigger(pending_wake_trigger.load(Ordering::Acquire))
        };
        let wake_micros = if wake_micros == 0 {
            ready_micros
        } else {
            i64::try_from(wake_micros).unwrap_or(ready_micros)
        };
        let (cadence_outcome, overflow_reconciled) = match outcome {
            CodeIndexReconcileOutcomeV1::Published(evidence) => (
                CodeIndexCadenceOutcomeV1::Published {
                    generation_id: evidence.generation_id.clone(),
                    reextracted_files: evidence.reextracted_files,
                    changed_chunks: evidence.changed_chunks,
                    reused_chunks: evidence.reused_chunks,
                },
                evidence.overflow_reconciled,
            ),
            CodeIndexReconcileOutcomeV1::Noop(CodeIndexNoopEvidenceV1 {
                snapshot_content_identity,
                overflow_reconciled,
            }) => (
                CodeIndexCadenceOutcomeV1::Noop {
                    snapshot_content_identity: snapshot_content_identity.clone(),
                },
                *overflow_reconciled,
            ),
        };
        let receipt = CodeIndexEventToReadyReceiptV1::new(
            project_root,
            trigger,
            wake_micros,
            ready_micros,
            cadence_outcome,
            overflow_reconciled,
        );
        telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(receipt);
    }

    #[cfg(test)]
    pub(super) fn latest_event_to_ready_receipt(&self) -> Option<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest()
            .cloned()
    }

    #[cfg(test)]
    pub(super) fn event_to_ready_receipts(&self) -> Vec<CodeIndexEventToReadyReceiptV1> {
        self.cadence_telemetry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receipts()
            .to_vec()
    }

    pub(crate) fn subscribe_generation_publications(
        &self,
    ) -> tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1> {
        self.generation_publications.subscribe()
    }

    fn publish_generation(
        sender: &tokio::sync::broadcast::Sender<CodeIndexGenerationPublishedV1>,
        project_root: PathBuf,
        evidence: &CodeIndexPublishEvidenceV1,
    ) {
        let _ = sender.send(CodeIndexGenerationPublishedV1 {
            project_root,
            repository_id: evidence.repository_id.clone(),
            generation_id: evidence.generation_id.clone(),
            snapshot_content_identity: evidence.snapshot_content_identity.clone(),
            observation_time_micros: now_micros().0,
        });
    }

    pub(in crate::daemon) fn open_worktree(
        &self,
        project_root: &Path,
        store_root: PathBuf,
    ) -> Result<CodeIndexWorktreeSchedulerV1, CodeIndexSchedulerErrorV1> {
        if self.max_worktrees == 0 {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is zero".to_owned(),
            ));
        }
        CodeIndexWorktreeSchedulerV1::open(project_root, store_root, Arc::clone(&self.byte_pool))
    }

    pub(in crate::daemon) fn byte_pool_stats(&self) -> CodeIndexBytePoolStatsV1 {
        self.byte_pool.stats()
    }

    pub async fn memory_stats(&self) -> CodeIndexSchedulerMemoryStatsV1 {
        let mounted = self.mounted.lock().await;
        CodeIndexSchedulerMemoryStatsV1 {
            mounted_worktrees: u64::try_from(mounted.len()).unwrap_or(u64::MAX),
            reconciling_worktrees: u64::try_from(
                mounted
                    .values()
                    .filter(|worktree| worktree.reconcile_in_progress.load(Ordering::Acquire))
                    .count(),
            )
            .unwrap_or(u64::MAX),
            retained_generation_encoded_bytes: mounted.values().fold(0_u64, |total, worktree| {
                total.saturating_add(
                    worktree
                        .active_generation_encoded_bytes
                        .load(Ordering::Acquire),
                )
            }),
        }
    }

    pub(in crate::daemon) async fn mount_worktree(
        &self,
        project_root: &Path,
        store_root: PathBuf,
        semantic_schedule: Option<
            crate::application::semantic_runtime::SavedCodeGenerationScheduleHookV1,
        >,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        // Serialize expensive mounts without pinning the registry map. The
        // initial reconcile can parse and hash an entire repository; holding
        // `mounted` here blocks every foreground query across every project.
        let _mount_admission = self.mount_admission.lock().await;
        let mounted = self.mounted.lock().await;
        if let Some(existing) = mounted.get(&project_root) {
            let scheduler = Arc::clone(&existing.scheduler);
            drop(mounted);
            let latest = {
                let mut scheduler = scheduler
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let latest = scheduler.latest_complete().map(|latest| latest.generation);
                scheduler.replace_semantic_schedule_hook(semantic_schedule.clone());
                latest
            };
            if let (Some(hook), Some(generation)) = (semantic_schedule, latest) {
                let _ = hook(&generation);
            }
            return Ok(false);
        }
        if mounted.len() >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        drop(mounted);
        let mut opened = self.open_worktree(
            &project_root,
            super::scoped_code_index_store_root(&store_root, &project_root),
        )?;
        if let Some(hook) = semantic_schedule.clone() {
            opened.replace_semantic_schedule_hook(Some(hook));
        }
        let restored_generation = opened.latest_complete();
        let repository_id = opened.identity().repository_id().clone();
        let worktree_id = opened.identity().worktree_id().clone();
        let reconcile_in_progress = opened.reconcile_in_progress();
        let active_generation_encoded_bytes = opened.active_generation_encoded_bytes();
        // Serve any retained complete generation immediately so admission stays
        // non-blocking, but never treat restore as a verified freshness claim.
        let serving_generation = Arc::new(RwLock::new(restored_generation.clone()));
        let scheduler = Arc::new(Mutex::new(opened));
        let semantic_evaluation_publication_gate = Arc::new(tokio::sync::Mutex::new(()));
        let (wake, shutting_down) = {
            let scheduler = scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                Arc::clone(&scheduler.wake),
                Arc::clone(&scheduler.shutting_down),
            )
        };
        let pending_wake_micros = Arc::new(AtomicU64::new(0));
        let pending_wake_trigger = Arc::new(AtomicU64::new(0));
        let worker_scheduler = Arc::clone(&scheduler);
        let worker_serving_generation = Arc::clone(&serving_generation);
        let worker_wake = Arc::clone(&wake);
        let worker_pending_wake_micros = Arc::clone(&pending_wake_micros);
        let worker_pending_wake_trigger = Arc::clone(&pending_wake_trigger);
        let worker_cadence_telemetry = Arc::clone(&self.cadence_telemetry);
        let worker_shutting_down = Arc::clone(&shutting_down);
        let worker_semantic_evaluation_publication_gate =
            Arc::clone(&semantic_evaluation_publication_gate);
        let worker_background_reconcile_admission =
            Arc::clone(&self.background_reconcile_admission);
        let worker_generation_publications = self.generation_publications.clone();
        let worker_project_root = project_root.clone();
        let task = tokio::spawn(async move {
            loop {
                worker_wake.notified().await;
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let _semantic_evaluation_publication =
                    worker_semantic_evaluation_publication_gate.lock().await;
                let Ok(_background_reconcile_admission) =
                    Arc::clone(&worker_background_reconcile_admission)
                        .acquire_owned()
                        .await
                else {
                    return;
                };
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let scheduler = Arc::clone(&worker_scheduler);
                let result = tokio::task::spawn_blocking(move || {
                    let mut scheduler = scheduler
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let result = scheduler.reconcile_now();
                    let latest = scheduler.latest_complete();
                    (result, latest)
                })
                .await;
                if let Ok((_, Some(latest))) = &result {
                    *worker_serving_generation
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(latest.clone());
                }
                if let Ok((Ok(outcome), _)) = &result {
                    if let CodeIndexReconcileOutcomeV1::Published(evidence) = outcome {
                        Self::publish_generation(
                            &worker_generation_publications,
                            worker_project_root.clone(),
                            evidence,
                        );
                    }
                    Self::record_reconcile_receipt(
                        &worker_cadence_telemetry,
                        worker_project_root.clone(),
                        &worker_pending_wake_micros,
                        &worker_pending_wake_trigger,
                        CodeIndexCadenceTriggerV1::Mount,
                        outcome,
                    );
                }
                if worker_shutting_down.load(Ordering::Acquire) {
                    return;
                }
                // A task panic must not permanently retire the mounted
                // worktree. The next coalesced hint wakes this worker again.
                let _ = result;
            }
        });
        let mut mounted = self.mounted.lock().await;
        if mounted.len() >= self.max_worktrees {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "code-index scheduler capacity is exhausted".to_owned(),
            ));
        }
        mounted.insert(
            project_root,
            MountedCodeIndexWorktreeV1 {
                repository_id,
                worktree_id,
                pr9_query_authority: None,
                semantic_query_authority: None,
                scheduler,
                serving_generation,
                wake: Arc::clone(&wake),
                pending_wake_micros: Arc::clone(&pending_wake_micros),
                pending_wake_trigger: Arc::clone(&pending_wake_trigger),
                shutting_down,
                reconcile_in_progress,
                active_generation_encoded_bytes,
                semantic_evaluation_publication_gate,
                task,
            },
        );
        if let (Some(hook), Some(latest)) = (semantic_schedule, restored_generation) {
            let _ = hook(&latest.generation);
        }
        // Always schedule a background verification pass. Retained generations
        // keep queries non-blocking, but open-time clocks must not suppress
        // cadence indefinitely (the live stale-index defect).
        Self::note_wake(
            &pending_wake_micros,
            &pending_wake_trigger,
            &wake,
            CodeIndexCadenceTriggerV1::Mount,
        );
        Ok(true)
    }

    /// Mount the accepted PR9 profile and query/cursor key owner for one exact
    /// admitted scope. The authority cannot be inherited by another project,
    /// repository, worktree, or ref.
    pub(in crate::daemon) async fn mount_pr9_query_authority(
        &self,
        project_root: &Path,
        scope: &tracedecay_application::ResolvedScope,
        authority: Arc<tracedecay_query::retrieval::Pr9QueryAuthorityV1>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let project_root = project_root.canonicalize()?;
        let mut mounted = self.mounted.lock().await;
        let worktree = mounted.get_mut(&project_root).ok_or_else(|| {
            CodeIndexSchedulerErrorV1::Identity(
                "cannot mount PR9 query authority before its worktree".to_owned(),
            )
        })?;
        if worktree.repository_id != scope.repository_id
            || worktree.worktree_id != scope.worktree_id
        {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "PR9 query authority scope does not match the mounted worktree".to_owned(),
            ));
        }
        worktree.pr9_query_authority = Some((scope.scope_digest.clone(), authority));
        Ok(())
    }

    /// Revoke the live PR9 authority for one exact admitted scope before a
    /// committed profile refresh. A failed replacement therefore leaves
    /// search unavailable instead of serving the prior profile.
    pub(in crate::daemon) async fn clear_pr9_query_authority(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        scope
            .validate()
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        let mut mounted = self.mounted.lock().await;
        let roots = mounted
            .iter()
            .filter(|(_, worktree)| {
                worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
            })
            .map(|(root, _)| root.clone())
            .collect::<Vec<_>>();
        if roots.is_empty() {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "cannot clear PR9 query authority before its worktree".to_owned(),
            ));
        }
        let mut scope_mismatch = false;
        for root in &roots {
            let worktree = mounted.get_mut(root).ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity("worktree disappeared".to_owned())
            })?;
            scope_mismatch |= worktree
                .pr9_query_authority
                .as_ref()
                .is_some_and(|(digest, _)| digest != &scope.scope_digest);
            worktree.pr9_query_authority = None;
        }
        if roots.len() != 1 {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "PR9 query authority scope is ambiguous".to_owned(),
            ));
        }
        if scope_mismatch {
            return Err(CodeIndexSchedulerErrorV1::Identity(
                "PR9 query authority scope does not match the mounted authority".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) async fn pr9_query_authority_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<Arc<tracedecay_query::retrieval::Pr9QueryAuthorityV1>> {
        let mounted = self.mounted.try_lock().ok()?;
        let mut matched = None;
        for worktree in mounted.values() {
            if worktree.repository_id != scope.repository_id
                || worktree.worktree_id != scope.worktree_id
            {
                continue;
            }
            let Some((scope_digest, authority)) = &worktree.pr9_query_authority else {
                return None;
            };
            if scope_digest != &scope.scope_digest || matched.is_some() {
                return None;
            }
            matched = Some(Arc::clone(authority));
        }
        matched
    }

    #[cfg(test)]
    pub(crate) async fn has_pr9_query_authority_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        self.pr9_query_authority_for_scope(scope).await.is_some()
    }

    /// Whether a worktree is currently mounted for `project_root`. Read-only
    /// map membership used by the Doctor code-index mount adapter to distinguish
    /// an unmounted worktree from a mounted-but-still-indexing one. Returns
    /// `false` when the path cannot be canonicalized (a path Doctor could never
    /// have mounted under).
    pub async fn is_worktree_mounted(&self, project_root: &Path) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        self.mounted.lock().await.contains_key(&project_root)
    }

    pub async fn notify_path(&self, project_root: &Path, path: PathBuf) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (scheduler, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
            )
        };
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .notify_path(path);
        // Scheduler wake already fired; stamp the cadence clock for the receipt.
        let _ = pending_wake_micros.compare_exchange(
            0,
            u64::try_from(now_micros().0).unwrap_or(u64::MAX),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        pending_wake_trigger.store(
            Self::pack_trigger(CodeIndexCadenceTriggerV1::HookHint),
            Ordering::Release,
        );
        true
    }

    /// Primary hint path: deliver the exact touched paths carried by a host
    /// after-file-edit hook into the mounted worktree's incremental queue.
    /// `rel_paths` are repository-relative; they are resolved against the
    /// project root. Returns `true` when a worktree was mounted to receive them.
    pub async fn notify_hook_paths(&self, project_root: &Path, rel_paths: &[String]) -> bool {
        let Ok(project_root) = project_root.canonicalize() else {
            return false;
        };
        let (scheduler, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&project_root) else {
                return false;
            };
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
            )
        };
        let absolute = rel_paths
            .iter()
            .map(|rel| project_root.join(rel))
            .collect::<Vec<_>>();
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .notify_hook_paths(absolute);
        let _ = pending_wake_micros.compare_exchange(
            0,
            u64::try_from(now_micros().0).unwrap_or(u64::MAX),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        pending_wake_trigger.store(
            Self::pack_trigger(CodeIndexCadenceTriggerV1::HookHint),
            Ordering::Release,
        );
        true
    }

    pub async fn latest_generation_id(&self, project_root: &Path) -> Option<CodeGenerationId> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        let worktree = mounted.get(&project_root)?;
        worktree
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .latest_complete()
            .map(|latest| latest.generation.manifest().generation_id.clone())
    }

    /// Exact live dashboard projection for one mounted worktree.
    ///
    /// The freshness ladder runs before projection. Generation and scope fields
    /// are copied from the durable sealed generation, never reconstructed from
    /// the dashboard's display path.
    pub(in crate::daemon) async fn dashboard_freshness(
        &self,
        project_root: &Path,
    ) -> Option<crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1> {
        let canonical_root = project_root.canonicalize().ok()?;
        let (scheduler, reconcile_in_progress, serving_generation) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&canonical_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&worktree.serving_generation),
            )
        };
        tokio::task::spawn_blocking(move || {
            let refreshing = reconcile_in_progress.load(Ordering::Acquire);
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    let latest = serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let (
                        repository_id,
                        worktree_id,
                        source_reference,
                        generation_id,
                        content_identity,
                        sealed,
                    ) = latest.as_ref().map_or(
                        (None, None, None, None, None, None),
                        |latest| {
                            let generation = &latest.generation;
                            let snapshot = generation.snapshot();
                            (
                                Some(snapshot.repository.as_str().to_owned()),
                                snapshot
                                    .worktree
                                    .as_ref()
                                    .map(|worktree| worktree.as_str().to_owned()),
                                snapshot
                                    .reference
                                    .as_ref()
                                    .map(|reference| reference.as_str().to_owned()),
                                Some(generation.manifest().generation_id.as_str().to_owned()),
                                Some(snapshot.content_identity.as_str().to_owned()),
                                Some(generation.manifest().seal.sealed_at.0),
                            )
                        },
                    );
                    return crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                        worktree_root: canonical_root.display().to_string(),
                        repository_id,
                        worktree_id,
                        source_reference,
                        latest_generation_id: generation_id,
                        snapshot_content_identity: content_identity,
                        sealed_at_micros: sealed,
                        last_reconcile_micros: None,
                        staleness_state: Some(
                            if latest.is_some() {
                                "refreshing"
                            } else {
                                "indexing"
                            }
                            .to_owned(),
                        ),
                        hook_hint_count: None,
                        coverage: "partial_refresh_in_progress".to_owned(),
                    };
                }
            };
            let reconciled = scheduler.ensure_fresh_for_query().is_ok();
            let verified = scheduler.verified_against_source();
            let latest = if reconciled {
                scheduler.latest_complete()
            } else {
                None
            };
            let hook_hint_count = scheduler.pending_hint_count();
            let (
                repository_id,
                worktree_id,
                source_reference,
                generation_id,
                content_identity,
                sealed,
            ) = latest
                .as_ref()
                .map_or((None, None, None, None, None, None), |latest| {
                    let generation = &latest.generation;
                    let snapshot = generation.snapshot();
                    (
                        Some(snapshot.repository.as_str().to_owned()),
                        snapshot
                            .worktree
                            .as_ref()
                            .map(|worktree| worktree.as_str().to_owned()),
                        snapshot
                            .reference
                            .as_ref()
                            .map(|reference| reference.as_str().to_owned()),
                        Some(generation.manifest().generation_id.as_str().to_owned()),
                        Some(snapshot.content_identity.as_str().to_owned()),
                        Some(generation.manifest().seal.sealed_at.0),
                    )
                });
            let staleness_state = if !reconciled {
                "unknown"
            } else if refreshing {
                if latest.is_some() {
                    "refreshing"
                } else {
                    "indexing"
                }
            } else if !verified {
                if latest.is_some() {
                    "stale"
                } else {
                    "indexing"
                }
            } else if latest.is_some() {
                "fresh"
            } else {
                "indexing"
            };
            crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                worktree_root: canonical_root.display().to_string(),
                repository_id,
                worktree_id,
                source_reference,
                latest_generation_id: generation_id,
                snapshot_content_identity: content_identity,
                sealed_at_micros: sealed,
                last_reconcile_micros: scheduler.last_reconciled_at_micros(),
                staleness_state: Some(staleness_state.to_owned()),
                hook_hint_count,
                coverage: if !reconciled {
                    "unknown_reconcile_failed"
                } else if !verified {
                    "partial_unverified_restore"
                } else if hook_hint_count.is_some() {
                    "complete"
                } else {
                    "partial_hook_hint_overflow"
                }
                .to_owned(),
            }
        })
        .await
        .ok()
    }

    /// Query-admission entry point: run the freshness ladder (tier-1 git
    /// metadata, tier-2 bounded staleness, tier-3 identity re-resolution) before
    /// returning the latest complete generation, so external out-of-band changes
    /// are reconciled without any standing filesystem watcher.
    pub(in crate::daemon) async fn latest_complete_fresh(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        // Clone the per-worktree handle under a short map lock, then drop the
        // registry guard *before* running freshness. The synchronous freshness
        // ladder (gix status + hashing + build_and_publish) must never run while
        // the registry map is locked, or one worktree's reconcile would
        // serialize every other worktree's queries and stall the executor.
        let (scheduler, serving_generation, wake, pending_wake_micros, pending_wake_trigger) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake_micros),
                Arc::clone(&worktree.pending_wake_trigger),
            )
        };
        // Run the synchronous reconcile off the async executor. When the
        // background worker already owns the scheduler, preserve the last
        // complete immutable generation instead of joining the in-progress
        // refresh; the next request observes the newly published generation.
        let authority_root = project_root.clone();
        let cadence_telemetry = Arc::clone(&self.cadence_telemetry);
        let (latest, publication) = tokio::task::spawn_blocking(move || {
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    // Serve prior generation without waiting, but schedule a
                    // follow-up verification so busy refresh cannot strand
                    // cadence indefinitely.
                    Self::note_wake(
                        &pending_wake_micros,
                        &pending_wake_trigger,
                        &wake,
                        CodeIndexCadenceTriggerV1::BusyFollowUp,
                    );
                    return serving_generation
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                        .map(|latest| (latest, None));
                }
            };
            let wake_micros = now_micros().0;
            let outcome = scheduler.ensure_fresh_for_query().ok()?;
            let latest = scheduler.latest_complete()?;
            *serving_generation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(latest.clone());
            if let Some(outcome) = outcome.as_ref() {
                // Prefer the earlier pending wake when one exists; otherwise this
                // query-admission reconcile is its own event-to-ready sample.
                let _ = pending_wake_micros.compare_exchange(
                    0,
                    u64::try_from(wake_micros).unwrap_or(u64::MAX),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                if pending_wake_trigger.load(Ordering::Acquire) == 0 {
                    pending_wake_trigger.store(
                        Self::pack_trigger(CodeIndexCadenceTriggerV1::QueryAdmission),
                        Ordering::Release,
                    );
                }
                Self::record_reconcile_receipt(
                    &cadence_telemetry,
                    project_root.clone(),
                    &pending_wake_micros,
                    &pending_wake_trigger,
                    CodeIndexCadenceTriggerV1::QueryAdmission,
                    outcome,
                );
            }
            let publication = outcome.as_ref().and_then(|outcome| match outcome {
                CodeIndexReconcileOutcomeV1::Published(evidence) => {
                    Some(CodeIndexGenerationPublishedV1 {
                        project_root: project_root.clone(),
                        repository_id: evidence.repository_id.clone(),
                        generation_id: evidence.generation_id.clone(),
                        snapshot_content_identity: evidence.snapshot_content_identity.clone(),
                        observation_time_micros: now_micros().0,
                    })
                }
                CodeIndexReconcileOutcomeV1::Noop(_) => None,
            });
            Some((latest, publication))
        })
        .await
        .ok()
        .flatten()?;
        if let Some(publication) = publication {
            let _ = self.generation_publications.send(publication);
        }
        if let Ok(authority) = latest.test_attribution_authority() {
            let mut authorities = self
                .test_attribution_authorities
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            authorities.insert(
                authority_root,
                (
                    latest.generation.manifest().generation_id.clone(),
                    authority,
                ),
            );
        }
        Some(latest)
    }

    /// Query-admission entry point for latency-sensitive application paths.
    /// It serves only a generation whose freshness is already proven; stale,
    /// restored-unverified, or busy schedulers abstain after scheduling the
    /// background worker instead of reconciling on the caller.
    pub(in crate::daemon) async fn latest_complete_ready(
        &self,
        project_root: &Path,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let project_root = project_root.canonicalize().ok()?;
        let (scheduler, serving_generation) = {
            let mounted = self.mounted.try_lock().ok()?;
            let worktree = mounted.get(&project_root)?;
            (
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
            )
        };
        let latest = tokio::task::spawn_blocking(move || {
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return None,
            };
            let latest = scheduler.latest_complete_ready_for_query().ok().flatten()?;
            *serving_generation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(latest.clone());
            Some(latest)
        })
        .await
        .ok()
        .flatten()?;
        if let Ok(authority) = latest.test_attribution_authority() {
            let mut authorities = self
                .test_attribution_authorities
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            authorities.insert(
                project_root,
                (
                    latest.generation.manifest().generation_id.clone(),
                    authority,
                ),
            );
        }
        Some(latest)
    }

    /// Resolve one mounted root by the exact admitted repository/worktree/ref
    /// scope, then run that root's freshness ladder. A request never inherits
    /// whichever mounted worktree sorts first.
    pub(in crate::daemon) async fn latest_complete_fresh_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let root = {
            let mounted = self.mounted.lock().await;
            let mut matched = None;
            for (root, worktree) in mounted.iter() {
                if worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some(root.clone());
                }
            }
            matched?
        };
        let latest = self.latest_complete_fresh(&root).await?;
        if Self::latest_matches_scope(&latest, scope) {
            Some(latest)
        } else {
            None
        }
    }

    /// Resolve one exact scope and admit only an already-current generation.
    pub(in crate::daemon) async fn latest_complete_ready_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<LatestCompleteCodeIndexV1> {
        let root = {
            let mounted = self.mounted.try_lock().ok()?;
            let mut matched = None;
            for (root, worktree) in mounted.iter() {
                if worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some(root.clone());
                }
            }
            matched?
        };
        let latest = self.latest_complete_ready(&root).await?;
        Self::latest_matches_scope(&latest, scope).then_some(latest)
    }

    pub(in crate::daemon) async fn semantic_evaluation_snapshot_for_scope(
        &self,
        scope: &tracedecay_application::ResolvedScope,
    ) -> Option<super::SemanticEvaluationCodeSnapshotV1> {
        self.latest_complete_fresh_for_scope(scope)
            .await
            .map(|latest| latest.semantic_evaluation_snapshot())
    }

    pub(in crate::daemon) async fn acquire_semantic_evaluation_publication_lease(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        expected: &super::SemanticEvaluationCodeSnapshotV1,
    ) -> Option<CodeIndexSemanticEvaluationPublicationLeaseV1> {
        let gate = {
            let mounted = self.mounted.lock().await;
            let mut matched = None;
            for worktree in mounted.values() {
                if worktree.repository_id != scope.repository_id
                    || worktree.worktree_id != scope.worktree_id
                {
                    continue;
                }
                if matched.is_some() {
                    return None;
                }
                matched = Some(Arc::clone(&worktree.semantic_evaluation_publication_gate));
            }
            matched?
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

    pub(super) fn latest_matches_scope(
        latest: &LatestCompleteCodeIndexV1,
        scope: &tracedecay_application::ResolvedScope,
    ) -> bool {
        let snapshot = latest.generation.snapshot();
        snapshot.repository == scope.repository_id
            && snapshot.worktree.as_ref() == Some(&scope.worktree_id)
            && snapshot.reference == scope.reference
    }

    /// The per-worktree scheduler handle, cloned out of the registry map. Test
    /// support for proving that holding one worktree's scheduler lock does not
    /// block another worktree's freshness query on the registry map.
    #[cfg(test)]
    pub(super) async fn scheduler_handle(
        &self,
        project_root: &Path,
    ) -> Option<Arc<Mutex<CodeIndexWorktreeSchedulerV1>>> {
        let project_root = project_root.canonicalize().ok()?;
        let mounted = self.mounted.lock().await;
        mounted
            .get(&project_root)
            .map(|worktree| Arc::clone(&worktree.scheduler))
    }

    pub async fn shutdown(&self) {
        let mounted = std::mem::take(&mut *self.mounted.lock().await);
        self.test_attribution_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for worktree in mounted.values() {
            worktree.shutting_down.store(true, Ordering::Release);
            worktree.wake.notify_one();
        }
        for (_, worktree) in mounted {
            let _ = worktree.task.await;
        }
    }
}

impl crate::application::feedback::cycle_production::ProductionFeedbackDocumentIdentityPort
    for CodeIndexSchedulerRegistryV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
        document_uri: Option<String>,
    ) -> crate::application::feedback::cycle_production::ProductionFeedbackDocumentIdentityFuture
    {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("feedback-code-index-root-unavailable"))?;
            let current = registry.latest_complete_ready(&root).await.ok_or_else(|| {
                LspRuntimeFailure::new("feedback-code-index-generation-unavailable")
            })?;
            let generation = &current.generation;
            let snapshot = generation.snapshot();
            let file = match document_uri {
                Some(uri) => {
                    let logical_path = feedback_document_logical_path(&root, &uri)?;
                    snapshot
                        .files
                        .iter()
                        .find(|file| file.logical_path == logical_path)
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("feedback-code-index-document-unavailable")
                        })?
                }
                None => snapshot
                    .files
                    .iter()
                    .find(|file| {
                        Path::new(&file.logical_path)
                            .extension()
                            .and_then(|ext| ext.to_str())
                            == Some("rs")
                    })
                    .ok_or_else(|| {
                        LspRuntimeFailure::new("feedback-code-index-rust-document-unavailable")
                    })?,
            };
            let generation_digest =
                ManifestDigest::new(generation.manifest().snapshot_digest.as_str().to_owned())
                    .map_err(|_| {
                        LspRuntimeFailure::new("feedback-code-index-generation-invalid")
                    })?;
            Ok(
                crate::application::feedback::cycle_production::ProductionFeedbackDocumentIdentityV1 {
                    generation_id: generation.manifest().generation_id.clone(),
                    generation_digest,
                    file: file.file_occurrence_id.clone(),
                    content_digest: file.content_digest.clone(),
                },
            )
        })
    }
}

/// The registry is the single mint for file and generation identity, so every
/// diagnostic producer resolves through here instead of inventing its own.
///
/// Without this, a producer had no way to reach the authority and fell back to
/// a repository-relative path; the LSP feedback projection then refused each
/// published record with `ImpactTargetFileMismatch` / `GenerationMismatch`,
/// because the saved-edit cycle's impact target is minted here as
/// `file.daemon.<digest>` under this generation.
impl crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1
    for CodeIndexSchedulerRegistryV1
{
    fn resolve(
        &self,
        project_root: PathBuf,
    ) -> crate::diagnostics_publication::CodeIndexPublicationIdentityFuture<'_> {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root.canonicalize().ok()?;
            let current = registry.latest_complete_ready(&root).await?;
            let snapshot = current.generation.snapshot();
            Some(
                crate::diagnostics_publication::CodeIndexPublicationIdentityV1::new(
                    current.generation.manifest().generation_id.clone(),
                    current.generation.manifest().seal.sealed_at,
                    snapshot.repository.clone(),
                    snapshot.worktree.clone(),
                    snapshot.reference.clone(),
                    snapshot.source_revision.clone(),
                    snapshot.files.iter().map(|file| {
                        (
                            file.logical_path.clone(),
                            file.file_occurrence_id.clone(),
                            file.content_digest.clone(),
                        )
                    }),
                ),
            )
        })
    }
}

impl crate::code_index::provider::GenerationTestAttributionJoinReadPort
    for CodeIndexSchedulerRegistryV1
{
    fn read_test_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> crate::code_index::provider::GenerationProviderReadV1<
        crate::code_index::test_attribution::GenerationTestJoinV1,
    > {
        let authorities = self
            .test_attribution_authorities
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut matching = authorities
            .values()
            .filter(|(candidate, _)| candidate == generation);
        let Some((_, authority)) = matching.next() else {
            return crate::code_index::provider::GenerationProviderReadV1::new(
                tracedecay_domain::ProviderEvaluationStateV1::Unavailable,
                crate::code_index::provider::GenerationProviderCoverageV1::Unavailable,
                None,
            )
            .unwrap_or_else(|_| panic!("static unavailable attribution read"));
        };
        if matching.next().is_some() {
            return crate::code_index::provider::GenerationProviderReadV1::new(
                tracedecay_domain::ProviderEvaluationStateV1::Unavailable,
                crate::code_index::provider::GenerationProviderCoverageV1::Unavailable,
                None,
            )
            .unwrap_or_else(|_| panic!("static ambiguous attribution read"));
        }
        crate::code_index::provider::GenerationTestAttributionJoinReadPort::read_test_attribution(
            authority, generation,
        )
    }
}

impl crate::application::lsp_runtime::LspCodeIndexProjectionIdentityPort
    for CodeIndexSchedulerRegistryV1
{
    fn current_identity(
        &self,
        project_root: PathBuf,
        document_relative_path: Option<String>,
    ) -> LspRuntimeFuture<
        Result<crate::application::lsp_runtime::LspCodeIndexProjectionIdentity, LspRuntimeFailure>,
    > {
        let registry = self.clone();
        Box::pin(async move {
            let root = project_root
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("lsp-code-index-root-unavailable"))?;
            let current = registry
                .latest_complete_ready(&root)
                .await
                .ok_or_else(|| LspRuntimeFailure::new("lsp-code-index-generation-unavailable"))?;
            let generation = &current.generation;
            let document_content_digest = document_relative_path
                .map(|path| path.replace('\\', "/"))
                .map(|logical_path| {
                    generation
                        .snapshot()
                        .files
                        .iter()
                        .find(|file| file.logical_path == logical_path)
                        .map(|file| file.content_digest.clone())
                        .ok_or_else(|| {
                            LspRuntimeFailure::new("lsp-code-index-document-unavailable")
                        })
                })
                .transpose()?;
            Ok(
                crate::application::lsp_runtime::LspCodeIndexProjectionIdentity {
                    code_generation_id: generation.manifest().generation_id.clone(),
                    snapshot_digest: generation.manifest().snapshot_digest.clone(),
                    invalidation_digest: generation.manifest().invalidation_digest.clone(),
                    snapshot_content_digest: generation.snapshot().content_identity.clone(),
                    document_content_digest,
                },
            )
        })
    }
}

fn feedback_document_logical_path(
    project_root: &Path,
    document_uri: &str,
) -> Result<String, LspRuntimeFailure> {
    let url = url::Url::parse(document_uri)
        .map_err(|_| LspRuntimeFailure::new("feedback-document-uri-invalid"))?;
    if url.scheme() != "file" || url.query().is_some() || url.fragment().is_some() {
        return Err(LspRuntimeFailure::new("feedback-document-uri-invalid"));
    }
    let path = url
        .to_file_path()
        .map_err(|()| LspRuntimeFailure::new("feedback-document-uri-invalid"))?;
    let relative = path
        .strip_prefix(project_root)
        .map_err(|_| LspRuntimeFailure::new("feedback-document-outside-root"))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(LspRuntimeFailure::new("feedback-document-uri-invalid"));
    }
    relative
        .to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| LspRuntimeFailure::new("feedback-document-path-unavailable"))
}

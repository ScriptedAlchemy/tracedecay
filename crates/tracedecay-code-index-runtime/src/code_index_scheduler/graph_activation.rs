use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_graph_db::GraphDbError;
use tracedecay_graph_db::{GraphCancellation, SealedGraphStateDigest};

use super::{
    CodeGraphProjectionError, CodeGraphServingAuthorityV1, CodeIndexProductionErrorV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    DaemonCodeIndexPublicationStoreV1, DurablePublicationPointerV1, LatestCodeTextGenerationV1,
    LatestCompleteCodeIndexV1,
};
use crate::code_graph_seat::{
    CodeGraphReplayBindingV1, CodeGraphSeatLeaseV1, CodeGraphSeatRuntimePortV1,
};
use crate::code_index::graph_projection::CodeGraphProjectionStore;

/// Test-only injected retryable activation failures, keyed by worktree id.
/// The worktree id is unique per test fixture, while generation ids are
/// content-derived and collide across tests that share a fixture template. A
/// positive count makes the memory activation authority fail that many
/// activations with a deadline error so worker retry behavior is observable.
#[cfg(any(test, feature = "test-helpers"))]
fn injected_activation_failures()
-> &'static std::sync::Mutex<std::collections::BTreeMap<String, usize>> {
    static FAILURES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    > = std::sync::OnceLock::new();
    FAILURES.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Test-only injected publication conflicts, keyed by worktree id.
/// A positive count makes the memory activation authority fail that many
/// activations with `GraphDbError::Conflict` so a first-conflict retry that
/// later seats is observable (issue #765).
#[cfg(any(test, feature = "test-helpers"))]
fn injected_activation_conflicts()
-> &'static std::sync::Mutex<std::collections::BTreeMap<String, usize>> {
    static CONFLICTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    > = std::sync::OnceLock::new();
    CONFLICTS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

#[cfg(any(test, feature = "test-helpers"))]
fn injected_activation_attempts()
-> &'static std::sync::Mutex<std::collections::BTreeMap<String, usize>> {
    static ATTEMPTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    > = std::sync::OnceLock::new();
    ATTEMPTS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

#[cfg(any(test, feature = "test-helpers"))]
fn injected_resident_memory_refusals()
-> &'static std::sync::Mutex<std::collections::BTreeSet<String>> {
    static REFUSALS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    REFUSALS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
}

#[cfg(any(test, feature = "test-helpers"))]
fn injected_terminal_activation_failures()
-> &'static std::sync::Mutex<std::collections::BTreeSet<String>> {
    static FAILURES: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    FAILURES.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
}

#[cfg(any(test, feature = "test-helpers"))]
struct InjectedActivationGateStateV1 {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(any(test, feature = "test-helpers"))]
fn injected_activation_gates()
-> &'static std::sync::Mutex<std::collections::BTreeMap<String, Arc<InjectedActivationGateStateV1>>>
{
    static GATES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, Arc<InjectedActivationGateStateV1>>>,
    > = std::sync::OnceLock::new();
    GATES.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

#[cfg(test)]
pub struct InjectedActivationGateV1 {
    state: Arc<InjectedActivationGateStateV1>,
}

#[cfg(test)]
impl InjectedActivationGateV1 {
    pub async fn wait_until_started(&self) {
        self.state.started.notified().await;
    }

    pub fn release(&self) {
        self.state.release.notify_one();
    }
}

#[cfg(test)]
impl Drop for InjectedActivationGateV1 {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
pub fn install_injected_activation_gate(worktree_id: &WorktreeId) -> InjectedActivationGateV1 {
    let state = Arc::new(InjectedActivationGateStateV1 {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    injected_activation_gates()
        .lock()
        .expect("injected activation gate map must not be poisoned")
        .insert(worktree_id.as_str().to_owned(), Arc::clone(&state));
    InjectedActivationGateV1 { state }
}

#[cfg(test)]
pub fn set_injected_activation_failures(worktree_id: &WorktreeId, failures: usize) {
    let mut injected = injected_activation_failures()
        .lock()
        .expect("injected activation failure gate must not be poisoned");
    if failures == 0 {
        injected.remove(worktree_id.as_str());
    } else {
        injected.insert(worktree_id.as_str().to_owned(), failures);
    }
}

#[cfg(test)]
pub fn set_injected_activation_conflicts(worktree_id: &WorktreeId, conflicts: usize) {
    let mut injected = injected_activation_conflicts()
        .lock()
        .expect("injected activation conflict gate must not be poisoned");
    if conflicts == 0 {
        injected.remove(worktree_id.as_str());
    } else {
        injected.insert(worktree_id.as_str().to_owned(), conflicts);
    }
}

#[cfg(test)]
pub fn injected_activation_attempt_count(worktree_id: &WorktreeId) -> usize {
    injected_activation_attempts()
        .lock()
        .expect("injected activation attempt map must not be poisoned")
        .get(worktree_id.as_str())
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
pub fn set_injected_resident_memory_refusal(worktree_id: &WorktreeId, refused: bool) {
    let mut refusals = injected_resident_memory_refusals()
        .lock()
        .expect("injected resident-memory refusal gate must not be poisoned");
    if refused {
        refusals.insert(worktree_id.as_str().to_owned());
    } else {
        refusals.remove(worktree_id.as_str());
    }
}

#[cfg(test)]
pub fn set_injected_terminal_activation_failure(worktree_id: &WorktreeId, failed: bool) {
    let mut failures = injected_terminal_activation_failures()
        .lock()
        .expect("injected terminal activation failure gate must not be poisoned");
    if failed {
        failures.insert(worktree_id.as_str().to_owned());
    } else {
        failures.remove(worktree_id.as_str());
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::expect_used)] // fixture gate: a poisoned injection mutex is a test-harness bug
fn has_injected_resident_memory_refusal(worktree_id: &WorktreeId) -> bool {
    injected_resident_memory_refusals()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(worktree_id.as_str())
}

#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::expect_used)] // fixture gate: a poisoned injection mutex is a test-harness bug
fn take_injected_activation_failure(worktree_id: &WorktreeId) -> bool {
    let mut injected = injected_activation_failures()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match injected.get_mut(worktree_id.as_str()) {
        Some(remaining) if *remaining > 0 => {
            *remaining = remaining.saturating_sub(1);
            true
        }
        _ => false,
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::expect_used)] // fixture gate: a poisoned injection mutex is a test-harness bug
fn take_injected_activation_conflict(worktree_id: &WorktreeId) -> bool {
    let mut injected = injected_activation_conflicts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match injected.get_mut(worktree_id.as_str()) {
        Some(remaining) if *remaining > 0 => {
            *remaining = remaining.saturating_sub(1);
            true
        }
        _ => false,
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::expect_used)] // fixture gate: a poisoned injection mutex is a test-harness bug
fn record_injected_activation_attempt(worktree_id: &WorktreeId) {
    *injected_activation_attempts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(worktree_id.as_str().to_owned())
        .or_insert(0) += 1;
}

#[cfg(any(test, feature = "test-helpers"))]
fn take_injected_terminal_activation_failure(worktree_id: &WorktreeId) -> bool {
    injected_terminal_activation_failures()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(worktree_id.as_str())
}

#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::expect_used)] // fixture gate: a poisoned injection mutex is a test-harness bug
fn take_injected_activation_gate(
    worktree_id: &WorktreeId,
) -> Option<Arc<InjectedActivationGateStateV1>> {
    injected_activation_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(worktree_id.as_str())
}

#[derive(Clone)]
pub enum CodeGraphActivationAuthorityV1 {
    Persistent {
        runtime: Arc<dyn CodeGraphSeatRuntimePortV1>,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
        policy: Arc<AtomicBool>,
    },
    #[cfg(any(test, feature = "test-helpers"))]
    Memory { policy: Arc<AtomicBool> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeGraphActivationPolicyV1 {
    Enabled,
    RefusedByConfiguration,
}

impl CodeGraphActivationPolicyV1 {
    #[hotpath::skip]
    pub const fn from_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::RefusedByConfiguration
        }
    }

    #[hotpath::skip]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl CodeGraphActivationAuthorityV1 {
    fn policy_cell(&self) -> &Arc<AtomicBool> {
        match self {
            Self::Persistent { policy, .. } => policy,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Memory { policy } => policy,
        }
    }

    pub fn update_policy(&self, policy: CodeGraphActivationPolicyV1) {
        self.policy_cell()
            .store(policy.is_enabled(), Ordering::Release);
    }

    pub fn policy(&self) -> CodeGraphActivationPolicyV1 {
        CodeGraphActivationPolicyV1::from_enabled(self.policy_cell().load(Ordering::Acquire))
    }

    /// Validate and seat an already-published revision-7 graph directly from
    /// its durable verified head. `Ok(false)` is an explicit abstention for
    /// non-persistent or disabled authorities; every persistent mismatch is a
    /// typed error so the scheduler can retain pending coverage and replay.
    #[hotpath::measure(future = true, label = "code_graph.activation.recover_verified_head")]
    pub async fn recover_verified_head(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
        latest: LatestCodeTextGenerationV1,
        replay_binding: CodeGraphReplayBindingV1,
        cancellation: Arc<AtomicBool>,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        if self.policy() == CodeGraphActivationPolicyV1::RefusedByConfiguration {
            return Ok(false);
        }
        match self {
            Self::Persistent {
                runtime,
                project_database,
                ..
            } => {
                let generation_id = latest.metadata().manifest().generation_id.clone();
                let retained = hotpath::future!(
                    runtime.retain_code_graph_runtime(
                        project_id.clone(),
                        repository_id.clone(),
                        worktree_id.clone(),
                        latest.metadata().snapshot().reference.clone(),
                        generation_id,
                        Arc::clone(project_database),
                        replay_binding,
                        None,
                    ),
                    label = "code_graph.activation.recover_head.retain_runtime"
                )
                .await
                .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
                retained
                    .sweep_aborted_read_bundle_temporaries()
                    .map_err(|error| {
                        CodeIndexSchedulerErrorV1::GraphActivation(error.to_string())
                    })?;
                let pending_catalog_warm = tokio::task::spawn_blocking(move || {
                    latest.activate_persistent_graph_head(retained, cancellation)
                })
                .await
                .map_err(|error| {
                    CodeIndexSchedulerErrorV1::GraphActivation(format!(
                        "verified graph head activation task failed: {error}"
                    ))
                })??;
                if let Some(pending_catalog_warm) = pending_catalog_warm {
                    drop(tokio::task::spawn_blocking(move || {
                        if let Err(error) = pending_catalog_warm.run() {
                            tracing::warn!(
                                error = %error,
                                "background recovered code graph catalog warm failed"
                            );
                        }
                    }));
                }
                Ok(true)
            }
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Memory { .. } => Ok(false),
        }
    }

    #[hotpath::measure(future = true, label = "code_graph.activation.total")]
    pub async fn activate(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
        latest: LatestCompleteCodeIndexV1,
        replay_binding: CodeGraphReplayBindingV1,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let policy = self.policy();
        if policy == CodeGraphActivationPolicyV1::RefusedByConfiguration {
            let reason = "code graph activation was refused by project configuration";
            latest.refuse_graph_activation(reason);
            return Err(CodeIndexSchedulerErrorV1::GraphActivationRefused(reason));
        }
        // Graph activation consumes the sealed generation directly. Text
        // serving is a separate bounded projection advanced by the mounted
        // scheduler after this generation is seated; requiring it here makes
        // the first partial text pass enter graph-retry backoff before that
        // worker can continue the projection.
        match self {
            Self::Persistent {
                runtime,
                project_database,
                ..
            } => {
                let generation_id = latest.generation().manifest().generation_id.clone();
                let retained = hotpath::future!(
                    runtime.retain_code_graph_runtime(
                        project_id.clone(),
                        repository_id.clone(),
                        worktree_id.clone(),
                        latest.generation().snapshot().reference.clone(),
                        generation_id,
                        Arc::clone(project_database),
                        replay_binding,
                        Some(latest.generation_handle()),
                    ),
                    label = "code_graph.activation.retain_runtime"
                )
                .await
                .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
                retained
                    .sweep_aborted_read_bundle_temporaries()
                    .map_err(|error| {
                        CodeIndexSchedulerErrorV1::GraphActivation(error.to_string())
                    })?;
                let pending_catalog_warm = tokio::task::spawn_blocking(move || {
                    latest.activate_persistent_graph(retained, cancellation)
                })
                .await
                .map_err(|error| {
                    CodeIndexSchedulerErrorV1::GraphActivation(format!(
                        "code graph activation task failed: {error}"
                    ))
                })??;
                if let Some(pending_catalog_warm) = pending_catalog_warm {
                    drop(tokio::task::spawn_blocking(move || {
                        if let Err(error) = pending_catalog_warm.run() {
                            tracing::warn!(
                                error = %error,
                                "background code graph interactive catalog warm failed"
                            );
                        }
                    }));
                }
                Ok(())
            }
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Memory { .. } => {
                if let Some(gate) = take_injected_activation_gate(worktree_id) {
                    gate.started.notify_one();
                    gate.release.notified().await;
                }
                record_injected_activation_attempt(worktree_id);
                if take_injected_terminal_activation_failure(worktree_id) {
                    return Err(CodeIndexSchedulerErrorV1::Identity(
                        "injected terminal graph activation failure".to_owned(),
                    ));
                }
                if has_injected_resident_memory_refusal(worktree_id) {
                    latest.refuse_graph_activation(
                        "code graph activation was refused by the resident-memory policy",
                    );
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(
                        CodeGraphProjectionError::BudgetExhausted {
                            budget: "resident_memory".to_owned(),
                            limit:
                                tracedecay_runtime_core::resident_memory::detected_process_resident_memory_limit_v1()
                                    .get(),
                        },
                    ));
                }
                if take_injected_activation_conflict(worktree_id) {
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(
                        GraphDbError::conflict("publication.prepare.expected_prior_head").into(),
                    ));
                }
                if take_injected_activation_failure(worktree_id) {
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(
                        CodeGraphProjectionError::DeadlineExceeded,
                    ));
                }
                latest.warm_serving_caches();
                Ok(())
            }
        }
    }
}

impl DaemonCodeIndexPublicationStoreV1 {
    pub fn sealed_replay_binding(
        &self,
        generation_id: &tracedecay_domain::CodeGenerationId,
    ) -> Result<CodeGraphReplayBindingV1, CodeIndexPublicationStoreErrorV1> {
        let pointer_bytes = std::fs::read(&self.active_path).map_err(Self::unavailable)?;
        let pointer: DurablePublicationPointerV1 =
            serde_json::from_slice(&pointer_bytes).map_err(|error| {
                Self::unavailable(format!(
                    "active code-generation pointer is corrupt: {error}"
                ))
            })?;
        Self::validate_generation_file(&pointer.generation_file)?;
        if pointer.generation_id != generation_id.as_str() {
            return Err(Self::unavailable(format!(
                "active code-generation pointer names {} instead of {}",
                pointer.generation_id, generation_id
            )));
        }
        let digest = pointer
            .state_digest
            .strip_prefix("sha256:")
            .ok_or_else(|| Self::unavailable("active code-generation digest is not sha256"))?;
        if pointer.generation_file != format!("generation-{digest}.json") {
            return Err(Self::unavailable(
                "active code-generation filename does not match its state digest",
            ));
        }
        Ok(CodeGraphReplayBindingV1 {
            generations_root: self.generations_root.clone(),
            sealed_state_digest: SealedGraphStateDigest::try_from(pointer.state_digest)
                .map_err(Self::unavailable)?,
        })
    }
}

impl CodeIndexWorktreeSchedulerV1 {
    pub fn code_graph_replay_binding(
        &self,
        generation_id: &tracedecay_domain::CodeGenerationId,
    ) -> Result<CodeGraphReplayBindingV1, CodeIndexSchedulerErrorV1> {
        self.publication
            .sealed_replay_binding(generation_id)
            .map_err(|error| CodeIndexProductionErrorV1::Publication(error).into())
    }
}

struct SchedulerGraphCancellation(Arc<AtomicBool>);

impl GraphCancellation for SchedulerGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct PendingInteractiveCatalogWarmV1 {
    retained: Box<dyn CodeGraphSeatLeaseV1 + Send>,
    generation_id: tracedecay_domain::CodeGenerationId,
    store: Arc<CodeGraphProjectionStore>,
    request_cancelled: Arc<AtomicBool>,
    cancellation: Arc<dyn GraphCancellation>,
}

impl PendingInteractiveCatalogWarmV1 {
    #[hotpath::measure(label = "code_graph.catalog.background_warm")]
    fn run(self) -> Result<(), CodeGraphProjectionError> {
        let catalog_loaded = match self
            .retained
            .load_sealed_read_bundle_catalog(&self.request_cancelled)
        {
            Ok(tracedecay_graph_db::SealedReadBundleArtifactStateV1::Loaded {
                artifact,
                bytes,
            }) => match self
                .store
                .install_interactive_catalog_artifact(&bytes, Arc::clone(&self.cancellation))
            {
                Ok(()) => {
                    tracing::info!(
                        generation = %self.generation_id,
                        bytes = artifact.bytes,
                        "code graph interactive catalog loaded from sealed read bundle"
                    );
                    true
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        generation = %self.generation_id,
                        "sealed read bundle catalog failed to install; re-deriving the catalog from the projection"
                    );
                    false
                }
            },
            Ok(tracedecay_graph_db::SealedReadBundleArtifactStateV1::Absent { reason }) => {
                tracing::info!(
                    generation = %self.generation_id,
                    reason = %reason,
                    "no sealed read bundle catalog; re-deriving the catalog from the projection"
                );
                false
            }
            Ok(tracedecay_graph_db::SealedReadBundleArtifactStateV1::Stale { detail }) => {
                tracing::warn!(
                    generation = %self.generation_id,
                    detail = %detail,
                    "sealed read bundle catalog is stale; re-deriving the catalog from the projection"
                );
                false
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    generation = %self.generation_id,
                    "sealed read bundle load failed; re-deriving the catalog from the projection"
                );
                false
            }
        };
        if catalog_loaded {
            return Ok(());
        }
        self.store
            .warm_interactive_catalog_with_cancellation(self.cancellation)
    }
}

impl LatestCodeTextGenerationV1 {
    #[hotpath::measure(label = "code_graph.activation.persistent_head")]
    fn activate_persistent_graph_head(
        &self,
        retained: Box<dyn CodeGraphSeatLeaseV1 + Send>,
        cancellation: Arc<AtomicBool>,
    ) -> Result<Option<PendingInteractiveCatalogWarmV1>, CodeIndexSchedulerErrorV1> {
        let generation_id = self.metadata().manifest().generation_id.clone();
        let snapshot = hotpath::measure_block!(
            "code_graph.activation.validate_verified_head",
            retained
                .recover_verified_snapshot_from_head(Arc::clone(&cancellation))
                .map_err(CodeGraphProjectionError::from)
        )?;
        let store = Arc::new(CodeGraphProjectionStore::from_verified_snapshot(
            snapshot,
            generation_id.clone(),
        )?);
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(SchedulerGraphCancellation(Arc::clone(&cancellation)));
        store.mark_interactive_catalog_warming()?;
        let reader = hotpath::measure_block!("code_graph.activation.head_evidence_reader", {
            store.evidence_reader_with_cancellation(
                &generation_id,
                Some(self.metadata().snapshot().repository.clone()),
                self.source_freshness().map_err(|error| {
                    CodeIndexSchedulerErrorV1::GraphActivation(error.to_string())
                })?,
                Arc::clone(&graph_cancellation),
            )
        })?;
        self.install_graph_serving(
            reader,
            Some(Arc::clone(&store)),
            CodeGraphServingAuthorityV1::Persistent {
                _lease: retained.authority(),
            },
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        Ok(Some(PendingInteractiveCatalogWarmV1 {
            retained,
            generation_id,
            store,
            request_cancelled: cancellation,
            cancellation: graph_cancellation,
        }))
    }
}

impl LatestCompleteCodeIndexV1 {
    #[hotpath::measure(label = "code_graph.activation.persistent")]
    fn activate_persistent_graph(
        &self,
        retained: Box<dyn CodeGraphSeatLeaseV1 + Send>,
        cancellation: Arc<AtomicBool>,
    ) -> Result<Option<PendingInteractiveCatalogWarmV1>, CodeIndexSchedulerErrorV1> {
        let generation_id = self.generation.manifest().generation_id.clone();
        retained
            .sweep_aborted_read_bundle_temporaries()
            .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        let authority = retained.authority();
        let snapshot = hotpath::measure_block!(
            "code_graph.activation.publish_verified_snapshot",
            retained
                .publish_verified_snapshot(&self.generation, Arc::clone(&cancellation))
                .map_err(CodeGraphProjectionError::from)
        )?;
        let store = Arc::new(CodeGraphProjectionStore::from_verified_snapshot(
            snapshot,
            generation_id.clone(),
        )?);
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(SchedulerGraphCancellation(Arc::clone(&cancellation)));
        // Bundle IO and catalog materialization are optional accelerators. The
        // immutable occurrence graph is already verified, so publish it first
        // and keep only catalog-dependent lookups in the typed warming state.
        store.mark_interactive_catalog_warming()?;
        let reader = hotpath::measure_block!("code_graph.activation.evidence_reader", {
            store.evidence_reader_with_cancellation(
                &generation_id,
                Some(self.generation.snapshot().repository.clone()),
                self.source_freshness().map_err(|error| {
                    CodeIndexSchedulerErrorV1::GraphActivation(error.to_string())
                })?,
                Arc::clone(&graph_cancellation),
            )
        })?;
        self.install_graph_serving(
            reader,
            Some(Arc::clone(&store)),
            CodeGraphServingAuthorityV1::Persistent { _lease: authority },
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        let _ = self.generation.test_attribution_authority();
        let _ = self.record_index();
        Ok(Some(PendingInteractiveCatalogWarmV1 {
            retained,
            generation_id,
            store,
            request_cancelled: cancellation,
            cancellation: graph_cancellation,
        }))
    }
}

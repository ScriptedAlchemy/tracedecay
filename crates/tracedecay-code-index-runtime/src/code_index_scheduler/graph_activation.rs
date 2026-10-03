use std::sync::{
    Arc, PoisonError, RwLock,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_domain::{CodeGenerationId, ProjectId, RepositoryId, WorktreeId, sha256_hex_suffix};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_graph_db::GraphDbError;
use tracedecay_graph_db::{GraphCancellation, SealedGraphStateDigest};

use super::{
    CodeGraphServingAuthorityV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    DaemonCodeIndexPublicationStoreV1, LatestCodeTextGenerationV1, LatestCompleteCodeIndexV1,
};
use crate::code_graph_seat::{
    CodeGraphBuildAdmissionV1, CodeGraphReplayBindingV1, CodeGraphSeatLeaseV1,
    CodeGraphSeatRuntimePortV1,
};
use crate::code_index::graph_projection::{CodeGraphProjectionError, CodeGraphProjectionStore};
use crate::code_index::production::{CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1};

/// Test-only injected retryable activation failures, keyed by worktree id.
/// The worktree id is unique per test fixture, while generation ids are
/// content-derived and collide across tests that share a fixture template. A
/// positive count makes the memory activation authority fail that many
/// activations with an unavailable graph runtime so worker retry behavior is
/// observable.
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

/// Test-only publications that run out their background budget, keyed by
/// worktree id: the memory activation authority answers every activation of
/// such a worktree with the deadline the persistent build reports.
#[cfg(any(test, feature = "test-helpers"))]
fn injected_publication_deadlines() -> &'static std::sync::Mutex<std::collections::BTreeSet<String>>
{
    static DEADLINES: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    DEADLINES.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
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
pub fn set_injected_publication_deadline(worktree_id: &WorktreeId, exceeded: bool) {
    let mut deadlines = injected_publication_deadlines()
        .lock()
        .expect("injected publication deadline gate must not be poisoned");
    if exceeded {
        deadlines.insert(worktree_id.as_str().to_owned());
    } else {
        deadlines.remove(worktree_id.as_str());
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
fn has_injected_publication_deadline(worktree_id: &WorktreeId) -> bool {
    injected_publication_deadlines()
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

/// The typed reason a generation reports while its native graph is refused by
/// the measured-RSS admission watermark. Shared by the persistent publication
/// path and the injected test refusal so status and tests name one verdict.
pub(crate) const RESIDENT_MEMORY_GRAPH_REFUSAL_REASON: &str =
    "code graph activation was refused by the resident-memory policy";

/// The typed reason a generation reports once its native graph publication
/// ran out its background budget. The build is a pure function of the sealed
/// generation, so the verdict stands until a new generation seals.
pub(crate) const GRAPH_PUBLICATION_DEADLINE_REASON: &str = "the sealed code graph publication \
     exceeded its background budget; this generation serves exact and lexical without a \
     native graph until the next generation seals";

/// Whether a projection error is the resident-memory budget refusal that
/// [`CodeIndexSchedulerErrorV1::is_graph_activation_refusal`] recognizes.
pub(crate) fn is_resident_memory_refusal(error: &CodeGraphProjectionError) -> bool {
    matches!(
        error,
        CodeGraphProjectionError::BudgetExhausted { budget, .. }
            if budget == tracedecay_graph_db::GraphBudgetKind::ResidentMemory.as_str()
    )
}

/// Record a spent publication budget on the generation, so status reports
/// the typed refusal and no later pass replays the same build.
fn refuse_spent_publication_budget(
    text: &LatestCodeTextGenerationV1,
    error: &CodeGraphProjectionError,
) {
    if matches!(error, CodeGraphProjectionError::DeadlineExceeded) {
        text.refuse_graph_activation(GRAPH_PUBLICATION_DEADLINE_REASON);
    }
}

#[derive(Clone)]
pub enum CodeGraphActivationAuthorityV1 {
    Persistent {
        runtime: Arc<dyn CodeGraphSeatRuntimePortV1>,
        project_database: Arc<tracedecay_runtime_core::db::Database>,
        policy: Arc<AtomicBool>,
        /// The generation whose durable graph this worktree last seated or
        /// published. It stays a retention root until a successor graph
        /// replaces it: the next refresh builds over its sealed graph, and
        /// a yielded engine reopens from it.
        seated: Arc<RwLock<Option<CodeGenerationId>>>,
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
    pub const fn from_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::RefusedByConfiguration
        }
    }

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

    /// The generation whose durable graph this worktree last seated.
    pub fn seated_graph_generation(&self) -> Option<CodeGenerationId> {
        match self {
            Self::Persistent { seated, .. } => seated
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Memory { .. } => None,
        }
    }

    fn record_seated(seated: &RwLock<Option<CodeGenerationId>>, generation: CodeGenerationId) {
        *seated.write().unwrap_or_else(PoisonError::into_inner) = Some(generation);
    }

    /// Validate and seat an already-published revision-7 graph directly from
    /// its durable verified head. `Ok(false)` is an explicit abstention for
    /// non-persistent or disabled authorities; every persistent mismatch is a
    /// typed error so the scheduler can retain pending coverage and replay.
    #[tracing::instrument(
        name = "code_graph.activation.recover_verified_head",
        level = "trace",
        skip_all
    )]
    pub async fn recover_verified_head(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
        latest: LatestCodeTextGenerationV1,
        replay_binding: CodeGraphReplayBindingV1,
        cancellation: Arc<AtomicBool>,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.recover_verified_generation_inner(
            project_id,
            repository_id,
            worktree_id,
            latest,
            replay_binding,
            cancellation,
            true,
        )
        .await
    }

    pub async fn recover_verified_generation(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
        latest: LatestCodeTextGenerationV1,
        replay_binding: CodeGraphReplayBindingV1,
        cancellation: Arc<AtomicBool>,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        self.recover_verified_generation_inner(
            project_id,
            repository_id,
            worktree_id,
            latest,
            replay_binding,
            cancellation,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn recover_verified_generation_inner(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
        latest: LatestCodeTextGenerationV1,
        replay_binding: CodeGraphReplayBindingV1,
        cancellation: Arc<AtomicBool>,
        require_current_head: bool,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        if self.policy() == CodeGraphActivationPolicyV1::RefusedByConfiguration {
            return Ok(false);
        }
        match self {
            Self::Persistent {
                runtime,
                project_database,
                seated,
                ..
            } => {
                let generation_id = latest.metadata().manifest().generation_id.clone();
                let retained = tracing::Instrument::instrument(
                    runtime.retain_code_graph_runtime(
                        project_id.clone(),
                        repository_id.clone(),
                        worktree_id.clone(),
                        latest.metadata().snapshot().reference.clone(),
                        generation_id.clone(),
                        Arc::clone(project_database),
                        replay_binding,
                    ),
                    tracing::trace_span!("code_graph.activation.recover_head.retain_runtime"),
                )
                .await
                .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
                let pending_catalog_warm = tokio::task::spawn_blocking(move || {
                    latest.activate_persistent_graph_generation(
                        retained,
                        cancellation,
                        require_current_head,
                    )
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
                // A historical generation read seats beside the live graph
                // and never replaces it.
                if require_current_head {
                    Self::record_seated(seated, generation_id);
                }
                Ok(true)
            }
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Memory { .. } => Ok(false),
        }
    }

    /// Publishes a sealed generation's graph head straight from its segments
    /// on disk, without decoding the generation.
    ///
    /// Graph prepare runs this before the serving decode so the corpus-sized
    /// graph build and the decoded generation are never resident together;
    /// the activation that follows recovers the head this published instead
    /// of building it. `Ok(false)` abstains for a refused policy or a
    /// non-persistent authority.
    #[tracing::instrument(
        name = "code_graph.activation.publish_sealed",
        level = "trace",
        skip_all
    )]
    pub async fn publish_sealed_graph(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
        latest: &LatestCodeTextGenerationV1,
        replay_binding: CodeGraphReplayBindingV1,
        admission: Option<Arc<dyn CodeGraphBuildAdmissionV1>>,
        cancellation: Arc<AtomicBool>,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        if self.policy() == CodeGraphActivationPolicyV1::RefusedByConfiguration {
            return Ok(false);
        }
        match self {
            Self::Persistent {
                runtime,
                project_database,
                seated,
                ..
            } => {
                let retained = tracing::Instrument::instrument(
                    runtime.retain_code_graph_runtime(
                        project_id.clone(),
                        repository_id.clone(),
                        worktree_id.clone(),
                        latest.metadata().snapshot().reference.clone(),
                        latest.metadata().manifest().generation_id.clone(),
                        Arc::clone(project_database),
                        replay_binding,
                    ),
                    tracing::trace_span!("code_graph.activation.publish_sealed.retain_runtime"),
                )
                .await
                .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
                tokio::task::spawn_blocking(move || match admission {
                    Some(admission) => retained
                        .publish_verified_snapshot_admitted(cancellation, admission)
                        .map(drop),
                    None => retained.publish_verified_snapshot(cancellation).map(drop),
                })
                .await
                .map_err(|error| {
                    CodeIndexSchedulerErrorV1::GraphActivation(format!(
                        "sealed graph publication task failed: {error}"
                    ))
                })?
                .map_err(CodeGraphProjectionError::from)
                .inspect_err(|error| refuse_spent_publication_budget(latest, error))?;
                Self::record_seated(seated, latest.metadata().manifest().generation_id.clone());
                Ok(true)
            }
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Memory { .. } => Ok(false),
        }
    }

    /// Seats `latest`'s graph. `predecessor` is the graph store of the
    /// generation `latest` replaces, when one is serving: a layered
    /// generation carries its interactive catalog from it.
    #[tracing::instrument(name = "code_graph.activation.total", level = "trace", skip_all)]
    pub async fn activate(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
        latest: LatestCompleteCodeIndexV1,
        predecessor: Option<Arc<CodeGraphProjectionStore>>,
        replay_binding: CodeGraphReplayBindingV1,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let policy = self.policy();
        if policy == CodeGraphActivationPolicyV1::RefusedByConfiguration {
            let reason = "code graph activation was refused by project configuration";
            latest.refuse_graph_activation(reason);
            return Err(CodeIndexSchedulerErrorV1::GraphActivationRefused(reason));
        }
        // Graph activation consumes the sealed generation directly. The
        // mounted scheduler therefore admits a full replay only after text
        // projection is ready; verified-head recovery has its own path above
        // and does not replay the sealed source.
        match self {
            Self::Persistent {
                runtime,
                project_database,
                seated,
                ..
            } => {
                let generation_id = latest.generation().manifest().generation_id.clone();
                let retained = tracing::Instrument::instrument(
                    runtime.retain_code_graph_runtime(
                        project_id.clone(),
                        repository_id.clone(),
                        worktree_id.clone(),
                        latest.generation().snapshot().reference.clone(),
                        generation_id.clone(),
                        Arc::clone(project_database),
                        replay_binding,
                    ),
                    tracing::trace_span!("code_graph.activation.retain_runtime"),
                )
                .await
                .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
                let pending_catalog_warm = tokio::task::spawn_blocking(move || {
                    latest.activate_persistent_graph(retained, predecessor, cancellation)
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
                Self::record_seated(seated, generation_id);
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
                    latest.refuse_graph_activation(RESIDENT_MEMORY_GRAPH_REFUSAL_REASON);
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(
                        CodeGraphProjectionError::BudgetExhausted {
                            budget: tracedecay_graph_db::GraphBudgetKind::ResidentMemory
                                .as_str()
                                .to_owned(),
                            limit:
                                tracedecay_runtime_core::resident_memory::detected_process_resident_memory_limit_v1()
                                    .get(),
                        },
                    ));
                }
                if has_injected_publication_deadline(worktree_id) {
                    let deadline = CodeGraphProjectionError::DeadlineExceeded;
                    refuse_spent_publication_budget(&latest.text, &deadline);
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(deadline));
                }
                if take_injected_activation_conflict(worktree_id) {
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(
                        GraphDbError::conflict("publication.prepare.expected_prior_head").into(),
                    ));
                }
                if take_injected_activation_failure(worktree_id) {
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(
                        CodeGraphProjectionError::Unavailable(
                            "injected unavailable graph runtime".to_owned(),
                        ),
                    ));
                }
                latest.prewarm_serving_derivations();
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
        let pointer = self.read_publication_pointer()?.ok_or_else(|| {
            Self::unavailable("code-generation replay binding has no publication pointer")
        })?;
        let entry = pointer
            .generation_index
            .iter()
            .find(|entry| entry.generation_id == generation_id.as_str())
            .ok_or_else(|| {
                Self::unavailable(format!(
                    "code generation {generation_id} is not retained in the publication index"
                ))
            })?;
        let digest = sha256_hex_suffix(&entry.state_digest)
            .ok_or_else(|| Self::unavailable("code-generation replay digest is not sha256"))?;
        if entry.generation_file != format!("generation-{digest}.json") {
            return Err(Self::unavailable(
                "code-generation replay filename does not match its state digest",
            ));
        }
        Ok(CodeGraphReplayBindingV1 {
            generations_root: self.generations_root.clone(),
            sealed_state_digest: SealedGraphStateDigest::try_from(entry.state_digest.clone())
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

    /// Whether the durable publication pointer names `generation_id`. Graph heads
    /// only follow the pointer, so a head on any other generation is older than it.
    pub fn names_active_publication(
        &self,
        generation_id: &tracedecay_domain::CodeGenerationId,
    ) -> Result<bool, CodeIndexSchedulerErrorV1> {
        Ok(self
            .publication
            .read_publication_pointer()
            .map_err(CodeIndexProductionErrorV1::Publication)?
            .is_some_and(|pointer| pointer.generation_id == generation_id.as_str()))
    }
}

struct SchedulerGraphCancellation(Arc<AtomicBool>);

impl GraphCancellation for SchedulerGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct PendingInteractiveCatalogWarmV1 {
    store: Arc<CodeGraphProjectionStore>,
    /// The graph store this generation replaced, whose ready catalog a
    /// layered generation carries instead of scanning its projection.
    predecessor: Option<Arc<CodeGraphProjectionStore>>,
    cancellation: Arc<dyn GraphCancellation>,
    owner: LatestCodeTextGenerationV1,
}

impl PendingInteractiveCatalogWarmV1 {
    /// The first warm settles the owner's graph either way, so the
    /// predecessor it held is released here and never outlives it.
    #[tracing::instrument(name = "code_graph.catalog.background_warm", level = "trace", skip_all)]
    fn run(self) -> Result<(), CodeGraphProjectionError> {
        let warmed = self.store.warm_interactive_catalog_with_cancellation(
            self.predecessor.as_deref(),
            self.cancellation,
        );
        self.owner.release_graph_predecessor();
        warmed
    }
}

impl LatestCodeTextGenerationV1 {
    #[tracing::instrument(
        name = "code_graph.activation.persistent_generation",
        level = "trace",
        skip_all
    )]
    fn activate_persistent_graph_generation(
        &self,
        retained: Box<dyn CodeGraphSeatLeaseV1 + Send>,
        cancellation: Arc<AtomicBool>,
        require_current_head: bool,
    ) -> Result<Option<PendingInteractiveCatalogWarmV1>, CodeIndexSchedulerErrorV1> {
        let generation_id = self.metadata().manifest().generation_id.clone();
        let snapshot = {
            let _span =
                tracing::trace_span!("code_graph.activation.validate_verified_head").entered();
            if require_current_head {
                retained.recover_verified_snapshot_from_head(Arc::clone(&cancellation))
            } else {
                retained.recover_verified_generation(Arc::clone(&cancellation))
            }
            .map_err(CodeGraphProjectionError::from)
        }?;
        let store = Arc::new(CodeGraphProjectionStore::from_verified_snapshot(
            snapshot,
            generation_id.clone(),
        )?);
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(SchedulerGraphCancellation(Arc::clone(&cancellation)));
        store.mark_interactive_catalog_warming()?;
        store.warm_serving_engine()?;
        let reader = {
            let _span =
                tracing::trace_span!("code_graph.activation.head_evidence_reader").entered();
            {
                store.evidence_reader_with_cancellation(
                    &generation_id,
                    Some(self.metadata().snapshot().repository.clone()),
                    self.source_freshness().map_err(|error| {
                        CodeIndexSchedulerErrorV1::GraphActivation(error.to_string())
                    })?,
                    Arc::clone(&graph_cancellation),
                )
            }
        }?;
        self.install_graph_serving(
            reader,
            Some(Arc::clone(&store)),
            CodeGraphServingAuthorityV1::Persistent {
                _lease: retained.authority(),
            },
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        // A recovered head has no in-memory predecessor to carry from.
        Ok(Some(PendingInteractiveCatalogWarmV1 {
            store,
            predecessor: None,
            cancellation: graph_cancellation,
            owner: self.clone(),
        }))
    }
}

impl LatestCompleteCodeIndexV1 {
    #[tracing::instrument(name = "code_graph.activation.persistent", level = "trace", skip_all)]
    fn activate_persistent_graph(
        &self,
        retained: Box<dyn CodeGraphSeatLeaseV1 + Send>,
        predecessor: Option<Arc<CodeGraphProjectionStore>>,
        cancellation: Arc<AtomicBool>,
    ) -> Result<Option<PendingInteractiveCatalogWarmV1>, CodeIndexSchedulerErrorV1> {
        let generation_id = self.generation.manifest().generation_id.clone();
        let authority = retained.authority();
        let snapshot = {
            let _span =
                tracing::trace_span!("code_graph.activation.publish_verified_snapshot").entered();
            retained
                .publish_verified_snapshot(Arc::clone(&cancellation))
                .map_err(CodeGraphProjectionError::from)
                .inspect_err(|error| {
                    // The publication stopped at the measured-RSS watermark.
                    // Record the typed refusal before the error reaches the
                    // scheduler so status reports `refused` with its reason
                    // instead of a `pending` graph that will never seat;
                    // exact and lexical serving stay installed.
                    if is_resident_memory_refusal(error) {
                        self.refuse_graph_activation(RESIDENT_MEMORY_GRAPH_REFUSAL_REASON);
                    }
                    refuse_spent_publication_budget(&self.text, error);
                })
        }?;
        let store = Arc::new(CodeGraphProjectionStore::from_verified_snapshot(
            snapshot,
            generation_id.clone(),
        )?);
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(SchedulerGraphCancellation(Arc::clone(&cancellation)));
        // The immutable occurrence graph is already verified, so publish it
        // first and keep only catalog-dependent lookups in the typed warming
        // state while the catalog is derived from it in the background.
        store.mark_interactive_catalog_warming()?;
        store.warm_serving_engine()?;
        let reader = {
            let _span = tracing::trace_span!("code_graph.activation.evidence_reader").entered();
            {
                store.evidence_reader_with_cancellation(
                    &generation_id,
                    Some(self.generation.snapshot().repository.clone()),
                    self.source_freshness().map_err(|error| {
                        CodeIndexSchedulerErrorV1::GraphActivation(error.to_string())
                    })?,
                    Arc::clone(&graph_cancellation),
                )
            }
        }?;
        self.install_graph_serving(
            reader,
            Some(Arc::clone(&store)),
            CodeGraphServingAuthorityV1::Persistent { _lease: authority },
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        let _ = self.generation.test_attribution_authority();
        let _ = self.record_index();
        Ok(Some(PendingInteractiveCatalogWarmV1 {
            store,
            predecessor,
            cancellation: graph_cancellation,
            owner: self.text.clone(),
        }))
    }
}

use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};
use tracedecay_graph_db::{GraphCancellation, SealedGraphStateDigest};

use super::{
    CodeGraphProjectionError, CodeGraphServingAuthorityV1, CodeIndexProductionErrorV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    DaemonCodeIndexPublicationStoreV1, DurablePublicationPointerV1, LatestCompleteCodeIndexV1,
};
use crate::code_index::graph_projection::CodeGraphProjectionStore;

/// Test-only injected retryable activation failures, keyed by worktree id.
/// The worktree id is unique per test fixture, while generation ids are
/// content-derived and collide across tests that share a fixture template. A
/// positive count makes the memory activation authority fail that many
/// activations with a deadline error so worker retry behavior is observable.
#[cfg(test)]
fn injected_activation_failures()
-> &'static std::sync::Mutex<std::collections::BTreeMap<String, usize>> {
    static FAILURES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    > = std::sync::OnceLock::new();
    FAILURES.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

#[cfg(test)]
fn injected_resident_memory_refusals()
-> &'static std::sync::Mutex<std::collections::BTreeSet<String>> {
    static REFUSALS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<String>>> =
        std::sync::OnceLock::new();
    REFUSALS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeSet::new()))
}

#[cfg(test)]
pub(super) fn set_injected_activation_failures(worktree_id: &WorktreeId, failures: usize) {
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
pub(super) fn set_injected_resident_memory_refusal(worktree_id: &WorktreeId, refused: bool) {
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
fn has_injected_resident_memory_refusal(worktree_id: &WorktreeId) -> bool {
    injected_resident_memory_refusals()
        .lock()
        .expect("injected resident-memory refusal gate must not be poisoned")
        .contains(worktree_id.as_str())
}

#[cfg(test)]
fn take_injected_activation_failure(worktree_id: &WorktreeId) -> bool {
    let mut injected = injected_activation_failures()
        .lock()
        .expect("injected activation failure gate must not be poisoned");
    match injected.get_mut(worktree_id.as_str()) {
        Some(remaining) if *remaining > 0 => {
            *remaining = remaining.saturating_sub(1);
            true
        }
        _ => false,
    }
}

#[derive(Clone)]
pub(super) enum CodeGraphActivationAuthorityV1 {
    Persistent {
        runtime:
            Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
        project_database: Arc<crate::db::Database>,
        policy: Arc<AtomicBool>,
    },
    #[cfg(test)]
    Memory { policy: Arc<AtomicBool> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::daemon) enum CodeGraphActivationPolicyV1 {
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

    pub(super) const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl CodeGraphActivationAuthorityV1 {
    fn policy_cell(&self) -> &Arc<AtomicBool> {
        match self {
            Self::Persistent { policy, .. } => policy,
            #[cfg(test)]
            Self::Memory { policy } => policy,
        }
    }

    pub(super) fn update_policy(&self, policy: CodeGraphActivationPolicyV1) {
        self.policy_cell()
            .store(policy.is_enabled(), Ordering::Release);
    }

    pub(super) fn policy(&self) -> CodeGraphActivationPolicyV1 {
        CodeGraphActivationPolicyV1::from_enabled(self.policy_cell().load(Ordering::Acquire))
    }

    #[hotpath::measure]
    pub(super) async fn activate(
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
        let text_latest = latest.clone();
        tokio::task::spawn_blocking(move || text_latest.activate_text_serving())
            .await
            .map_err(|error| {
                CodeIndexSchedulerErrorV1::GraphActivation(format!(
                    "code-index text activation task failed: {error}"
                ))
            })?
            .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        match self {
            Self::Persistent {
                runtime,
                project_database,
                ..
            } => {
                let generation_id = latest.generation().manifest().generation_id.clone();
                let retained = runtime
                    .retain_code_graph_runtime(
                        project_id.clone(),
                        repository_id.clone(),
                        worktree_id.clone(),
                        latest.generation().snapshot().reference.clone(),
                        generation_id,
                        Arc::clone(project_database),
                        replay_binding,
                        Some(latest.generation_handle()),
                    )
                    .await
                    .map_err(|error| {
                        CodeIndexSchedulerErrorV1::GraphActivation(error.to_string())
                    })?;
                tokio::task::spawn_blocking(move || {
                    latest.activate_persistent_graph(retained, cancellation)
                })
                .await
                .map_err(|error| {
                    CodeIndexSchedulerErrorV1::GraphActivation(format!(
                        "code graph activation task failed: {error}"
                    ))
                })?
            }
            #[cfg(test)]
            Self::Memory { .. } => {
                if has_injected_resident_memory_refusal(worktree_id) {
                    latest.refuse_graph_activation(
                        "code graph activation was refused by the resident-memory policy",
                    );
                    return Err(CodeIndexSchedulerErrorV1::GraphProjection(
                        CodeGraphProjectionError::BudgetExhausted {
                            budget: "resident_memory".to_owned(),
                            limit: tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1.get(),
                        },
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodeGraphReplayBindingV1 {
    pub generations_root: PathBuf,
    pub sealed_state_digest: SealedGraphStateDigest,
}

impl DaemonCodeIndexPublicationStoreV1 {
    pub(super) fn sealed_replay_binding(
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
            return Err(Self::unavailable(
                "active code-generation pointer names a different generation",
            ));
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
    pub(super) fn code_graph_replay_binding(
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

impl LatestCompleteCodeIndexV1 {
    pub(super) fn activate_persistent_graph(
        &self,
        retained: crate::daemon::store_runtime::session_registry::RetainedCodeGraphRuntimeV1,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), CodeIndexSchedulerErrorV1> {
        let generation_id = self.generation.manifest().generation_id.clone();
        let authority = retained.authority();
        let snapshot = retained
            .publish_verified_snapshot(&self.generation, Arc::clone(&cancellation))
            .map_err(CodeGraphProjectionError::from)?;
        let store = Arc::new(CodeGraphProjectionStore::from_verified_snapshot(
            snapshot,
            generation_id.clone(),
        )?);
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(SchedulerGraphCancellation(Arc::clone(&cancellation)));
        store.warm_interactive_catalog_with_cancellation(Arc::clone(&graph_cancellation))?;
        let reader = store.evidence_reader_with_cancellation(
            &generation_id,
            Some(self.generation.snapshot().repository.clone()),
            self.source_freshness()
                .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?,
            graph_cancellation,
        )?;
        self.install_graph_serving(
            reader,
            Some(Arc::clone(&store)),
            CodeGraphServingAuthorityV1::Persistent { _lease: authority },
        )
        .map_err(|error| CodeIndexSchedulerErrorV1::GraphActivation(error.to_string()))?;
        let _ = self.generation.test_attribution_authority();
        let _ = self.record_index();
        Ok(())
    }
}

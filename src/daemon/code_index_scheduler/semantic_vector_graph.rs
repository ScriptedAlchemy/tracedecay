//! Daemon implementation of the semantic-vector graph provider port.
//!
//! The semantic runtime lives in `tracedecay-usecases` and cannot see daemon
//! session-registry types, so this adapter resolves the mounted worktree's
//! repository/worktree identity and retains the code-graph runtime that owns
//! the durable semantic-vector projection
//! (docs/plans/tracedecay-v2/39-embedded-grafeo-graph-database.md Task 4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::ProjectId;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_store::RetainedGraphStoreLeaseV1;

use crate::application::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticRuntimeFuture, SemanticVectorGraphErrorV1,
    SemanticVectorGraphProviderV1,
};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;

use super::registry::SchedulerGraphCancellationV1;
use super::{CodeIndexSchedulerRegistryV1, registry::CodeIndexServingScopeV1};

/// Process-local registry so daemon maintenance and Doctor can resolve the
/// mounted project's vector graph without threading the provider through
/// every retention call stack. Registered unconditionally at code-index
/// mount, so retention semantics hold even when the semantic runtime itself
/// is not configured (vectors published earlier still pin their sources).
fn project_vector_graph_providers()
-> &'static Mutex<BTreeMap<PathBuf, Arc<dyn SemanticVectorGraphProviderV1>>> {
    static PROVIDERS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<dyn SemanticVectorGraphProviderV1>>>> =
        OnceLock::new();
    PROVIDERS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn provider_key(project_root: &Path) -> PathBuf {
    project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf())
}

pub(crate) fn register_project_semantic_vector_graph_provider(
    project_root: &Path,
    provider: Arc<dyn SemanticVectorGraphProviderV1>,
) {
    project_vector_graph_providers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(provider_key(project_root), provider);
}

pub(crate) fn unregister_project_semantic_vector_graph_provider(project_root: &Path) {
    project_vector_graph_providers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&provider_key(project_root));
}

pub(crate) fn project_semantic_vector_graph_provider(
    project_root: &Path,
) -> Option<Arc<dyn SemanticVectorGraphProviderV1>> {
    project_vector_graph_providers()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&provider_key(project_root))
        .cloned()
}

/// Resolve semantic-vector graph runtimes for one mounted project.
pub(crate) struct DaemonSemanticVectorGraphProviderV1 {
    project_id: ProjectId,
    project_root: PathBuf,
    schedulers: CodeIndexSchedulerRegistryV1,
    runtime: Arc<DaemonSessionRuntimeRegistryV1>,
}

impl DaemonSemanticVectorGraphProviderV1 {
    pub(crate) fn new(
        project_id: ProjectId,
        project_root: PathBuf,
        schedulers: CodeIndexSchedulerRegistryV1,
        runtime: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Self {
        Self {
            project_id,
            project_root,
            schedulers,
            runtime,
        }
    }

    async fn serving_scope(&self) -> Result<CodeIndexServingScopeV1, SemanticVectorGraphErrorV1> {
        self.schedulers
            .serving_code_scope(&self.project_root)
            .await
            .ok_or_else(|| {
                SemanticVectorGraphErrorV1::Unavailable(
                    "code-index worktree is not mounted for this project".to_owned(),
                )
            })
    }

    async fn retain(
        &self,
        scope: &CodeIndexServingScopeV1,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1> {
        let retained = self
            .runtime
            .retain_code_graph_runtime(
                self.project_id.clone(),
                scope.repository_id.clone(),
                scope.worktree_id.clone(),
                generation.snapshot().reference.clone(),
                generation.manifest().generation_id.clone(),
                Arc::clone(&scope.shutting_down),
            )
            .await
            .map_err(|error| SemanticVectorGraphErrorV1::Rejected(error.to_string()))?;
        let cancellation: Arc<dyn GraphCancellation> = Arc::new(SchedulerGraphCancellationV1 {
            shutting_down: Arc::clone(&scope.shutting_down),
        });
        let authority: Arc<dyn RetainedGraphStoreLeaseV1> = retained.authority;
        Ok(RetainedSemanticVectorGraphV1::new(
            retained.database,
            cancellation,
            authority,
        ))
    }
}

impl SemanticVectorGraphProviderV1 for DaemonSemanticVectorGraphProviderV1 {
    fn graph_for_generation<'a>(
        &'a self,
        generation: &'a CodeIndexPublishedGenerationV1,
    ) -> SemanticRuntimeFuture<'a, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        Box::pin(async move {
            let scope = self.serving_scope().await?;
            self.retain(&scope, generation).await
        })
    }

    fn graph_for_current(
        &self,
    ) -> SemanticRuntimeFuture<'_, Result<RetainedSemanticVectorGraphV1, SemanticVectorGraphErrorV1>>
    {
        Box::pin(async move {
            let scope = self.serving_scope().await?;
            let generation = scope.serving_generation.clone().ok_or_else(|| {
                SemanticVectorGraphErrorV1::Unavailable(
                    "no code generation is currently serving for this project".to_owned(),
                )
            })?;
            self.retain(&scope, generation.as_ref()).await
        })
    }
}

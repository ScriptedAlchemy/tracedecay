use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use tracedecay_domain::{CodeGenerationId, RefId, RepositoryId, WorktreeId};
use tracedecay_graph_db::{GraphCancellation, GraphDb, GraphDbRegistration};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalCodeGraphStoreLeaseV1, StoreRuntimeKey,
};
use tracedecay_store::{CodeShardScopeV1, ProjectId, RetainedGraphStoreLeaseV1, StoreShardIdV1};

use super::{DaemonSessionRuntimeRegistryV1, Result, session_registry_error};

const GRAPH_OPEN_DEADLINE: Duration = Duration::from_secs(30);

struct AtomicGraphCancellationV1 {
    cancelled: Arc<AtomicBool>,
}

impl AtomicGraphCancellationV1 {
    fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self { cancelled }
    }
}

impl GraphCancellation for AtomicGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

pub(crate) struct RetainedCodeGraphRuntimeV1 {
    pub(crate) database: Arc<GraphDb>,
    pub(crate) authority: Arc<CanonicalCodeGraphStoreLeaseV1>,
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn retain_code_graph_runtime(
        &self,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        reference: Option<RefId>,
        generation_id: CodeGenerationId,
        request_cancelled: Arc<AtomicBool>,
    ) -> Result<RetainedCodeGraphRuntimeV1> {
        let project_shard = StoreShardIdV1::project(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id.clone(),
        );
        let code_scope = match reference {
            Some(ref_id) => CodeShardScopeV1::Branch {
                worktree_id,
                ref_id,
            },
            None => CodeShardScopeV1::Worktree { worktree_id },
        };
        let code_shard = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
            repository_id,
            code_scope,
        );
        let authority = self
            .registry
            .retain_code_graph_store(
                StoreRuntimeKey::new(project_shard, self.incarnation),
                code_shard,
                generation_id,
            )
            .await
            .map_err(|failure| {
                session_registry_error("retain exact code graph authority", format!("{failure:?}"))
            })?;
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = authority.clone();
        let registration = GraphDbRegistration {
            authority_lease,
            cancellation: Arc::new(AtomicGraphCancellationV1::new(request_cancelled)),
            lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.graph_lifecycle_cancelled,
            ))),
            deadline: Instant::now() + GRAPH_OPEN_DEADLINE,
        };
        let graph_registry = self.graph_registry.clone();
        let database = tokio::task::spawn_blocking(move || graph_registry.resolve(registration))
            .await
            .map_err(|error| {
                session_registry_error("join code graph runtime open", error.to_string())
            })?
            .map_err(|error| {
                session_registry_error("open code graph runtime", error.to_string())
            })?;
        Ok(RetainedCodeGraphRuntimeV1 {
            database,
            authority,
        })
    }

    /// Retains the daemon-owned native relation graph for one exact session
    /// shard and opens it through the shared graph registry.
    pub(crate) async fn retain_session_relation_graph_runtime(
        &self,
        shard_id: StoreShardIdV1,
    ) -> Result<Arc<GraphDb>> {
        let authority = self
            .registry
            .retain_graph_store(StoreRuntimeKey::new(shard_id, self.incarnation))
            .await
            .map_err(|failure| {
                session_registry_error(
                    "retain exact session relation graph authority",
                    format!("{failure:?}"),
                )
            })?;
        let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = authority;
        let registration = GraphDbRegistration {
            authority_lease,
            cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.graph_lifecycle_cancelled,
            ))),
            lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
                &self.graph_lifecycle_cancelled,
            ))),
            deadline: Instant::now() + GRAPH_OPEN_DEADLINE,
        };
        let graph_registry = self.graph_registry.clone();
        tokio::task::spawn_blocking(move || graph_registry.resolve(registration))
            .await
            .map_err(|error| {
                session_registry_error("join session relation graph open", error.to_string())
            })?
            .map_err(|error| {
                session_registry_error("open session relation graph runtime", error.to_string())
            })
    }
}

impl Drop for DaemonSessionRuntimeRegistryV1 {
    fn drop(&mut self) {
        self.graph_lifecycle_cancelled
            .store(true, Ordering::Release);
    }
}

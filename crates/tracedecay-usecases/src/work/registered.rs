use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::store_runtime::{
    VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1,
};

#[derive(Clone)]
pub struct RegisteredWorkTopologyV1 {
    source: tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    runtime: VerifiedGraphRuntimeWeakProxyV1,
}

impl RegisteredWorkTopologyV1 {
    pub fn verified_snapshot(
        &self,
        authority: &tracedecay_domain::WorkAuthority,
        cancelled: Arc<AtomicBool>,
    ) -> Result<
        tracedecay_runtime_core::work_topology::WorkTopologyStore,
        tracedecay_runtime_core::work_topology::WorkTopologyError,
    > {
        let events = self
            .source
            .load_authority_events(authority)
            .map_err(|error| {
                tracedecay_runtime_core::work_topology::WorkTopologyError::Unavailable(
                    error.to_string(),
                )
            })?;
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(tracedecay_graph_db::GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        tracedecay_runtime_core::work_topology::WorkTopologyStore::publish_from_events(
            &events,
            &check,
            |manifest, key| {
                self.runtime
                    .publish_verified_manifest(manifest, key, Arc::clone(&cancelled))
            },
        )
    }
}

#[derive(Clone)]
pub struct RegisteredWorkflowTopologyV1 {
    source: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    runtime: VerifiedGraphRuntimeWeakProxyV1,
}

impl RegisteredWorkflowTopologyV1 {
    pub fn verified_snapshot(
        &self,
        definition_id: &tracedecay_domain::WorkflowDefinitionId,
        definition_version: u64,
        cancelled: Arc<AtomicBool>,
    ) -> Result<
        tracedecay_runtime_core::workflow_topology::WorkflowTopologyStore,
        tracedecay_runtime_core::workflow_topology::WorkflowTopologyError,
    > {
        let definition = self
            .source
            .load_definition_source(definition_id, definition_version)
            .map_err(|error| {
                tracedecay_runtime_core::workflow_topology::WorkflowTopologyError::Unavailable(
                    format!("{error:?}"),
                )
            })?
            .ok_or_else(|| {
                tracedecay_runtime_core::workflow_topology::WorkflowTopologyError::Unavailable(
                    "workflow definition source is missing".to_owned(),
                )
            })?;
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err(tracedecay_graph_db::GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        tracedecay_runtime_core::workflow_topology::WorkflowTopologyStore::publish_from_definition(
            &definition,
            &check,
            |manifest, key| {
                self.runtime
                    .publish_verified_manifest(manifest, key, Arc::clone(&cancelled))
            },
        )
    }
}

/// Core Work command and projection services over the registered exact-SQL
/// channel.
pub struct RegisteredWorkApplicationServicesV1 {
    commands:
        tracedecay_application::WorkService<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>,
    projections: tracedecay_application::WorkProjectionReadService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    topology: RegisteredWorkTopologyV1,
    attempts: tracedecay_application::WorkAttemptService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    run_control: tracedecay_application::WorkRunControlService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    placement: tracedecay_application::WorkPlacementService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    artifact_hydration: tracedecay_application::WorkArtifactHydrationService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    duplicate_adjudications: tracedecay_application::WorkDuplicateAdjudicationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
}

/// The Work product graph authority: its verified reads and its journaled
/// mutations, both over the same registered store.
///
/// This is a second Work authority, not a view of the first. The task services
/// above are scoped by `WorkAuthority`; this one is scoped by the registered
/// profile owner, which is also where its owner identity comes from — the
/// store's own binding, never a value a request supplied.
pub struct RegisteredWorkProductServicesV1 {
    reads: tracedecay_application::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    mutations: tracedecay_application::WorkProductMutationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    attempts: tracedecay_application::WorkProductAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    synthesis: tracedecay_application::WorkProductSynthesisAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    retry: tracedecay_application::WorkProductRetryServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_application::RuntimeWorkRetryEvidenceV1,
    >,
}

impl RegisteredWorkProductServicesV1 {
    /// Attaches the Work product graph authority over the registered exact-SQL
    /// handle.
    ///
    /// The catalog binding is supplied by the caller rather than minted here,
    /// because a service composed against a capability the catalog does not
    /// advertise could never authorize a request: it would look wired and
    /// answer nothing. Whichever adapter mounts a Work product operation
    /// passes that operation's own capability and use-case ids.
    ///
    /// The owner identity is NOT a parameter. It is resolved from the store's
    /// own registered binding, so no caller can ask for another profile's Work
    /// product by naming it.
    pub fn attach(
        db: &RegisteredGlobalDb,
        binding: tracedecay_application::WorkProductBindingV1,
    ) -> tracedecay_domain::errors::Result<Self> {
        let storage = db.work_storage()?;
        Ok(Self {
            reads: tracedecay_application::WorkProductReadServiceV1::new(
                storage.clone(),
                storage.clone(),
                binding,
            ),
            mutations: tracedecay_application::WorkProductMutationServiceV1::new(
                storage.clone(),
                storage.clone(),
                storage.clone(),
            ),
            attempts: tracedecay_application::WorkProductAttemptServiceV1::new(storage.clone()),
            synthesis: tracedecay_application::WorkProductSynthesisAttemptServiceV1::new(
                storage.clone(),
            ),
            retry: tracedecay_application::WorkProductRetryServiceV1::new(
                storage,
                tracedecay_application::RuntimeWorkRetryEvidenceV1,
            ),
        })
    }

    pub const fn reads(
        &self,
    ) -> &tracedecay_application::WorkProductReadServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.reads
    }

    pub const fn mutations(
        &self,
    ) -> &tracedecay_application::WorkProductMutationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.mutations
    }

    pub const fn attempts(
        &self,
    ) -> &tracedecay_application::WorkProductAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.attempts
    }

    pub const fn synthesis(
        &self,
    ) -> &tracedecay_application::WorkProductSynthesisAttemptServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.synthesis
    }

    pub const fn retry(
        &self,
    ) -> &tracedecay_application::WorkProductRetryServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_application::RuntimeWorkRetryEvidenceV1,
    > {
        &self.retry
    }
}

impl RegisteredWorkApplicationServicesV1 {
    pub fn attach(db: &RegisteredGlobalDb) -> tracedecay_domain::errors::Result<Self> {
        let storage = db.work_storage()?;
        let runtime = db.project_graph_runtime().cloned().ok_or_else(|| {
            attach_error(
                "attach registered Work topology",
                "project graph runtime is not bound",
            )
        })?;
        Ok(Self {
            commands: tracedecay_application::WorkService::new(storage.clone()),
            projections: tracedecay_application::WorkProjectionReadService::new(storage.clone()),
            attempts: tracedecay_application::WorkAttemptService::new(storage.clone()),
            run_control: tracedecay_application::WorkRunControlService::new(storage.clone()),
            placement: tracedecay_application::WorkPlacementService::new(storage.clone()),
            artifact_hydration: tracedecay_application::WorkArtifactHydrationService::new(
                storage.clone(),
            ),
            duplicate_adjudications:
                tracedecay_application::WorkDuplicateAdjudicationServiceV1::new(storage.clone()),
            topology: RegisteredWorkTopologyV1 {
                source: storage,
                runtime,
            },
        })
    }

    pub fn commands(
        &self,
    ) -> &tracedecay_application::WorkService<tracedecay_rusqlite_runtime::work::WorkSqliteStorage>
    {
        &self.commands
    }

    pub fn projections(
        &self,
    ) -> &tracedecay_application::WorkProjectionReadService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.projections
    }

    pub fn topology(&self) -> &RegisteredWorkTopologyV1 {
        &self.topology
    }

    pub fn attempts(
        &self,
    ) -> &tracedecay_application::WorkAttemptService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.attempts
    }

    /// The run-level pause/resume authority.
    ///
    /// It is a separate service from [`Self::attempts`] because the aggregate
    /// it owns is separate: an attempt lease fences one attempt, while the run
    /// control fences every future reservation of the run.
    pub const fn run_control(
        &self,
    ) -> &tracedecay_application::WorkRunControlService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.run_control
    }

    /// The placement preflight/admit/status/release authority.
    pub const fn placement(
        &self,
    ) -> &tracedecay_application::WorkPlacementService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.placement
    }

    /// The artifact and evidence hydration read authority.
    pub const fn artifact_hydration(
        &self,
    ) -> &tracedecay_application::WorkArtifactHydrationService<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.artifact_hydration
    }

    /// Explicit revisioned duplicate-effort adjudication authority.
    pub const fn duplicate_adjudications(
        &self,
    ) -> &tracedecay_application::WorkDuplicateAdjudicationServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    > {
        &self.duplicate_adjudications
    }
}

/// Workflow definition reads and journaled mutation authority over the
/// registered exact-SQL channel.
///
/// [`WorkflowSqliteAuthority`]: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority
pub struct RegisteredWorkflowApplicationServicesV1 {
    definitions: tracedecay_application::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    >,
    effects: tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    topology: RegisteredWorkflowTopologyV1,
}

impl RegisteredWorkflowApplicationServicesV1 {
    pub fn attach(db: &RegisteredGlobalDb) -> tracedecay_domain::errors::Result<Self> {
        let authority = db.workflow_storage()?;
        let runtime = db.project_graph_runtime().cloned().ok_or_else(|| {
            attach_error(
                "attach registered workflow topology",
                "project graph runtime is not bound",
            )
        })?;
        Ok(Self {
            definitions: tracedecay_application::WorkflowDefinitionService::new(authority.clone()),
            effects: authority.clone(),
            topology: RegisteredWorkflowTopologyV1 {
                source: authority,
                runtime,
            },
        })
    }

    pub fn definitions(
        &self,
    ) -> &tracedecay_application::WorkflowDefinitionService<
        tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority,
    > {
        &self.definitions
    }

    pub fn effects(&self) -> &tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority {
        &self.effects
    }

    pub fn has_pending_effects(
        &self,
        worktree_id: &tracedecay_domain::WorktreeId,
    ) -> Result<bool, tracedecay_application::WorkflowEffectAuthorityErrorV1> {
        tracedecay_application::WorkflowEffectAuthorityPortV1::has_pending_effects(
            &self.effects,
            worktree_id,
        )
    }

    pub fn topology(&self) -> &RegisteredWorkflowTopologyV1 {
        &self.topology
    }
}

/// Attaches product intelligence to the canonical verified Work graph and
/// rooted-evidence authorities owned by this registered exact-SQL store.
pub fn work_intelligence_service(
    db: &RegisteredGlobalDb,
    binding: tracedecay_application::WorkProductBindingV1,
) -> tracedecay_domain::errors::Result<
    tracedecay_application::WorkIntelligenceServiceV1<
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
> {
    let storage = db.work_storage()?;
    Ok(tracedecay_application::WorkIntelligenceServiceV1::new(
        storage.clone(),
        storage,
        binding,
    ))
}

fn attach_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

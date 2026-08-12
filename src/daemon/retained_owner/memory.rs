use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::retained_surfaces::{
    FactFeedbackRequestV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreGetRequestV1, FactStoreListRequestV1, FactStoreProbeRequestV1,
    FactStoreReasonRequestV1, FactStoreRelatedRequestV1, FactStoreRemoveRequestV1,
    FactStoreSearchRequestV1, FactStoreUpdateRequestV1, MemoryScopeV1, MemoryStatusRequestV1,
    RetainedProjectSelectorV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, CancellationStage, RetainedMemoryExecutionPortV1, RetainedMemoryRequestV1,
    RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceExecutionFutureV1, now_micros,
};
use tracedecay_domain::{FactOwnerV1, ManifestDigest};
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactIdV1, ProjectMemoryFactListQueryV1,
    ProjectMemoryFactSearchKindV1,
};
use tracedecay_usecases::memory::{
    MemoryApplication, MemoryOperationContext, ProjectMemoryFactAddRequestOutcome,
};

use super::map_execution_error;
use super::memory_mapping;
use super::memory_mutation::{fresh_one_shot_commit_gate, validate_memory_mutation};
use super::memory_stage::bounded_memory_operation;
use super::memory_tracking::{TrackedExplicitSearch, track_explicit_search};
use super::receipts::{
    effective_memory_deadline, evidence_outcome, memory_expiry_partial, prepare_retained_effect,
};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::Database;
use crate::store::DatabaseFactStore;
use crate::tracedecay::TraceDecay;

macro_rules! bounded_memory_read {
    ($context:expr, $future:expr) => {
        bounded_memory_operation($context, memory_mapping::READ_CANCELLATION_STAGES, $future)
    };
}

macro_rules! bounded_memory_effect {
    ($context:expr, $future:expr) => {
        bounded_memory_operation(
            $context,
            memory_mapping::EFFECT_CANCELLATION_STAGES,
            $future,
        )
    };
}

macro_rules! execute_scoped_memory {
    (
        $port:expr,
        $context:expr,
        $memory_scope:expr,
        $selector:expr,
        $access:expr,
        $executor:ident($request:expr)
    ) => {{
        match &$port.authority {
            DirectRetainedMemoryAuthorityV1::Project { cg, project_root } => {
                let (cg, _) = bounded_memory_read!($context, async {
                    Ok::<_, RetainedSurfaceExecutionErrorV1>(Arc::clone(&*cg.read().await))
                })
                .await?;
                let (target, _) = bounded_memory_read!($context, async {
                    super::memory_target::open_project_retained_memory_target(
                        &cg,
                        project_root,
                        &$context.request_context.scope().project_id,
                        $memory_scope,
                        $selector,
                        $access,
                    )
                    .await
                })
                .await?;
                $executor(
                    $context,
                    target.database(),
                    target.owner().clone(),
                    $request,
                    &$port.configuration_digest,
                )
                .await
            }
            DirectRetainedMemoryAuthorityV1::Profile { registry } => {
                memory_mapping::ensure_profile_request_scope($memory_scope, $selector)?;
                let (database, _) = bounded_memory_read!($context, async {
                    crate::daemon::store_runtime::session_registry::open_user_memory_db(registry)
                        .await
                        .map_err(map_execution_error)
                })
                .await?;
                $executor(
                    $context,
                    &database,
                    FactOwnerV1::Profile,
                    $request,
                    &$port.configuration_digest,
                )
                .await
            }
        }
    }};
}

enum DirectRetainedMemoryAuthorityV1<'a> {
    Project {
        cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
        project_root: PathBuf,
    },
    Profile {
        registry: &'a DaemonSessionRuntimeRegistryV1,
    },
}

enum Read<'a> {
    Search(&'a FactStoreSearchRequestV1),
    Probe(&'a FactStoreProbeRequestV1),
    Related(&'a FactStoreRelatedRequestV1),
    Reason(&'a FactStoreReasonRequestV1),
    Contradict(&'a FactStoreContradictRequestV1),
    Get(&'a FactStoreGetRequestV1),
    List(&'a FactStoreListRequestV1),
}
#[derive(Clone, Copy)]
enum SemanticRead<'a> {
    Probe(&'a FactStoreProbeRequestV1),
    Related(&'a FactStoreRelatedRequestV1),
    Reason(&'a FactStoreReasonRequestV1),
}
impl Read<'_> {
    fn scope(&self) -> (Option<MemoryScopeV1>, Option<&RetainedProjectSelectorV1>) {
        match self {
            Self::Search(request) => memory_mapping::read_scope(&request.options),
            Self::Probe(request) => memory_mapping::read_scope(&request.options),
            Self::Related(request) => memory_mapping::read_scope(&request.options),
            Self::Reason(request) => memory_mapping::read_scope(&request.options),
            Self::Contradict(request) => (request.memory_scope, request.project_selector.as_ref()),
            Self::Get(request) => (request.memory_scope, request.project_selector.as_ref()),
            Self::List(request) => memory_mapping::read_scope(&request.options),
        }
    }
}
pub(super) struct DirectRetainedMemoryPortV1<'a> {
    authority: DirectRetainedMemoryAuthorityV1<'a>,
    configuration_digest: ManifestDigest,
}
impl DirectRetainedMemoryPortV1<'static> {
    pub(super) fn project(
        cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
        project_root: PathBuf,
        configuration_digest: ManifestDigest,
    ) -> Self {
        Self {
            authority: DirectRetainedMemoryAuthorityV1::Project { cg, project_root },
            configuration_digest,
        }
    }
}

impl<'a> DirectRetainedMemoryPortV1<'a> {
    pub(super) fn profile(
        registry: &'a DaemonSessionRuntimeRegistryV1,
        configuration_digest: ManifestDigest,
    ) -> Self {
        Self {
            authority: DirectRetainedMemoryAuthorityV1::Profile { registry },
            configuration_digest,
        }
    }

    async fn execute_add(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &FactStoreAddRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        execute_scoped_memory!(
            self,
            context,
            request.memory_scope,
            request.project_selector.as_ref(),
            super::memory_target::MemoryTargetAccessV1::Write,
            execute_add_on_db(request)
        )
    }

    async fn execute_read(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: Read<'_>,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let (memory_scope, selector) = request.scope();
        execute_scoped_memory!(
            self,
            context,
            memory_scope,
            selector,
            super::memory_target::MemoryTargetAccessV1::Read,
            execute_read_on_db(request)
        )
    }

    async fn execute_status(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &MemoryStatusRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        execute_scoped_memory!(
            self,
            context,
            request.memory_scope,
            request.project_selector.as_ref(),
            super::memory_target::MemoryTargetAccessV1::Read,
            execute_status_on_db(request)
        )
    }

    async fn execute_update(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &FactStoreUpdateRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        execute_scoped_memory!(
            self,
            context,
            request.memory_scope,
            request.project_selector.as_ref(),
            super::memory_target::MemoryTargetAccessV1::Write,
            execute_update_on_db(request)
        )
    }

    async fn execute_remove(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &FactStoreRemoveRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        execute_scoped_memory!(
            self,
            context,
            request.memory_scope,
            request.project_selector.as_ref(),
            super::memory_target::MemoryTargetAccessV1::Write,
            execute_remove_on_db(request)
        )
    }

    async fn execute_feedback(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: &FactFeedbackRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        execute_scoped_memory!(
            self,
            context,
            request.memory_scope,
            request.project_selector.as_ref(),
            super::memory_target::MemoryTargetAccessV1::Write,
            execute_feedback_on_db(request)
        )
    }
}

impl RetainedMemoryExecutionPortV1 for DirectRetainedMemoryPortV1<'_> {
    fn execute_memory<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: RetainedMemoryRequestV1<'a>,
    ) -> RetainedSurfaceExecutionFutureV1<'a> {
        Box::pin(async move {
            match request {
                RetainedMemoryRequestV1::FactStoreAdd(request) => {
                    self.execute_add(&context, request).await
                }
                RetainedMemoryRequestV1::FactStoreSearch(request) => {
                    self.execute_read(&context, Read::Search(request)).await
                }
                RetainedMemoryRequestV1::FactStoreProbe(request) => {
                    self.execute_read(&context, Read::Probe(request)).await
                }
                RetainedMemoryRequestV1::FactStoreRelated(request) => {
                    self.execute_read(&context, Read::Related(request)).await
                }
                RetainedMemoryRequestV1::FactStoreReason(request) => {
                    self.execute_read(&context, Read::Reason(request)).await
                }
                RetainedMemoryRequestV1::FactStoreContradict(request) => {
                    self.execute_read(&context, Read::Contradict(request)).await
                }
                RetainedMemoryRequestV1::FactStoreGet(request) => {
                    self.execute_read(&context, Read::Get(request)).await
                }
                RetainedMemoryRequestV1::FactStoreUpdate(request) => {
                    self.execute_update(&context, request).await
                }
                RetainedMemoryRequestV1::FactStoreRemove(request) => {
                    self.execute_remove(&context, request).await
                }
                RetainedMemoryRequestV1::FactStoreList(request) => {
                    self.execute_read(&context, Read::List(request)).await
                }
                RetainedMemoryRequestV1::FactFeedback(request) => {
                    self.execute_feedback(&context, request).await
                }
                RetainedMemoryRequestV1::MemoryStatus(request) => {
                    self.execute_status(&context, request).await
                }
            }
        })
    }
}

async fn execute_add_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreAddRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let add_request = memory_mapping::add_request(request)?;
    let preflight = memory
        .preflight_project_memory_fact_add(
            add_request,
            Some(context.request_context.actor().clone()),
        )
        .map_err(memory_mapping::map_memory_error)?;
    let operation_id = preflight.operation_id().as_str().to_owned();
    let prepared = prepare_retained_effect(
        context,
        RetainedSurfaceOperation::FactStoreAdd,
        configuration_digest,
        preflight.effect_material(),
        &operation_id,
    )?;
    let write_control = fact_write_control(context);
    let (outcome, settled_after_expiry) = bounded_memory_effect!(context, async {
        Ok(memory
            .add_preflighted_project_memory_fact(preflight, &write_control)
            .await)
    })
    .await?;
    let outcome = validate_memory_mutation(outcome, &prepared, |outcome| match outcome {
        ProjectMemoryFactAddRequestOutcome::RejectedSecretLike => None,
        ProjectMemoryFactAddRequestOutcome::Applied(outcome) => outcome
            .commit_receipt()
            .map(tracedecay_store::FactCommitReceipt::committed_state_digest),
    })?;
    let committed_receipt = match &outcome {
        ProjectMemoryFactAddRequestOutcome::RejectedSecretLike => None,
        ProjectMemoryFactAddRequestOutcome::Applied(outcome) => outcome.commit_receipt(),
    };
    let public = match memory_mapping::add_result(&outcome) {
        Ok(public) => public,
        Err(error) => {
            let Some(commit) = committed_receipt else {
                return Err(error);
            };
            return prepared.memory_projection_failed(commit.committed_state_digest());
        }
    };
    let result = RetainedSurfaceResultV1::FactStoreAdd(public.clone());
    let committed_state = match committed_receipt {
        Some(commit) => commit.committed_state_digest().clone(),
        None => prepared
            .material_committed_state_digest(&memory_mapping::add_committed_state(&outcome)?)?,
    };
    prepared.complete_with_digest(
        context,
        &committed_state,
        tracedecay_application::ReconciliationState::Reconciled,
        result,
        memory_expiry_partial(settled_after_expiry),
    )
}

async fn execute_update_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreUpdateRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let logical_effect = memory_mapping::update_logical_effect(&owner, request)?;
    let operation_context =
        memory_mapping::memory_operation_context(context, &owner, "update", &logical_effect)?;
    let operation_id = operation_context.operation_id().as_str().to_owned();
    let prepared = prepare_retained_effect(
        context,
        RetainedSurfaceOperation::FactStoreUpdate,
        configuration_digest,
        &logical_effect,
        &operation_id,
    )?;
    let command = memory_mapping::update_command(
        owner,
        request,
        operation_context.operation_id().clone(),
        context.request_context.actor().clone(),
    )?;
    let write_control = fact_write_control(context);
    let (outcome, settled_after_expiry) = bounded_memory_effect!(context, async {
        Ok(memory
            .update_project_memory_fact(command, &write_control)
            .await)
    })
    .await?;
    let outcome = validate_memory_mutation(outcome, &prepared, |outcome| {
        Some(outcome.commit_receipt().committed_state_digest())
    })?;
    let commit = outcome.commit_receipt();
    let result = match memory_mapping::update_result(&outcome) {
        Ok(result) => RetainedSurfaceResultV1::FactStoreUpdate(result),
        Err(_) => {
            return prepared.memory_projection_failed(commit.committed_state_digest());
        }
    };
    prepared.complete_with_digest(
        context,
        commit.committed_state_digest(),
        tracedecay_application::ReconciliationState::Reconciled,
        result,
        memory_expiry_partial(settled_after_expiry),
    )
}

async fn execute_remove_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreRemoveRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let logical_effect = memory_mapping::remove_logical_effect(&owner, request)?;
    let operation_context =
        memory_mapping::memory_operation_context(context, &owner, "remove", &logical_effect)?;
    let operation_id = operation_context.operation_id().as_str().to_owned();
    let prepared = prepare_retained_effect(
        context,
        RetainedSurfaceOperation::FactStoreRemove,
        configuration_digest,
        &logical_effect,
        &operation_id,
    )?;
    let command = memory_mapping::remove_command(
        owner,
        request,
        operation_context.operation_id().clone(),
        context.request_context.actor().clone(),
    )?;
    let write_control = fact_write_control(context);
    let (outcome, settled_after_expiry) = bounded_memory_effect!(context, async {
        Ok(memory
            .remove_project_memory_fact(command, &write_control)
            .await)
    })
    .await?;
    let outcome = validate_memory_mutation(outcome, &prepared, |outcome| {
        outcome
            .commit_receipt()
            .map(tracedecay_store::FactCommitReceipt::committed_state_digest)
    })?;
    let committed_receipt = outcome.commit_receipt();
    let public = match memory_mapping::remove_result(&outcome) {
        Ok(public) => public,
        Err(error) => {
            let Some(commit) = committed_receipt else {
                return Err(error);
            };
            return prepared.memory_projection_failed(commit.committed_state_digest());
        }
    };
    let result = RetainedSurfaceResultV1::FactStoreRemove(public.clone());
    let partial = memory_expiry_partial(settled_after_expiry);
    if let Some(commit) = committed_receipt {
        return prepared.complete_with_digest(
            context,
            commit.committed_state_digest(),
            tracedecay_application::ReconciliationState::Reconciled,
            result,
            partial,
        );
    }
    prepared.complete(
        context,
        &public,
        tracedecay_application::ReconciliationState::Reconciled,
        result,
        partial,
    )
}

async fn execute_feedback_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactFeedbackRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let logical_effect = memory_mapping::feedback_logical_effect(&owner, request)?;
    let operation_context =
        memory_mapping::memory_operation_context(context, &owner, "feedback", &logical_effect)?;
    let operation_id = operation_context.operation_id().as_str().to_owned();
    let prepared = prepare_retained_effect(
        context,
        RetainedSurfaceOperation::FactFeedback,
        configuration_digest,
        &logical_effect,
        &operation_id,
    )?;
    let command = memory_mapping::feedback_command(
        owner,
        request,
        operation_context.operation_id().clone(),
        context.request_context.actor().clone(),
    )?;
    let write_control = fact_write_control(context);
    let (outcome, settled_after_expiry) = bounded_memory_effect!(context, async {
        Ok(memory
            .record_project_memory_fact_feedback(command, &write_control)
            .await)
    })
    .await?;
    let outcome = validate_memory_mutation(outcome, &prepared, |outcome| {
        Some(outcome.commit_receipt().committed_state_digest())
    })?;
    let commit = outcome.commit_receipt();
    let result = match memory_mapping::feedback_result(&outcome, request.action) {
        Ok(result) => RetainedSurfaceResultV1::FactFeedback(result),
        Err(_) => {
            return prepared.memory_projection_failed(commit.committed_state_digest());
        }
    };
    prepared.complete_with_digest(
        context,
        commit.committed_state_digest(),
        tracedecay_application::ReconciliationState::Reconciled,
        result,
        memory_expiry_partial(settled_after_expiry),
    )
}

async fn execute_read_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: Read<'_>,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    match request {
        Read::Search(request) => {
            search_on_db(context, database, owner, request, configuration_digest).await
        }
        Read::Probe(request) => {
            semantic_search_on_db(context, database, owner, SemanticRead::Probe(request)).await
        }
        Read::Related(request) => {
            semantic_search_on_db(context, database, owner, SemanticRead::Related(request)).await
        }
        Read::Reason(request) => {
            semantic_search_on_db(context, database, owner, SemanticRead::Reason(request)).await
        }
        Read::Contradict(request) => contradict_on_db(context, database, owner, request).await,
        Read::Get(request) => get_on_db(context, database, owner, request).await,
        Read::List(request) => list_on_db(context, database, owner, request).await,
    }
}

async fn search_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreSearchRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let query = memory_mapping::search_query(
        owner.clone(),
        ProjectMemoryFactSearchKindV1::Search,
        Some(request.query.clone()),
        &request.options,
        request.after.as_ref(),
    )?;
    let logical_effect = memory_mapping::search_logical_effect(&owner, request)?;
    let request_id = context.request_context.request_id().as_str();
    let actor = Some(context.request_context.actor().clone());
    let operation_context =
        MemoryOperationContext::from_request_id(&owner, "search", request_id, actor)
            .map_err(memory_mapping::map_memory_error)?;
    let read_control = fact_read_control(context);
    let (page, _) = bounded_memory_read!(context, async {
        memory
            .search_project_memory_facts(query, &read_control)
            .await
            .map_err(memory_mapping::map_memory_error)
    })
    .await?;
    let prepared = if page.hits().is_empty() {
        None
    } else {
        Some(prepare_retained_effect(
            context,
            RetainedSurfaceOperation::FactStoreSearch,
            configuration_digest,
            &logical_effect,
            operation_context.operation_id().as_str(),
        )?)
    };
    let tracked = if database.is_writable() {
        track_explicit_search(
            context,
            &memory,
            &owner,
            operation_context.operation_id().clone(),
            &page,
        )
        .await?
    } else {
        TrackedExplicitSearch::default()
    };
    if tracked.authority_result_invalid {
        let committed_state = tracked
            .committed_state()
            .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
        let prepared = prepared
            .as_ref()
            .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
        return prepared.partial_with_digest(
            committed_state,
            "application.retained.memory-search-authority-result-invalid",
            "Retrieval telemetry committed, but the authority result failed validation.",
        );
    }
    let mut mapped = match memory_mapping::search_page(&page) {
        Ok(mapped) => mapped,
        Err(error) => {
            let Some(committed_state) = tracked.committed_state() else {
                return Err(error);
            };
            let prepared = prepared
                .as_ref()
                .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
            return prepared.partial_with_digest(
                committed_state,
                "application.retained.memory-search-projection-failed",
                "Retrieval telemetry committed, but the public search projection could not be assembled.",
            );
        }
    };
    if tracked.receipt.is_some()
        && memory_mapping::refresh_search_hits(&mut mapped, &tracked.projections).is_err()
    {
        let Some(committed_state) = tracked.committed_state() else {
            return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
        };
        let prepared = prepared
            .as_ref()
            .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
        return prepared.partial_with_digest(
            committed_state,
            "application.retained.memory-search-telemetry-projection-failed",
            "Retrieval telemetry committed, but the refreshed public facts could not be assembled.",
        );
    }
    if tracked.settled_after_expiry {
        let Some(committed_state) = tracked.committed_state() else {
            return Err(RetainedSurfaceExecutionErrorV1::TimedOut(
                CancellationStage::DuringRead,
            ));
        };
        let prepared = prepared
            .as_ref()
            .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
        return prepared.partial_with_digest(
            committed_state,
            "application.retained.memory-search-admission-expiry-after-telemetry-commit",
            "Retrieval telemetry committed after the request or capability grant expired.",
        );
    }
    let result = memory_mapping::exact_search_result(mapped);
    match evidence_outcome(context, RetainedSurfaceOperation::FactStoreSearch, result) {
        Ok(outcome) => Ok(outcome),
        Err(RetainedSurfaceExecutionErrorV1::TimedOut(stage)) => {
            let Some(committed_state) = tracked.committed_state() else {
                return Err(RetainedSurfaceExecutionErrorV1::TimedOut(stage));
            };
            let prepared = prepared
                .as_ref()
                .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
            prepared.memory_expiry_failed(committed_state)
        }
        Err(error) => {
            let Some(committed_state) = tracked.committed_state() else {
                return Err(error);
            };
            let prepared = prepared
                .as_ref()
                .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
            prepared.partial_with_digest(
                committed_state,
                "application.retained.memory-search-delivery-failed",
                "Retrieval telemetry committed, but the evidence packet could not be assembled.",
            )
        }
    }
}

async fn semantic_search_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: SemanticRead<'_>,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let (kind, query_text, options, after, operation) = match request {
        SemanticRead::Probe(request) => (
            ProjectMemoryFactSearchKindV1::Probe,
            Some(request.entity.clone()),
            &request.options,
            request.after.as_ref(),
            RetainedSurfaceOperation::FactStoreProbe,
        ),
        SemanticRead::Related(request) => (
            ProjectMemoryFactSearchKindV1::Related {
                entity: request.entity.clone(),
            },
            None,
            &request.options,
            request.after.as_ref(),
            RetainedSurfaceOperation::FactStoreRelated,
        ),
        SemanticRead::Reason(request) => {
            memory_mapping::validate_reason_entities(&request.entities)?;
            let entities = request.entities.clone();
            (
                ProjectMemoryFactSearchKindV1::Reason { entities },
                None,
                &request.options,
                request.after.as_ref(),
                RetainedSurfaceOperation::FactStoreReason,
            )
        }
    };
    let memory = memory_application(database, owner.clone())?;
    let query = memory_mapping::search_query(owner, kind, query_text, options, after)?;
    let read_control = fact_read_control(context);
    let (page, _) = bounded_memory_read!(context, async {
        let page = match request {
            SemanticRead::Probe(_) => {
                memory
                    .probe_project_memory_facts(query, &read_control)
                    .await
            }
            SemanticRead::Related(_) => {
                memory
                    .related_project_memory_facts(query, &read_control)
                    .await
            }
            SemanticRead::Reason(_) => {
                memory
                    .reason_project_memory_facts(query, &read_control)
                    .await
            }
        };
        page.map_err(memory_mapping::map_memory_error)
    })
    .await?;
    let mapped = memory_mapping::search_page(&page)?;
    let result = memory_mapping::semantic_search_result(operation, mapped)?;
    evidence_outcome(context, operation, result)
}

async fn contradict_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreContradictRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let query = ProjectMemoryFactContradictionQueryV1::new(
        owner,
        request.category.map(memory_mapping::domain_category),
        request.threshold_millionths.unwrap_or(300_000),
        memory_mapping::fact_limit(request.limit)?,
    )
    .map_err(memory_mapping::map_store_error)?;
    let read_control = fact_read_control(context);
    let (page, _) = bounded_memory_read!(context, async {
        memory
            .find_project_memory_contradictions(query, &read_control)
            .await
            .map_err(memory_mapping::map_memory_error)
    })
    .await?;
    let result =
        RetainedSurfaceResultV1::FactStoreContradict(memory_mapping::contradiction_page(&page)?);
    evidence_outcome(
        context,
        RetainedSurfaceOperation::FactStoreContradict,
        result,
    )
}

async fn get_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreGetRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let target = ProjectMemoryFactIdV1::new(owner, request.fact_id.clone())
        .map_err(memory_mapping::map_store_error)?;
    let read_control = fact_read_control(context);
    let (projection, _) = bounded_memory_read!(context, async {
        memory
            .get_project_memory_fact(target.clone(), &read_control)
            .await
            .map_err(memory_mapping::map_memory_error)
    })
    .await?;
    let projection = projection.ok_or(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)?;
    let history_query = ProjectMemoryFactFeedbackHistoryQueryV1::new(
        target,
        None,
        memory_mapping::MAX_RETAINED_FEEDBACK_HISTORY_LIMIT,
    )
    .map_err(memory_mapping::map_store_error)?;
    let (history, _) = bounded_memory_read!(context, async {
        memory
            .get_project_memory_feedback_history(history_query, &read_control)
            .await
            .map_err(memory_mapping::map_memory_error)
    })
    .await?;
    let result = memory_mapping::get_result(&projection, &history)?;
    evidence_outcome(context, RetainedSurfaceOperation::FactStoreGet, result)
}

async fn list_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreListRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let query = ProjectMemoryFactListQueryV1::new(
        owner,
        request
            .options
            .category
            .map(memory_mapping::domain_category),
        memory_mapping::confidence(request.options.min_trust)?,
        request.after_fact_id.clone(),
        memory_mapping::fact_limit(request.options.limit)?,
    )
    .map_err(memory_mapping::map_store_error)?;
    let read_control = fact_read_control(context);
    let (page, _) = bounded_memory_read!(context, async {
        memory
            .list_project_memory_facts(query, &read_control)
            .await
            .map_err(memory_mapping::map_memory_error)
    })
    .await?;
    let result = RetainedSurfaceResultV1::FactStoreList(memory_mapping::list_page(&page)?);
    evidence_outcome(context, RetainedSurfaceOperation::FactStoreList, result)
}

async fn execute_status_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    _: &MemoryStatusRequestV1,
    _: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner)?;
    let read_control = fact_read_control(context);
    let (status, _) = bounded_memory_read!(context, async {
        memory
            .project_memory_status(&read_control)
            .await
            .map_err(memory_mapping::map_memory_error)
    })
    .await?;
    let result = memory_mapping::status_result(&status);
    evidence_outcome(context, RetainedSurfaceOperation::MemoryStatus, result)
}

fn memory_application(
    database: &Database,
    owner: FactOwnerV1,
) -> Result<MemoryApplication<DatabaseFactStore<'_>>, RetainedSurfaceExecutionErrorV1> {
    MemoryApplication::new(owner, DatabaseFactStore::new(database))
        .map_err(memory_mapping::map_memory_error)
}

fn fact_read_control(context: &RetainedSurfaceExecutionContextV1<'_>) -> FactReadControl {
    let signal = context.cancellation_signal.clone();
    let expires_at = effective_memory_deadline(context).expires_at;
    FactReadControl::new(Arc::new(move || {
        signal.is_cancelled() || expires_at <= now_micros()
    }))
}

pub(super) fn fact_write_control(
    context: &RetainedSurfaceExecutionContextV1<'_>,
) -> FactWriteControl {
    let interrupted_signal = context.cancellation_signal.clone();
    let commit_signal = context.cancellation_signal.clone();
    let expires_at = effective_memory_deadline(context).expires_at;
    let commit_expires_at = expires_at;
    FactWriteControl::new(
        Arc::new(move || interrupted_signal.is_cancelled() || expires_at <= now_micros()),
        fresh_one_shot_commit_gate(Arc::new(move || {
            commit_signal.is_cancelled()
                || commit_expires_at <= now_micros()
                || !commit_signal.try_begin_commit()
        })),
    )
}

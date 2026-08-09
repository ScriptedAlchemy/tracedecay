//! Direct typed retained-memory execution over canonical memory authorities.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tracedecay_application::retained_surfaces::{
    FactCategoryV1, FactCollectionEntryV1, FactFeedbackActionV1, FactFeedbackRequestV1,
    FactFeedbackResultV1, FactReadOptionsV1, FactStoreAddRequestV1, FactStoreAddResultV1,
    FactStoreContradictRequestV1, FactStoreContradictResultV1, FactStoreGetRequestV1,
    FactStoreGetResultV1, FactStoreListRequestV1, FactStoreListResultV1, FactStoreProbeRequestV1,
    FactStoreProbeResultV1, FactStoreReasonRequestV1, FactStoreReasonResultV1,
    FactStoreRelatedRequestV1, FactStoreRelatedResultV1, FactStoreRemoveRequestV1,
    FactStoreRemoveResultV1, FactStoreSearchRequestV1, FactStoreSearchResultV1,
    FactStoreUpdateRequestV1, FactStoreUpdateResultV1, MemoryFeedbackFunnelV1, MemoryScopeV1,
    MemoryStatusRequestV1, MemoryStatusResultV1, MemoryStatusV1, RetainedOutcomeStatusV1,
    RetainedProjectSelectorV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationOutcome, RetainedMemoryExecutionPortV1, RetainedMemoryRequestV1,
    RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceExecutionFutureV1,
};
use tracedecay_domain::{FactOwnerV1, ManifestDigest};
use tracedecay_runtime_core::memory::types::{
    AddFactRequest, MemoryCategory, MemoryStatus, SearchFactsRequest, UpdateFactRequest,
};
use tracedecay_usecases::memory::{
    FactUpdateEffect, MemoryApplication, MemoryOperationContext, memory_application_error,
};

use super::receipts::{effect_outcome, evidence_outcome, fact_mutation, no_op_effect_outcome};
use super::{bounded_execution, map_execution_error};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::Database;
use crate::errors::TraceDecayError;
use crate::store::DatabaseFactStore;
use crate::store::memory::FactWriteControl;
use crate::tracedecay::TraceDecay;

const MAX_RETAINED_FACT_LIMIT: usize = 200;

macro_rules! execute_scoped_memory {
    (
        $port:expr,
        $context:expr,
        $memory_scope:expr,
        $selector:expr,
        $project_id:expr,
        $project_path:expr,
        $executor:ident($request:expr)
    ) => {{
        match &$port.authority {
            DirectRetainedMemoryAuthorityV1::Project { cg, project_root } => {
                ensure_project_request_scope(
                    $context,
                    $memory_scope,
                    $selector,
                    $project_id,
                    $project_path,
                    project_root,
                )?;
                let cg = bounded_execution($context, async {
                    Ok::<_, TraceDecayError>(Arc::clone(&*cg.read().await))
                })
                .await?;
                let owner = cg.project_memory_owner().map_err(map_execution_error)?;
                ensure_project_owner($context, &owner)?;
                let database = bounded_execution($context, cg.project_memory_db()).await?;
                let outcome = $executor(
                    $context,
                    database.as_db(),
                    owner,
                    $request,
                    &$port.configuration_digest,
                )
                .await?;
                Ok(outcome)
            }
            DirectRetainedMemoryAuthorityV1::Profile { registry } => {
                ensure_profile_request_scope($memory_scope, $selector, $project_id, $project_path)?;
                let database =
                    bounded_execution($context, crate::memory::user::open_user_memory_db(registry))
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

macro_rules! collection_evidence_outcome {
    ($context:expr, $operation:expr, $variant:ident, $result:ident, $entries:expr) => {{
        let entries = $entries;
        evidence_outcome(
            $context,
            $operation,
            RetainedSurfaceResultV1::$variant($result {
                count: entries.len(),
                facts: entries.clone(),
                results: entries,
            }),
        )
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

impl Read<'_> {
    fn options(&self) -> &FactReadOptionsV1 {
        match self {
            Self::Search(request) => &request.options,
            Self::Probe(request) => &request.options,
            Self::Related(request) => &request.options,
            Self::Reason(request) => &request.options,
            Self::Contradict(request) => &request.options,
            Self::Get(request) => &request.options,
            Self::List(request) => &request.options,
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
            request.project_id.as_deref(),
            request.project_path.as_deref(),
            execute_add_on_database(request)
        )
    }

    async fn execute_read(
        &self,
        context: &RetainedSurfaceExecutionContextV1<'_>,
        request: Read<'_>,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        let options = request.options();
        execute_scoped_memory!(
            self,
            context,
            options.memory_scope,
            options.project_selector.as_ref(),
            options.project_id.as_deref(),
            options.project_path.as_deref(),
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
            request.project_id.as_deref(),
            request.project_path.as_deref(),
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
            request.project_id.as_deref(),
            request.project_path.as_deref(),
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
            request.project_id.as_deref(),
            request.project_path.as_deref(),
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
            None,
            None,
            None,
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

async fn execute_add_on_database(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreAddRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = controlled_memory_application(context, database, owner.clone())?;
    let operation_context = memory_operation_context(context, &owner, "add")?;
    let effect = bounded_execution(context, async {
        memory
            .add_fact_effect(add_request(request)?, operation_context)
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let outcome = effect.outcome;
    let result = RetainedSurfaceResultV1::FactStoreAdd(FactStoreAddResultV1 {
        count: usize::from(outcome.fact.is_some()),
        fact: outcome.fact,
        diff: outcome.diff.diff,
        closest_fact_id: outcome.diff.closest_fact_id,
        similarity: outcome.diff.similarity,
        reason: outcome.diff.reason,
        mutation: effect.mutation.as_ref().map(fact_mutation).transpose()?,
    });
    match effect.mutation {
        Some(mutation) => effect_outcome(configuration_digest, context, result, &mutation),
        None => no_op_effect_outcome(configuration_digest, context, request, result),
    }
}

async fn execute_read_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: Read<'_>,
    _: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    match request {
        Read::Search(request) => search_on_db(context, database, owner, request).await,
        Read::Probe(request) => probe_on_db(context, database, owner, request).await,
        Read::Related(request) => related_on_db(context, database, owner, request).await,
        Read::Reason(request) => reason_on_db(context, database, owner, request).await,
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
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(database))
        .map_err(memory_application_error)
        .map_err(map_execution_error)?;
    let operation_context = memory_operation_context(context, &owner, "search")?;
    let hits = bounded_execution(context, async {
        memory
            .search_facts(search_request(request)?, operation_context)
            .await
            .map_err(memory_application_error)
    })
    .await?;
    collection_evidence_outcome!(
        context,
        RetainedSurfaceOperation::FactStoreSearch,
        FactStoreSearch,
        FactStoreSearchResultV1,
        search_entries(hits)
    )
}

async fn probe_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreProbeRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let operation_context = memory_operation_context(context, &owner, "probe")?;
    let hits = bounded_execution(context, async {
        memory
            .probe_facts(
                search_request_for(&request.entity, &request.options)?,
                operation_context,
            )
            .await
            .map_err(memory_application_error)
    })
    .await?;
    collection_evidence_outcome!(
        context,
        RetainedSurfaceOperation::FactStoreProbe,
        FactStoreProbe,
        FactStoreProbeResultV1,
        search_entries(hits)
    )
}

async fn related_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreRelatedRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let operation_context = memory_operation_context(context, &owner, "related")?;
    let hits = bounded_execution(context, async {
        memory
            .related_facts(
                search_request_for(&request.entity, &request.options)?,
                operation_context,
            )
            .await
            .map_err(memory_application_error)
    })
    .await?;
    collection_evidence_outcome!(
        context,
        RetainedSurfaceOperation::FactStoreRelated,
        FactStoreRelated,
        FactStoreRelatedResultV1,
        search_entries(hits)
    )
}

async fn reason_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreReasonRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let mut entities = request.entities.clone();
    if let Some(entity) = &request.entity {
        entities.push(entity.clone());
    }
    if entities.is_empty() {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let memory = memory_application(database, owner.clone())?;
    let operation_context = memory_operation_context(context, &owner, "reason")?;
    let category = request.options.category.map(memory_category);
    let limit = fact_limit(request.options.limit)?;
    let hits = bounded_execution(context, async {
        memory
            .reason_facts(
                entities,
                category,
                request.options.min_trust,
                limit,
                operation_context,
            )
            .await
            .map_err(memory_application_error)
    })
    .await?;
    collection_evidence_outcome!(
        context,
        RetainedSurfaceOperation::FactStoreReason,
        FactStoreReason,
        FactStoreReasonResultV1,
        search_entries(hits)
    )
}

async fn contradict_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreContradictRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner)?;
    let limit = fact_limit(request.options.limit)?;
    let contradictions = bounded_execution(context, async {
        memory
            .contradict_facts(
                request.options.category.map(memory_category),
                request.threshold.unwrap_or(0.3),
                limit,
            )
            .await
            .map_err(memory_application_error)
    })
    .await?;
    collection_evidence_outcome!(
        context,
        RetainedSurfaceOperation::FactStoreContradict,
        FactStoreContradict,
        FactStoreContradictResultV1,
        contradictions
            .into_iter()
            .map(FactCollectionEntryV1::Contradiction)
            .collect::<Vec<_>>()
    )
}

async fn get_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreGetRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner)?;
    let fact = bounded_execution(context, async {
        memory
            .get_fact(request.fact_id.clone())
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let fact = fact.ok_or(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)?;
    let trust_history = bounded_execution(context, async {
        memory
            .fact_trust_history(request.fact_id.clone(), MAX_RETAINED_FACT_LIMIT)
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let result = RetainedSurfaceResultV1::FactStoreGet(FactStoreGetResultV1 {
        count: 1,
        fact: Some(fact),
        trust_history,
    });
    evidence_outcome(context, RetainedSurfaceOperation::FactStoreGet, result)
}

async fn list_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreListRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner.clone())?;
    let operation_context = memory_operation_context(context, &owner, "list")?;
    let limit = fact_limit(request.options.limit)?;
    let facts = bounded_execution(context, async {
        memory
            .list_facts(
                request.options.category.map(memory_category),
                request.options.min_trust,
                limit,
                operation_context,
            )
            .await
            .map_err(memory_application_error)
    })
    .await?;
    collection_evidence_outcome!(
        context,
        RetainedSurfaceOperation::FactStoreList,
        FactStoreList,
        FactStoreListResultV1,
        facts
            .into_iter()
            .map(FactCollectionEntryV1::Fact)
            .collect::<Vec<_>>()
    )
}

async fn execute_status_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    _: &MemoryStatusRequestV1,
    _: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner)?;
    let status = bounded_execution(context, async {
        memory
            .memory_status()
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let result = RetainedSurfaceResultV1::MemoryStatus(MemoryStatusResultV1 {
        status: RetainedOutcomeStatusV1::Ok,
        memory: memory_status(status),
    });
    evidence_outcome(context, RetainedSurfaceOperation::MemoryStatus, result)
}

fn controlled_memory_application<'database>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &'database Database,
    owner: FactOwnerV1,
) -> Result<MemoryApplication<DatabaseFactStore<'database>>, RetainedSurfaceExecutionErrorV1> {
    let cancellation = context.cancellation_signal.clone();
    let interruption = cancellation.clone();
    let store = DatabaseFactStore::new_controlled(
        database,
        FactWriteControl::new(
            Arc::new(move || interruption.is_cancelled()),
            Arc::new(move || cancellation.try_begin_commit()),
        ),
    );
    MemoryApplication::new(owner, store)
        .map_err(memory_application_error)
        .map_err(map_execution_error)
}

async fn execute_update_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreUpdateRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = controlled_memory_application(context, database, owner.clone())?;
    let operation_context = memory_operation_context(context, &owner, "update")?;
    let effect = bounded_execution(context, async {
        memory
            .update_fact_effect(update_request(request), operation_context)
            .await
            .map_err(memory_application_error)
    })
    .await?;
    match effect {
        FactUpdateEffect::Updated { fact, mutation } => {
            let result = RetainedSurfaceResultV1::FactStoreUpdate(FactStoreUpdateResultV1 {
                count: 1,
                fact: Some(*fact),
                diff: None,
                reason: None,
                error: None,
                mutation: Some(fact_mutation(&mutation)?),
            });
            effect_outcome(configuration_digest, context, result, &mutation)
        }
        FactUpdateEffect::RejectedSecretLike { reason } => {
            let result = RetainedSurfaceResultV1::FactStoreUpdate(FactStoreUpdateResultV1 {
                count: 0,
                fact: None,
                diff: Some(
                    tracedecay_application::retained_surfaces::FactDiffKindV1::RejectedSecretLike,
                ),
                reason: Some(reason.clone()),
                error: Some(reason),
                mutation: None,
            });
            no_op_effect_outcome(configuration_digest, context, request, result)
        }
    }
}

async fn execute_remove_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreRemoveRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = controlled_memory_application(context, database, owner.clone())?;
    let operation_context = memory_operation_context(context, &owner, "remove")?;
    let effect = bounded_execution(context, async {
        memory
            .remove_fact_effect(request.fact_id.clone(), operation_context)
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let result = RetainedSurfaceResultV1::FactStoreRemove(FactStoreRemoveResultV1 {
        count: usize::from(effect.removed),
        removed: effect.removed,
        mutation: effect.mutation.as_ref().map(fact_mutation).transpose()?,
    });
    match effect.mutation {
        Some(mutation) => effect_outcome(configuration_digest, context, result, &mutation),
        None => no_op_effect_outcome(configuration_digest, context, request, result),
    }
}

async fn execute_feedback_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactFeedbackRequestV1,
    configuration_digest: &ManifestDigest,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = controlled_memory_application(context, database, owner.clone())?;
    let fact_exists = bounded_execution(context, async {
        memory
            .get_fact(request.fact_id.clone())
            .await
            .map_err(memory_application_error)
    })
    .await?
    .is_some();
    if !fact_exists {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    let operation_context = memory_operation_context(context, &owner, "feedback")?;
    let effect = bounded_execution(context, async {
        memory
            .record_fact_feedback_effect(
                request.fact_id.clone(),
                feedback_action(request)?,
                request.source.clone(),
                request.note.clone(),
                operation_context,
            )
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let result = RetainedSurfaceResultV1::FactFeedback(FactFeedbackResultV1 {
        status: RetainedOutcomeStatusV1::Recorded,
        feedback: effect.feedback,
        mutation: fact_mutation(&effect.mutation)?,
    });
    effect_outcome(configuration_digest, context, result, &effect.mutation)
}

fn add_request(request: &FactStoreAddRequestV1) -> Result<AddFactRequest, TraceDecayError> {
    let mut entities = request.entities.clone();
    if let Some(entity) = request.entity.as_ref()
        && !entities.contains(entity)
    {
        entities.push(entity.clone());
    }
    let tags = request.tags.clone();
    let mut metadata = request
        .metadata
        .clone()
        .map(|metadata| metadata.into_iter().collect::<Map<String, Value>>())
        .unwrap_or_default();
    if !tags.is_empty() {
        metadata.insert("tags".to_owned(), Value::from(tags.clone()));
    }
    Ok(AddFactRequest {
        content: request.content.clone(),
        category: request
            .category
            .unwrap_or(tracedecay_application::retained_surfaces::FactCategoryV1::General)
            .into(),
        source: request.source.clone(),
        tags,
        entities,
        trust: request.trust,
        metadata: Value::Object(metadata),
    })
}

fn search_request(
    request: &FactStoreSearchRequestV1,
) -> Result<SearchFactsRequest, TraceDecayError> {
    search_request_for(&request.query, &request.options)
}

fn search_request_for(
    query: &str,
    options: &FactReadOptionsV1,
) -> Result<SearchFactsRequest, TraceDecayError> {
    Ok(SearchFactsRequest {
        query: query.to_owned(),
        category: options.category.map(memory_category),
        limit: Some(fact_limit(options.limit)?),
        min_trust: options.min_trust,
        include_why: true,
    })
}

fn fact_limit(limit: Option<u64>) -> Result<usize, TraceDecayError> {
    let limit = limit
        .map(usize::try_from)
        .transpose()
        .map_err(|_| TraceDecayError::Config {
            message: "retained fact search limit exceeds this platform".to_owned(),
        })?
        .unwrap_or(20)
        .clamp(1, MAX_RETAINED_FACT_LIMIT);
    Ok(limit)
}

const fn memory_category(category: FactCategoryV1) -> MemoryCategory {
    match category {
        FactCategoryV1::General => MemoryCategory::General,
        FactCategoryV1::UserPref => MemoryCategory::UserPref,
        FactCategoryV1::Project => MemoryCategory::Project,
        FactCategoryV1::Tool => MemoryCategory::Tool,
        FactCategoryV1::Decision => MemoryCategory::Decision,
        FactCategoryV1::CodeArea => MemoryCategory::CodeArea,
    }
}

fn memory_application(
    database: &Database,
    owner: FactOwnerV1,
) -> Result<MemoryApplication<DatabaseFactStore<'_>>, RetainedSurfaceExecutionErrorV1> {
    MemoryApplication::new(owner, DatabaseFactStore::new(database))
        .map_err(memory_application_error)
        .map_err(map_execution_error)
}

fn search_entries(
    hits: Vec<tracedecay_application::retained_surfaces::FactSearchHitV1>,
) -> Vec<FactCollectionEntryV1> {
    hits.into_iter()
        .map(FactCollectionEntryV1::Search)
        .collect()
}

fn update_request(request: &FactStoreUpdateRequestV1) -> UpdateFactRequest {
    UpdateFactRequest {
        fact_id: request.fact_id.clone(),
        content: request.content.clone(),
        category: request.category.map(memory_category),
        tags: request.tags.clone(),
        entities: request.entities.clone(),
        trust: request.trust,
        source: request.source.clone(),
        metadata: request
            .metadata
            .clone()
            .map(|metadata| Value::Object(metadata.into_iter().collect::<Map<_, _>>())),
    }
}

fn feedback_action(
    request: &FactFeedbackRequestV1,
) -> Result<FactFeedbackActionV1, TraceDecayError> {
    if let Some(action) = request.action {
        return Ok(action);
    }
    match (
        request.helpful.unwrap_or(false),
        request.unhelpful.unwrap_or(false),
    ) {
        (true, false) => Ok(FactFeedbackActionV1::Helpful),
        (false, true) => Ok(FactFeedbackActionV1::Unhelpful),
        _ => Err(TraceDecayError::Config {
            message: "missing feedback action: set action, helpful, or unhelpful".to_owned(),
        }),
    }
}

fn memory_status(status: MemoryStatus) -> MemoryStatusV1 {
    MemoryStatusV1 {
        fact_count: status.fact_count,
        entity_count: status.entity_count,
        algebra_name: status.algebra_name,
        hrr_dim: status.hrr_dim,
        estimated_capacity: status.estimated_capacity,
        trust_0_025_count: status.trust_0_025_count,
        trust_025_050_count: status.trust_025_050_count,
        trust_050_075_count: status.trust_050_075_count,
        trust_075_100_count: status.trust_075_100_count,
        below_default_recall_threshold_count: status.below_default_recall_threshold_count,
        helpful_count: status.helpful_count,
        unhelpful_count: status.unhelpful_count,
        feedback_funnel: MemoryFeedbackFunnelV1 {
            retrieval_count_total: status.feedback_funnel.retrieval_count_total,
            access_count_total: status.feedback_funnel.access_count_total,
            retrieved_fact_count: status.feedback_funnel.retrieved_fact_count,
            rated_fact_count: status.feedback_funnel.rated_fact_count,
            feedback_total: status.feedback_funnel.feedback_total,
            seen_to_feedback_ratio: status.feedback_funnel.seen_to_feedback_ratio,
        },
    }
}

fn memory_operation_context(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    owner: &FactOwnerV1,
    action: &str,
) -> Result<MemoryOperationContext, RetainedSurfaceExecutionErrorV1> {
    MemoryOperationContext::from_trusted_request_id(
        owner,
        action,
        context.request_context.request_id().as_str(),
        Some(context.request_context.actor().clone()),
    )
    .map_err(memory_application_error)
    .map_err(map_execution_error)
}

fn ensure_project_owner(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    owner: &FactOwnerV1,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    match owner {
        FactOwnerV1::Project { project_id }
            if project_id == &context.request_context.scope().project_id =>
        {
            Ok(())
        }
        FactOwnerV1::Project { .. } | FactOwnerV1::Profile => {
            Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)
        }
    }
}

fn ensure_project_request_scope(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    memory_scope: Option<MemoryScopeV1>,
    selector: Option<&RetainedProjectSelectorV1>,
    project_id: Option<&str>,
    project_path: Option<&str>,
    mounted_root: &Path,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    if memory_scope == Some(MemoryScopeV1::User)
        || project_id
            .is_some_and(|value| value != context.request_context.scope().project_id.as_str())
        || project_path.is_some_and(|value| Path::new(value) != mounted_root)
        || selector.is_some_and(|selector| {
            selector
                .project_id
                .as_deref()
                .is_some_and(|value| value != context.request_context.scope().project_id.as_str())
                || selector
                    .path
                    .as_deref()
                    .is_some_and(|value| Path::new(value) != mounted_root)
                || selector
                    .project_path
                    .as_deref()
                    .is_some_and(|value| Path::new(value) != mounted_root)
        })
    {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    Ok(())
}

fn ensure_profile_request_scope(
    memory_scope: Option<MemoryScopeV1>,
    selector: Option<&RetainedProjectSelectorV1>,
    project_id: Option<&str>,
    project_path: Option<&str>,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    if memory_scope != Some(MemoryScopeV1::User)
        || selector.is_some()
        || project_id.is_some()
        || project_path.is_some()
    {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    Ok(())
}

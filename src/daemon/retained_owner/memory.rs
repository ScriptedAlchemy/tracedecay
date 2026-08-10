//! Direct typed retained-memory execution over canonical memory authorities.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tracedecay_application::retained_surfaces::{
    FactCategoryV1, FactCollectionEntryV1, FactContradictionV1, FactFeedbackRequestV1,
    FactReadOptionsV1, FactSearchHitV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreContradictResultV1, FactStoreGetRequestV1, FactStoreGetResultV1,
    FactStoreListRequestV1, FactStoreListResultV1, FactStoreProbeRequestV1, FactStoreProbeResultV1,
    FactStoreReasonRequestV1, FactStoreReasonResultV1, FactStoreRelatedRequestV1,
    FactStoreRelatedResultV1, FactStoreRemoveRequestV1, FactStoreSearchRequestV1,
    FactStoreSearchResultV1, FactStoreUpdateRequestV1, FactV1, MemoryFeedbackFunnelV1,
    MemoryRepairStatsV1, MemoryScopeV1, MemoryStatusRequestV1, MemoryStatusResultV1,
    MemoryStatusV1, RetainedFactIdV1, RetainedOutcomeStatusV1, RetainedProjectSelectorV1,
    RetainedSurfaceOperation, RetainedSurfaceResultV1, TrustHistoryEntryV1,
};
use tracedecay_application::{
    ApplicationOutcome, RetainedMemoryExecutionPortV1, RetainedMemoryRequestV1,
    RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfaceExecutionFutureV1,
};
use tracedecay_domain::{FactOwnerV1, ManifestDigest};
use tracedecay_runtime_core::memory::types::{
    ContradictionResult, FactRecord, FactSearchResult, MemoryCategory, MemoryStatus,
    SearchFactsRequest, TrustHistoryEntry,
};
use tracedecay_usecases::memory::{
    MemoryApplication, MemoryOperationContext, memory_application_error,
};

use super::receipts::evidence_outcome;
use super::{bounded_execution, map_execution_error};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::Database;
use crate::errors::TraceDecayError;
use crate::store::DatabaseFactStore;
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
        _: &RetainedSurfaceExecutionContextV1<'_>,
        _: &FactStoreAddRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        unsupported_memory_effect()
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
        _: &RetainedSurfaceExecutionContextV1<'_>,
        _: &FactStoreUpdateRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        unsupported_memory_effect()
    }

    async fn execute_remove(
        &self,
        _: &RetainedSurfaceExecutionContextV1<'_>,
        _: &FactStoreRemoveRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        unsupported_memory_effect()
    }

    async fn execute_feedback(
        &self,
        _: &RetainedSurfaceExecutionContextV1<'_>,
        _: &FactFeedbackRequestV1,
    ) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
        unsupported_memory_effect()
    }
}

fn unsupported_memory_effect<T>() -> Result<T, RetainedSurfaceExecutionErrorV1> {
    // The canonical memory use cases currently return only post-write
    // projections. Until the lower authority returns its durable commit
    // receipt, an effect cannot truthfully populate the application effect
    // contract. Reject before opening a mounted store or issuing a write.
    Err(RetainedSurfaceExecutionErrorV1::Unsupported)
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
        search_entries(hits)?
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
        search_entries(hits)?
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
        search_entries(hits)?
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
    let limit = fact_limit(request.options.limit).map_err(map_execution_error)?;
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
        search_entries(hits)?
    )
}

async fn contradict_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreContradictRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner)?;
    let limit = fact_limit(request.options.limit).map_err(map_execution_error)?;
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
            .map(contradiction)
            .map(|entry| entry.map(FactCollectionEntryV1::Contradiction))
            .collect::<Result<Vec<_>, _>>()?
    )
}

async fn get_on_db(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    database: &Database,
    owner: FactOwnerV1,
    request: &FactStoreGetRequestV1,
) -> Result<ApplicationOutcome<RetainedSurfaceResultV1>, RetainedSurfaceExecutionErrorV1> {
    let memory = memory_application(database, owner)?;
    let fact_id = fact_id(&request.fact_id)?;
    let fact_record = bounded_execution(context, async {
        memory
            .get_fact(fact_id)
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let fact_record =
        fact_record.ok_or(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)?;
    let trust_history = bounded_execution(context, async {
        memory
            .fact_trust_history(fact_id, MAX_RETAINED_FACT_LIMIT)
            .await
            .map_err(memory_application_error)
    })
    .await?;
    let result = RetainedSurfaceResultV1::FactStoreGet(FactStoreGetResultV1 {
        count: 1,
        fact: Some(fact(fact_record)?),
        trust_history: trust_history.into_iter().map(trust_history_entry).collect(),
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
    let limit = fact_limit(request.options.limit).map_err(map_execution_error)?;
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
            .map(fact)
            .map(|entry| entry.map(FactCollectionEntryV1::Fact))
            .collect::<Result<Vec<_>, _>>()?
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
    hits: Vec<FactSearchResult>,
) -> Result<Vec<FactCollectionEntryV1>, RetainedSurfaceExecutionErrorV1> {
    hits.into_iter()
        .map(search_hit)
        .map(|entry| entry.map(FactCollectionEntryV1::Search))
        .collect()
}

fn fact_id(id: &RetainedFactIdV1) -> Result<i64, RetainedSurfaceExecutionErrorV1> {
    let value = match id {
        RetainedFactIdV1::Numeric(value) => {
            i64::try_from(*value).map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?
        }
        RetainedFactIdV1::Text(value) => value
            .parse::<i64>()
            .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?,
    };
    if value <= 0 {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    Ok(value)
}

fn fact(record: FactRecord) -> Result<FactV1, RetainedSurfaceExecutionErrorV1> {
    let metadata = match record.metadata {
        Value::Object(metadata) => metadata.into_iter().collect(),
        _ => return Err(RetainedSurfaceExecutionErrorV1::Unavailable),
    };
    Ok(FactV1 {
        fact_id: record.fact_id,
        content: record.content,
        category: fact_category(record.category),
        tags: record.tags,
        entities: record.entities,
        trust_score: record.trust_score,
        source: record.source,
        retrieval_count: record.retrieval_count,
        access_count: record.access_count,
        helpful_count: record.helpful_count,
        unhelpful_count: record.unhelpful_count,
        created_at: record.created_at,
        updated_at: record.updated_at,
        last_retrieved_at: record.last_retrieved_at,
        last_recalled_at: record.last_recalled_at,
        last_feedback_at: record.last_feedback_at,
        metadata,
    })
}

fn search_hit(hit: FactSearchResult) -> Result<FactSearchHitV1, RetainedSurfaceExecutionErrorV1> {
    Ok(FactSearchHitV1 {
        fact: fact(hit.fact)?,
        score: hit.score,
        fts_score: hit.fts_score,
        jaccard_score: hit.jaccard_score,
        holographic_score: hit.holographic_score,
        trust_score: hit.trust_score,
        why: hit.why,
    })
}

fn contradiction(
    contradiction: ContradictionResult,
) -> Result<FactContradictionV1, RetainedSurfaceExecutionErrorV1> {
    Ok(FactContradictionV1 {
        existing_fact: fact(contradiction.existing_fact)?,
        new_content: contradiction.new_content,
        score: contradiction.score,
        why: contradiction.why,
    })
}

fn trust_history_entry(entry: TrustHistoryEntry) -> TrustHistoryEntryV1 {
    TrustHistoryEntryV1 {
        timestamp: entry.timestamp,
        action: match entry.action {
            tracedecay_runtime_core::memory::types::FeedbackAction::Helpful => {
                tracedecay_application::retained_surfaces::FactFeedbackActionV1::Helpful
            }
            tracedecay_runtime_core::memory::types::FeedbackAction::Unhelpful => {
                tracedecay_application::retained_surfaces::FactFeedbackActionV1::Unhelpful
            }
        },
        old_trust: entry.old_trust,
        new_trust: entry.new_trust,
        delta: entry.delta,
        source: entry.source,
        note: entry.note,
    }
}

const fn fact_category(category: MemoryCategory) -> FactCategoryV1 {
    match category {
        MemoryCategory::General => FactCategoryV1::General,
        MemoryCategory::UserPref => FactCategoryV1::UserPref,
        MemoryCategory::Project => FactCategoryV1::Project,
        MemoryCategory::Tool => FactCategoryV1::Tool,
        MemoryCategory::Decision => FactCategoryV1::Decision,
        MemoryCategory::CodeArea => FactCategoryV1::CodeArea,
    }
}

fn memory_status(status: MemoryStatus) -> MemoryStatusV1 {
    MemoryStatusV1 {
        fact_count: status.fact_count,
        entity_count: status.entity_count,
        bank_count: status.bank_count,
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
        missing_vector_count: status.missing_vector_count,
        repair: MemoryRepairStatsV1 {
            missing_vectors_repaired: status.repair.missing_vectors_repaired,
            banks_rebuilt: status.repair.banks_rebuilt,
        },
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
    MemoryOperationContext::from_request_id(
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

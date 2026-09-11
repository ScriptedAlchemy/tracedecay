//! Canonical retained-memory request and result mapping.

use crate::memory::{
    MemoryApplicationError, ProjectMemoryFactAddRequest, ProjectMemoryFactAddRequestOutcome,
};
use serde::Serialize;
use serde_json::Value;
use tracedecay_contracts::RetainedSurfaceExecutionErrorV1;
use tracedecay_contracts::retained_surfaces::{
    FactCategoryV1, FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1,
    FactContradictionV1, FactFeedbackActionV1, FactFeedbackDetailsAvailabilityV1,
    FactFeedbackRequestV1, FactFeedbackV1, FactIdentitySourceResultV1, FactPayloadAccessV1,
    FactProjectionV1, FactReadOptionsV1, FactRetrievalTelemetryDegradationV1,
    FactRetrievalTelemetryV1, FactSearchCursorV1, FactSearchGraphCoverageV1,
    FactSearchGraphDegradationV1, FactSearchHitV1, FactSearchScoresV1, FactSourceLabelPatchV1,
    FactStatusV1, FactStoreAddCommitV1, FactStoreAddRequestV1, FactStoreAddResultV1,
    FactStoreContradictResultV1, FactStoreGetResultV1, FactStoreListResultV1,
    FactStoreProbeResultV1, FactStoreReasonResultV1, FactStoreRelatedResultV1,
    FactStoreRemoveRequestV1, FactStoreRemoveResultV1, FactStoreSearchRequestV1,
    FactStoreSearchResultV1, FactStoreSupersedeRequestV1, FactStoreSupersedeResultV1,
    FactStoreUpdateRequestV1, FactStoreUpdateResultV1, FactTelemetryV1, FactV1, MemoryAlgebraV1,
    MemoryFeedbackFunnelV1, MemoryScopeV1, MemoryStatusResultV1, MemoryStatusV1,
    RetainedProjectSelectorV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    TrustHistoryEntryV1,
};
use tracedecay_domain::{
    ActorId, Confidence, FactId, FactIdentitySourceV1, FactOwnerV1, PayloadAccessState,
    ProvenanceId,
};
use tracedecay_store::{
    FactCommitReceipt, FactStoreError, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactContradictionPageV1, ProjectMemoryFactFeedbackActionV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackDetailsAvailabilityV1,
    ProjectMemoryFactFeedbackHistoryV1, ProjectMemoryFactIdV1, ProjectMemoryFactPageV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactSearchCursorV1,
    ProjectMemoryFactSearchFilterV1, ProjectMemoryFactSearchGraphCoverageV1,
    ProjectMemoryFactSearchGraphDegradationV1, ProjectMemoryFactSearchHitV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchQuery,
    ProjectMemoryFactSupersedeCommandV1, ProjectMemoryFactSupersedeOutcomeV1,
    ProjectMemoryFactUnavailableV1, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, ProjectMemoryFactUpdatePatchV1, ProjectMemoryFactV1,
    ProjectMemoryMemoryStatusV1,
};

pub const MAX_RETAINED_FACT_LIMIT: usize = 200;
pub const MAX_RETAINED_FEEDBACK_HISTORY_LIMIT: usize = 1_000;

pub fn read_scope(
    options: &FactReadOptionsV1,
) -> (Option<MemoryScopeV1>, Option<&RetainedProjectSelectorV1>) {
    (options.memory_scope, options.project_selector.as_ref())
}

pub fn normalize_reason_entities(
    entities: &[String],
) -> Result<Vec<String>, RetainedSurfaceExecutionErrorV1> {
    if entities.is_empty() {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    let mut entities = entities.to_vec();
    entities.sort();
    if entities.windows(2).any(|pair| pair.first() == pair.get(1)) {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    Ok(entities)
}

pub fn ensure_profile_request_scope(
    memory_scope: Option<MemoryScopeV1>,
    selector: Option<&RetainedProjectSelectorV1>,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    if memory_scope != Some(MemoryScopeV1::User) || selector.is_some() {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    Ok(())
}

pub fn add_request(
    request: &FactStoreAddRequestV1,
) -> Result<ProjectMemoryFactAddRequest, RetainedSurfaceExecutionErrorV1> {
    Ok(ProjectMemoryFactAddRequest {
        content: request.content.clone(),
        category: request.category.unwrap_or(FactCategoryV1::General),
        source_label: request.source_label.clone(),
        tags: request.tags.clone(),
        entities: request.entities.clone(),
        trust: confidence(request.trust)?,
        metadata: Value::Object(
            request
                .metadata
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        ),
    })
}

fn update_patch(
    request: &FactStoreUpdateRequestV1,
) -> Result<ProjectMemoryFactUpdatePatchV1, RetainedSurfaceExecutionErrorV1> {
    let source_label = request.source_label.as_ref().map(|patch| match patch {
        FactSourceLabelPatchV1::Set { value } => Some(value.clone()),
        FactSourceLabelPatchV1::Clear => None,
    });
    ProjectMemoryFactUpdatePatchV1::new(
        request.content.clone(),
        request.category,
        source_label,
        request.tags.clone(),
        request.entities.clone(),
        request.metadata.as_ref().map(|metadata| {
            Value::Object(
                metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        }),
        confidence(request.trust)?,
    )
    .map_err(map_store_error)
}

fn fact_target(
    owner: FactOwnerV1,
    fact_id: &FactId,
) -> Result<ProjectMemoryFactIdV1, RetainedSurfaceExecutionErrorV1> {
    ProjectMemoryFactIdV1::new(owner, fact_id.clone()).map_err(map_store_error)
}

fn logical_effect_value<T: Serialize>(
    effect: &T,
) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
    serde_json::to_value(effect).map_err(|error| {
        RetainedSurfaceExecutionErrorV1::unavailable(format!(
            "the memory effect payload could not be serialized: {error}"
        ))
    })
}

// Retained memory operations validated once at the mapping boundary.
//
// Logical-effect identity (the digest that makes a retry the same durable
// operation) and the store command or query are both derived from one set of
// normalized values, so they cannot disagree, and the owned patch values move
// into the command instead of being rebuilt from the public request. The
// serialized effect tuples are durable identity inputs: their labels, field
// order, and wire shapes must not change without a digest version.

/// A fact update whose target and patch validated once.
pub struct PreparedFactUpdate<'a> {
    target: ProjectMemoryFactIdV1,
    patch: ProjectMemoryFactUpdatePatchV1,
    request: &'a FactStoreUpdateRequestV1,
}

impl<'a> PreparedFactUpdate<'a> {
    pub fn new(
        owner: FactOwnerV1,
        request: &'a FactStoreUpdateRequestV1,
    ) -> Result<Self, RetainedSurfaceExecutionErrorV1> {
        Ok(Self {
            target: fact_target(owner, &request.fact_id)?,
            patch: update_patch(request)?,
            request,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.target.owner()
    }

    pub fn logical_effect(&self) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
        logical_effect_value(&(
            "project-memory-fact-update.v1",
            self.target.owner(),
            self.target.fact_id(),
            &self.request.expected_last_event_id,
            self.patch.content(),
            self.patch.category(),
            // The public patch keeps `Clear` distinct from "not patched"; the
            // store patch's nested `Option` would flatten both to `null`.
            &self.request.source_label,
            self.patch.tags(),
            self.patch.entities(),
            self.patch.trust(),
            self.patch.metadata(),
        ))
    }

    pub fn into_command(
        self,
        operation_id: ProvenanceId,
        actor: ActorId,
    ) -> Result<ProjectMemoryFactUpdateCommandV1, RetainedSurfaceExecutionErrorV1> {
        ProjectMemoryFactUpdateCommandV1::new(
            self.target,
            operation_id,
            self.request.expected_last_event_id.clone(),
            self.patch,
            Some(actor),
        )
        .map_err(map_store_error)
    }
}

/// A fact removal whose target validated once.
pub struct PreparedFactRemove<'a> {
    target: ProjectMemoryFactIdV1,
    request: &'a FactStoreRemoveRequestV1,
}

impl<'a> PreparedFactRemove<'a> {
    pub fn new(
        owner: FactOwnerV1,
        request: &'a FactStoreRemoveRequestV1,
    ) -> Result<Self, RetainedSurfaceExecutionErrorV1> {
        Ok(Self {
            target: fact_target(owner, &request.fact_id)?,
            request,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.target.owner()
    }

    pub fn logical_effect(&self) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
        logical_effect_value(&(
            "project-memory-fact-remove.v1",
            self.target.owner(),
            self.target.fact_id(),
            &self.request.expected_last_event_id,
        ))
    }

    pub fn into_command(
        self,
        operation_id: ProvenanceId,
        actor: ActorId,
    ) -> Result<ProjectMemoryFactRemoveCommandV1, RetainedSurfaceExecutionErrorV1> {
        ProjectMemoryFactRemoveCommandV1::new(
            self.target,
            operation_id,
            self.request.expected_last_event_id.clone(),
            Some(actor),
        )
        .map_err(map_store_error)
    }
}

/// A fact supersession whose target and successor validated once.
pub struct PreparedFactSupersede<'a> {
    target: ProjectMemoryFactIdV1,
    successor: ProjectMemoryFactIdV1,
    request: &'a FactStoreSupersedeRequestV1,
}

impl<'a> PreparedFactSupersede<'a> {
    pub fn new(
        owner: FactOwnerV1,
        request: &'a FactStoreSupersedeRequestV1,
    ) -> Result<Self, RetainedSurfaceExecutionErrorV1> {
        Ok(Self {
            target: fact_target(owner.clone(), &request.fact_id)?,
            successor: fact_target(owner, &request.superseded_by)?,
            request,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.target.owner()
    }

    pub fn logical_effect(&self) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
        logical_effect_value(&(
            "project-memory-fact-supersede.v1",
            self.target.owner(),
            self.target.fact_id(),
            self.successor.fact_id(),
            &self.request.expected_last_event_id,
        ))
    }

    pub fn into_command(
        self,
        operation_id: ProvenanceId,
        actor: ActorId,
    ) -> Result<ProjectMemoryFactSupersedeCommandV1, RetainedSurfaceExecutionErrorV1> {
        ProjectMemoryFactSupersedeCommandV1::new(
            self.target,
            self.successor,
            operation_id,
            self.request.expected_last_event_id.clone(),
            Some(actor),
        )
        .map_err(map_store_error)
    }
}

/// Fact feedback whose target validated once.
pub struct PreparedFactFeedback<'a> {
    target: ProjectMemoryFactIdV1,
    request: &'a FactFeedbackRequestV1,
}

impl<'a> PreparedFactFeedback<'a> {
    pub fn new(
        owner: FactOwnerV1,
        request: &'a FactFeedbackRequestV1,
    ) -> Result<Self, RetainedSurfaceExecutionErrorV1> {
        Ok(Self {
            target: fact_target(owner, &request.fact_id)?,
            request,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.target.owner()
    }

    pub fn logical_effect(&self) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
        logical_effect_value(&(
            "project-memory-fact-feedback.v1",
            self.target.owner(),
            self.target.fact_id(),
            &self.request.expected_last_event_id,
            self.request.action,
            &self.request.source_label,
            &self.request.reason,
        ))
    }

    pub fn into_command(
        self,
        operation_id: ProvenanceId,
        actor: ActorId,
    ) -> Result<ProjectMemoryFactFeedbackCommandV1, RetainedSurfaceExecutionErrorV1> {
        ProjectMemoryFactFeedbackCommandV1::new(
            self.target,
            operation_id,
            self.request.expected_last_event_id.clone(),
            feedback_action(self.request.action),
            Some(actor),
            self.request.source_label.clone(),
            self.request.reason.clone(),
        )
        .map_err(map_store_error)
    }
}

/// Trust floor an exact search applies when the request names none. The store
/// applies the same floor to a filter without one, so passing it explicitly
/// keeps the query and the identity on one value without changing results.
const DEFAULT_SEARCH_MIN_TRUST: f64 = 0.3;

/// An exact fact search whose query, trust floor, and page size validated
/// once; probe/related/reason reads keep using [`search_query`] because they
/// admit no retained effect.
pub struct PreparedFactSearch<'a> {
    query: ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    request: &'a FactStoreSearchRequestV1,
}

impl<'a> PreparedFactSearch<'a> {
    pub fn new(
        owner: FactOwnerV1,
        request: &'a FactStoreSearchRequestV1,
    ) -> Result<Self, RetainedSurfaceExecutionErrorV1> {
        let min_trust = Confidence::new(
            request
                .options
                .min_trust
                .unwrap_or(DEFAULT_SEARCH_MIN_TRUST),
        )
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?;
        let query = fact_search_query(
            owner,
            ProjectMemoryFactSearchKindV1::Search,
            Some(request.query.clone()),
            request.options.category,
            Some(min_trust),
            fact_limit(request.options.limit)?,
            request.after.as_ref(),
        )?;
        Ok(Self {
            query,
            min_trust,
            request,
        })
    }

    pub fn logical_effect(&self) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
        logical_effect_value(&(
            "project-memory-fact-search.v1",
            self.query.owner(),
            self.query.query(),
            self.query.filter().category(),
            self.min_trust,
            self.query.limit(),
            &self.request.after,
        ))
    }

    pub fn into_query(self) -> ProjectMemoryFactSearchQuery {
        self.query
    }
}

pub fn search_query(
    owner: FactOwnerV1,
    kind: ProjectMemoryFactSearchKindV1,
    query: Option<String>,
    options: &FactReadOptionsV1,
    after: Option<&FactSearchCursorV1>,
) -> Result<ProjectMemoryFactSearchQuery, RetainedSurfaceExecutionErrorV1> {
    fact_search_query(
        owner,
        kind,
        query,
        options.category,
        confidence(options.min_trust)?,
        fact_limit(options.limit)?,
        after,
    )
}

fn fact_search_query(
    owner: FactOwnerV1,
    kind: ProjectMemoryFactSearchKindV1,
    query: Option<String>,
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
    limit: usize,
    after: Option<&FactSearchCursorV1>,
) -> Result<ProjectMemoryFactSearchQuery, RetainedSurfaceExecutionErrorV1> {
    let filter =
        ProjectMemoryFactSearchFilterV1::new(category, min_trust, None).map_err(map_store_error)?;
    let after = after
        .map(|cursor| {
            ProjectMemoryFactSearchCursorV1::new(
                cursor.score_millionths,
                cursor.updated_at,
                cursor.fact_id.clone(),
            )
        })
        .transpose()
        .map_err(map_store_error)?;
    ProjectMemoryFactSearchQuery::with_filter(owner, kind, query, filter, after, limit)
        .map_err(map_store_error)
}

pub fn fact_limit(limit: Option<u64>) -> Result<usize, RetainedSurfaceExecutionErrorV1> {
    let limit = limit
        .map(usize::try_from)
        .transpose()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)?
        .unwrap_or(20);
    if !(1..=MAX_RETAINED_FACT_LIMIT).contains(&limit) {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    Ok(limit)
}

pub fn confidence(
    value: Option<f64>,
) -> Result<Option<Confidence>, RetainedSurfaceExecutionErrorV1> {
    value
        .map(Confidence::new)
        .transpose()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
}

pub const fn feedback_action(action: FactFeedbackActionV1) -> ProjectMemoryFactFeedbackActionV1 {
    match action {
        FactFeedbackActionV1::Helpful => ProjectMemoryFactFeedbackActionV1::Helpful,
        FactFeedbackActionV1::Unhelpful => ProjectMemoryFactFeedbackActionV1::Unhelpful,
    }
}

fn public_feedback_action(action: ProjectMemoryFactFeedbackActionV1) -> FactFeedbackActionV1 {
    match action {
        ProjectMemoryFactFeedbackActionV1::Helpful => FactFeedbackActionV1::Helpful,
        ProjectMemoryFactFeedbackActionV1::Unhelpful => FactFeedbackActionV1::Unhelpful,
    }
}

pub fn public_owner(owner: &FactOwnerV1) -> FactCommitOwnerV1 {
    match owner {
        FactOwnerV1::Profile => FactCommitOwnerV1::Profile,
        FactOwnerV1::Project { project_id } => FactCommitOwnerV1::Project {
            project_id: project_id.clone(),
        },
    }
}

pub fn commit_receipt(receipt: &FactCommitReceipt, replayed: bool) -> FactCommitReceiptV1 {
    FactCommitReceiptV1 {
        disposition: if replayed {
            FactCommitDispositionV1::IdempotentReplay
        } else {
            FactCommitDispositionV1::Committed
        },
        fact_id: receipt.fact_id().clone(),
        owner: public_owner(receipt.owner()),
        committed_event_ids: receipt.committed_event_ids().to_vec(),
        last_event_id: receipt.last_event_id().clone(),
        active_assertion_id: receipt.active_assertion_id().cloned(),
    }
}

pub fn projection(
    projection: &ProjectMemoryFactProjectionV1,
) -> Result<FactProjectionV1, RetainedSurfaceExecutionErrorV1> {
    match projection {
        ProjectMemoryFactProjectionV1::Available(fact) => {
            let fact_projection = Box::new(available_fact(fact)?);
            match fact.superseded_by() {
                Some(superseded_by) => Ok(FactProjectionV1::Superseded {
                    fact: fact_projection,
                    superseded_by: superseded_by.clone(),
                }),
                None => Ok(FactProjectionV1::Available {
                    fact: fact_projection,
                }),
            }
        }
        ProjectMemoryFactProjectionV1::Unavailable(fact) => Ok(FactProjectionV1::Unavailable {
            status: unavailable_fact(fact)?,
        }),
    }
}

pub fn available_fact(
    fact: &ProjectMemoryFactV1,
) -> Result<FactV1, RetainedSurfaceExecutionErrorV1> {
    let metadata = match fact.metadata() {
        Value::Object(metadata) => metadata.clone().into_iter().collect(),
        _ => {
            return Err(RetainedSurfaceExecutionErrorV1::unavailable(
                "the stored fact metadata payload is not a JSON object",
            ));
        }
    };
    let source = match fact.source() {
        FactIdentitySourceV1::Evidence {
            anchor_id,
            stable_key,
        } => FactIdentitySourceResultV1::Evidence {
            anchor_id: anchor_id.clone(),
            stable_key: stable_key.clone(),
        },
        FactIdentitySourceV1::Application { operation_id } => {
            FactIdentitySourceResultV1::Application {
                operation_id: operation_id.clone(),
            }
        }
    };
    let telemetry = fact.telemetry();
    Ok(FactV1 {
        owner: public_owner(fact.owner()),
        fact_id: fact.fact_id().clone(),
        content: fact.content().to_owned(),
        category: fact.category(),
        tags: fact.tags().to_vec(),
        entities: fact.entities().to_vec(),
        trust_score_millionths: confidence_millionths(fact.trust()),
        source,
        source_label: fact.source_label().map(str::to_owned),
        active_assertion_id: fact.active_assertion_id().clone(),
        last_event_id: fact.last_event_id().clone(),
        projected_as_of: fact.projected_as_of(),
        telemetry: FactTelemetryV1 {
            retrieval_count: telemetry.retrieval_count(),
            access_count: telemetry.access_count(),
            helpful_count: telemetry.helpful_count(),
            unhelpful_count: telemetry.unhelpful_count(),
            created_at: telemetry.created_at(),
            updated_at: telemetry.updated_at(),
            last_retrieved_at: telemetry.last_retrieved_at(),
            last_recalled_at: telemetry.last_recalled_at(),
            last_feedback_at: telemetry.last_feedback_at(),
        },
        metadata,
    })
}

fn unavailable_fact(
    fact: &ProjectMemoryFactUnavailableV1,
) -> Result<FactStatusV1, RetainedSurfaceExecutionErrorV1> {
    let status = fact.status();
    let payload_access = match status.payload_access() {
        PayloadAccessState::Eligible => {
            return Err(RetainedSurfaceExecutionErrorV1::unavailable(
                "an unavailable fact projection reported an eligible payload state",
            ));
        }
        PayloadAccessState::Redacted => FactPayloadAccessV1::Redacted,
        PayloadAccessState::Quarantined => FactPayloadAccessV1::Quarantined,
        PayloadAccessState::RetentionExpired => FactPayloadAccessV1::RetentionExpired,
        PayloadAccessState::Deleted => FactPayloadAccessV1::Deleted,
        PayloadAccessState::Unavailable => FactPayloadAccessV1::Unavailable,
        PayloadAccessState::Ambiguous => FactPayloadAccessV1::Ambiguous,
    };
    Ok(FactStatusV1 {
        owner: public_owner(status.owner()),
        fact_id: status.fact_id().clone(),
        payload_access,
        projected_as_of: status.projected_as_of(),
    })
}

pub fn search_page(
    page: &ProjectMemoryFactSearchPageV1,
) -> Result<MappedSearchPageV1, RetainedSurfaceExecutionErrorV1> {
    Ok(MappedSearchPageV1 {
        owner: public_owner(page.owner()),
        hits: page
            .hits()
            .iter()
            .map(search_hit)
            .collect::<Result<Vec<_>, _>>()?,
        next_after: page.next_after().map(search_cursor),
        graph_coverage: graph_coverage(page.graph_coverage()),
    })
}

pub struct MappedSearchPageV1 {
    pub owner: FactCommitOwnerV1,
    pub hits: Vec<FactSearchHitV1>,
    pub next_after: Option<FactSearchCursorV1>,
    pub graph_coverage: FactSearchGraphCoverageV1,
}

pub fn probe_result(page: MappedSearchPageV1) -> FactStoreProbeResultV1 {
    FactStoreProbeResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    }
}

pub fn related_result(page: MappedSearchPageV1) -> FactStoreRelatedResultV1 {
    FactStoreRelatedResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    }
}

pub fn reason_result(page: MappedSearchPageV1) -> FactStoreReasonResultV1 {
    FactStoreReasonResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    }
}

pub fn exact_search_result(
    page: MappedSearchPageV1,
    retrieval_telemetry: FactRetrievalTelemetryV1,
) -> RetainedSurfaceResultV1 {
    RetainedSurfaceResultV1::FactStoreSearch(FactStoreSearchResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
        retrieval_telemetry,
    })
}

/// Degradations of the retrieval-telemetry write lane that a served search
/// result absorbs as a typed state instead of a refusal. Request-scoped
/// terminals (cancellation, timeout, invalid request) and store-health
/// signals (reset required) still fail the operation.
pub fn retrieval_telemetry_degradation(
    error: &RetainedSurfaceExecutionErrorV1,
) -> Option<FactRetrievalTelemetryDegradationV1> {
    match error {
        RetainedSurfaceExecutionErrorV1::Unavailable { .. } => {
            Some(FactRetrievalTelemetryDegradationV1::Unavailable)
        }
        RetainedSurfaceExecutionErrorV1::Saturated => {
            Some(FactRetrievalTelemetryDegradationV1::Saturated)
        }
        _ => None,
    }
}

pub fn semantic_search_result(
    operation: RetainedSurfaceOperation,
    page: MappedSearchPageV1,
) -> Result<RetainedSurfaceResultV1, RetainedSurfaceExecutionErrorV1> {
    match operation {
        RetainedSurfaceOperation::FactStoreProbe => {
            Ok(RetainedSurfaceResultV1::FactStoreProbe(probe_result(page)))
        }
        RetainedSurfaceOperation::FactStoreRelated => Ok(
            RetainedSurfaceResultV1::FactStoreRelated(related_result(page)),
        ),
        RetainedSurfaceOperation::FactStoreReason => Ok(RetainedSurfaceResultV1::FactStoreReason(
            reason_result(page),
        )),
        _ => Err(RetainedSurfaceExecutionErrorV1::InvalidRequest),
    }
}

pub fn refresh_search_hits(
    page: &mut MappedSearchPageV1,
    projections: &[ProjectMemoryFactProjectionV1],
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    for hit in &mut page.hits {
        let projection = projections
            .iter()
            .find(|projection| projection.fact_id() == &hit.fact.fact_id)
            .ok_or_else(|| {
                RetainedSurfaceExecutionErrorV1::unavailable(
                    "a refreshed search hit lost its fact projection",
                )
            })?;
        let ProjectMemoryFactProjectionV1::Available(fact) = projection else {
            return Err(RetainedSurfaceExecutionErrorV1::unavailable(
                "a refreshed search hit projection is no longer available",
            ));
        };
        hit.fact = available_fact(fact)?;
    }
    Ok(())
}

fn search_hit(
    hit: &ProjectMemoryFactSearchHitV1,
) -> Result<FactSearchHitV1, RetainedSurfaceExecutionErrorV1> {
    let scores = hit.scores();
    Ok(FactSearchHitV1 {
        fact: available_fact(hit.fact())?,
        scores: FactSearchScoresV1 {
            score_millionths: scores.score_millionths(),
            fts_score_millionths: scores.fts_score_millionths(),
            jaccard_score_millionths: scores.jaccard_score_millionths(),
            holographic_score_millionths: scores.holographic_score_millionths(),
            trust_score_millionths: scores.trust_score_millionths(),
        },
        why: hit.why().map(str::to_owned),
    })
}

fn search_cursor(cursor: &tracedecay_store::ProjectMemoryFactSearchCursorV1) -> FactSearchCursorV1 {
    FactSearchCursorV1 {
        score_millionths: cursor.score_millionths(),
        updated_at: cursor.updated_at(),
        fact_id: cursor.fact_id().clone(),
    }
}

fn graph_coverage(coverage: ProjectMemoryFactSearchGraphCoverageV1) -> FactSearchGraphCoverageV1 {
    match coverage {
        ProjectMemoryFactSearchGraphCoverageV1::NotApplicable => {
            FactSearchGraphCoverageV1::NotApplicable
        }
        ProjectMemoryFactSearchGraphCoverageV1::NotMounted => FactSearchGraphCoverageV1::NotMounted,
        ProjectMemoryFactSearchGraphCoverageV1::Complete {
            root_count,
            relation_count,
            expanded_fact_count,
        } => FactSearchGraphCoverageV1::Complete {
            root_count,
            relation_count,
            expanded_fact_count,
        },
        ProjectMemoryFactSearchGraphCoverageV1::Degraded { reason } => {
            FactSearchGraphCoverageV1::Degraded {
                reason: match reason {
                    ProjectMemoryFactSearchGraphDegradationV1::Conflict => {
                        FactSearchGraphDegradationV1::Conflict
                    }
                    ProjectMemoryFactSearchGraphDegradationV1::Unavailable => {
                        FactSearchGraphDegradationV1::Unavailable
                    }
                    ProjectMemoryFactSearchGraphDegradationV1::BudgetExhausted => {
                        FactSearchGraphDegradationV1::BudgetExhausted
                    }
                    ProjectMemoryFactSearchGraphDegradationV1::DeadlineExceeded => {
                        FactSearchGraphDegradationV1::DeadlineExceeded
                    }
                },
            }
        }
    }
}

pub fn contradiction_page(
    page: &ProjectMemoryFactContradictionPageV1,
) -> Result<FactStoreContradictResultV1, RetainedSurfaceExecutionErrorV1> {
    Ok(FactStoreContradictResultV1 {
        owner: public_owner(page.owner()),
        contradictions: page
            .contradictions()
            .iter()
            .map(|entry| {
                Ok(FactContradictionV1 {
                    existing_fact: available_fact(entry.existing())?,
                    new_content: entry.new_content().to_owned(),
                    score_millionths: entry.score_millionths(),
                    why: entry.why().map(str::to_owned),
                })
            })
            .collect::<Result<Vec<_>, RetainedSurfaceExecutionErrorV1>>()?,
    })
}

pub fn feedback_history(
    history: &ProjectMemoryFactFeedbackHistoryV1,
) -> Result<Vec<TrustHistoryEntryV1>, RetainedSurfaceExecutionErrorV1> {
    if history.next_after().is_some() {
        return Err(RetainedSurfaceExecutionErrorV1::Saturated);
    }
    Ok(history
        .events()
        .iter()
        .map(|entry| TrustHistoryEntryV1 {
            event_id: entry.event_id().clone(),
            occurred_at: entry.occurred_at(),
            action: public_feedback_action(entry.action()),
            old_trust_millionths: confidence_millionths(entry.old_trust()),
            new_trust_millionths: confidence_millionths(entry.new_trust()),
            source_label: entry.source().map(str::to_owned),
            reason: entry.note().map(str::to_owned),
            details_availability: match entry.details_availability() {
                ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available => {
                    FactFeedbackDetailsAvailabilityV1::Available
                }
                ProjectMemoryFactFeedbackDetailsAvailabilityV1::Redacted => {
                    FactFeedbackDetailsAvailabilityV1::Redacted
                }
                ProjectMemoryFactFeedbackDetailsAvailabilityV1::Unknown => {
                    FactFeedbackDetailsAvailabilityV1::Unknown
                }
            },
        })
        .collect())
}

pub fn get_result(
    fact_projection: &ProjectMemoryFactProjectionV1,
    history: &ProjectMemoryFactFeedbackHistoryV1,
) -> Result<RetainedSurfaceResultV1, RetainedSurfaceExecutionErrorV1> {
    Ok(RetainedSurfaceResultV1::FactStoreGet(
        FactStoreGetResultV1 {
            fact: projection(fact_projection)?,
            trust_history: feedback_history(history)?,
        },
    ))
}

pub fn status_result(status: &ProjectMemoryMemoryStatusV1) -> RetainedSurfaceResultV1 {
    RetainedSurfaceResultV1::MemoryStatus(memory_status_result(status))
}

pub fn memory_status_result(status: &ProjectMemoryMemoryStatusV1) -> MemoryStatusResultV1 {
    MemoryStatusResultV1 {
        memory: memory_status(status),
    }
}

pub fn list_page(
    page: &ProjectMemoryFactPageV1,
) -> Result<FactStoreListResultV1, RetainedSurfaceExecutionErrorV1> {
    Ok(FactStoreListResultV1 {
        owner: public_owner(page.owner()),
        facts: page
            .facts()
            .iter()
            .map(projection)
            .collect::<Result<Vec<_>, _>>()?,
        next_after_fact_id: page.next_after_fact_id().cloned(),
    })
}

pub fn add_result(
    outcome: &ProjectMemoryFactAddRequestOutcome,
) -> Result<FactStoreAddResultV1, RetainedSurfaceExecutionErrorV1> {
    let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
        return Ok(FactStoreAddResultV1::SecretRejected);
    };
    let fact = projection(outcome.fact())?;
    let closest = outcome
        .closest_fact_id()
        .map(|target| target.fact_id().clone());
    let Some(receipt) = outcome.commit_receipt() else {
        if outcome.disposition() != ProjectMemoryFactAddDispositionV1::NearDuplicate
            || outcome.commit_replayed()
        {
            return Err(RetainedSurfaceExecutionErrorV1::unavailable(
                "the fact add outcome carried no commit receipt",
            ));
        }
        return Ok(FactStoreAddResultV1::NormalizedDuplicate {
            fact,
            closest_fact_id: closest.ok_or_else(|| {
                RetainedSurfaceExecutionErrorV1::unavailable(
                    "the normalized-duplicate fact outcome carried no closest fact id",
                )
            })?,
        });
    };
    let commit = commit_receipt(receipt, outcome.commit_replayed());
    let result = match outcome.disposition() {
        ProjectMemoryFactAddDispositionV1::Added => FactStoreAddCommitV1::Added { fact, commit },
        ProjectMemoryFactAddDispositionV1::NearDuplicate => FactStoreAddCommitV1::NearDuplicate {
            fact,
            closest_fact_id: closest.ok_or_else(near_duplicate_missing_closest)?,
            similarity_millionths: outcome
                .similarity_millionths()
                .ok_or_else(near_duplicate_missing_similarity)?,
            commit,
        },
        ProjectMemoryFactAddDispositionV1::PossibleConflict => {
            FactStoreAddCommitV1::PossibleConflict {
                fact,
                closest_fact_id: closest.ok_or_else(near_duplicate_missing_closest)?,
                similarity_millionths: outcome
                    .similarity_millionths()
                    .ok_or_else(near_duplicate_missing_similarity)?,
                commit,
            }
        }
    };
    Ok(FactStoreAddResultV1::Committed { result })
}

fn near_duplicate_missing_closest() -> RetainedSurfaceExecutionErrorV1 {
    RetainedSurfaceExecutionErrorV1::unavailable(
        "the near-duplicate fact outcome carried no closest fact id",
    )
}

fn near_duplicate_missing_similarity() -> RetainedSurfaceExecutionErrorV1 {
    RetainedSurfaceExecutionErrorV1::unavailable(
        "the near-duplicate fact outcome carried no similarity score",
    )
}

pub fn add_committed_state(
    outcome: &ProjectMemoryFactAddRequestOutcome,
) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
    let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
        return serde_json::to_value(("project-memory-fact-add-no-write.v1", "secret_rejected"))
            .map_err(|error| {
                RetainedSurfaceExecutionErrorV1::unavailable(format!(
                    "the memory effect payload could not be serialized: {error}"
                ))
            });
    };
    if let Some(receipt) = outcome.commit_receipt() {
        return serde_json::to_value(receipt).map_err(|error| {
            RetainedSurfaceExecutionErrorV1::unavailable(format!(
                "the memory effect payload could not be serialized: {error}"
            ))
        });
    }
    if outcome.disposition() != ProjectMemoryFactAddDispositionV1::NearDuplicate {
        return Err(RetainedSurfaceExecutionErrorV1::unavailable(
            "the fact add outcome without a commit was not a normalized duplicate",
        ));
    }
    serde_json::to_value((
        "project-memory-fact-add-no-write.v1",
        "normalized_duplicate",
        outcome
            .closest_fact_id()
            .map(ProjectMemoryFactIdV1::fact_id),
        outcome.similarity_millionths(),
    ))
    .map_err(|error| {
        RetainedSurfaceExecutionErrorV1::unavailable(format!(
            "the memory effect payload could not be serialized: {error}"
        ))
    })
}

pub fn update_result(
    outcome: &ProjectMemoryFactUpdateOutcomeV1,
) -> Result<FactStoreUpdateResultV1, RetainedSurfaceExecutionErrorV1> {
    Ok(FactStoreUpdateResultV1 {
        fact: projection(outcome.fact())?,
        trust_delta_millionths: outcome.trust_delta_millionths(),
        commit: commit_receipt(outcome.commit_receipt(), outcome.commit_replayed()),
    })
}

pub fn remove_result(
    outcome: &ProjectMemoryFactRemoveOutcomeV1,
) -> Result<FactStoreRemoveResultV1, RetainedSurfaceExecutionErrorV1> {
    match (
        outcome.was_removed(),
        outcome.fact(),
        outcome.commit_receipt(),
    ) {
        (true, Some(fact), Some(receipt)) => Ok(FactStoreRemoveResultV1::Removed {
            fact: projection(fact)?,
            remaining_fact_count: outcome.remaining_fact_count(),
            commit: commit_receipt(receipt, outcome.commit_replayed()),
        }),
        (false, Some(fact), None) if !outcome.commit_replayed() => {
            Ok(FactStoreRemoveResultV1::AlreadyRemoved {
                fact: projection(fact)?,
                remaining_fact_count: outcome.remaining_fact_count(),
            })
        }
        (false, None, None) if !outcome.commit_replayed() => {
            Ok(FactStoreRemoveResultV1::NotFound {
                remaining_fact_count: outcome.remaining_fact_count(),
            })
        }
        _ => Err(RetainedSurfaceExecutionErrorV1::unavailable(
            "the fact remove outcome had an inconsistent receipt shape",
        )),
    }
}

pub fn supersede_result(
    outcome: &ProjectMemoryFactSupersedeOutcomeV1,
) -> Result<FactStoreSupersedeResultV1, RetainedSurfaceExecutionErrorV1> {
    match outcome {
        ProjectMemoryFactSupersedeOutcomeV1::Superseded {
            fact_id,
            superseded_by,
            commit_receipt: receipt,
            commit_replayed,
        } => Ok(FactStoreSupersedeResultV1::Superseded {
            fact_id: fact_id.clone(),
            superseded_by: superseded_by.clone(),
            commit: commit_receipt(receipt, *commit_replayed),
        }),
        ProjectMemoryFactSupersedeOutcomeV1::AlreadySuperseded {
            fact_id,
            superseded_by,
        } => Ok(FactStoreSupersedeResultV1::AlreadySuperseded {
            fact_id: fact_id.clone(),
            superseded_by: superseded_by.clone(),
        }),
        ProjectMemoryFactSupersedeOutcomeV1::NotFound => Ok(FactStoreSupersedeResultV1::NotFound),
    }
}

pub fn feedback_result(
    outcome: &tracedecay_store::ProjectMemoryFactFeedbackOutcomeV1,
    action: FactFeedbackActionV1,
) -> Result<
    tracedecay_contracts::retained_surfaces::FactFeedbackResultV1,
    RetainedSurfaceExecutionErrorV1,
> {
    Ok(
        tracedecay_contracts::retained_surfaces::FactFeedbackResultV1 {
            fact: projection(outcome.fact())?,
            feedback: FactFeedbackV1 {
                event_id: outcome.event_id().clone(),
                fact_id: outcome.fact().fact_id().clone(),
                action,
                old_trust_millionths: confidence_millionths(outcome.old_trust()),
                new_trust_millionths: confidence_millionths(outcome.new_trust()),
                trust_delta_millionths: outcome.trust_delta_millionths(),
                helpful_count: outcome.helpful_count(),
                unhelpful_count: outcome.unhelpful_count(),
            },
            commit: commit_receipt(outcome.commit_receipt(), outcome.commit_replayed()),
        },
    )
}

pub fn memory_status(status: &ProjectMemoryMemoryStatusV1) -> MemoryStatusV1 {
    let algebra = status.algebra();
    let funnel = status.feedback_funnel();
    MemoryStatusV1 {
        owner: public_owner(status.owner()),
        fact_count: status.fact_count(),
        entity_count: status.entity_count(),
        algebra: MemoryAlgebraV1 {
            name: algebra.name().to_owned(),
            hrr_dim: algebra.hrr_dim(),
            estimated_capacity: algebra.estimated_capacity(),
        },
        trust_0_025_count: status.trust_0_025_count(),
        trust_025_050_count: status.trust_025_050_count(),
        trust_050_075_count: status.trust_050_075_count(),
        trust_075_100_count: status.trust_075_100_count(),
        below_default_recall_threshold_count: status.below_default_recall_threshold_count(),
        helpful_count: status.helpful_count(),
        unhelpful_count: status.unhelpful_count(),
        feedback_funnel: MemoryFeedbackFunnelV1 {
            retrieval_count_total: funnel.retrieval_count_total(),
            access_count_total: funnel.access_count_total(),
            retrieved_fact_count: funnel.retrieved_fact_count(),
            rated_fact_count: funnel.rated_fact_count(),
            feedback_total: funnel.feedback_total(),
            seen_to_feedback_ratio: funnel.seen_to_feedback_ratio(),
        },
    }
}

pub fn map_memory_error(error: MemoryApplicationError) -> RetainedSurfaceExecutionErrorV1 {
    match error {
        MemoryApplicationError::InvalidOwner(_) | MemoryApplicationError::InvalidInput { .. } => {
            RetainedSurfaceExecutionErrorV1::InvalidRequest
        }
        MemoryApplicationError::OwnerMismatch { .. } => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        MemoryApplicationError::Store(error) | MemoryApplicationError::Cancelled(error) => {
            map_store_error(error)
        }
        error @ (MemoryApplicationError::InvalidAuthorityResult { .. }
        | MemoryApplicationError::InvalidEvidenceAnchor(_)
        | MemoryApplicationError::EvidenceAnchor(_)) => {
            RetainedSurfaceExecutionErrorV1::unavailable(error.to_string())
        }
    }
}

pub fn map_store_error(error: FactStoreError) -> RetainedSurfaceExecutionErrorV1 {
    match error {
        FactStoreError::InvalidQueryLimit { .. } | FactStoreError::Contract(_) => {
            RetainedSurfaceExecutionErrorV1::InvalidRequest
        }
        FactStoreError::OwnerMismatch
        | FactStoreError::FactNotFound { .. }
        | FactStoreError::FactUnavailable { .. }
        | FactStoreError::FactDeleted { .. } => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        FactStoreError::CommitConflict { .. }
        | FactStoreError::OperationConflict
        | FactStoreError::RelationConflict { .. }
        | FactStoreError::GraphConflict => RetainedSurfaceExecutionErrorV1::Conflict,
        FactStoreError::GraphCancelled | FactStoreError::ReadCancelled => {
            RetainedSurfaceExecutionErrorV1::Cancelled(
                tracedecay_contracts::CancellationStage::DuringRead,
            )
        }
        FactStoreError::GraphBudgetExhausted => RetainedSurfaceExecutionErrorV1::Saturated,
        FactStoreError::GraphDeadlineExceeded => RetainedSurfaceExecutionErrorV1::TimedOut(
            tracedecay_contracts::CancellationStage::DuringRead,
        ),
        FactStoreError::GraphResetRequired { owner, .. } => match owner {
            FactOwnerV1::Profile => RetainedSurfaceExecutionErrorV1::ProfileResetRequired,
            FactOwnerV1::Project { .. } => RetainedSurfaceExecutionErrorV1::ProjectResetRequired,
        },
        error => RetainedSurfaceExecutionErrorV1::unavailable(error.to_string()),
    }
}

fn confidence_millionths(value: Confidence) -> u32 {
    (value.as_f64() * 1_000_000.0).round() as u32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};
    use tracedecay_contracts::RetainedSurfaceExecutionErrorV1;
    use tracedecay_contracts::retained_surfaces::{
        FactCategoryV1, FactFeedbackActionV1, FactFeedbackRequestV1, FactReadOptionsV1,
        FactSearchCursorV1, FactSourceLabelPatchV1, FactStoreRemoveRequestV1,
        FactStoreSearchRequestV1, FactStoreSupersedeRequestV1, FactStoreUpdateRequestV1,
        MemoryScopeV1, RetainedProjectSelectorV1,
    };
    use tracedecay_domain::{
        ActorId, FactEventId, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1,
        ProjectId, ProvenanceId, UtcMicros, canonical_sha256,
    };
    use tracedecay_store::FactStoreError;

    use super::{
        FactRetrievalTelemetryDegradationV1, MAX_RETAINED_FACT_LIMIT, PreparedFactFeedback,
        PreparedFactRemove, PreparedFactSearch, PreparedFactSupersede, PreparedFactUpdate,
        confidence, fact_limit, map_store_error, retrieval_telemetry_degradation,
    };

    fn owner() -> FactOwnerV1 {
        FactOwnerV1::Project {
            project_id: ProjectId::new("project.retained-memory-mapping").expect("project id"),
        }
    }

    fn fact_id(owner: &FactOwnerV1, operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                owner.clone(),
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new(operation.to_owned()).expect("operation id"),
                },
            )
            .expect("identity material"),
        )
        .expect("fact id")
    }

    fn event_id(label: &str) -> FactEventId {
        FactEventId::new(format!("event.mapping.{label}")).expect("event id")
    }

    fn operation_id() -> ProvenanceId {
        ProvenanceId::new("memory-operation.effect.mapping".to_owned()).expect("operation id")
    }

    fn actor() -> ActorId {
        ActorId::new("actor.memory.mapping").expect("actor id")
    }

    fn metadata() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("zeta".to_owned(), json!({"nested": [1, 2, {"k": "v"}]})),
            ("alpha".to_owned(), json!(true)),
        ])
    }

    fn update_request(
        owner: &FactOwnerV1,
        source_label: Option<FactSourceLabelPatchV1>,
    ) -> FactStoreUpdateRequestV1 {
        FactStoreUpdateRequestV1 {
            fact_id: fact_id(owner, "operation.mapping.update"),
            expected_last_event_id: Some(event_id("update")),
            content: Some("Project Phoenix uses deterministic Amari Memory".to_owned()),
            memory_scope: None,
            category: Some(FactCategoryV1::Decision),
            tags: Some(vec!["memory".to_owned(), "holographic".to_owned()]),
            entities: Some(vec![
                "Amari Memory".to_owned(),
                "Project Phoenix".to_owned(),
            ]),
            trust: Some(0.75),
            source_label,
            metadata: Some(metadata()),
            project_selector: None,
        }
    }

    fn assert_same_identity(prepared: &Value, legacy: &Value, label: &str) {
        assert_eq!(prepared, legacy, "{label}: logical effect value");
        assert_eq!(
            canonical_sha256(prepared).expect("prepared digest"),
            canonical_sha256(legacy).expect("legacy digest"),
            "{label}: logical effect digest"
        );
    }

    /// The prepared values must reproduce the logical-effect tuples the
    /// retained memory surface digested before request preparation existed:
    /// an operation id derived from these bytes is a replay of a committed
    /// effect, so any drift here would silently fork idempotency.
    #[test]
    fn prepared_effects_reproduce_the_retained_identity_tuples() {
        let owner = owner();
        for (label, request) in [
            (
                "update.set",
                update_request(
                    &owner,
                    Some(FactSourceLabelPatchV1::Set {
                        value: "mcp-test".to_owned(),
                    }),
                ),
            ),
            (
                "update.clear",
                update_request(&owner, Some(FactSourceLabelPatchV1::Clear)),
            ),
            ("update.untouched-label", update_request(&owner, None)),
            (
                "update.trust-only",
                FactStoreUpdateRequestV1 {
                    content: None,
                    category: None,
                    tags: None,
                    entities: None,
                    metadata: None,
                    expected_last_event_id: None,
                    ..update_request(&owner, None)
                },
            ),
        ] {
            let prepared = PreparedFactUpdate::new(owner.clone(), &request).expect(label);
            let legacy = serde_json::to_value((
                "project-memory-fact-update.v1",
                &owner,
                &request.fact_id,
                &request.expected_last_event_id,
                &request.content,
                request.category,
                &request.source_label,
                &request.tags,
                &request.entities,
                confidence(request.trust).expect(label),
                &request.metadata,
            ))
            .expect(label);
            assert_same_identity(&prepared.logical_effect().expect(label), &legacy, label);
        }

        let remove = FactStoreRemoveRequestV1 {
            fact_id: fact_id(&owner, "operation.mapping.remove"),
            expected_last_event_id: Some(event_id("remove")),
            memory_scope: None,
            project_selector: None,
        };
        let prepared = PreparedFactRemove::new(owner.clone(), &remove).expect("remove");
        let legacy = serde_json::to_value((
            "project-memory-fact-remove.v1",
            &owner,
            &remove.fact_id,
            &remove.expected_last_event_id,
        ))
        .expect("remove");
        assert_same_identity(
            &prepared.logical_effect().expect("remove"),
            &legacy,
            "remove",
        );

        let supersede = FactStoreSupersedeRequestV1 {
            fact_id: fact_id(&owner, "operation.mapping.supersede.old"),
            superseded_by: fact_id(&owner, "operation.mapping.supersede.new"),
            expected_last_event_id: None,
            memory_scope: None,
            project_selector: None,
        };
        let prepared = PreparedFactSupersede::new(owner.clone(), &supersede).expect("supersede");
        let legacy = serde_json::to_value((
            "project-memory-fact-supersede.v1",
            &owner,
            &supersede.fact_id,
            &supersede.superseded_by,
            &supersede.expected_last_event_id,
        ))
        .expect("supersede");
        assert_same_identity(
            &prepared.logical_effect().expect("supersede"),
            &legacy,
            "supersede",
        );

        let feedback = FactFeedbackRequestV1 {
            fact_id: fact_id(&owner, "operation.mapping.feedback"),
            expected_last_event_id: Some(event_id("feedback")),
            action: FactFeedbackActionV1::Unhelpful,
            source_label: Some("reviewer".to_owned()),
            reason: Some("stale after the Q3 retro".to_owned()),
            memory_scope: None,
            project_selector: None,
        };
        let prepared = PreparedFactFeedback::new(owner.clone(), &feedback).expect("feedback");
        let legacy = serde_json::to_value((
            "project-memory-fact-feedback.v1",
            &owner,
            &feedback.fact_id,
            &feedback.expected_last_event_id,
            feedback.action,
            &feedback.source_label,
            &feedback.reason,
        ))
        .expect("feedback");
        assert_same_identity(
            &prepared.logical_effect().expect("feedback"),
            &legacy,
            "feedback",
        );

        for (label, options, after) in [
            ("search.defaults", FactReadOptionsV1::default(), None),
            (
                "search.explicit",
                FactReadOptionsV1 {
                    category: Some(FactCategoryV1::Project),
                    min_trust: Some(0.6),
                    limit: Some(5),
                    ..FactReadOptionsV1::default()
                },
                Some(FactSearchCursorV1 {
                    score_millionths: 750_000,
                    updated_at: UtcMicros(1_700_000_000_000_000),
                    fact_id: fact_id(&owner, "operation.mapping.search.cursor"),
                }),
            ),
        ] {
            let request = FactStoreSearchRequestV1 {
                query: "canonical identity".to_owned(),
                options,
                after,
            };
            let prepared = PreparedFactSearch::new(owner.clone(), &request).expect(label);
            let legacy = serde_json::to_value((
                "project-memory-fact-search.v1",
                &owner,
                &request.query,
                request.options.category,
                confidence(Some(request.options.min_trust.unwrap_or(0.3))).expect(label),
                fact_limit(request.options.limit).expect(label),
                &request.after,
            ))
            .expect(label);
            assert_same_identity(&prepared.logical_effect().expect(label), &legacy, label);
        }
    }

    /// Invalid public input fails while preparing, before any identity or
    /// command exists, with the same typed error class the surface reports.
    #[test]
    fn invalid_requests_fail_before_effect_identity() {
        let owner = owner();
        let foreign_owner = FactOwnerV1::Project {
            project_id: ProjectId::new("project.retained-memory-other").expect("project id"),
        };
        let mut out_of_range_trust = update_request(&owner, None);
        out_of_range_trust.trust = Some(1.5);
        assert!(matches!(
            PreparedFactUpdate::new(owner.clone(), &out_of_range_trust),
            Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
        ));
        let empty_patch = FactStoreUpdateRequestV1 {
            content: None,
            category: None,
            tags: None,
            entities: None,
            trust: None,
            metadata: None,
            ..update_request(&owner, None)
        };
        assert!(matches!(
            PreparedFactUpdate::new(owner.clone(), &empty_patch),
            Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
        ));
        let foreign_fact = FactStoreUpdateRequestV1 {
            fact_id: fact_id(&foreign_owner, "operation.mapping.update"),
            ..update_request(&owner, None)
        };
        assert!(matches!(
            PreparedFactUpdate::new(owner.clone(), &foreign_fact),
            Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)
        ));
        assert!(matches!(
            PreparedFactRemove::new(
                owner.clone(),
                &FactStoreRemoveRequestV1 {
                    fact_id: fact_id(&foreign_owner, "operation.mapping.remove"),
                    expected_last_event_id: None,
                    memory_scope: None,
                    project_selector: None,
                },
            ),
            Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)
        ));
        assert!(matches!(
            PreparedFactSupersede::new(
                owner.clone(),
                &FactStoreSupersedeRequestV1 {
                    fact_id: fact_id(&owner, "operation.mapping.supersede.old"),
                    superseded_by: fact_id(&foreign_owner, "operation.mapping.supersede.new"),
                    expected_last_event_id: None,
                    memory_scope: None,
                    project_selector: None,
                },
            ),
            Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized)
        ));
        let self_supersession = FactStoreSupersedeRequestV1 {
            fact_id: fact_id(&owner, "operation.mapping.supersede.same"),
            superseded_by: fact_id(&owner, "operation.mapping.supersede.same"),
            expected_last_event_id: None,
            memory_scope: None,
            project_selector: None,
        };
        assert!(matches!(
            PreparedFactSupersede::new(owner.clone(), &self_supersession)
                .expect("targets validate")
                .into_command(operation_id(), actor()),
            Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
        ));
        let blank_feedback_label = FactFeedbackRequestV1 {
            fact_id: fact_id(&owner, "operation.mapping.feedback"),
            expected_last_event_id: None,
            action: FactFeedbackActionV1::Helpful,
            source_label: Some("   ".to_owned()),
            reason: None,
            memory_scope: None,
            project_selector: None,
        };
        assert!(matches!(
            PreparedFactFeedback::new(owner.clone(), &blank_feedback_label)
                .expect("target validates")
                .into_command(operation_id(), actor()),
            Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
        ));
        for (label, options) in [
            (
                "zero limit",
                FactReadOptionsV1 {
                    limit: Some(0),
                    ..FactReadOptionsV1::default()
                },
            ),
            (
                "trust above one",
                FactReadOptionsV1 {
                    min_trust: Some(1.2),
                    ..FactReadOptionsV1::default()
                },
            ),
        ] {
            let request = FactStoreSearchRequestV1 {
                query: "canonical identity".to_owned(),
                options,
                after: None,
            };
            assert!(
                matches!(
                    PreparedFactSearch::new(owner.clone(), &request),
                    Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
                ),
                "{label}"
            );
        }
    }

    #[test]
    fn retained_limits_reject_zero_and_oversized_pages() {
        assert_eq!(fact_limit(None), Ok(20));
        assert_eq!(fact_limit(Some(1)), Ok(1));
        assert_eq!(fact_limit(Some(MAX_RETAINED_FACT_LIMIT as u64)), Ok(200));
        assert_eq!(
            fact_limit(Some(0)),
            Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
        );
        assert_eq!(
            fact_limit(Some((MAX_RETAINED_FACT_LIMIT + 1) as u64)),
            Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
        );
    }

    #[test]
    fn graph_failures_keep_distinct_retained_terminal_states() {
        assert_eq!(
            map_store_error(FactStoreError::GraphCancelled),
            RetainedSurfaceExecutionErrorV1::Cancelled(
                tracedecay_contracts::CancellationStage::DuringRead
            )
        );
        assert_eq!(
            map_store_error(FactStoreError::ReadCancelled),
            RetainedSurfaceExecutionErrorV1::Cancelled(
                tracedecay_contracts::CancellationStage::DuringRead
            )
        );
        assert_eq!(
            map_store_error(FactStoreError::GraphBudgetExhausted),
            RetainedSurfaceExecutionErrorV1::Saturated
        );
        assert_eq!(
            map_store_error(FactStoreError::GraphDeadlineExceeded),
            RetainedSurfaceExecutionErrorV1::TimedOut(
                tracedecay_contracts::CancellationStage::DuringRead
            )
        );
        assert_eq!(
            map_store_error(FactStoreError::OperationConflict),
            RetainedSurfaceExecutionErrorV1::Conflict
        );
        assert_eq!(
            map_store_error(FactStoreError::GraphResetRequired {
                owner: FactOwnerV1::Profile,
                reason: "profile graph reset".to_owned(),
            }),
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired
        );
        assert_eq!(
            map_store_error(FactStoreError::GraphResetRequired {
                owner: FactOwnerV1::Project {
                    project_id: ProjectId::new("project.graph-reset").expect("project id"),
                },
                reason: "project graph reset".to_owned(),
            }),
            RetainedSurfaceExecutionErrorV1::ProjectResetRequired
        );
    }

    /// A served search absorbs only unavailability-class telemetry failures;
    /// request-scoped and store-health terminals still fail the operation.
    #[test]
    fn retrieval_telemetry_degrades_only_on_unavailability() {
        assert_eq!(
            retrieval_telemetry_degradation(&RetainedSurfaceExecutionErrorV1::unavailable(
                "telemetry write lane unavailable"
            )),
            Some(FactRetrievalTelemetryDegradationV1::Unavailable)
        );
        assert_eq!(
            retrieval_telemetry_degradation(&RetainedSurfaceExecutionErrorV1::Saturated),
            Some(FactRetrievalTelemetryDegradationV1::Saturated)
        );
        for terminal in [
            RetainedSurfaceExecutionErrorV1::InvalidRequest,
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized,
            RetainedSurfaceExecutionErrorV1::Conflict,
            RetainedSurfaceExecutionErrorV1::Stale,
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired,
            RetainedSurfaceExecutionErrorV1::ProjectResetRequired,
            RetainedSurfaceExecutionErrorV1::Cancelled(
                tracedecay_contracts::CancellationStage::DuringRead,
            ),
            RetainedSurfaceExecutionErrorV1::TimedOut(
                tracedecay_contracts::CancellationStage::DuringRead,
            ),
        ] {
            assert_eq!(
                retrieval_telemetry_degradation(&terminal),
                None,
                "{terminal:?} must fail the search instead of degrading telemetry"
            );
        }
    }

    #[test]
    fn logical_search_identity_excludes_equivalent_routing_fields() {
        let project_id = ProjectId::new("project.retained-logical-search").expect("project id");
        let owner = FactOwnerV1::Project {
            project_id: project_id.clone(),
        };
        let direct = FactStoreSearchRequestV1 {
            query: "canonical identity".to_owned(),
            options: FactReadOptionsV1::default(),
            after: None,
        };
        let mut explicitly_routed = direct.clone();
        explicitly_routed.options.memory_scope = Some(MemoryScopeV1::Project);
        explicitly_routed.options.project_selector = Some(RetainedProjectSelectorV1 { project_id });

        assert_eq!(
            PreparedFactSearch::new(owner.clone(), &direct)
                .expect("direct search")
                .logical_effect(),
            PreparedFactSearch::new(owner, &explicitly_routed)
                .expect("routed search")
                .logical_effect()
        );
    }
}

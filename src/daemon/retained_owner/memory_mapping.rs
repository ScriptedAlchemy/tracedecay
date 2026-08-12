//! Canonical retained-memory request and result mapping.

use serde::Serialize;
use serde_json::Value;
use tracedecay_application::retained_surfaces::{
    FactCategoryV1, FactCommitDispositionV1, FactCommitOwnerV1, FactCommitReceiptV1,
    FactContradictionV1, FactFeedbackActionV1, FactFeedbackDetailsAvailabilityV1,
    FactFeedbackRequestV1, FactFeedbackV1, FactIdentitySourceResultV1, FactPayloadAccessV1,
    FactProjectionV1, FactReadOptionsV1, FactSearchCursorV1, FactSearchGraphCoverageV1,
    FactSearchGraphDegradationV1, FactSearchHitV1, FactSearchScoresV1, FactSourceLabelPatchV1,
    FactStatusV1, FactStoreAddCommitV1, FactStoreAddRequestV1, FactStoreAddResultV1,
    FactStoreContradictResultV1, FactStoreGetResultV1, FactStoreListResultV1,
    FactStoreProbeResultV1, FactStoreReasonResultV1, FactStoreRelatedResultV1,
    FactStoreRemoveRequestV1, FactStoreRemoveResultV1, FactStoreSearchRequestV1,
    FactStoreSearchResultV1, FactStoreUpdateRequestV1, FactStoreUpdateResultV1, FactTelemetryV1,
    FactV1, MemoryAlgebraV1, MemoryFeedbackFunnelV1, MemoryScopeV1, MemoryStatusResultV1,
    MemoryStatusV1, RetainedProjectSelectorV1, RetainedSurfaceOperation, RetainedSurfaceResultV1,
    TrustHistoryEntryV1,
};
use tracedecay_application::{
    CancellationStage, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
};
use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1 as DomainFactCategoryV1, FactIdentitySourceV1, FactOwnerV1,
    PayloadAccessState, ProvenanceId,
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
    ProjectMemoryFactUnavailableV1, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, ProjectMemoryFactUpdatePatchV1, ProjectMemoryFactV1,
    ProjectMemoryMemoryStatusV1,
};
use tracedecay_usecases::memory::{
    MemoryApplicationError, MemoryOperationContext, ProjectMemoryFactAddRequest,
    ProjectMemoryFactAddRequestOutcome,
};

pub(super) const MAX_RETAINED_FACT_LIMIT: usize = 200;
pub(super) const MAX_RETAINED_FEEDBACK_HISTORY_LIMIT: usize = 1_000;

pub(super) fn read_scope(
    options: &FactReadOptionsV1,
) -> (Option<MemoryScopeV1>, Option<&RetainedProjectSelectorV1>) {
    (options.memory_scope, options.project_selector.as_ref())
}

pub(super) fn validate_reason_entities(
    entities: &[String],
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    if entities.is_empty() || entities.windows(2).any(|pair| pair.first() >= pair.get(1)) {
        return Err(RetainedSurfaceExecutionErrorV1::InvalidRequest);
    }
    Ok(())
}

pub(super) fn ensure_profile_request_scope(
    memory_scope: Option<MemoryScopeV1>,
    selector: Option<&RetainedProjectSelectorV1>,
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    if memory_scope != Some(MemoryScopeV1::User) || selector.is_some() {
        return Err(RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized);
    }
    Ok(())
}

pub(super) fn add_request(
    request: &FactStoreAddRequestV1,
) -> Result<ProjectMemoryFactAddRequest, RetainedSurfaceExecutionErrorV1> {
    Ok(ProjectMemoryFactAddRequest {
        content: request.content.clone(),
        category: domain_category(request.category.unwrap_or(FactCategoryV1::General)),
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

pub(super) fn update_patch(
    request: &FactStoreUpdateRequestV1,
) -> Result<ProjectMemoryFactUpdatePatchV1, RetainedSurfaceExecutionErrorV1> {
    let source_label = request.source_label.as_ref().map(|patch| match patch {
        FactSourceLabelPatchV1::Set { value } => Some(value.clone()),
        FactSourceLabelPatchV1::Clear => None,
    });
    ProjectMemoryFactUpdatePatchV1::new(
        request.content.clone(),
        request.category.map(domain_category),
        source_label,
        request.tags.clone(),
        request.entities.clone(),
        request
            .metadata
            .clone()
            .map(|metadata| Value::Object(metadata.into_iter().collect::<serde_json::Map<_, _>>())),
        confidence(request.trust)?,
    )
    .map_err(map_store_error)
}

pub(super) fn update_logical_effect(
    owner: &FactOwnerV1,
    request: &FactStoreUpdateRequestV1,
) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
    let target = ProjectMemoryFactIdV1::new(owner.clone(), request.fact_id.clone())
        .map_err(map_store_error)?;
    update_patch(request)?;
    serde_json::to_value((
        "project-memory-fact-update.v1",
        target.owner(),
        target.fact_id(),
        &request.expected_last_event_id,
        &request.content,
        request.category.map(domain_category),
        &request.source_label,
        &request.tags,
        &request.entities,
        confidence(request.trust)?,
        &request.metadata,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

pub(super) fn remove_logical_effect(
    owner: &FactOwnerV1,
    request: &FactStoreRemoveRequestV1,
) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
    let target = ProjectMemoryFactIdV1::new(owner.clone(), request.fact_id.clone())
        .map_err(map_store_error)?;
    serde_json::to_value((
        "project-memory-fact-remove.v1",
        target.owner(),
        target.fact_id(),
        &request.expected_last_event_id,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

pub(super) fn feedback_logical_effect(
    owner: &FactOwnerV1,
    request: &FactFeedbackRequestV1,
) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
    let target = ProjectMemoryFactIdV1::new(owner.clone(), request.fact_id.clone())
        .map_err(map_store_error)?;
    serde_json::to_value((
        "project-memory-fact-feedback.v1",
        target.owner(),
        target.fact_id(),
        &request.expected_last_event_id,
        request.action,
        &request.source_label,
        &request.reason,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

pub(super) fn search_logical_effect(
    owner: &FactOwnerV1,
    request: &FactStoreSearchRequestV1,
) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
    serde_json::to_value((
        "project-memory-fact-search.v1",
        owner,
        &request.query,
        request.options.category.map(domain_category),
        confidence(Some(request.options.min_trust.unwrap_or(0.3)))?,
        fact_limit(request.options.limit)?,
        &request.after,
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

pub(super) fn update_command(
    owner: FactOwnerV1,
    request: &FactStoreUpdateRequestV1,
    operation_id: ProvenanceId,
    actor: ActorId,
) -> Result<ProjectMemoryFactUpdateCommandV1, RetainedSurfaceExecutionErrorV1> {
    let target =
        ProjectMemoryFactIdV1::new(owner, request.fact_id.clone()).map_err(map_store_error)?;
    ProjectMemoryFactUpdateCommandV1::new(
        target,
        operation_id,
        request.expected_last_event_id.clone(),
        update_patch(request)?,
        Some(actor),
    )
    .map_err(map_store_error)
}

pub(super) fn remove_command(
    owner: FactOwnerV1,
    request: &FactStoreRemoveRequestV1,
    operation_id: ProvenanceId,
    actor: ActorId,
) -> Result<ProjectMemoryFactRemoveCommandV1, RetainedSurfaceExecutionErrorV1> {
    let target =
        ProjectMemoryFactIdV1::new(owner, request.fact_id.clone()).map_err(map_store_error)?;
    ProjectMemoryFactRemoveCommandV1::new(
        target,
        operation_id,
        request.expected_last_event_id.clone(),
        Some(actor),
    )
    .map_err(map_store_error)
}

pub(super) fn feedback_command(
    owner: FactOwnerV1,
    request: &FactFeedbackRequestV1,
    operation_id: ProvenanceId,
    actor: ActorId,
) -> Result<ProjectMemoryFactFeedbackCommandV1, RetainedSurfaceExecutionErrorV1> {
    let target =
        ProjectMemoryFactIdV1::new(owner, request.fact_id.clone()).map_err(map_store_error)?;
    ProjectMemoryFactFeedbackCommandV1::new(
        target,
        operation_id,
        request.expected_last_event_id.clone(),
        feedback_action(request.action),
        Some(actor),
        request.source_label.clone(),
        request.reason.clone(),
    )
    .map_err(map_store_error)
}

pub(super) fn search_query(
    owner: FactOwnerV1,
    kind: ProjectMemoryFactSearchKindV1,
    query: Option<String>,
    options: &FactReadOptionsV1,
    after: Option<&FactSearchCursorV1>,
) -> Result<ProjectMemoryFactSearchQuery, RetainedSurfaceExecutionErrorV1> {
    let filter = ProjectMemoryFactSearchFilterV1::new(
        options.category.map(domain_category),
        confidence(options.min_trust)?,
        None,
    )
    .map_err(map_store_error)?;
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
    ProjectMemoryFactSearchQuery::with_filter(
        owner,
        kind,
        query,
        filter,
        after,
        fact_limit(options.limit)?,
    )
    .map_err(map_store_error)
}

pub(super) fn fact_limit(limit: Option<u64>) -> Result<usize, RetainedSurfaceExecutionErrorV1> {
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

pub(super) fn confidence(
    value: Option<f64>,
) -> Result<Option<Confidence>, RetainedSurfaceExecutionErrorV1> {
    value
        .map(Confidence::new)
        .transpose()
        .map_err(|_| RetainedSurfaceExecutionErrorV1::InvalidRequest)
}

pub(super) const fn domain_category(category: FactCategoryV1) -> DomainFactCategoryV1 {
    match category {
        FactCategoryV1::General => DomainFactCategoryV1::General,
        FactCategoryV1::UserPref => DomainFactCategoryV1::UserPref,
        FactCategoryV1::Project => DomainFactCategoryV1::Project,
        FactCategoryV1::Tool => DomainFactCategoryV1::Tool,
        FactCategoryV1::Decision => DomainFactCategoryV1::Decision,
        FactCategoryV1::CodeArea => DomainFactCategoryV1::CodeArea,
    }
}

const fn public_category(category: DomainFactCategoryV1) -> FactCategoryV1 {
    match category {
        DomainFactCategoryV1::General => FactCategoryV1::General,
        DomainFactCategoryV1::UserPref => FactCategoryV1::UserPref,
        DomainFactCategoryV1::Project => FactCategoryV1::Project,
        DomainFactCategoryV1::Tool => FactCategoryV1::Tool,
        DomainFactCategoryV1::Decision => FactCategoryV1::Decision,
        DomainFactCategoryV1::CodeArea => FactCategoryV1::CodeArea,
    }
}

pub(super) const fn feedback_action(
    action: FactFeedbackActionV1,
) -> ProjectMemoryFactFeedbackActionV1 {
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

pub(super) fn public_owner(owner: &FactOwnerV1) -> FactCommitOwnerV1 {
    match owner {
        FactOwnerV1::Profile => FactCommitOwnerV1::Profile,
        FactOwnerV1::Project { project_id } => FactCommitOwnerV1::Project {
            project_id: project_id.clone(),
        },
    }
}

pub(super) fn commit_receipt(receipt: &FactCommitReceipt, replayed: bool) -> FactCommitReceiptV1 {
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

pub(super) fn projection(
    projection: &ProjectMemoryFactProjectionV1,
) -> Result<FactProjectionV1, RetainedSurfaceExecutionErrorV1> {
    match projection {
        ProjectMemoryFactProjectionV1::Available(fact) => Ok(FactProjectionV1::Available {
            fact: available_fact(fact)?,
        }),
        ProjectMemoryFactProjectionV1::Unavailable(fact) => Ok(FactProjectionV1::Unavailable {
            status: unavailable_fact(fact)?,
        }),
    }
}

pub(super) fn available_fact(
    fact: &ProjectMemoryFactV1,
) -> Result<FactV1, RetainedSurfaceExecutionErrorV1> {
    let metadata = match fact.metadata() {
        Value::Object(metadata) => metadata.clone().into_iter().collect(),
        _ => return Err(RetainedSurfaceExecutionErrorV1::Unavailable),
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
        category: public_category(fact.category()),
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
            return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
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

pub(super) fn search_page(
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

pub(crate) fn public_search_page(
    page: &ProjectMemoryFactSearchPageV1,
) -> Result<FactStoreSearchResultV1, RetainedSurfaceExecutionErrorV1> {
    let page = search_page(page)?;
    Ok(FactStoreSearchResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    })
}

pub(super) struct MappedSearchPageV1 {
    pub(super) owner: FactCommitOwnerV1,
    pub(super) hits: Vec<FactSearchHitV1>,
    pub(super) next_after: Option<FactSearchCursorV1>,
    pub(super) graph_coverage: FactSearchGraphCoverageV1,
}

pub(super) fn probe_result(page: MappedSearchPageV1) -> FactStoreProbeResultV1 {
    FactStoreProbeResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    }
}

pub(super) fn related_result(page: MappedSearchPageV1) -> FactStoreRelatedResultV1 {
    FactStoreRelatedResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    }
}

pub(super) fn reason_result(page: MappedSearchPageV1) -> FactStoreReasonResultV1 {
    FactStoreReasonResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    }
}

pub(super) fn exact_search_result(page: MappedSearchPageV1) -> RetainedSurfaceResultV1 {
    RetainedSurfaceResultV1::FactStoreSearch(FactStoreSearchResultV1 {
        owner: page.owner,
        hits: page.hits,
        next_after: page.next_after,
        graph_coverage: page.graph_coverage,
    })
}

pub(super) fn semantic_search_result(
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

pub(super) fn refresh_search_hits(
    page: &mut MappedSearchPageV1,
    projections: &[ProjectMemoryFactProjectionV1],
) -> Result<(), RetainedSurfaceExecutionErrorV1> {
    for hit in &mut page.hits {
        let projection = projections
            .iter()
            .find(|projection| projection.fact_id() == &hit.fact.fact_id)
            .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
        let ProjectMemoryFactProjectionV1::Available(fact) = projection else {
            return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
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

pub(super) fn contradiction_page(
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

pub(super) fn feedback_history(
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

pub(super) fn get_result(
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

pub(super) fn status_result(status: &ProjectMemoryMemoryStatusV1) -> RetainedSurfaceResultV1 {
    RetainedSurfaceResultV1::MemoryStatus(memory_status_result(status))
}

pub(crate) fn memory_status_result(status: &ProjectMemoryMemoryStatusV1) -> MemoryStatusResultV1 {
    MemoryStatusResultV1 {
        memory: memory_status(status),
    }
}

pub(super) fn list_page(
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

pub(super) fn add_result(
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
            return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
        }
        return Ok(FactStoreAddResultV1::NormalizedDuplicate {
            fact,
            closest_fact_id: closest.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?,
        });
    };
    let commit = commit_receipt(receipt, outcome.commit_replayed());
    let result = match outcome.disposition() {
        ProjectMemoryFactAddDispositionV1::Added => FactStoreAddCommitV1::Added { fact, commit },
        ProjectMemoryFactAddDispositionV1::NearDuplicate => FactStoreAddCommitV1::NearDuplicate {
            fact,
            closest_fact_id: closest.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?,
            similarity_millionths: outcome
                .similarity_millionths()
                .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?,
            commit,
        },
        ProjectMemoryFactAddDispositionV1::PossibleConflict => {
            FactStoreAddCommitV1::PossibleConflict {
                fact,
                closest_fact_id: closest.ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?,
                similarity_millionths: outcome
                    .similarity_millionths()
                    .ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?,
                commit,
            }
        }
    };
    Ok(FactStoreAddResultV1::Committed { result })
}

pub(super) fn add_committed_state(
    outcome: &ProjectMemoryFactAddRequestOutcome,
) -> Result<Value, RetainedSurfaceExecutionErrorV1> {
    let ProjectMemoryFactAddRequestOutcome::Applied(outcome) = outcome else {
        return serde_json::to_value(("project-memory-fact-add-no-write.v1", "secret_rejected"))
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable);
    };
    if let Some(receipt) = outcome.commit_receipt() {
        return serde_json::to_value(receipt)
            .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable);
    }
    if outcome.disposition() != ProjectMemoryFactAddDispositionV1::NearDuplicate {
        return Err(RetainedSurfaceExecutionErrorV1::Unavailable);
    }
    serde_json::to_value((
        "project-memory-fact-add-no-write.v1",
        "normalized_duplicate",
        outcome
            .closest_fact_id()
            .map(ProjectMemoryFactIdV1::fact_id),
        outcome.similarity_millionths(),
    ))
    .map_err(|_| RetainedSurfaceExecutionErrorV1::Unavailable)
}

pub(super) fn update_result(
    outcome: &ProjectMemoryFactUpdateOutcomeV1,
) -> Result<FactStoreUpdateResultV1, RetainedSurfaceExecutionErrorV1> {
    Ok(FactStoreUpdateResultV1 {
        fact: projection(outcome.fact())?,
        trust_delta_millionths: outcome.trust_delta_millionths(),
        commit: commit_receipt(outcome.commit_receipt(), outcome.commit_replayed()),
    })
}

pub(super) fn remove_result(
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
        _ => Err(RetainedSurfaceExecutionErrorV1::Unavailable),
    }
}

pub(super) fn feedback_result(
    outcome: &tracedecay_store::ProjectMemoryFactFeedbackOutcomeV1,
    action: FactFeedbackActionV1,
) -> Result<
    tracedecay_application::retained_surfaces::FactFeedbackResultV1,
    RetainedSurfaceExecutionErrorV1,
> {
    Ok(
        tracedecay_application::retained_surfaces::FactFeedbackResultV1 {
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

pub(crate) fn memory_status(status: &ProjectMemoryMemoryStatusV1) -> MemoryStatusV1 {
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

pub(super) fn map_memory_error(error: MemoryApplicationError) -> RetainedSurfaceExecutionErrorV1 {
    match error {
        MemoryApplicationError::InvalidOwner(_) | MemoryApplicationError::InvalidInput { .. } => {
            RetainedSurfaceExecutionErrorV1::InvalidRequest
        }
        MemoryApplicationError::OwnerMismatch { .. } => {
            RetainedSurfaceExecutionErrorV1::NotFoundOrNotAuthorized
        }
        MemoryApplicationError::Store(error) => map_store_error(error),
        MemoryApplicationError::InvalidAuthorityResult { .. }
        | MemoryApplicationError::InvalidEvidenceAnchor(_)
        | MemoryApplicationError::EvidenceAnchor(_) => RetainedSurfaceExecutionErrorV1::Unavailable,
    }
}

pub(super) fn map_store_error(error: FactStoreError) -> RetainedSurfaceExecutionErrorV1 {
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
            RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::DuringRead)
        }
        FactStoreError::GraphBudgetExhausted => RetainedSurfaceExecutionErrorV1::Saturated,
        FactStoreError::GraphDeadlineExceeded => {
            RetainedSurfaceExecutionErrorV1::TimedOut(CancellationStage::DuringRead)
        }
        FactStoreError::GraphResetRequired { owner, .. } => match owner {
            FactOwnerV1::Profile => RetainedSurfaceExecutionErrorV1::ProfileResetRequired,
            FactOwnerV1::Project { .. } => RetainedSurfaceExecutionErrorV1::ProjectResetRequired,
        },
        _ => RetainedSurfaceExecutionErrorV1::Unavailable,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MemoryCancellationStages {
    pub(super) before: CancellationStage,
    pub(super) active: CancellationStage,
}

pub(super) const READ_CANCELLATION_STAGES: MemoryCancellationStages = MemoryCancellationStages {
    before: CancellationStage::BeforeRead,
    active: CancellationStage::DuringRead,
};

pub(super) const EFFECT_CANCELLATION_STAGES: MemoryCancellationStages = MemoryCancellationStages {
    before: CancellationStage::BeforeEffect,
    active: CancellationStage::EffectInFlight,
};

pub(super) fn memory_operation_context<T: Serialize>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    owner: &FactOwnerV1,
    action: &str,
    logical_effect: &T,
) -> Result<MemoryOperationContext, RetainedSurfaceExecutionErrorV1> {
    MemoryOperationContext::from_logical_effect(
        owner,
        action,
        logical_effect,
        Some(context.request_context.actor().clone()),
    )
    .map_err(map_memory_error)
}

fn confidence_millionths(value: Confidence) -> u32 {
    (value.as_f64() * 1_000_000.0).round() as u32
}

#[cfg(test)]
#[path = "memory_mapping_tests.rs"]
mod tests;

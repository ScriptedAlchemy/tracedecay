use std::sync::Mutex;

use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId, Confidence,
    CoverageReportV1, EntityId, EntityKind, EntityRef, EvidenceClass, FactAssertionId, FactEventId,
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, ObservationScopeV1,
    PayloadAccessState, PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId,
    ProjectionGenerationId, ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId,
    RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, ScopeResolutionId, SourceStoreId,
    UtcMicros, VectorWatermark,
};
use tracedecay_store::{
    FactAsOfResponseV1, FactCommitReceipt, FactContradictionStateV1, FactCurrentResponseV1,
    FactLineageCursor, FactLineageResponseV1, FactProposalPromotionStateV1, FactQueryCoverageV1,
    FactStoreResult,
};

use super::*;
use tracedecay_runtime_core::memory::types::{AddFactRequest, MemoryCategory};

#[derive(Default)]
struct FakeAuthority {
    committed: Mutex<Vec<FactWriteBatch>>,
    next_commit_outcome: Mutex<Option<FactCommitOutcome>>,
    promotions: Mutex<Vec<PromoteFactProposal>>,
    promotion_conflict: Mutex<Option<Option<FactProposalPromotionStateV1>>>,
    current_queries: Mutex<Vec<CurrentFactsQuery>>,
    current_results: Mutex<Vec<StoredFactV1>>,
    current_fact_queries: Mutex<Vec<FactCurrentQuery>>,
    current_fact_result: Mutex<Option<StoredFactV1>>,
    as_of_queries: Mutex<Vec<FactAsOfQuery>>,
    as_of_result: Mutex<Option<StoredFactV1>>,
    lineage_queries: Mutex<Vec<FactLineageQuery>>,
    lineage_results: Mutex<Vec<FactLineageEventV1>>,
    legacy_queries: Mutex<Vec<LegacyFactQuery>>,
    legacy_result: Mutex<Option<FactId>>,
    anchor_queries: Mutex<Vec<RetrievalAnchorId>>,
    feedback_history: Mutex<Option<CompatibilityFactFeedbackHistoryV1>>,
    feedback_requests: Mutex<Vec<CompatibilityFactFeedbackCommandV1>>,
    compatibility_calls: Mutex<Vec<&'static str>>,
}

#[derive(Default)]
struct UnavailableEvidenceResolver {
    requests: Mutex<Vec<(FactOwnerV1, RetrievalAnchorId)>>,
}

impl EvidenceAnchorResolver for UnavailableEvidenceResolver {
    async fn resolve_evidence_anchor(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError> {
        self.requests
            .lock()
            .unwrap()
            .push((owner, anchor_id.clone()));
        Err(EvidenceAnchorResolutionError::Unavailable { anchor_id })
    }
}

struct StaticEvidenceResolver {
    record: ResolvedEvidenceAnchorV1,
}

impl EvidenceAnchorResolver for StaticEvidenceResolver {
    async fn resolve_evidence_anchor(
        &self,
        _owner: FactOwnerV1,
        _anchor_id: RetrievalAnchorId,
    ) -> Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError> {
        Ok(self.record.clone())
    }
}

/// The fake holds bare facts and no visibility ledger, so it can only report
/// what it returned as unmeasured. Stating that here keeps the fabrication
/// visible in the double instead of hidden in a `FactStore` trait default.
fn unmeasured_response_metadata(returned: bool) -> (FactQueryCoverageV1, FactContradictionStateV1) {
    (
        FactQueryCoverageV1::new(0, 0, u64::from(returned), 0),
        FactContradictionStateV1::Unknown,
    )
}

impl FactStore for FakeAuthority {
    async fn commit_fact(&self, batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
        let outcome = self
            .next_commit_outcome
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| committed_outcome(&batch));
        self.committed.lock().unwrap().push(batch);
        Ok(outcome)
    }

    async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> FactStoreResult<Vec<StoredFactV1>> {
        self.current_queries.lock().unwrap().push(query);
        Ok(self.current_results.lock().unwrap().clone())
    }

    async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        self.as_of_queries.lock().unwrap().push(query);
        Ok(self.as_of_result.lock().unwrap().clone())
    }

    async fn query_fact_as_of_response(
        &self,
        query: FactAsOfQuery,
    ) -> FactStoreResult<FactAsOfResponseV1> {
        let fact = self.query_fact_as_of(query).await?;
        let (coverage, contradiction) = unmeasured_response_metadata(fact.is_some());
        Ok(FactAsOfResponseV1::new(fact, coverage, contradiction))
    }

    async fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        self.current_fact_queries.lock().unwrap().push(query);
        Ok(self.current_fact_result.lock().unwrap().clone())
    }

    async fn query_fact_current_response(
        &self,
        query: FactCurrentQuery,
    ) -> FactStoreResult<FactCurrentResponseV1> {
        let fact = self.query_fact_current(query).await?;
        let (coverage, contradiction) = unmeasured_response_metadata(fact.is_some());
        Ok(FactCurrentResponseV1::new(fact, coverage, contradiction))
    }

    async fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> FactStoreResult<Vec<FactLineageEventV1>> {
        self.lineage_queries.lock().unwrap().push(query);
        Ok(self.lineage_results.lock().unwrap().clone())
    }

    async fn query_fact_lineage_response(
        &self,
        query: FactLineageQuery,
    ) -> FactStoreResult<FactLineageResponseV1> {
        let events = self.query_fact_lineage(query).await?;
        let (coverage, contradiction) = unmeasured_response_metadata(!events.is_empty());
        Ok(FactLineageResponseV1::new(events, coverage, contradiction))
    }

    async fn resolve_legacy_fact(&self, query: LegacyFactQuery) -> FactStoreResult<Option<FactId>> {
        self.legacy_queries.lock().unwrap().push(query);
        Ok(self.legacy_result.lock().unwrap().clone())
    }

    async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
        self.anchor_queries
            .lock()
            .unwrap()
            .push(query.anchor_id().clone());
        Ok(None)
    }
}

impl FactProposalStore for FakeAuthority {
    async fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError> {
        if let Some(actual) = self.promotion_conflict.lock().unwrap().take() {
            return Err(FactProposalStoreError::ProposalStateConflict {
                proposal_id: promotion.proposal_id().clone(),
                expected: promotion.expected_state(),
                actual,
            });
        }
        let outcome = committed_outcome(promotion.batch());
        let result = PromoteFactProposalOutcome::new(
            promotion.proposal_id().clone(),
            promotion.expected_state(),
            outcome,
        )
        .map_err(FactStoreError::from)?;
        self.promotions.lock().unwrap().push(promotion);
        Ok(result)
    }
}

impl FactCompatibilityStore for FakeAuthority {
    async fn list_compatibility_facts(
        &self,
        query: CompatibilityFactListQueryV1,
    ) -> Result<CompatibilityFactPageV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("list");
        Ok(CompatibilityFactPageV1::new(
            query.owner().clone(),
            vec![],
            None,
        )?)
    }

    async fn search_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("search");
        Ok(CompatibilityFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
        )?)
    }

    async fn probe_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("probe");
        Ok(CompatibilityFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
        )?)
    }

    async fn related_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("related");
        Ok(CompatibilityFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
        )?)
    }

    async fn reason_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> Result<CompatibilityFactSearchPageV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("reason");
        Ok(CompatibilityFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
        )?)
    }

    async fn find_compatibility_contradictions(
        &self,
        query: CompatibilityFactContradictionQueryV1,
    ) -> Result<CompatibilityFactContradictionPageV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("contradictions");
        Ok(CompatibilityFactContradictionPageV1::new(
            query.owner().clone(),
            vec![],
        )?)
    }

    async fn get_compatibility_fact(
        &self,
        _target: CompatibilityFactTargetV1,
    ) -> Result<Option<CompatibilityFactProjectionV1>, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("get");
        Ok(None)
    }

    async fn compatibility_fact_history(
        &self,
        query: CompatibilityFactHistoryQueryV1,
    ) -> Result<CompatibilityFactHistoryV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("history");
        let Some(fact_id) = query.target().canonical_fact_id() else {
            return Err(compatibility_fixture_error());
        };
        Ok(CompatibilityFactHistoryV1::new(
            query.target().owner().clone(),
            fact_id.clone(),
            vec![],
            None,
        )?)
    }

    async fn compatibility_memory_status(
        &self,
        owner: FactOwnerV1,
    ) -> Result<CompatibilityMemoryStatusV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("status");
        compatibility_memory_status(owner)
    }

    async fn inspect_compatibility_fact(
        &self,
        _target: CompatibilityFactTargetV1,
    ) -> Result<Option<CompatibilityFactInspectionV1>, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("inspect");
        Ok(None)
    }

    async fn add_compatibility_fact(
        &self,
        _request: CompatibilityFactAddCommandV1,
    ) -> Result<CompatibilityFactAddOutcomeV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("add");
        Ok(CompatibilityFactAddOutcomeV1::new(
            None,
            tracedecay_store::CompatibilityFactAddDispositionV1::RejectedSecretLike,
            None,
            None,
            Some("fixture rejection".to_owned()),
        )?)
    }

    async fn update_compatibility_fact(
        &self,
        _request: CompatibilityFactUpdateCommandV1,
    ) -> Result<CompatibilityFactUpdateOutcomeV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("update");
        Err(compatibility_fixture_error())
    }

    async fn remove_compatibility_fact(
        &self,
        _request: CompatibilityFactRemoveCommandV1,
    ) -> Result<CompatibilityFactRemoveOutcomeV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("remove");
        Err(compatibility_fixture_error())
    }

    async fn record_compatibility_fact_feedback(
        &self,
        request: CompatibilityFactFeedbackCommandV1,
    ) -> Result<CompatibilityFactFeedbackOutcomeV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("feedback");
        self.feedback_requests.lock().unwrap().push(request);
        Err(compatibility_fixture_error())
    }

    async fn compatibility_fact_feedback_history(
        &self,
        _query: CompatibilityFactFeedbackHistoryQueryV1,
    ) -> Result<CompatibilityFactFeedbackHistoryV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("feedback-history");
        self.feedback_history
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(compatibility_fixture_error)
    }

    async fn find_compatibility_fact_by_content_digest(
        &self,
        _query: CompatibilityFactContentDigestQueryV1,
    ) -> Result<Option<CompatibilityFactProjectionV1>, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("exact-content");
        Ok(None)
    }

    async fn apply_compatibility_fact_curation(
        &self,
        _request: CompatibilityFactCurationBatchV1,
    ) -> Result<CompatibilityFactCurationReceiptV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("curation");
        Err(compatibility_fixture_error())
    }

    async fn merge_compatibility_facts(
        &self,
        _request: CompatibilityFactMergeCommandV1,
    ) -> Result<CompatibilityFactMergeOutcomeV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("merge");
        Err(compatibility_fixture_error())
    }

    async fn repair_compatibility_memory(
        &self,
        _request: CompatibilityMemoryRepairCommandV1,
    ) -> Result<CompatibilityMemoryRepairStatsV1, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("repair");
        Ok(CompatibilityMemoryRepairStatsV1::default())
    }

    async fn dashboard_compatibility_memory_overview(
        &self,
        _query: CompatibilityDashboardMemoryOverviewQueryV1,
    ) -> Result<CompatibilityDashboardMemoryOverviewV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("dashboard-overview");
        Err(compatibility_fixture_error())
    }

    async fn dashboard_compatibility_fact_detail(
        &self,
        _query: CompatibilityDashboardFactDetailQueryV1,
    ) -> Result<Option<CompatibilityDashboardFactDetailV1>, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("dashboard-detail");
        Ok(None)
    }

    async fn dashboard_compatibility_vector_points(
        &self,
        _query: CompatibilityDashboardVectorPointsQueryV1,
    ) -> Result<Vec<CompatibilityDashboardVectorPointV1>, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("dashboard-vectors");
        Ok(vec![])
    }

    async fn dashboard_compatibility_memory_oplog(
        &self,
        _query: CompatibilityDashboardOplogQueryV1,
    ) -> Result<Vec<CompatibilityDashboardOplogEntryV1>, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("dashboard-oplog");
        Ok(vec![])
    }

    async fn record_compatibility_fact_retrieval(
        &self,
        _request: CompatibilityFactRetrievalCommandV1,
    ) -> Result<Vec<CompatibilityFactProjectionV1>, FactCompatibilityStoreError> {
        self.compatibility_calls.lock().unwrap().push("retrieval");
        Ok(vec![])
    }

    async fn submit_compatibility_fact_proposal(
        &self,
        _proposal_id: ProvenanceId,
        _request: CompatibilityFactAddCommandV1,
        _submitter: Option<ActorId>,
    ) -> Result<CompatibilityFactProposalRecordV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-submit");
        Err(compatibility_fixture_error())
    }

    async fn get_compatibility_fact_proposal(
        &self,
        _owner: FactOwnerV1,
        _proposal_id: ProvenanceId,
    ) -> Result<Option<CompatibilityFactProposalRecordV1>, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-get");
        Ok(None)
    }

    async fn list_compatibility_fact_proposals(
        &self,
        _owner: FactOwnerV1,
        _state: Option<CompatibilityFactProposalStateV1>,
        _after_proposal_id: Option<ProvenanceId>,
        _limit: usize,
    ) -> Result<CompatibilityFactProposalPageV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-list");
        Err(compatibility_fixture_error())
    }

    async fn count_pending_compatibility_fact_proposals(
        &self,
        _owner: FactOwnerV1,
    ) -> Result<u64, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-count-pending");
        Ok(0)
    }

    async fn reject_compatibility_fact_proposal(
        &self,
        _owner: FactOwnerV1,
        _proposal_id: ProvenanceId,
        _expected_revision: CompatibilityFactProposalRevisionV1,
        _reviewer: ActorId,
        _reason: String,
    ) -> Result<CompatibilityFactProposalRecordV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-reject");
        Err(compatibility_fixture_error())
    }

    async fn import_legacy_compatibility_fact_proposals(
        &self,
        _request: CompatibilityFactProposalImportV1,
    ) -> Result<CompatibilityFactProposalImportReceiptV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-import");
        Err(compatibility_fixture_error())
    }

    async fn promote_compatibility_fact_proposal(
        &self,
        _request: CompatibilityFactProposalPromotionV1,
    ) -> Result<CompatibilityFactProposalRecordV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-promote");
        Err(compatibility_fixture_error())
    }

    async fn promote_compatibility_fact_proposal_with_disposition(
        &self,
        _request: CompatibilityFactProposalPromotionV1,
    ) -> Result<CompatibilityFactProposalPromotionResultV1, FactCompatibilityStoreError> {
        self.compatibility_calls
            .lock()
            .unwrap()
            .push("proposal-promote-disposition");
        Err(compatibility_fixture_error())
    }
}

fn compatibility_fixture_error() -> FactCompatibilityStoreError {
    FactCompatibilityStoreError::Store(FactStoreError::Contract(DomainError::NonCanonical {
        field: "fake compatibility authority",
    }))
}

fn compatibility_memory_status(
    owner: FactOwnerV1,
) -> Result<CompatibilityMemoryStatusV1, FactCompatibilityStoreError> {
    Ok(CompatibilityMemoryStatusV1::new(
        owner,
        0,
        0,
        0,
        tracedecay_store::CompatibilityMemoryAlgebraV1::new("fixture".to_owned(), 1, 1)?,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        tracedecay_store::CompatibilityProjectionStateV1::Ready,
        tracedecay_store::CompatibilityMemoryRepairStatsV1::default(),
        tracedecay_store::CompatibilityMemoryFeedbackFunnelV1::new(0, 0, 0, 0, 0),
    )?)
}

fn owner() -> FactOwnerV1 {
    FactOwnerV1::Project {
        project_id: ProjectId::new("project.memory.application").unwrap(),
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String, Error = DomainError>,
{
    T::try_from(value.to_owned()).unwrap()
}

fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            owner,
            FactIdentitySourceV1::Application {
                operation_id: id(operation),
            },
        )
        .unwrap(),
    )
    .unwrap()
}

fn batch(owner: FactOwnerV1, operation: &str) -> FactWriteBatch {
    let fact_id = fact_id(owner.clone(), operation);
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(1),
        None,
    )
    .unwrap();
    FactWriteBatch::new(
        fact_id,
        owner,
        None,
        vec![event],
        vec![],
        vec![],
        None,
        None,
    )
    .unwrap()
}

fn committed_outcome(batch: &FactWriteBatch) -> FactCommitOutcome {
    let event_ids: Vec<FactEventId> = batch
        .events()
        .iter()
        .map(|event| event.event_id().clone())
        .collect();
    let last_event_id = event_ids.last().unwrap().clone();
    let active_assertion_id: Option<FactAssertionId> = batch
        .assertion()
        .map(|assertion| assertion.assertion_id().clone());
    FactCommitOutcome::Committed(
        FactCommitReceipt::new(
            batch.fact_id().clone(),
            batch.owner().clone(),
            event_ids,
            last_event_id,
            active_assertion_id,
        )
        .unwrap(),
    )
}

fn stored_fact(owner: FactOwnerV1, operation: &str, projected_as_of: UtcMicros) -> StoredFactV1 {
    let fact_id = fact_id(owner.clone(), operation);
    StoredFactV1::new(
        fact_id,
        owner,
        None,
        PayloadAccessState::Deleted,
        Confidence::new(0.5).unwrap(),
        id(&format!("assertion.{operation}")),
        id(&format!("event.{operation}")),
        None,
        projected_as_of,
    )
    .unwrap()
}

fn profile_anchor() -> RetrievalAnchorRecordV2 {
    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new("entity.memory.external").unwrap(),
            kind: EntityKind::Document,
        }),
        owner: ObservationScopeV1::Profile,
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.memory.external").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![],
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.memory.external").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.memory.external").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
            capability_id: CapabilityId::new("capability.memory.external").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.memory.external").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

fn legacy_add_request() -> AddFactRequest {
    AddFactRequest {
        content: "legacy conversion fixture".to_owned(),
        category: MemoryCategory::Project,
        source: None,
        tags: vec![],
        entities: vec![],
        trust: None,
        metadata: serde_json::json!({}),
    }
}

#[tokio::test]
async fn canonical_batch_is_the_single_write_boundary() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let write = batch(owner(), "operation.memory.commit");
    let expected_fact_id = write.fact_id().clone();

    let outcome = application.commit_fact(write).await.unwrap();

    assert!(matches!(outcome, FactCommitOutcome::Committed(_)));
    let committed = application.authority.committed.lock().unwrap();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].fact_id(), &expected_fact_id);
}

#[tokio::test]
async fn idempotent_replay_preserves_the_canonical_commit_identity() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let write = batch(owner(), "operation.memory.replay");
    let replay = match committed_outcome(&write) {
        FactCommitOutcome::Committed(receipt) => FactCommitOutcome::IdempotentReplay(receipt),
        _ => unreachable!("fixture always commits"),
    };
    *application.authority.next_commit_outcome.lock().unwrap() = Some(replay);

    let outcome = application.commit_fact(write).await.unwrap();

    assert!(matches!(outcome, FactCommitOutcome::IdempotentReplay(_)));
    assert_eq!(application.authority.committed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn evidence_resolution_is_owner_bound_at_the_daemon_boundary() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let resolver = UnavailableEvidenceResolver::default();
    let anchor_id = id::<RetrievalAnchorId>("anchor.memory.external");

    let error = application
        .resolve_evidence_anchor(&resolver, anchor_id.clone())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::EvidenceAnchor(EvidenceAnchorResolutionError::Unavailable {
            anchor_id: actual,
        }) if actual == anchor_id
    ));
    assert_eq!(
        resolver.requests.lock().unwrap().as_slice(),
        &[(owner(), anchor_id)]
    );
}

#[tokio::test]
async fn evidence_resolution_rejects_a_cross_owner_daemon_reply() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let record = profile_anchor();
    let anchor_id = record.anchor_id().clone();
    let resolver = StaticEvidenceResolver {
        record: ResolvedEvidenceAnchorV1::new(record).unwrap(),
    };

    let error = application
        .resolve_evidence_anchor(&resolver, anchor_id)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::InvalidAuthorityResult {
            invariant: "resolved evidence anchor identity and owner"
        }
    ));
}

#[tokio::test]
async fn owner_mismatch_is_rejected_before_authority_access() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let error = application
        .commit_fact(batch(FactOwnerV1::Profile, "operation.profile.commit"))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::OwnerMismatch { .. }
    ));
    assert!(application.authority.committed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn query_owner_mismatch_is_rejected_before_authority_access() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let query = CurrentFactsQuery::new(FactOwnerV1::Profile, None, 10).unwrap();

    let error = application.query_current_facts(query).await.unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::OwnerMismatch { .. }
    ));
    assert!(
        application
            .authority
            .current_queries
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn compatibility_reads_use_finite_owner_bound_authority_methods() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let fact_id = fact_id(owner(), "operation.compatibility.read");
    let target = CompatibilityFactTargetV1::Canonical(
        tracedecay_store::CompatibilityFactIdV1::new(owner(), fact_id).unwrap(),
    );
    let search = CompatibilityFactSearchQuery::new(
        owner(),
        tracedecay_store::CompatibilityFactSearchKindV1::Search,
        Some("compatibility fixture".to_owned()),
        None,
        10,
    )
    .unwrap();

    assert!(
        application
            .list_compatibility_facts(
                CompatibilityFactListQueryV1::new(owner(), None, None, None, 10).unwrap(),
            )
            .await
            .unwrap()
            .facts()
            .is_empty()
    );
    assert!(
        application
            .search_compatibility_facts(search)
            .await
            .unwrap()
            .hits()
            .is_empty()
    );
    assert!(
        application
            .get_compatibility_fact(target.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        application
            .get_compatibility_history(
                CompatibilityFactHistoryQueryV1::new(target.clone(), None, 10).unwrap(),
            )
            .await
            .unwrap()
            .events()
            .is_empty()
    );
    assert_eq!(
        application
            .compatibility_memory_status()
            .await
            .unwrap()
            .owner(),
        &owner()
    );
    assert!(
        application
            .inspect_compatibility_fact(target)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        application
            .authority
            .compatibility_calls
            .lock()
            .unwrap()
            .as_slice(),
        ["list", "search", "get", "history", "status", "inspect"]
    );
}

#[tokio::test]
async fn compatibility_read_owner_mismatch_never_reaches_authority() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let error = application
        .list_compatibility_facts(
            CompatibilityFactListQueryV1::new(FactOwnerV1::Profile, None, None, None, 10).unwrap(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::OwnerMismatch { .. }
    ));
    assert!(
        application
            .authority
            .compatibility_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn proposal_cas_and_batch_commit_are_one_authority_operation() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let promotion = PromoteFactProposal::new(
        id("proposal.memory.1"),
        owner(),
        FactProposalPromotionStateV1::PendingApproval,
        Some(id("actor.reviewer")),
        batch(owner(), "operation.proposal.promote"),
    )
    .unwrap();

    let outcome = application.promote_fact_proposal(promotion).await.unwrap();

    assert!(matches!(outcome.commit(), FactCommitOutcome::Committed(_)));
    assert_eq!(application.authority.promotions.lock().unwrap().len(), 1);
    assert!(application.authority.committed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn proposal_cas_conflict_is_typed_and_does_not_commit_a_batch() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    *application.authority.promotion_conflict.lock().unwrap() =
        Some(Some(FactProposalPromotionStateV1::Applying));
    let promotion = PromoteFactProposal::new(
        id("proposal.memory.conflict"),
        owner(),
        FactProposalPromotionStateV1::PendingApproval,
        Some(id("actor.reviewer")),
        batch(owner(), "operation.proposal.conflict"),
    )
    .unwrap();

    let error = application
        .promote_fact_proposal(promotion)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::Authority(FactProposalStoreError::ProposalStateConflict {
            expected: FactProposalPromotionStateV1::PendingApproval,
            actual: Some(FactProposalPromotionStateV1::Applying),
            ..
        })
    ));
    assert!(application.authority.promotions.lock().unwrap().is_empty());
    assert!(application.authority.committed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn typed_queries_propagate_without_identity_loss() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let fact_id = fact_id(owner(), "operation.memory.query");
    let current = CurrentFactsQuery::new(owner(), None, 10).unwrap();
    let current_fact = FactCurrentQuery::new(owner(), fact_id.clone()).unwrap();
    let as_of = FactAsOfQuery::new(owner(), fact_id.clone(), UtcMicros(5)).unwrap();
    let lineage = FactLineageQuery::new(owner(), fact_id, None, 10).unwrap();
    let legacy = LegacyFactQuery::new(owner(), id::<SourceStoreId>("store.legacy"), 7).unwrap();
    let anchor_id = id::<RetrievalAnchorId>("anchor.memory.query");
    let anchor_query = RetrievalAnchorQuery::new(owner(), anchor_id.clone()).unwrap();

    assert!(
        application
            .query_current_facts(current)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        application
            .query_fact_current(current_fact)
            .await
            .unwrap()
            .is_none()
    );
    assert!(application.query_fact_as_of(as_of).await.unwrap().is_none());
    assert!(
        application
            .query_fact_lineage(lineage)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        application
            .resolve_legacy_fact(legacy)
            .await
            .unwrap()
            .is_none()
    );
    let anchor: Option<RetrievalAnchorRecordV2> = application
        .get_retrieval_anchor(anchor_query)
        .await
        .unwrap();
    assert!(anchor.is_none());

    assert_eq!(
        application.authority.current_queries.lock().unwrap().len(),
        1
    );
    assert_eq!(
        application
            .authority
            .current_fact_queries
            .lock()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(application.authority.as_of_queries.lock().unwrap().len(), 1);
    assert_eq!(
        application.authority.lineage_queries.lock().unwrap().len(),
        1
    );
    assert_eq!(
        application.authority.legacy_queries.lock().unwrap().len(),
        1
    );
    assert_eq!(
        application
            .authority
            .anchor_queries
            .lock()
            .unwrap()
            .as_slice(),
        &[anchor_id]
    );
}

#[tokio::test]
async fn current_page_must_advance_cursor_and_stay_bounded() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let first = stored_fact(owner(), "operation.current.first", UtcMicros(1));
    *application.authority.current_results.lock().unwrap() = vec![first.clone()];

    let error = application
        .query_current_facts(
            CurrentFactsQuery::new(owner(), Some(first.fact_id().clone()), 1).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        MemoryApplicationError::InvalidAuthorityResult { .. }
    ));

    let second = stored_fact(owner(), "operation.current.second", UtcMicros(2));
    let mut results = vec![first, second];
    results.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
    *application.authority.current_results.lock().unwrap() = results;

    let error = application
        .query_current_facts(CurrentFactsQuery::new(owner(), None, 1).unwrap())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        MemoryApplicationError::InvalidAuthorityResult { .. }
    ));
}

#[tokio::test]
async fn as_of_result_cannot_project_after_requested_time() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let fact = stored_fact(owner(), "operation.as-of.future", UtcMicros(6));
    *application.authority.as_of_result.lock().unwrap() = Some(fact.clone());

    let error = application
        .query_fact_as_of(
            FactAsOfQuery::new(owner(), fact.fact_id().clone(), UtcMicros(5)).unwrap(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::InvalidAuthorityResult { .. }
    ));
}

#[tokio::test]
async fn lineage_page_must_advance_cursor_and_stay_bounded() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let fact_id = fact_id(owner(), "operation.lineage.cursor");
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(1),
        None,
    )
    .unwrap();
    let cursor = FactLineageCursor::new(event.occurred_at(), event.event_id().clone()).unwrap();
    *application.authority.lineage_results.lock().unwrap() = vec![event];

    let error = application
        .query_fact_lineage(FactLineageQuery::new(owner(), fact_id, Some(cursor), 1).unwrap())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::InvalidAuthorityResult { .. }
    ));
}

#[tokio::test]
async fn legacy_resolution_cannot_cross_owner_boundary() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    *application.authority.legacy_result.lock().unwrap() = Some(fact_id(
        FactOwnerV1::Profile,
        "operation.legacy.cross-owner",
    ));

    let error = application
        .resolve_legacy_fact(
            LegacyFactQuery::new(owner(), id::<SourceStoreId>("store.legacy"), 7).unwrap(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::InvalidAuthorityResult { .. }
    ));
}

#[tokio::test]
async fn v1_feedback_defaults_an_omitted_source_to_mcp() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let context = MemoryOperationContext::from_trusted_request_id(
        &owner(),
        "feedback",
        "fixture-feedback-mcp",
        None,
    )
    .unwrap();

    let _ = application
        .record_fact_feedback_v1(
            FeedbackRequest {
                fact_id: 1,
                action: FeedbackAction::Helpful,
                source: None,
                note: None,
            },
            context,
        )
        .await;

    let requests = application.authority.feedback_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].source(), Some("mcp"));
}

#[tokio::test]
async fn v1_trust_history_never_claims_incomplete_repair_is_complete() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    *application.authority.feedback_history.lock().unwrap() = Some(
        CompatibilityFactFeedbackHistoryV1::new_with_repair_progress(
            owner(),
            vec![],
            None,
            CompatibilityFeedbackRepairProgressV1::Incomplete {
                processed: 1,
                remaining: Some(2),
            },
        )
        .unwrap(),
    );

    let typed = application
        .fact_trust_history_with_progress_v1(1, 10)
        .await
        .unwrap();
    assert!(typed.entries.is_empty());
    assert!(matches!(
        typed.repair_progress,
        CompatibilityFeedbackRepairProgressV1::Incomplete {
            processed: 1,
            remaining: Some(2)
        }
    ));

    let error = application.fact_trust_history_v1(1, 10).await.unwrap_err();
    assert!(matches!(
        error,
        MemoryApplicationError::FeedbackHistoryUnavailable {
            progress: CompatibilityFeedbackRepairProgressV1::Incomplete { .. }
        }
    ));
    assert_eq!(
        application
            .authority
            .compatibility_calls
            .lock()
            .unwrap()
            .as_slice(),
        ["feedback-history", "feedback-history"]
    );
}

#[test]
fn automation_run_identity_is_typed_not_fact_payload_metadata() {
    let mut request = legacy_add_request();
    request.metadata = serde_json::json!({
        "automation_run_id": "caller-controlled-run-id",
        "fixture": "retained",
    });

    let command = automation_fact_proposal_add_command(
        owner(),
        request,
        "run_01J4A7P5MQ1X9DX2P9BQNQW75T",
        "proposal-typed-run-id",
        None,
    )
    .unwrap();

    assert_eq!(
        command.automation_run_id(),
        Some("run_01J4A7P5MQ1X9DX2P9BQNQW75T")
    );
    assert_eq!(
        command.metadata().get("fixture"),
        Some(&serde_json::Value::String("retained".to_owned()))
    );
    assert!(command.metadata().get("automation_run_id").is_none());
}

#[test]
fn stored_fact_fixture_remains_canonical() {
    let stored = stored_fact(owner(), "operation.memory.fixture", UtcMicros(2));
    let fact_id = stored.fact_id().clone();
    assert_eq!(stored.fact_id(), &fact_id);
}

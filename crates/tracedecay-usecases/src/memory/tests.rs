use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId, Confidence,
    CoverageReportV1, EntityId, EntityKind, EntityRef, EvidenceClass, FactAssertionId, FactEventId,
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, ObservationScopeV1,
    PayloadAccessState, PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId,
    ProjectionGenerationId, ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId,
    RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, ScopeResolutionId, UtcMicros,
    VectorWatermark,
};
use tracedecay_store::{
    FactAsOfResponseV1, FactCommitReceipt, FactContradictionStateV1, FactCurrentResponseV1,
    FactLineageCursor, FactLineageResponseV1, FactQueryCoverageV1, FactStoreResult,
    ProjectMemoryAutomationRunReceiptsV1, ProjectMemoryFactIdV1,
};

use super::*;

mod project_memory_contracts;
mod read_control;
mod retrieval;

#[path = "tests/curation_mutations.rs"]
mod curation_mutations;

#[derive(Default)]
struct FakeAuthority {
    committed: Mutex<Vec<FactWriteBatch>>,
    next_commit_outcome: Mutex<Option<FactCommitOutcome>>,
    current_queries: Mutex<Vec<CurrentFactsQuery>>,
    current_results: Mutex<Vec<StoredFactV1>>,
    current_fact_queries: Mutex<Vec<FactCurrentQuery>>,
    current_fact_result: Mutex<Option<StoredFactV1>>,
    as_of_queries: Mutex<Vec<FactAsOfQuery>>,
    as_of_result: Mutex<Option<StoredFactV1>>,
    lineage_queries: Mutex<Vec<FactLineageQuery>>,
    lineage_results: Mutex<Vec<FactLineageEventV1>>,
    anchor_queries: Mutex<Vec<RetrievalAnchorId>>,
    authority_calls: Mutex<Vec<&'static str>>,
    search_read_controls: Mutex<Vec<usize>>,
    retrieval_outcome: Mutex<Option<ProjectMemoryFactRetrievalOutcomeV1>>,
    automatic_fact_apply_result: Mutex<Option<ProjectMemoryAutomaticFactApplyResultV1>>,
    merge_outcome: Mutex<Option<ProjectMemoryFactMergeOutcomeV1>>,
    curation_requests: Mutex<Vec<ProjectMemoryFactCurationBatchV1>>,
    curation_receipt: Mutex<Option<ProjectMemoryFactCurationReceiptV1>>,
    automation_run_receipts: Mutex<Option<ProjectMemoryAutomationRunReceiptsV1>>,
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
    ) -> Result<ResolvedEvidenceAnchor, EvidenceAnchorResolutionError> {
        self.requests
            .lock()
            .unwrap()
            .push((owner, anchor_id.clone()));
        Err(EvidenceAnchorResolutionError::Unavailable { anchor_id })
    }
}

struct StaticEvidenceResolver {
    record: ResolvedEvidenceAnchor,
}

impl EvidenceAnchorResolver for StaticEvidenceResolver {
    async fn resolve_evidence_anchor(
        &self,
        _owner: FactOwnerV1,
        _anchor_id: RetrievalAnchorId,
    ) -> Result<ResolvedEvidenceAnchor, EvidenceAnchorResolutionError> {
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
    async fn commit_fact(
        &self,
        batch: FactWriteBatch,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<FactCommitOutcome> {
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

impl ProjectMemoryFactStore for FakeAuthority {
    async fn purge_project_memory_superseded_payloads(
        &self,
        owner: FactOwnerV1,
        _after: Option<ProjectMemoryPrivacyPurgeCursorV1>,
        _limit: usize,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryPrivacyPurgeReceiptV1> {
        self.authority_calls.lock().unwrap().push("privacy-purge");
        ProjectMemoryPrivacyPurgeReceiptV1::new(
            owner,
            "test-detector-revision".to_owned(),
            0,
            0,
            None,
        )
    }

    async fn list_project_memory_facts(
        &self,
        query: ProjectMemoryFactListQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactPageV1> {
        self.authority_calls.lock().unwrap().push("list");
        ProjectMemoryFactPageV1::new(query.owner().clone(), vec![], None)
    }

    async fn search_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        self.search_read_controls
            .lock()
            .unwrap()
            .push(std::ptr::from_ref(read_control).addr());
        self.authority_calls.lock().unwrap().push("search");
        ProjectMemoryFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
            ProjectMemoryFactSearchGraphCoverageV1::NotMounted,
        )
    }

    async fn probe_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        self.authority_calls.lock().unwrap().push("probe");
        ProjectMemoryFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
            ProjectMemoryFactSearchGraphCoverageV1::NotApplicable,
        )
    }

    async fn related_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        self.authority_calls.lock().unwrap().push("related");
        ProjectMemoryFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
            ProjectMemoryFactSearchGraphCoverageV1::NotMounted,
        )
    }

    async fn reason_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        self.authority_calls.lock().unwrap().push("reason");
        ProjectMemoryFactSearchPageV1::new(
            query.owner().clone(),
            vec![],
            None,
            ProjectMemoryFactSearchGraphCoverageV1::NotApplicable,
        )
    }

    async fn find_project_memory_contradictions(
        &self,
        query: ProjectMemoryFactContradictionQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactContradictionPageV1> {
        self.authority_calls.lock().unwrap().push("contradictions");
        ProjectMemoryFactContradictionPageV1::new(query.owner().clone(), vec![])
    }

    async fn get_project_memory_fact(
        &self,
        _target: ProjectMemoryFactIdV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
        self.authority_calls.lock().unwrap().push("get");
        Ok(None)
    }

    async fn project_memory_fact_history(
        &self,
        query: ProjectMemoryFactHistoryQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactHistoryV1> {
        self.authority_calls.lock().unwrap().push("history");
        ProjectMemoryFactHistoryV1::new(
            query.target().owner().clone(),
            query.target().fact_id().clone(),
            vec![],
            None,
        )
    }

    async fn project_memory_status(
        &self,
        owner: FactOwnerV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryMemoryStatusV1> {
        self.authority_calls.lock().unwrap().push("status");
        project_memory_status(owner)
    }

    async fn inspect_project_memory_fact(
        &self,
        _target: ProjectMemoryFactIdV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryFactInspectionV1>> {
        self.authority_calls.lock().unwrap().push("inspect");
        Ok(None)
    }

    async fn add_project_memory_fact(
        &self,
        _request: ProjectMemoryFactAddCommandV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactAddOutcomeV1> {
        self.authority_calls.lock().unwrap().push("add");
        Err(authority_fixture_error())
    }

    async fn update_project_memory_fact(
        &self,
        _request: ProjectMemoryFactUpdateCommandV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactUpdateOutcomeV1> {
        self.authority_calls.lock().unwrap().push("update");
        Err(authority_fixture_error())
    }

    async fn remove_project_memory_fact(
        &self,
        _request: ProjectMemoryFactRemoveCommandV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactRemoveOutcomeV1> {
        self.authority_calls.lock().unwrap().push("remove");
        Err(authority_fixture_error())
    }

    async fn record_project_memory_fact_feedback(
        &self,
        _request: ProjectMemoryFactFeedbackCommandV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactFeedbackOutcomeV1> {
        self.authority_calls.lock().unwrap().push("feedback");
        Err(authority_fixture_error())
    }

    async fn project_memory_fact_feedback_history(
        &self,
        query: ProjectMemoryFactFeedbackHistoryQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactFeedbackHistoryV1> {
        self.authority_calls
            .lock()
            .unwrap()
            .push("feedback-history");
        ProjectMemoryFactFeedbackHistoryV1::new(query.target().owner().clone(), vec![], None)
    }

    async fn find_project_memory_fact_by_content_digest(
        &self,
        _query: ProjectMemoryFactContentDigestQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
        self.authority_calls.lock().unwrap().push("exact-content");
        Ok(None)
    }

    async fn apply_project_memory_fact_curation(
        &self,
        request: ProjectMemoryFactCurationBatchV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactCurationReceiptV1> {
        self.authority_calls.lock().unwrap().push("curation");
        self.curation_requests.lock().unwrap().push(request);
        self.curation_receipt
            .lock()
            .unwrap()
            .take()
            .ok_or_else(authority_fixture_error)
    }

    async fn merge_project_memory_facts(
        &self,
        _request: ProjectMemoryFactMergeCommandV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactMergeOutcomeV1> {
        self.authority_calls.lock().unwrap().push("merge");
        self.merge_outcome
            .lock()
            .unwrap()
            .take()
            .ok_or_else(authority_fixture_error)
    }

    async fn dashboard_project_memory_overview(
        &self,
        _query: ProjectMemoryDashboardMemoryOverviewQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryDashboardMemoryOverviewV1> {
        Err(authority_fixture_error())
    }

    async fn dashboard_project_memory_fact_detail(
        &self,
        _query: ProjectMemoryDashboardFactDetailQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryDashboardFactDetailV1>> {
        Ok(None)
    }

    async fn dashboard_project_memory_vector_points(
        &self,
        _query: ProjectMemoryDashboardVectorPointsQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<Vec<ProjectMemoryDashboardVectorPointV1>> {
        self.authority_calls
            .lock()
            .unwrap()
            .push("dashboard-vectors");
        Ok(vec![])
    }

    async fn dashboard_project_memory_oplog(
        &self,
        _query: ProjectMemoryDashboardOplogQueryV1,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<Vec<ProjectMemoryDashboardOplogEntryV1>> {
        self.authority_calls.lock().unwrap().push("dashboard-oplog");
        Ok(vec![])
    }

    async fn record_project_memory_fact_retrieval(
        &self,
        _request: ProjectMemoryFactRetrievalCommandV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactRetrievalOutcomeV1> {
        let outcome = self.retrieval_outcome.lock().unwrap().take();
        outcome.ok_or_else(authority_fixture_error)
    }

    async fn apply_project_memory_automatic_fact(
        &self,
        _apply_id: ProvenanceId,
        _request: ProjectMemoryFactAddCommandV1,
        _evidence: ProjectMemoryAutomaticFactEvidenceV1,
        _write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryAutomaticFactApplyResultV1> {
        self.authority_calls
            .lock()
            .unwrap()
            .push("automatic-fact-apply");
        self.automatic_fact_apply_result
            .lock()
            .unwrap()
            .take()
            .ok_or_else(authority_fixture_error)
    }

    async fn get_project_memory_automatic_fact_receipt(
        &self,
        _owner: FactOwnerV1,
        _apply_id: ProvenanceId,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryAutomaticFactReceiptV1>> {
        self.authority_calls
            .lock()
            .unwrap()
            .push("automatic-fact-get");
        Ok(None)
    }

    async fn list_project_memory_automatic_fact_receipts(
        &self,
        owner: FactOwnerV1,
        _state: Option<ProjectMemoryAutomaticFactStateV1>,
        _after_apply_id: Option<ProvenanceId>,
        _limit: usize,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryAutomaticFactReceiptPageV1> {
        self.authority_calls
            .lock()
            .unwrap()
            .push("automatic-fact-list");
        ProjectMemoryAutomaticFactReceiptPageV1::new(owner, vec![], None)
    }

    async fn project_memory_automation_run_receipts(
        &self,
        owner: FactOwnerV1,
        run_id: tracedecay_domain::RunId,
        _read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryAutomationRunReceiptsV1> {
        self.authority_calls
            .lock()
            .unwrap()
            .push("automation-run-receipts");
        match self.automation_run_receipts.lock().unwrap().take() {
            Some(receipts) => Ok(receipts),
            None => ProjectMemoryAutomationRunReceiptsV1::new(owner, run_id, None, vec![]),
        }
    }
}

fn authority_fixture_error() -> FactStoreError {
    FactStoreError::Contract(DomainError::NonCanonical {
        field: "fake project-memory authority",
    })
}

fn project_memory_status(owner: FactOwnerV1) -> FactStoreResult<ProjectMemoryMemoryStatusV1> {
    ProjectMemoryMemoryStatusV1::new(
        owner,
        0,
        0,
        tracedecay_store::ProjectMemoryMemoryAlgebraV1::new("fixture".to_owned(), 1, 1)?,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        tracedecay_store::ProjectMemoryMemoryFeedbackFunnelV1::new(0, 0, 0, 0, 0),
    )
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
    FactWriteBatch::new(fact_id, owner, None, vec![event], vec![], vec![], None).unwrap()
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

fn fact_add_request() -> ProjectMemoryFactAddRequest {
    ProjectMemoryFactAddRequest {
        content: "canonical add fixture".to_owned(),
        category: tracedecay_domain::FactCategoryV1::Project,
        source_label: None,
        tags: vec![],
        entities: vec![],
        trust: None,
        metadata: serde_json::json!({}),
    }
}

fn write_control() -> FactWriteControl {
    let interrupted = Arc::new(AtomicBool::new(false));
    let commit_started = Arc::new(AtomicBool::new(false));
    FactWriteControl::new(
        {
            let interrupted = interrupted.clone();
            Arc::new(move || interrupted.load(Ordering::Acquire))
        },
        Arc::new(move || {
            commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }),
    )
}

#[tokio::test]
async fn canonical_batch_is_the_single_write_boundary() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let write = batch(owner(), "operation.memory.commit");
    let expected_fact_id = write.fact_id().clone();

    let outcome = application
        .commit_fact(write, &write_control())
        .await
        .unwrap();

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

    let outcome = application
        .commit_fact(write, &write_control())
        .await
        .unwrap();

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
        record: ResolvedEvidenceAnchor::new(record).unwrap(),
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
        .commit_fact(
            batch(FactOwnerV1::Profile, "operation.profile.commit"),
            &write_control(),
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::OwnerMismatch { .. }
    ));
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
async fn automation_receipt_recovery_rejects_foreign_authority_identity() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let requested_run = tracedecay_domain::RunId::new("run.requested-recovery").unwrap();
    *application
        .authority
        .automation_run_receipts
        .lock()
        .unwrap() = Some(
        ProjectMemoryAutomationRunReceiptsV1::new(
            FactOwnerV1::Profile,
            requested_run.clone(),
            None,
            vec![],
        )
        .unwrap(),
    );

    let result = application
        .project_memory_automation_run_receipts(
            requested_run,
            &FactReadControl::new(Arc::new(|| false)),
        )
        .await;

    assert!(matches!(
        result,
        Err(MemoryApplicationError::InvalidAuthorityResult {
            invariant: "memory automation receipt recovery identity",
        })
    ));
}

#[test]
fn graph_reset_required_keeps_profile_and_project_authority_in_root_errors() {
    let profile = memory_application_error(MemoryApplicationError::Store(
        FactStoreError::GraphResetRequired {
            owner: FactOwnerV1::Profile,
            reason: "profile graph generation mismatch".to_owned(),
        },
    ));
    assert!(matches!(
        profile,
        TraceDecayError::ResetRequired { authority, reason }
            if authority == "profile memory graph"
                && reason == "profile graph generation mismatch"
    ));

    let project = memory_application_error(MemoryApplicationError::Store(
        FactStoreError::GraphResetRequired {
            owner: owner(),
            reason: "project graph generation mismatch".to_owned(),
        },
    ));
    assert!(matches!(
        project,
        TraceDecayError::ResetRequired { authority, reason }
            if authority == "project memory graph"
                && reason == "project graph generation mismatch"
    ));
}

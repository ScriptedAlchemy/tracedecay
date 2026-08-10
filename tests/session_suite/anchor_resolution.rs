//! Typed seven-state evidence-anchor resolution through the daemon product
//! path: current, drifted, redacted, expired, deleted, unavailable, and
//! ambiguous, each reported with coverage and watermark drift, without ever
//! silently switching owner, target, or source generation.

use std::collections::BTreeMap;

use serde_json::json;
use tempfile::TempDir;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::{
    AnchorResolutionStateV2, ClaudeByteRangeV1, ClaudeFileGenerationV1,
    ClaudeObservationIdentityMaterialV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1,
    ComponentVersion, DurableClaudeObservationV1, FactId, FactLineageEventV1, FactOwnerV1,
    ObservationScopeV1, PayloadAccessState, PayloadReferenceV1, ProjectionGenerationId,
    RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts,
    RetrievalAnchorTargetV2, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, ShardId, UtcMicros,
    VectorWatermark, WatermarkDriftV1,
};
use tracedecay_store::{
    AnchoredObservationWrite, CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1,
    FactCommitOutcome, FactCurrentQuery, FactCurrentResponseV1, FactLineageQuery,
    FactLineageResponseV1, FactStore, FactStoreResult, FactWriteBatch, LegacyFactQuery,
    ObservationCommitReceipt, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite, RetrievalAnchorQuery, SESSION_MESSAGE_PROJECTOR_VERSION,
    StoredFactV1, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};
use tracedecay_usecases::anchor_resolution::{
    EvidenceAnchorReportResolver, EvidenceAnchorResolutionReport,
};
use tracedecay_usecases::host_admission::{HostAdmissionFacade, HostAdmissionScope};
use tracedecay_usecases::memory::MemoryApplication;

const GENERATION: u64 = 7;
const PROJECTION_SHARD: &str = "observation.projection";

fn source() -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new("session.anchor-resolution").unwrap()).unwrap()
}

fn scope() -> ObservationScopeV1 {
    ObservationScopeV1::Profile
}

fn observation(start: u64, end: u64, receipt_id: &str, body: &str) -> DurableClaudeObservationV1 {
    let payload = json!({
        "kind": "assistant_message",
        "body": body,
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::new(
        source(),
        scope(),
        ClaudeFileGenerationV1::new(GENERATION).unwrap(),
        ClaudeByteRangeV1::new(start, end).unwrap(),
    )
    .unwrap();
    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.test").unwrap(),
        payload,
    )
    .unwrap()
}

fn write(
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) -> ObservationWrite {
    let next_cursor = ClaudeSourceCursorV1::new(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().position().end(),
    )
    .unwrap();
    ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap()
}

fn watermark(components: &[(&str, u64)]) -> VectorWatermark {
    VectorWatermark {
        components: components
            .iter()
            .map(|(shard, sequence)| (ShardId::new(*shard).unwrap(), *sequence))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn anchored_write(write: ObservationWrite) -> AnchoredObservationWrite {
    anchored_write_with(
        write,
        PayloadAccessState::Eligible,
        VectorWatermark::default(),
    )
}

/// Builds the product-path anchored write with an explicitly declared payload
/// access and frozen projection watermark, exactly as a tombstone-retaining or
/// watermark-freezing ingress would persist it.
fn anchored_write_with(
    write: ObservationWrite,
    payload_access: PayloadAccessState,
    projection_watermark: VectorWatermark,
) -> AnchoredObservationWrite {
    let projection_generation =
        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION).unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "anchor-resolution.v1")
            .unwrap();
    let base = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let anchor = RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: base.target().clone(),
        owner: base.owner().clone(),
        aliases: base.aliases().to_vec(),
        occurred_at: base.occurred_at(),
        ingested_at: base.ingested_at(),
        evidence_class: base.evidence_class(),
        source_generation: base.source_generation().clone(),
        projection_generation: projection_generation.clone(),
        projection_watermark,
        coverage: base.coverage().clone(),
        source_observations: base.source_observations().to_vec(),
        source_anchors: base.source_anchors().to_vec(),
        authorization: base.authorization().clone(),
        payload_access,
        retention_class: base.retention_class().clone(),
        durability: base.durability().clone(),
    })
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

async fn persist(
    store: &impl ObservationStore,
    anchored: AnchoredObservationWrite,
) -> ObservationCommitReceipt {
    match store.persist_observation(anchored).await.unwrap() {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("observation persistence must commit, got {other:?}"),
    }
}

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

fn profile_facade(runtime: &HostAdmissionTestRuntimeV1) -> HostAdmissionFacade<'_> {
    runtime.facade()
}

async fn resolve(
    facade: &HostAdmissionFacade<'_>,
    anchor_id: &RetrievalAnchorId,
) -> EvidenceAnchorResolutionReport {
    facade
        .resolve_evidence_anchor_report(FactOwnerV1::Profile, anchor_id.clone())
        .await
        .unwrap()
}

fn assert_payload_free(report: &EvidenceAnchorResolutionReport) {
    let wire = serde_json::to_value(report.resolution()).unwrap();
    let object = wire.as_object().unwrap();
    for key in ["payload", "query", "path", "source_locator"] {
        assert!(!object.contains_key(key), "resolution wire leaks {key}");
    }
}

#[tokio::test]
async fn current_resolution_reports_state_coverage_and_exact_record() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(0, 100, "receipt.anchor-current", "stable sanitized payload");
    let receipt = persist(&store, anchored_write(write(candidate, None))).await;
    let facade = profile_facade(&runtime);

    let report = resolve(&facade, receipt.retrieval_anchor().anchor_id()).await;

    assert_eq!(report.state(), AnchorResolutionStateV2::Current);
    assert_eq!(report.record(), Some(receipt.retrieval_anchor()));
    assert_eq!(
        report.resolution().anchor_id(),
        receipt.retrieval_anchor().anchor_id()
    );
    assert_eq!(
        report.resolution().coverage(),
        receipt.retrieval_anchor().coverage()
    );
    assert_eq!(
        report.resolution().watermark().drift,
        WatermarkDriftV1::Exact
    );
    assert_eq!(
        report.resolution().payload_access(),
        PayloadAccessState::Eligible
    );
    assert_eq!(
        report.resolution().authorization(),
        receipt.retrieval_anchor().authorization()
    );
    assert_payload_free(&report);
}

#[tokio::test]
async fn search_result_resolves_exact_source_after_index_change_with_drift_and_coverage() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    // The search-time anchor freezes the projection watermark at the source
    // observation's own sequence.
    let first = observation(0, 100, "receipt.anchor-drift", "first sanitized payload");
    let first_id = first.observation_id().clone();
    let first_receipt = persist(
        &store,
        anchored_write_with(
            write(first, None),
            PayloadAccessState::Eligible,
            watermark(&[(PROJECTION_SHARD, 1)]),
        ),
    )
    .await;
    let anchor_id = first_receipt.retrieval_anchor().anchor_id().clone();
    let facade = profile_facade(&runtime);

    // Once the projection folds in the source observation, the anchor is current.
    store.project_observation(&first_id).await.unwrap();
    let report = resolve(&facade, &anchor_id).await;
    assert_eq!(report.state(), AnchorResolutionStateV2::Current);

    // Ranking advances: a newer observation is retained and projected, moving
    // the index past the anchor's frozen watermark.
    let second = observation(
        100,
        200,
        "receipt.anchor-drift-next",
        "second sanitized payload",
    );
    let second_id = second.observation_id().clone();
    let second_receipt = persist(
        &store,
        anchored_write(write(
            second,
            Some(first_receipt.committed_cursor().clone()),
        )),
    )
    .await;
    store.project_observation(&second_id).await.unwrap();

    // The same search result still resolves to its exact source observation;
    // only freshness changes, and coverage rides along.
    let drifted = resolve(&facade, &anchor_id).await;
    assert_eq!(
        drifted.state(),
        AnchorResolutionStateV2::Drifted {
            drift: WatermarkDriftV1::ObservedAhead
        }
    );
    assert_eq!(drifted.record(), Some(first_receipt.retrieval_anchor()));
    assert_eq!(
        drifted.record().unwrap().target(),
        &RetrievalAnchorTargetV2::ExactObservation(first_id.clone())
    );
    assert_eq!(
        drifted.resolution().watermark().frozen,
        watermark(&[(PROJECTION_SHARD, 1)])
    );
    assert_eq!(
        drifted.resolution().watermark().observed,
        watermark(&[(PROJECTION_SHARD, 2)])
    );
    assert_eq!(
        drifted.resolution().coverage(),
        first_receipt.retrieval_anchor().coverage()
    );
    assert_eq!(
        drifted.resolution().authorization(),
        first_receipt.retrieval_anchor().authorization()
    );
    assert_payload_free(&drifted);

    // An index rebuild (ranking/index version change) preserves the anchor id,
    // the exact source observation, and the reported drift and coverage.
    let mut rebuilt = false;
    for _ in 0..32 {
        if store
            .rebuild_projection(second_receipt.sequence())
            .await
            .unwrap()
            .is_complete()
        {
            rebuilt = true;
            break;
        }
    }
    assert!(rebuilt, "projection rebuild must complete");
    let after_rebuild = resolve(&facade, &anchor_id).await;
    assert_eq!(after_rebuild.state(), drifted.state());
    assert_eq!(after_rebuild.record(), drifted.record());
    assert_eq!(
        after_rebuild.resolution().watermark(),
        drifted.resolution().watermark()
    );
    assert_eq!(
        after_rebuild.resolution().coverage(),
        drifted.resolution().coverage()
    );
}

#[tokio::test]
async fn redacted_expired_and_deleted_targets_report_typed_states_without_payload() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let facade = profile_facade(&runtime);
    let cases = [
        (
            PayloadAccessState::Redacted,
            AnchorResolutionStateV2::Redacted,
        ),
        (
            PayloadAccessState::Quarantined,
            AnchorResolutionStateV2::Redacted,
        ),
        (
            PayloadAccessState::RetentionExpired,
            AnchorResolutionStateV2::Expired,
        ),
        (
            PayloadAccessState::Deleted,
            AnchorResolutionStateV2::Deleted,
        ),
    ];
    let mut offset = 0;
    let mut expected_cursor = None;
    for (access, expected) in cases {
        let candidate = observation(
            offset,
            offset + 100,
            &format!("receipt.anchor-tombstone-{offset}"),
            "tombstoned sanitized payload",
        );
        offset += 100;
        let receipt = persist(
            &store,
            anchored_write_with(
                write(candidate, expected_cursor),
                access,
                VectorWatermark::default(),
            ),
        )
        .await;
        expected_cursor = Some(receipt.committed_cursor().clone());

        let report = resolve(&facade, receipt.retrieval_anchor().anchor_id()).await;

        assert_eq!(report.state(), expected, "{access:?}");
        assert_eq!(report.resolution().payload_access(), access, "{access:?}");
        assert_eq!(
            report.record(),
            Some(receipt.retrieval_anchor()),
            "{access:?}"
        );
        assert_eq!(
            report.resolution().coverage(),
            receipt.retrieval_anchor().coverage(),
            "{access:?}"
        );
        assert_payload_free(&report);
    }
}

#[tokio::test]
async fn unavailable_resolution_reports_typed_state_and_never_leaks_existence() {
    // Owner X retains an anchor that genuinely exists.
    let owner_x = TempDir::new().unwrap();
    let runtime_x = profile_runtime(&owner_x).await;
    let store_x = runtime_x
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(
        0,
        100,
        "receipt.anchor-unavailable",
        "owner-x sanitized payload",
    );
    let receipt = persist(&store_x, anchored_write(write(candidate, None))).await;
    let existing_anchor_id = receipt.retrieval_anchor().anchor_id().clone();

    // A valid owner resolving a never-created anchor gets the typed state.
    let facade_x = profile_facade(&runtime_x);
    let never_existed_anchor_id = RetrievalAnchorId::new("retrieval.never-existed").unwrap();
    let absent = resolve(&facade_x, &never_existed_anchor_id).await;
    assert_eq!(absent.state(), AnchorResolutionStateV2::Unavailable);
    assert_eq!(
        absent.resolution().payload_access(),
        PayloadAccessState::Unavailable
    );
    assert!(absent.record().is_none());
    assert_eq!(
        absent.resolution().watermark().drift,
        WatermarkDriftV1::Exact
    );
    absent.resolution().authorization().validate().unwrap();
    assert_payload_free(&absent);

    // An isolated unauthorized authority cannot distinguish an anchor that
    // exists under owner X from one that never existed at all.
    let owner_y = TempDir::new().unwrap();
    let runtime_y = profile_runtime(&owner_y).await;
    let facade_y = profile_facade(&runtime_y);
    let existing_elsewhere = resolve(&facade_y, &existing_anchor_id).await;
    let never_existed = resolve(&facade_y, &never_existed_anchor_id).await;
    assert_eq!(existing_elsewhere.state(), absent.state());
    assert_eq!(never_existed.state(), absent.state());
    assert_eq!(existing_elsewhere.record(), never_existed.record());
    assert_eq!(
        existing_elsewhere.resolution().coverage(),
        never_existed.resolution().coverage()
    );
    assert_eq!(
        existing_elsewhere.resolution().watermark(),
        never_existed.resolution().watermark()
    );
    assert_eq!(
        existing_elsewhere.resolution().payload_access(),
        never_existed.resolution().payload_access()
    );
    assert_payload_free(&existing_elsewhere);
}

#[tokio::test]
async fn ambiguous_resolution_reports_typed_state_from_record_and_store_conflict() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let facade = profile_facade(&runtime);

    // A retained record that itself declares ambiguous payload access.
    let declared = observation(
        0,
        100,
        "receipt.anchor-ambiguous",
        "ambiguous sanitized payload",
    );
    let declared_receipt = persist(
        &store,
        anchored_write_with(
            write(declared, None),
            PayloadAccessState::Ambiguous,
            VectorWatermark::default(),
        ),
    )
    .await;
    let declared_report = resolve(&facade, declared_receipt.retrieval_anchor().anchor_id()).await;
    assert_eq!(declared_report.state(), AnchorResolutionStateV2::Ambiguous);
    assert_eq!(
        declared_report.resolution().payload_access(),
        PayloadAccessState::Ambiguous
    );
    assert_eq!(
        declared_report.record(),
        Some(declared_receipt.retrieval_anchor())
    );
    assert_payload_free(&declared_report);

    // A store-level binding conflict: one anchor id bound to two different
    // observations. No single record may be presented.
    let first = observation(
        100,
        200,
        "receipt.anchor-conflict-a",
        "first conflicting payload",
    );
    let first_receipt = persist(
        &store,
        anchored_write(write(
            first,
            Some(declared_receipt.committed_cursor().clone()),
        )),
    )
    .await;
    let conflict_anchor_id = first_receipt.retrieval_anchor().anchor_id().clone();
    let raw_conn =
        rusqlite::Connection::open(runtime.database_path(HostAdmissionScope::Profile).unwrap())
            .unwrap();
    // Simulate out-of-band store corruption: a second binding for the same
    // anchor id that the authoritative writer would never commit. The foreign
    // key exemption stays local to this raw connection.
    raw_conn
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO observation_repository_provenance (
                 observation_id, availability_json, capture_json, retrieval_anchor_id, owner_json
             )
             SELECT 'observation.anchor-conflict-ghost', '{}', '{}', anchor_id, owner_json
             FROM retrieval_anchors WHERE anchor_id = ?1",
            rusqlite::params![conflict_anchor_id.as_str()],
        )
        .expect("conflicting anchor binding must insert");
    drop(raw_conn);

    let conflict_report = resolve(&facade, &conflict_anchor_id).await;
    assert_eq!(conflict_report.state(), AnchorResolutionStateV2::Ambiguous);
    assert_eq!(
        conflict_report.resolution().payload_access(),
        PayloadAccessState::Ambiguous
    );
    assert!(conflict_report.record().is_none());
    assert_payload_free(&conflict_report);
}

/// Fact-store stub: report resolution never touches the fact authority.
struct UnavailableFactStore;

impl FactStore for UnavailableFactStore {
    async fn commit_fact(&self, _batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
        unreachable!("report resolution never commits facts")
    }

    async fn query_current_facts(
        &self,
        _query: CurrentFactsQuery,
    ) -> FactStoreResult<Vec<StoredFactV1>> {
        unreachable!("report resolution never queries facts")
    }

    async fn query_fact_current(
        &self,
        _query: FactCurrentQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        unreachable!("report resolution never queries facts")
    }

    async fn query_fact_current_response(
        &self,
        _query: FactCurrentQuery,
    ) -> FactStoreResult<FactCurrentResponseV1> {
        unreachable!("report resolution never queries facts")
    }

    async fn query_fact_as_of(
        &self,
        _query: FactAsOfQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        unreachable!("report resolution never queries facts")
    }

    async fn query_fact_as_of_response(
        &self,
        _query: FactAsOfQuery,
    ) -> FactStoreResult<FactAsOfResponseV1> {
        unreachable!("report resolution never queries facts")
    }

    async fn query_fact_lineage(
        &self,
        _query: FactLineageQuery,
    ) -> FactStoreResult<Vec<FactLineageEventV1>> {
        unreachable!("report resolution never queries fact lineage")
    }

    async fn query_fact_lineage_response(
        &self,
        _query: FactLineageQuery,
    ) -> FactStoreResult<FactLineageResponseV1> {
        unreachable!("report resolution never queries fact lineage")
    }

    async fn resolve_legacy_fact(
        &self,
        _query: LegacyFactQuery,
    ) -> FactStoreResult<Option<FactId>> {
        unreachable!("report resolution never resolves legacy facts")
    }

    async fn get_retrieval_anchor(
        &self,
        _query: RetrievalAnchorQuery,
    ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
        unreachable!("report resolution never reads fact-shard anchors")
    }
}

#[tokio::test]
async fn memory_application_report_resolution_rechecks_owner_and_identity() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(
        0,
        100,
        "receipt.anchor-application",
        "application sanitized payload",
    );
    let receipt = persist(&store, anchored_write(write(candidate, None))).await;
    let facade = profile_facade(&runtime);
    let application = MemoryApplication::new(FactOwnerV1::Profile, UnavailableFactStore).unwrap();

    let report = application
        .resolve_evidence_anchor_report(&facade, receipt.retrieval_anchor().anchor_id().clone())
        .await
        .unwrap();
    assert_eq!(report.state(), AnchorResolutionStateV2::Current);
    assert_eq!(report.record(), Some(receipt.retrieval_anchor()));

    let unavailable = application
        .resolve_evidence_anchor_report(
            &facade,
            RetrievalAnchorId::new("retrieval.application-absent").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unavailable.state(), AnchorResolutionStateV2::Unavailable);
    assert!(unavailable.record().is_none());
}

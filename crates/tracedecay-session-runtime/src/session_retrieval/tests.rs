use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, RetrievalAnchorId, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, TemporalModeV1,
    UtcMicros, derive_exact_observation_anchor_id,
};
use tracedecay_lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    SessionRecord, SessionTemporalSnapshotRequestV1, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};
use tracedecay_temporal_query::context::CompactContext;
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, TemporalAuthorizedRoot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use tracedecay_temporal_query::ranking::{RankedCandidate, RetrieverContribution};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;
use tracedecay_temporal_query::{TemporalHydratedResult, TemporalKernelResult};

#[derive(Clone)]
struct RealPageFixture {
    provider: String,
    session_id: String,
    message_id: String,
    projected_message_id: String,
    text: String,
    anchor_id: RetrievalAnchorId,
    active_generation: u64,
}

fn test_binding_digest(label: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(label.as_bytes())))
}

#[test]
fn real_page_fixture_rejects_legacy_binding_digests_and_accepts_test_digests() {
    for (field, invalid) in [
        ("root_digest", "root.page"),
        ("request_digest", "request.page.session.page.00"),
        ("access_digest", "access.page.session.page.00"),
        ("configuration", "page-test"),
    ] {
        assert!(BindingDigest::new(field, invalid).is_err());
        assert!(BindingDigest::new(field, test_binding_digest(invalid)).is_ok());
    }
}

fn real_page_root(root_id: &str) -> TemporalAuthorizedRoot {
    TemporalAuthorizedRoot::profile("profile.page", "store.page", root_id)
        .expect("registered profile root")
}

fn real_page_snapshot(
    session_id: &str,
    active_generation: u64,
    root: TemporalAuthorizedRoot,
    cancelled: bool,
) -> TemporalExecutionSnapshot {
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            SessionId::new(session_id).expect("session"),
            test_binding_digest("root.page"),
            test_binding_digest(&format!("request.page.{session_id}")),
            test_binding_digest(&format!("access.page.{session_id}")),
            TemporalModeV1::Current,
            tracedecay_domain::RetrievalGrainV1::LogicalMessage,
        )
        .expect("snapshot request")
        .with_authorized_root(root)
        .expect("root binding")
        .with_cancellation_requested(cancelled),
        TemporalWatermarks {
            generation: active_generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new(
                "configuration",
                test_binding_digest("page-test"),
            )
            .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn real_page_candidate(fixture: &RealPageFixture) -> RankedCandidate {
    RankedCandidate {
        stable_id: fixture.message_id.clone(),
        anchor_id: fixture.anchor_id.clone(),
        normalized_score_micros: 1_000_000,
        knowledge_at_micros: 1,
        logical_message: None,
        turn: None,
        session: Some(fixture.session_id.clone()),
        source: Some(fixture.provider.clone()),
        evidence_role: Some("assistant".to_owned()),
        contributions: Vec::new(),
    }
}

fn real_page_kernel(
    snapshot: TemporalExecutionSnapshot,
    ranked: Vec<RankedCandidate>,
    hydrated: Vec<TemporalHydratedResult>,
) -> TemporalKernelResult {
    TemporalKernelResult {
        snapshot,
        ranked,
        hydrated,
        context: CompactContext {
            rendered: String::new(),
            bundle: tracedecay_domain::CompactContextBundleV1 {
                records: Vec::new(),
                omissions: Vec::new(),
                continuation_anchors: Vec::new(),
                coverage: TemporalCoverageCountsV1::default(),
                conflicts: Vec::new(),
                lineage: Vec::new(),
                encoded_bytes: 0,
            },
            accounted_bytes: 0,
            estimated_tokens: 0,
            estimator_version: "page-fixture".to_owned(),
        },
        coverage: TemporalCoverageCountsV1::default(),
        conflicts: Vec::new(),
        lineage: Vec::new(),
        summary_omissions: Vec::new(),
        next_cursor: None,
    }
}

async fn seed_real_page_fixture(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    root: &TemporalAuthorizedRoot,
    rank: usize,
) -> RealPageFixture {
    let provider = if rank.is_multiple_of(2) {
        "codex"
    } else {
        "claude"
    }
    .to_owned();
    let session_id = format!("session.page.{rank:02}");
    let message_id = format!("message.page.{rank:02}");
    let text = format!("canonical content {rank}");
    assert!(
        database
            .upsert_session(&SessionRecord {
                provider: provider.clone(),
                session_id: session_id.clone(),
                project_key: root.project_key().to_owned(),
                project_path: format!("/fixture/{rank:02}"),
                title: Some(format!("fixture session {rank}")),
                started_at: Some(i64::try_from(rank).expect("timestamp")),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await,
        "seed canonical owning session"
    );

    let provider_id = ProviderId::new(provider.clone()).expect("provider");
    let session = SessionId::new(session_id.clone()).expect("session");
    let source = ObservationSourceIdentityV1::for_provider(provider_id.clone(), session.clone())
        .expect("observation source");
    let ordinal = u64::try_from(rank).expect("ordinal");
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).expect("source range");
    let record_id = ObservationId::new(format!("record.page.{rank:02}")).expect("record id");
    let projected_message_id = if provider == "claude" {
        record_id.as_str().to_owned()
    } else {
        message_id.clone()
    };
    let relations = CanonicalObservationRelationsV1::new(session)
        .with_message_id(ObservationId::new(message_id.clone()).expect("message id"));
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider_id,
        "message",
        record_id.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!(text),
            model: Some("fixture-model".to_owned()),
            timestamp: Some(i64::try_from(rank).expect("timestamp")),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .expect("canonical message envelope");
    let payload = serde_json::to_value(envelope).expect("canonical payload");
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).expect("generation"),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .expect("observation identity"),
        SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(format!("receipt.page.{rank:02}")).expect("receipt id"),
                tracedecay_domain::ComponentVersion::new("sanitizer.page-fixture.v1")
                    .expect("sanitizer"),
            )
            .expect("receipt reference"),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).expect("payload reference")),
        )
        .expect("receipt"),
        RetentionClass::new("retention.page-fixture").expect("retention"),
        payload,
    )
    .expect("durable observation");
    let store = database.observation_store();
    let previous_cursor = store
        .get_source_cursor(observation.source(), observation.scope())
        .await
        .expect("source cursor");
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .expect("next cursor");
    let write = ObservationWrite::new(observation.clone(), previous_cursor, next_cursor)
        .expect("observation write");
    let projection_generation =
        ProjectionGenerationId::new("projection.page-fixture.v1").expect("projection generation");
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "session-page-fixture")
            .expect("resolution authorization");
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .expect("retrieval anchor");
    store
        .persist_observation(
            AnchoredObservationWrite::new(write, anchor, projection_generation)
                .expect("anchored observation"),
        )
        .await
        .expect("persist canonical observation");
    store
        .project_observation(observation.observation_id())
        .await
        .expect("project canonical observation");
    database
        .lcm_protect_session_raw_messages(&provider, &session_id)
        .await
        .expect("protect canonical raw message");
    tracedecay_session_temporal_store::GlobalDbSessionTemporalStore::new(database)
        .materialize_pending_session_refresh_for_test(
            &SessionId::new(session_id.clone()).expect("refresh session"),
        )
        .await
        .expect("materialize canonical temporal occurrence");
    let active_generation = database
        .freeze_session_temporal_snapshot_result(SessionTemporalSnapshotRequestV1::new(
            SessionId::new(session_id.clone()).expect("frozen session"),
        ))
        .await
        .expect("freeze materialized temporal snapshot")
        .watermarks()
        .active_generation()
        .value();

    RealPageFixture {
        provider,
        session_id,
        message_id,
        projected_message_id,
        text,
        anchor_id: derive_exact_observation_anchor_id(
            observation.scope(),
            observation.observation_id(),
        )
        .expect("canonical anchor id"),
        active_generation,
    }
}

#[tokio::test]
async fn fifty_real_page_results_use_one_registered_frozen_snapshot() {
    const RESULTS: usize = 50;
    let harness =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("page-batch").await;
    let root = real_page_root("root.page");
    let mut fixtures = Vec::with_capacity(RESULTS);
    for rank in 0..RESULTS {
        fixtures.push(seed_real_page_fixture(harness.registered.as_ref(), &root, rank).await);
    }
    let service = DaemonSessionRetrievalService::new(
        harness.registered.clone(),
        registered_profile_retrieval_root(&harness.registered),
        None,
    )
    .expect("registered retrieval service");
    let before = harness
        .registered
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;

    let mut items = fixtures
        .iter()
        .map(|fixture| {
            real_page_kernel(
                real_page_snapshot(
                    &fixture.session_id,
                    fixture.active_generation,
                    root.clone(),
                    false,
                ),
                vec![real_page_candidate(fixture)],
                vec![TemporalHydratedResult::available_for_test(
                    0,
                    &fixture.message_id,
                    fixture.anchor_id.clone(),
                    fixture.text.as_bytes(),
                )],
            )
        })
        .collect::<Vec<_>>();
    let summary_fixture = &fixtures[25];
    let mut summary = real_page_candidate(summary_fixture);
    summary.stable_id = "summary.page.25".to_owned();
    summary.evidence_role = Some("summary".to_owned());
    summary.contributions = vec![RetrieverContribution {
        channel: tracedecay_temporal_query::candidates::CandidateChannel::Summary,
        source: Some(summary_fixture.provider.clone()),
        retriever_record_id: "summary.page.25".to_owned(),
        retriever_ordinal: 0,
        raw_score: 1,
        calibrated_score_micros: 1,
        exact_ranges: Vec::new(),
    }];
    items.insert(
        25,
        real_page_kernel(
            real_page_snapshot(
                &summary_fixture.session_id,
                summary_fixture.active_generation,
                root.clone(),
                false,
            ),
            vec![summary],
            vec![TemporalHydratedResult::available_for_test(
                0,
                "summary.page.25",
                summary_fixture.anchor_id.clone(),
                b"canonical summary content".to_vec(),
            )],
        ),
    );
    for (stable_id, reason) in [
        ("denied.page", HydrationStateV1::Unauthorized),
        ("redacted.page", HydrationStateV1::Redacted),
        ("unavailable.page", HydrationStateV1::RetainedButUnavailable),
    ] {
        let fixture = &fixtures[0];
        let mut candidate = real_page_candidate(fixture);
        candidate.stable_id = stable_id.to_owned();
        items.push(real_page_kernel(
            real_page_snapshot(
                &fixture.session_id,
                fixture.active_generation,
                root.clone(),
                false,
            ),
            vec![candidate],
            vec![TemporalHydratedResult::unavailable_for_test(
                0,
                stable_id,
                fixture.anchor_id.clone(),
                reason,
            )],
        ));
    }

    let (page, skipped, rendering_omitted) = service.page(items).await.expect("real page");
    let after = harness
        .registered
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;
    assert_eq!(
        after,
        before.saturating_add(1),
        "fifty owning sessions and a summary must share one frozen registered snapshot"
    );
    assert_eq!((skipped, rendering_omitted), (3, 0));
    assert_eq!(page.results.len(), RESULTS + 1);
    assert_eq!(page.results[25].message.message_id, "summary.page.25");
    assert_eq!(page.results[25].message.text, "canonical summary content");
    assert_eq!(page.results[25].message.role, "summary");
    assert_eq!(page.results[25].session.provider, summary_fixture.provider);
    assert_eq!(
        page.results[25].session.session_id,
        summary_fixture.session_id
    );
    assert_eq!(
        page.results
            .iter()
            .filter(|result| result.message.role != "summary")
            .map(|result| result.message.message_id.as_str())
            .collect::<Vec<_>>(),
        fixtures
            .iter()
            .map(|fixture| fixture.projected_message_id.as_str())
            .collect::<Vec<_>>(),
        "available messages retain their rank order without promotion"
    );
    for (result, fixture) in page
        .results
        .iter()
        .filter(|result| result.message.role != "summary")
        .zip(&fixtures)
    {
        assert_eq!(result.session.provider, fixture.provider);
        assert_eq!(result.session.session_id, fixture.session_id);
        assert_eq!(result.message.provider, fixture.provider);
        assert_eq!(result.message.session_id, fixture.session_id);
        assert_eq!(result.message.text, fixture.text);
    }
    assert_eq!(
        page.temporal
            .omissions
            .iter()
            .map(|omission| omission.reason)
            .collect::<Vec<_>>(),
        vec![
            HydrationStateV1::Unauthorized,
            HydrationStateV1::Redacted,
            HydrationStateV1::RetainedButUnavailable,
        ],
        "typed denial, redaction, and unavailability stay omissions"
    );
}

#[tokio::test]
async fn real_page_rejects_mixed_roots_and_honors_cancellation_checkpoints() {
    let harness =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("page-scope").await;
    let root = real_page_root("root.page");
    let fixture = seed_real_page_fixture(harness.registered.as_ref(), &root, 0).await;
    let service = DaemonSessionRetrievalService::new(
        harness.registered.clone(),
        registered_profile_retrieval_root(&harness.registered),
        None,
    )
    .expect("registered retrieval service");
    let hydrated = || {
        TemporalHydratedResult::available_for_test(
            0,
            &fixture.message_id,
            fixture.anchor_id.clone(),
            fixture.text.as_bytes(),
        )
    };
    let before = harness
        .registered
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;

    let mixed = service
        .page(vec![
            real_page_kernel(
                real_page_snapshot(
                    &fixture.session_id,
                    fixture.active_generation,
                    root.clone(),
                    false,
                ),
                vec![real_page_candidate(&fixture)],
                vec![hydrated()],
            ),
            real_page_kernel(
                real_page_snapshot(
                    &fixture.session_id,
                    fixture.active_generation,
                    TemporalAuthorizedRoot::profile(
                        "profile.page",
                        "store.foreign",
                        "root.foreign",
                    )
                    .expect("foreign root"),
                    false,
                ),
                vec![real_page_candidate(&fixture)],
                vec![hydrated()],
            ),
        ])
        .await;
    assert!(matches!(
        mixed,
        Err(SessionTemporalExecutionError::WrongScope)
    ));
    assert_eq!(
        harness
            .registered
            .read_connection()
            .reader_pool_occupancy()
            .expect("registered reader pool")
            .snapshot_admissions,
        before,
        "mixed roots are rejected before registered snapshot admission"
    );

    let pre_admission_cancelled = service
        .page(vec![real_page_kernel(
            real_page_snapshot(
                &fixture.session_id,
                fixture.active_generation,
                root.clone(),
                true,
            ),
            vec![real_page_candidate(&fixture)],
            vec![hydrated()],
        )])
        .await;
    assert!(matches!(
        pre_admission_cancelled,
        Err(SessionTemporalExecutionError::Cancelled)
    ));
    assert_eq!(
        harness
            .registered
            .read_connection()
            .reader_pool_occupancy()
            .expect("registered reader pool")
            .snapshot_admissions,
        before,
        "cancellation before page admission does not open a snapshot"
    );

    let cancelled_between_items = service
        .page(vec![
            real_page_kernel(
                real_page_snapshot(
                    &fixture.session_id,
                    fixture.active_generation,
                    root.clone(),
                    false,
                ),
                vec![real_page_candidate(&fixture)],
                vec![hydrated()],
            ),
            real_page_kernel(
                real_page_snapshot(&fixture.session_id, fixture.active_generation, root, true),
                vec![real_page_candidate(&fixture)],
                vec![hydrated()],
            ),
        ])
        .await;
    assert!(matches!(
        cancelled_between_items,
        Err(SessionTemporalExecutionError::Cancelled)
    ));
    assert_eq!(
        harness
            .registered
            .read_connection()
            .reader_pool_occupancy()
            .expect("registered reader pool")
            .snapshot_admissions,
        before.saturating_add(1),
        "cancellation between page items stops the one admitted frozen snapshot"
    );
}

#[test]
fn stored_retrieval_does_not_require_refresh_worker() {
    assert!(!requires_refresh_worker(
        SessionFreshnessPolicy::AllowStored
    ));
    assert!(requires_refresh_worker(
        SessionFreshnessPolicy::RequireFresh
    ));
}

fn typed<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("typed test identity")
}

#[test]
fn profile_serving_identity_rejects_mismatched_profile_and_runtime_shard() {
    assert!(
        profile_retrieval_root(
            "profile.durable-session-retrieval",
            "profile.foreign",
            "store.profile.durable-session-retrieval",
            "root.profile.durable-session-retrieval",
        )
        .is_none()
    );
}

#[test]
fn profile_serving_identity_rejects_mismatched_store() {
    assert!(
        profile_retrieval_root(
            "profile.durable-session-retrieval",
            "profile.durable-session-retrieval",
            "store.profile.foreign",
            "root.profile.durable-session-retrieval",
        )
        .is_none()
    );
}

#[test]
fn profile_serving_identity_rejects_mismatched_root() {
    assert!(
        profile_retrieval_root(
            "profile.durable-session-retrieval",
            "profile.durable-session-retrieval",
            "store.profile.durable-session-retrieval",
            "root.profile.foreign",
        )
        .is_none()
    );
}

#[test]
fn profile_serving_identity_accepts_exact_profile_store_root_and_shard() {
    let brain_id = typed::<tracedecay_domain::BrainId>("brain.session-retrieval");
    let profile_id = typed::<tracedecay_domain::UserProfileId>("profile.durable-session-retrieval");
    let root = profile_retrieval_root(
        profile_id.as_str(),
        profile_id.as_str(),
        "store.profile.durable-session-retrieval",
        "root.profile.durable-session-retrieval",
    )
    .expect("exact profile retrieval identity");

    assert_eq!(root.identity.profile_id().as_str(), profile_id.as_str());
    assert_eq!(
        root.identity.store_id().as_str(),
        "store.profile.durable-session-retrieval"
    );
    assert_eq!(
        root.identity.root_id().as_str(),
        "root.profile.durable-session-retrieval"
    );
    assert_eq!(
        root.expected_runtime_shard,
        Some(StoreShardIdV1::profile_sessions(brain_id, profile_id))
    );
}

#[tokio::test]
async fn service_rejects_foreign_shard_before_read_admission() {
    let harness =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("foreign-shard")
            .await;
    let before = harness
        .registered
        .read_connection()
        .reader_pool_occupancy()
        .expect("registered reader pool")
        .snapshot_admissions;
    let root = profile_retrieval_root(
        "profile.foreign",
        "profile.foreign",
        "store.profile.foreign",
        "root.profile.foreign",
    )
    .expect("foreign profile retrieval root");

    assert!(DaemonSessionRetrievalService::new(harness.registered.clone(), root, None).is_none());
    assert_eq!(
        harness
            .registered
            .read_connection()
            .reader_pool_occupancy()
            .expect("registered reader pool")
            .snapshot_admissions,
        before,
        "identity mismatch must fail before retrieval admits a read snapshot"
    );
}

#[tokio::test]
async fn service_accepts_exact_registered_identity() {
    let harness =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("exact-shard").await;
    let root = registered_profile_retrieval_root(&harness.registered);

    assert!(DaemonSessionRetrievalService::new(harness.registered.clone(), root, None).is_some());
}

fn profile_retrieval_root(
    profile_id: &str,
    shard_profile_id: &str,
    store_id: &str,
    root_id: &str,
) -> Option<DaemonSessionRetrievalRoot> {
    let profile_id = typed::<tracedecay_domain::UserProfileId>(profile_id);
    let shard_profile_id = typed::<tracedecay_domain::UserProfileId>(shard_profile_id);
    let store_id = SessionStoreId::new(store_id).expect("store identity");
    let root_id = SessionRootId::new(root_id).expect("root identity");
    let runtime_shard = StoreShardIdV1::profile_sessions(
        typed::<tracedecay_domain::BrainId>("brain.session-retrieval"),
        shard_profile_id,
    );
    DaemonSessionRetrievalRoot::profile(SessionRetrievalServingIdentityV1 {
        project_id: None,
        profile_id: ProfileId::new(profile_id.as_str().to_owned()).expect("profile identity"),
        store_id,
        root_id,
        expected_runtime_shard: runtime_shard,
        serving_db: Path::new("/profile/session.db").to_path_buf(),
        project_root: Path::new("/profile").to_path_buf(),
    })
}

fn registered_profile_retrieval_root(
    database: &RegisteredGlobalDbLeaseV1,
) -> DaemonSessionRetrievalRoot {
    let profile_root = database.db_path().parent().expect("profile root");
    let profile_id = &database.binding().shard_id.profile_id;
    let suffix = profile_id
        .as_str()
        .strip_prefix("profile.")
        .expect("profile identity prefix");
    let serving = SessionRetrievalServingIdentityV1::profile(
        ProfileId::new(profile_id.as_str().to_owned()).expect("profile identity"),
        SessionStoreId::new(format!("store.profile.{suffix}")).expect("profile store identity"),
        SessionRootId::new(format!("root.profile.{suffix}")).expect("profile root identity"),
        &database.binding().shard_id,
        database.db_path(),
        profile_root,
    )
    .expect("registered profile serving identity");
    DaemonSessionRetrievalRoot::profile(serving).expect("registered profile retrieval identity")
}

#[test]
fn denied_shared_anchor_stays_at_its_rank_without_promoting_lower_candidate() {
    fn ranked(stable_id: &str, anchor: &RetrievalAnchorId) -> RankedCandidate {
        RankedCandidate {
            stable_id: stable_id.to_string(),
            anchor_id: anchor.clone(),
            normalized_score_micros: 1,
            knowledge_at_micros: 1,
            logical_message: None,
            turn: None,
            session: Some(format!("session.{stable_id}")),
            source: Some("cursor".to_string()),
            evidence_role: Some("assistant".to_string()),
            contributions: Vec::new(),
        }
    }

    let anchor = RetrievalAnchorId::new("anchor.shared").unwrap();
    let selected = [ranked("denied", &anchor), ranked("lower", &anchor)];
    let hydrated = [
        TemporalHydratedResult::unavailable_for_test(
            0,
            "denied",
            anchor.clone(),
            HydrationStateV1::Unauthorized,
        ),
        TemporalHydratedResult::available_for_test(
            1,
            "lower",
            anchor.clone(),
            b"lower candidate".to_vec(),
        ),
    ];

    let omission = page_hydration_slot(0, &selected[0], &hydrated).unwrap_err();
    assert_eq!(omission.rank, 0);
    assert_eq!(omission.anchor, anchor);
    assert_eq!(omission.reason, HydrationStateV1::Unauthorized);

    let lower = page_hydration_slot(1, &selected[1], &hydrated).unwrap();
    assert_eq!(lower.rank(), 1);
    assert_eq!(lower.stable_id(), "lower");
}

#[test]
fn complete_page_with_typed_omission_becomes_partial_and_keeps_coverage() {
    let anchor = RetrievalAnchorId::new("anchor.omitted").unwrap();
    let page = SessionRetrievalPageView {
        results: Vec::new(),
        temporal: SessionTemporalMetadataView {
            coverage: TemporalCoverageCountsV1 {
                visible: 0,
                hidden: 0,
                unknown: 1,
                redacted: 0,
            },
            omissions: vec![SessionRetrievalOmissionView {
                rank: 0,
                anchor: anchor.clone(),
                reason: HydrationStateV1::Unauthorized,
            }],
            ..SessionTemporalMetadataView::default()
        },
    };

    let SessionRetrievalServiceOutcome::Partial {
        page,
        freshness,
        omitted,
    } = complete_page_outcome(page, SessionDataFreshness::Fresh, 1)
    else {
        panic!("complete page with an omission must become partial");
    };
    assert_eq!(freshness, SessionDataFreshness::Fresh);
    assert_eq!(omitted, 1);
    assert_eq!(page.temporal.coverage.unknown, 1);
    assert_eq!(page.temporal.omissions[0].rank, 0);
    assert_eq!(page.temporal.omissions[0].anchor, anchor);
    assert_eq!(
        page.temporal.omissions[0].reason,
        HydrationStateV1::Unauthorized
    );
}

#[test]
fn stale_lcm_retrieval_remains_typed_instead_of_generic_unavailable() {
    let freshness = SessionDataFreshness::Stored { generation_lag: 7 };

    let describe = describe_retrieval_outcome(
        SessionRetrievalOutcome::Stale { freshness },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::Stale { freshness },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );

    assert!(matches!(
        describe,
        LcmDescribeServiceOutcome::Stale {
            retrieval: LcmRetrievalOutcome::Stale {
                freshness: LcmDataFreshness::Stored { generation_lag: 7 }
            },
            ..
        }
    ));
    assert!(matches!(
        expand,
        LcmExpandServiceOutcome::Stale {
            retrieval: LcmRetrievalOutcome::Stale {
                freshness: LcmDataFreshness::Stored { generation_lag: 7 }
            },
            ..
        }
    ));
}

#[test]
fn cursor_stale_lcm_retrieval_requires_cursorless_restart() {
    let describe = describe_retrieval_outcome(
        SessionRetrievalOutcome::CursorStale,
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::CursorStale,
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );

    assert_eq!(describe, LcmDescribeServiceOutcome::CursorStale);
    assert_eq!(expand, LcmExpandServiceOutcome::CursorStale);
}

#[tokio::test]
async fn cursor_stale_session_retrieval_remains_typed_at_daemon_boundary() {
    let harness =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("cursor-stale").await;
    let service = DaemonSessionRetrievalService::new(
        harness.registered.clone(),
        registered_profile_retrieval_root(&harness.registered),
        None,
    )
    .expect("registered retrieval service");

    assert_eq!(
        service
            .public_outcome(SessionRetrievalOutcome::CursorStale)
            .await,
        SessionRetrievalServiceOutcome::CursorStale
    );
}

#[test]
fn reset_required_lcm_retrieval_preserves_the_owning_store_scope() {
    let describe = describe_retrieval_outcome(
        SessionRetrievalOutcome::ResetRequired,
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::ResetRequired,
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Profile,
    );

    assert!(matches!(
        describe,
        LcmDescribeServiceOutcome::ResetRequired {
            store_scope: SessionRetrievalStoreScope::Project
        }
    ));
    assert!(matches!(
        expand,
        LcmExpandServiceOutcome::ResetRequired {
            store_scope: SessionRetrievalStoreScope::Profile
        }
    ));
}

#[test]
fn zero_item_partial_lcm_retrieval_remains_partial_instead_of_deleted() {
    let freshness = SessionDataFreshness::Partial { generation_lag: 3 };

    let describe = describe_retrieval_outcome(
        SessionRetrievalOutcome::Partial {
            items: Vec::new(),
            freshness,
            omitted: 5,
        },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );
    let expand = expand_retrieval_outcome(
        SessionRetrievalOutcome::Partial {
            items: Vec::new(),
            freshness,
            omitted: 5,
        },
        RetrievalGrainV1::Summary,
        SessionTemporalMetadataView::default(),
        SessionRetrievalStoreScope::Project,
    );

    assert!(matches!(
        describe,
        LcmDescribeServiceOutcome::Partial {
            description: None,
            retrieval: LcmRetrievalOutcome::Partial {
                freshness: LcmDataFreshness::Partial { generation_lag: 3 },
                omitted: 5,
            },
            ..
        }
    ));
    assert!(matches!(
        expand,
        LcmExpandServiceOutcome::Partial {
            expansion: None,
            retrieval: LcmRetrievalOutcome::Partial {
                freshness: LcmDataFreshness::Partial { generation_lag: 3 },
                omitted: 5,
            },
            ..
        }
    ));
}

#[test]
fn rendering_deadlines_remain_distinct_from_cancellation() {
    for error in [
        TemporalKernelError::DeadlineExceeded,
        TemporalKernelError::Port(TemporalPortError::DeadlineExceeded),
        TemporalKernelError::Hydration(HydrationError::Interrupted(
            TemporalPortError::DeadlineExceeded,
        )),
        TemporalKernelError::Context(ContextError::Interrupted(
            TemporalPortError::DeadlineExceeded,
        )),
    ] {
        assert!(temporal_kernel_deadline(&error));
    }
    assert!(!temporal_kernel_deadline(&TemporalKernelError::Cancelled));
}

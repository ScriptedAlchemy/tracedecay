use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use serde_json::json;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
    DurableObservationV1, HydrationStateV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass, RetrievalGrainV1,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, TemporalModeV1, UtcMicros,
};
use tracedecay_global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_global_db::{GlobalDbObservationStore, RegisteredGlobalDb};
use tracedecay_sessions::runtime::lcm::types::LcmImmutableSummaryPublication;
use tracedecay_sessions::runtime::lcm::{
    LcmContentSlice, LcmExpandRequest, LcmExpandResponse, LcmExpandTarget, LcmSourceRef,
    LcmSummaryNodeDraft,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};
use tracedecay_temporal_query::ports::{
    BindingDigest, ExecutionControl, KernelVersions, TemporalExecutionSnapshot,
    TemporalSnapshotRequest, TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

const PROVIDER: &str = "lcm-benchmark";
const SESSION_ID: &str = "session.lcm-expansion";
const AUTHORITY: &str = "lcm-expansion-benchmark";
const SOURCE_COUNT: usize = 16;
const SOURCE_TEXT_BYTES: usize = 1_024;
const TOP_SUMMARY_ID: &str = "summary.lcm-expansion.root";
const TOP_SUMMARY_TEXT: &str = "deterministic LCM expansion root";

struct Fixture {
    _profile: tempfile::TempDir,
    runtime: RegisteredGlobalDbTestRuntime,
    snapshot: TemporalExecutionSnapshot,
    expected_source_content: Vec<String>,
}

impl Fixture {
    fn database(&self) -> &RegisteredGlobalDb {
        self.runtime.profile_database()
    }
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn receipt(receipt_id: String, payload: &serde_json::Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).expect("benchmark receipt id is valid"),
            ComponentVersion::new("sanitizer.lcm-expansion-benchmark.v1")
                .expect("benchmark sanitizer version is valid"),
        )
        .expect("benchmark receipt reference is valid"),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).expect("benchmark payload is canonical")),
    )
    .expect("benchmark sanitization receipt is valid")
}

fn observation(session_id: &SessionId, ordinal: u64, content: &str) -> DurableObservationV1 {
    let provider = ProviderId::new(PROVIDER).expect("benchmark provider is valid");
    let source = ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
        .expect("benchmark source identity is valid");
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1)
        .expect("benchmark source range is valid");
    let record_id = ObservationId::new(format!("record.lcm-expansion.{ordinal}"))
        .expect("benchmark record id is valid");
    let message_id = ObservationId::new(format!("message.lcm-expansion.{ordinal}"))
        .expect("benchmark message id is valid");
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id.clone()).with_message_id(message_id),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({ "text": content }),
            model: Some("model.lcm-expansion-benchmark".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).expect("ordinal fits i64")),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .expect("benchmark observation envelope is valid");
    let payload = serde_json::to_value(envelope).expect("benchmark envelope encodes");
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).expect("benchmark generation is non-zero"),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .expect("benchmark observation identity is valid");
    DurableObservationV1::new(
        identity,
        receipt(format!("receipt.lcm-expansion.{ordinal}"), &payload),
        RetentionClass::new("retention.lcm-expansion-benchmark")
            .expect("benchmark retention class is valid"),
        payload,
    )
    .expect("benchmark observation is valid")
}

async fn persist_observation(
    store: &GlobalDbObservationStore<'_>,
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> (ObservationSourceCursorV1, String) {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .expect("benchmark observation cursor is valid");
    let projection_generation =
        ProjectionGenerationId::new("projection.lcm-expansion-benchmark.v1")
            .expect("benchmark projection generation is valid");
    let authorization = build_observation_resolution_authorization_v1(&observation, AUTHORITY)
        .expect("benchmark observation authorization is valid");
    let access_digest = authorization.access_policy_digest.as_str().to_owned();
    let anchor = build_observation_retrieval_anchor_v2(
        &observation,
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .expect("benchmark retrieval anchor is valid");
    let write = ObservationWrite::new(observation.clone(), expected_cursor, next_cursor.clone())
        .expect("benchmark observation write is valid");
    let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation)
        .expect("benchmark anchored observation is valid");
    store
        .persist_observation(anchored)
        .await
        .expect("persist benchmark observation through production store");
    store
        .project_observation(observation.observation_id())
        .await
        .expect("project benchmark observation through production projector");
    (next_cursor, access_digest)
}

fn leaf_publication(index: usize, store_id: i64) -> LcmImmutableSummaryPublication {
    LcmImmutableSummaryPublication {
        summary_id: format!("summary.lcm-expansion.leaf.{index:02}"),
        predecessor_summary_id: None,
        draft: LcmSummaryNodeDraft {
            provider: PROVIDER.to_owned(),
            conversation_id: format!("conversation.lcm-expansion.{index:02}"),
            session_id: SESSION_ID.to_owned(),
            depth: 0,
            summary_text: leaf_summary_text(index),
            source_refs: vec![LcmSourceRef::RawMessage { store_id }],
            source_token_count: 256,
            summary_token_count: 256,
            source_time_start: Some(1_750_000_000),
            source_time_end: Some(1_750_000_100),
            expand_hint: None,
            metadata_json: None,
        },
    }
}

fn leaf_summary_text(index: usize) -> String {
    let prefix = format!("leaf-summary-{index:02}:");
    format!("{prefix}{}", "y".repeat(SOURCE_TEXT_BYTES - prefix.len()))
}

fn root_publication() -> LcmImmutableSummaryPublication {
    LcmImmutableSummaryPublication {
        summary_id: TOP_SUMMARY_ID.to_owned(),
        predecessor_summary_id: None,
        draft: LcmSummaryNodeDraft {
            provider: PROVIDER.to_owned(),
            conversation_id: "conversation.lcm-expansion.root".to_owned(),
            session_id: SESSION_ID.to_owned(),
            depth: 1,
            summary_text: TOP_SUMMARY_TEXT.to_owned(),
            source_refs: (0..SOURCE_COUNT)
                .map(|index| LcmSourceRef::SummaryNode {
                    node_id: format!("summary.lcm-expansion.leaf.{index:02}"),
                })
                .collect(),
            source_token_count: i64::try_from(SOURCE_COUNT * 256).expect("source count fits i64"),
            summary_token_count: 6,
            source_time_start: Some(1_750_000_000),
            source_time_end: Some(1_750_000_100),
            expand_hint: None,
            metadata_json: None,
        },
    }
}

fn expansion_request() -> LcmExpandRequest {
    LcmExpandRequest {
        provider: PROVIDER.to_owned(),
        session_id: SESSION_ID.to_owned(),
        target: LcmExpandTarget::SummaryNode {
            node_id: TOP_SUMMARY_ID.to_owned(),
        },
        content_slice: Some(LcmContentSlice {
            offset: 0,
            limit: TOP_SUMMARY_TEXT.len(),
        }),
        source_offset: 0,
        source_limit: Some(SOURCE_COUNT),
    }
}

async fn build_fixture() -> Fixture {
    let profile = tempfile::tempdir().expect("create private benchmark profile");
    let runtime = RegisteredGlobalDbTestRuntime::profile(profile.path())
        .await
        .expect("open registered production store fixture");
    let database = runtime.profile_database();
    let store = GlobalDbObservationStore::with_runtime(database.runtime(), database.authority());
    let session_id = SessionId::new(SESSION_ID).expect("benchmark session id is valid");
    let mut cursor = None;
    let mut access_digest = None;
    let mut expected_source_content = Vec::with_capacity(SOURCE_COUNT);
    let publication_control = ExecutionControl::default();
    for index in 0..SOURCE_COUNT {
        let content = format!("source-{index:02}:{}", "x".repeat(SOURCE_TEXT_BYTES));
        let durable = observation(
            &session_id,
            u64::try_from(index).expect("source index fits u64"),
            &content,
        );
        let message_id = format!("message.lcm-expansion.{index}");
        let (next_cursor, retained_access_digest) =
            persist_observation(&store, durable, cursor).await;
        cursor = Some(next_cursor);
        access_digest = Some(retained_access_digest);
        let store_id = database
            .lcm_raw_message_store_id(PROVIDER, &message_id)
            .await
            .expect("read benchmark raw-message locator")
            .expect("projected benchmark raw message is present");
        database
            .lcm_publish_immutable_summary_guarded(
                leaf_publication(index, store_id),
                &publication_control,
                || Ok(()),
            )
            .await
            .expect("publish benchmark leaf through production authority");
        expected_source_content.push(leaf_summary_text(index));
    }
    let root_receipt = database
        .lcm_publish_immutable_summary_guarded(root_publication(), &publication_control, || Ok(()))
        .await
        .expect("publish benchmark root through production authority");
    let generation = u64::try_from(root_receipt.generation)
        .expect("published benchmark generation is non-negative");
    let snapshot = TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            session_id,
            digest('1'),
            digest('2'),
            access_digest.expect("benchmark access digest is retained"),
            TemporalModeV1::Current,
            RetrievalGrainV1::Summary,
        )
        .expect("benchmark temporal request is valid"),
        TemporalWatermarks {
            generation,
            source: 0,
            projection: 0,
            index: 0,
            summary: generation,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('3'))
                .expect("benchmark configuration digest is valid"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("benchmark temporal snapshot is valid");
    Fixture {
        _profile: profile,
        runtime,
        snapshot,
        expected_source_content,
    }
}

async fn expand(fixture: &Fixture) -> LcmExpandResponse {
    let executor = RegisteredGlobalDbSessionTemporalExecution::new(fixture.database());
    let mut expansion = executor
        .render_lcm_expand(
            expansion_request(),
            TOP_SUMMARY_TEXT,
            fixture.snapshot.request().execution_control(),
        )
        .await
        .expect("render benchmark expansion through production authority");
    executor
        .hydrate_lcm_summary_sources(
            &fixture.snapshot,
            PROVIDER,
            &SessionId::new(SESSION_ID).expect("benchmark session id is valid"),
            LcmContentSlice {
                offset: 0,
                limit: SOURCE_TEXT_BYTES,
            },
            &mut expansion,
        )
        .await
        .expect("hydrate benchmark sources through production authority");
    expansion
}

fn assert_expansion(fixture: &Fixture, expansion: &LcmExpandResponse) {
    assert_eq!(expansion.content, TOP_SUMMARY_TEXT);
    assert_eq!(expansion.summary_sources.len(), SOURCE_COUNT);
    for (source, expected) in expansion
        .summary_sources
        .iter()
        .zip(&fixture.expected_source_content)
    {
        assert_eq!(source.state, HydrationStateV1::Available);
        assert_eq!(&source.content, expected);
    }
}

fn lcm_expansion(criterion: &mut Criterion) {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build benchmark runtime");
    let fixture = tokio.block_on(build_fixture());
    let preflight = tokio.block_on(expand(&fixture));
    assert_expansion(&fixture, &preflight);

    let mut group = criterion.benchmark_group("lcm_expansion");
    group.throughput(Throughput::Elements(SOURCE_COUNT as u64));
    group.bench_function("render_and_hydrate_summary_sources", |bencher| {
        bencher
            .to_async(&tokio)
            .iter(|| async { black_box(expand(&fixture).await) });
    });
    group.finish();
}

criterion_group!(benches, lcm_expansion);
criterion_main!(benches);

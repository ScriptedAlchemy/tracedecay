use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    AnchorProvenanceRelationV2, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CopyProofV1, DurableObservationV1, LogicalCopyRecordV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
    RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId, TemporalAssertionKindV1, TemporalValidityV1, UtcMicros,
    derive_exact_observation_anchor_id,
};
use tracedecay_graph_db::NeverCancelled;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjection, ObservationProjectionStore, ObservationStore,
    ObservationWrite, ProjectionSkipReason, ProjectionStoreError,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCompletionRequestV1,
    SessionRefreshFrontierV1, SessionRefreshProgressV1, SessionRefreshStore,
    SessionRefreshTerminalStateV1, SessionStoreError, SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use super::super::refresh::SessionRefreshRestartStateV1;
use super::materialize::*;
use super::persist::persist_occurrences;
use super::record_canonical_observation_effect;
use crate::GlobalDbSessionTemporalStore;
use crate::handle::SessionTemporalRegisteredDb;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, SessionTemporalFixtureCountV1,
    open_registered_test_database_fixture,
};
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows, TestConnection, params,
};

struct QueryCountingConnection<'a, T> {
    inner: &'a T,
    queries: AtomicUsize,
}

impl<'a, T> QueryCountingConnection<'a, T> {
    fn new(inner: &'a T) -> Self {
        Self {
            inner,
            queries: AtomicUsize::new(0),
        }
    }

    fn query_count(&self) -> usize {
        self.queries.load(Ordering::Relaxed)
    }
}

impl<T: QueryExecutor> QueryExecutor for QueryCountingConnection<'_, T> {
    async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
    where
        P: IntoParams,
    {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.inner.query(sql, params).await
    }
}

impl<T: Executor> Executor for QueryCountingConnection<'_, T> {
    async fn execute<P>(&self, sql: &str, params: P) -> EngineResult<u64>
    where
        P: IntoParams,
    {
        self.inner.execute(sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> EngineResult<()> {
        self.inner.execute_batch(sql).await
    }
}

impl<T: crate::handle::SessionTemporalQuery> crate::handle::SessionTemporalQuery
    for QueryCountingConnection<'_, T>
{
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl std::future::Future<Output = Result<Rows, tracedecay_runtime_core::db::engine::Error>> + Send
    where
        P: IntoParams + Send,
    {
        self.queries.fetch_add(1, Ordering::Relaxed);
        crate::handle::SessionTemporalQuery::query(self.inner, sql, params)
    }
}

impl<T: crate::handle::SessionTemporalExec> crate::handle::SessionTemporalExec
    for QueryCountingConnection<'_, T>
{
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl std::future::Future<Output = Result<u64, tracedecay_runtime_core::db::engine::Error>> + Send
    where
        P: IntoParams + Send,
    {
        crate::handle::SessionTemporalExec::execute(self.inner, sql, params)
    }

    fn execute_batch(
        &self,
        sql: &str,
    ) -> impl std::future::Future<Output = Result<(), tracedecay_runtime_core::db::engine::Error>> + Send
    {
        crate::handle::SessionTemporalExec::execute_batch(self.inner, sql)
    }
}

fn fixture_session(value: &str) -> SessionId {
    SessionId::new(value).unwrap()
}

fn temporal_store(
    runtime: &HostAdmissionTestRuntimeV1,
) -> GlobalDbSessionTemporalStore<'_, RegisteredGlobalDb> {
    GlobalDbSessionTemporalStore::new(
        runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile session-temporal store"),
    )
}

fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.projector-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).unwrap()),
    )
    .unwrap()
}

fn fixture_observation(
    session_id: &SessionId,
    ordinal: u64,
    lineage: Option<(AnchorProvenanceRelationV2, RetrievalAnchorId)>,
    include_parent: bool,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let provider = ProviderId::new(format!("projector-test-{ordinal}")).unwrap();
    let record_id = ObservationId::new(format!("record.projector.{ordinal}")).unwrap();
    let mut relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_thread_id(ObservationId::new("thread.projector").unwrap())
        .with_turn_id(ObservationId::new("turn.projector").unwrap())
        .with_message_id(ObservationId::new(format!("message.projector.{ordinal}")).unwrap())
        .with_agent_id(ObservationId::new("agent.projector").unwrap());
    if include_parent && ordinal > 0 {
        relations = relations.with_parent_message_id(
            ObservationId::new(format!("message.projector.{}", ordinal - 1)).unwrap(),
        );
    }
    fixture_observation_from_facts(
        session_id,
        ordinal,
        provider,
        record_id,
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": format!("projector {ordinal}")}),
            model: Some("model.projector".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        }],
        lineage,
    )
}

fn fixture_multi_output_observation(
    session_id: &SessionId,
    ordinal: u64,
    lineage: Option<(AnchorProvenanceRelationV2, RetrievalAnchorId)>,
    output_count: usize,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    assert!(output_count > 1);
    let provider = ProviderId::new("cursor").unwrap();
    let record_id = ObservationId::new(format!("record.projector.multi.{ordinal}")).unwrap();
    let relations = CanonicalObservationRelationsV1::new(session_id.clone())
        .with_thread_id(ObservationId::new("thread.projector.multi").unwrap())
        .with_turn_id(ObservationId::new("turn.projector.multi").unwrap())
        .with_message_id(ObservationId::new(format!("message.projector.multi.{ordinal}")).unwrap())
        .with_agent_id(ObservationId::new("agent.projector.multi").unwrap());
    let mut facts = vec![
        CanonicalObservationFactV1::Session {
            project_path: None,
            location_path: None,
            transcript_path: None,
            title: None,
            started_at: None,
            ended_at: None,
            source: Some("cursor_composer".to_owned()),
            native_source: None,
            profile: None,
            location_provenance: None,
        },
        CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": "multi-output primary"}),
            model: Some("model.projector".to_owned()),
            timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
        },
    ];
    facts.extend(
        (1..output_count).map(|index| CanonicalObservationFactV1::ToolInvocation {
            invocation_id: ObservationId::new(format!("tool.projector.multi.{ordinal}.{index}"))
                .unwrap(),
            name: "Read".to_owned(),
            arguments: json!({"index": index}),
        }),
    );
    fixture_observation_from_facts(
        session_id, ordinal, provider, record_id, relations, facts, lineage,
    )
}

fn fixture_observation_from_facts(
    session_id: &SessionId,
    ordinal: u64,
    provider: ProviderId,
    record_id: ObservationId,
    relations: CanonicalObservationRelationsV1,
    facts: Vec<CanonicalObservationFactV1>,
    lineage: Option<(AnchorProvenanceRelationV2, RetrievalAnchorId)>,
) -> (DurableObservationV1, AnchoredObservationWrite) {
    let source =
        ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone()).unwrap();
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        relations,
        facts,
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt(&format!("receipt.projector.{ordinal}"), &payload),
        RetentionClass::new("retention.projector-test").unwrap(),
        payload,
    )
    .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation.clone(), None, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.projector-test.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "projector-test",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let mut anchor_json = serde_json::to_value(anchor).unwrap();
    if let Some((relation, anchor_id)) = lineage {
        anchor_json["source_anchors"] = json!([{
            "relation": relation,
            "anchor_id": anchor_id,
            "owner": write.observation().scope(),
        }]);
    }
    let anchor: RetrievalAnchorRecordV2 = serde_json::from_value(anchor_json).unwrap();
    let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap();
    (observation, anchored)
}

fn fixture_goal_observation() -> (DurableObservationV1, AnchoredObservationWrite) {
    let record_id = ObservationId::new("record.goal.fixture").unwrap();
    let encoded = include_str!(
        "../../../../tests/fixtures/provider_normalization/codex/thread_goal_updated.expected_envelope.json"
    )
    .replace("$STABLE_RECORD_ID", record_id.as_str());
    let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_str(&encoded).unwrap();
    let provider = envelope.provider().clone();
    let session_id = envelope.relations().session_id().clone();
    let range = envelope.evidence().range();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id).unwrap();
    let payload = serde_json::to_value(&envelope).unwrap();
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Profile,
        ObservationSourceGenerationV1::new(1).unwrap(),
        range,
        envelope.evidence().ordering_domain(),
        record_id,
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        identity,
        fixture_receipt("receipt.goal.fixture", &payload),
        RetentionClass::new("retention.projector-test").unwrap(),
        payload,
    )
    .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation.clone(), None, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new("projection.projector-test.v1").unwrap();
    let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
        write.observation(),
        "projector-test",
    )
    .unwrap();
    let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap();
    (observation, anchored)
}

async fn persist_fixture(
    runtime: &HostAdmissionTestRuntimeV1,
    observation: DurableObservationV1,
    anchored: AnchoredObservationWrite,
) {
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .expect("registered profile observation store");
    store.persist_observation(anchored).await.unwrap();
    store
        .project_observation(observation.observation_id())
        .await
        .unwrap();
}

async fn ready_single_observation_projection(
    session_name: &str,
) -> (
    TempDir,
    HostAdmissionTestRuntimeV1,
    SessionRefreshProgressV1,
    SessionTemporalProjectionBatchV1,
) {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let session_id = fixture_session(session_name);
    let (observation, write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    let store = temporal_store(&runtime);
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    (tmp, runtime, progress, batch)
}

async fn ready_single_observation_completion(
    session_name: &str,
) -> (
    TempDir,
    HostAdmissionTestRuntimeV1,
    SessionRefreshCompletionRequestV1,
    i64,
) {
    let (tmp, runtime, progress, batch) = ready_single_observation_projection(session_name).await;
    let request = SessionRefreshCompletionRequestV1::new(
        progress.operation_id().clone(),
        progress.session_id().clone(),
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    let generation = i64::try_from(batch.generation().value()).unwrap();
    temporal_store(&runtime)
        .persist_session_refresh_projection_batch(progress, batch)
        .await
        .unwrap();
    (tmp, runtime, request, generation)
}

#[tokio::test]
async fn checked_in_codex_goal_materializes_one_generation_bound_occurrence() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let (observation, anchored) = fixture_goal_observation();
    let session_id = observation.source().session_id().clone();
    let expected_anchor =
        derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
            .unwrap();
    Box::pin(persist_fixture(&runtime, observation, anchored)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (_, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(batch.occurrences().len(), 1);
    assert!(batch.copies().is_empty());
    assert!(batch.assertions().is_empty());
    let occurrence = &batch.occurrences()[0];
    assert_eq!(occurrence.session_id, session_id);
    assert_eq!(occurrence.retrieval_anchor_id, expected_anchor);
    assert_eq!(
        occurrence
            .message_id
            .as_ref()
            .map(tracedecay_domain::MessageId::as_str),
        Some("record.goal.fixture")
    );
    assert_eq!(
        occurrence.valid_time,
        TemporalValidityV1::Known {
            valid_at: UtcMicros(1_783_500_569_000_000)
        }
    );
}

#[tokio::test]
async fn multi_output_projection_reuses_source_derivation_and_activates_shared_anchor_assertions() {
    const OUTPUT_COUNT: usize = 64;

    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.multi-output");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    let first_observation_id = first.observation_id().clone();
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    Box::pin(persist_fixture(&runtime, first, first_write)).await;
    let (multi, multi_write) = fixture_multi_output_observation(
        &session_id,
        1,
        Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
        OUTPUT_COUNT,
    );
    let multi_observation_id = multi.observation_id().clone();
    Box::pin(persist_fixture(&runtime, multi, multi_write)).await;

    let begin = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.occurrences().len(), OUTPUT_COUNT + 1);
    assert_eq!(batch.assertions().len(), 1);
    assert_eq!(
        batch.assertions()[0].kind,
        TemporalAssertionKindV1::Supersedes
    );

    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let effects = [
        (first_observation_id, 0, 1),
        (multi_observation_id, 1, OUTPUT_COUNT),
    ];
    let (rematerialized_occurrences, materialization_work) =
        materialize_effect_occurrences(&transaction, &effects, batch.occurrences().len())
            .await
            .unwrap();
    assert_eq!(rematerialized_occurrences.as_slice(), batch.occurrences());
    assert_eq!(materialization_work.envelope_parses, 2);
    let (_, relation_assertions, relation_work) = derive_retained_projection_relations(
        &transaction,
        &session_id,
        batch.occurrences(),
        &ParentMessageResolver::default(),
    )
    .await
    .unwrap();
    assert_eq!(relation_assertions.len(), 1);
    assert_eq!(relation_work.envelope_parses, 2);
    let counted = QueryCountingConnection::new(&transaction);
    let work = persist_occurrences(&counted, &batch, &ExecutionControl::default())
        .await
        .unwrap();
    assert_eq!(work.source_projections, 2);
    assert_eq!(work.envelope_parses, 2);
    assert_eq!(
        work.indexed_outputs,
        u64::try_from(batch.occurrences().len()).unwrap()
    );
    assert_eq!(
        work.output_lookups,
        u64::try_from(batch.occurrences().len()).unwrap()
    );
    assert_eq!(
        materialization_work
            .envelope_parses
            .saturating_add(relation_work.envelope_parses)
            .saturating_add(work.envelope_parses),
        6
    );
    assert!(
        counted.query_count() <= batch.occurrences().len() + 4,
        "each occurrence needs one anchor lookup and each of the two source observations needs one observation/effect pair, but persistence ran {} queries for {} occurrences",
        counted.query_count(),
        batch.occurrences().len()
    );
    // `counted` holds no Drop impl, so dropping it only extends its borrows.
    drop(transaction);

    store
        .persist_session_refresh_projection_batch(progress, batch)
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = recovery.progress().unwrap();
    let request = SessionRefreshCompletionRequestV1::new(
        begin.operation_id().clone(),
        session_id.clone(),
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    assert_eq!(
        store
            .complete_session_refresh(request, ExecutionControl::default())
            .await
            .unwrap()
            .state(),
        SessionRefreshTerminalStateV1::Complete
    );
    for (kind, expected) in [
        (
            SessionTemporalFixtureCountV1::Occurrences,
            i64::try_from(OUTPUT_COUNT + 1).unwrap(),
        ),
        (SessionTemporalFixtureCountV1::Assertions, 1),
    ] {
        assert_eq!(
            runtime
                .session_temporal_fixture_count_for_test(HostAdmissionScope::Profile, kind)
                .await
                .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn relation_batch_persists_restarts_and_completes_without_duplicates() {
    let tmp = TempDir::new().unwrap();
    let session_id = fixture_session("session.projector.relation-restart");
    let operation_id;
    {
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let store = temporal_store(&runtime);
        let (first, first_write) = fixture_observation(&session_id, 0, None, false);
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        Box::pin(persist_fixture(&runtime, first, first_write)).await;
        let (second, second_write) = fixture_observation(
            &session_id,
            1,
            Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
            true,
        );
        Box::pin(persist_fixture(&runtime, second, second_write)).await;
        let begin = store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(2, 0).unwrap(),
            ))
            .await
            .unwrap();
        operation_id = begin.operation_id().clone();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.occurrences().len(), 2);
        // A parent-message link is conversation threading, not a logical copy:
        // the derived copy edge requires the occurrence's own logical message id
        // to be the parent link, so a reply contributes no copy record.
        assert!(batch.copies().is_empty());
        assert_eq!(batch.assertions().len(), 1);
        assert_eq!(batch.item_count(), 3);
        assert_eq!(progress.committed_records(), 3);
        assert_eq!(progress.coverage().visible, 3);
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }

    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovery.restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    assert!(
        store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
            .is_none()
    );
    let progress = recovery.progress().unwrap();
    let request = SessionRefreshCompletionRequestV1::new(
        operation_id,
        session_id,
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    let receipt = store
        .complete_session_refresh(request.clone(), ExecutionControl::default())
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        store
            .complete_session_refresh(request, ExecutionControl::default())
            .await
            .unwrap(),
        receipt
    );
    for (kind, expected) in [
        (SessionTemporalFixtureCountV1::ProjectionReceipts, 1),
        (SessionTemporalFixtureCountV1::Occurrences, 2),
        (SessionTemporalFixtureCountV1::Assertions, 1),
        (SessionTemporalFixtureCountV1::RefreshReceipts, 1),
    ] {
        assert_eq!(
            runtime
                .session_temporal_fixture_count_for_test(HostAdmissionScope::Profile, kind)
                .await
                .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn terminal_receipt_rejects_corrupted_derived_evidence() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.derived-receipt-corruption");
    for ordinal in 0..3 {
        let (observation, write) = fixture_observation(&session_id, ordinal, None, ordinal > 0);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
    }
    let begin = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(3, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    let generation = batch.generation();
    store
        .persist_session_refresh_projection_batch(progress.clone(), batch)
        .await
        .unwrap();

    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let changed = transaction
        .execute(
            "UPDATE session_derived_evidence
             SET evidence_json = json_set(evidence_json, '$.corrupted', 1)
             WHERE session_id = ?1 AND generation = ?2",
            params![
                session_id.as_str(),
                i64::try_from(generation.value()).unwrap()
            ],
        )
        .await
        .unwrap();
    assert!(
        changed > 0,
        "fixture must produce receipt-bound derived evidence"
    );
    transaction.commit().await.unwrap();

    let request = SessionRefreshCompletionRequestV1::new(
        begin.operation_id().clone(),
        session_id,
        progress.frontier(),
        *progress.coverage(),
    )
    .unwrap();
    let error = store
        .complete_session_refresh(request, ExecutionControl::default())
        .await
        .expect_err("derived evidence corruption must invalidate the terminal receipt");
    assert!(matches!(error, SessionStoreError::Storage { .. }));
}

#[tokio::test]
async fn cancelled_terminal_persistence_rolls_back_candidate_state() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.cancelled-terminal-persist");
    let (observation, write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, observation, write)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    let error = store
        .persist_session_refresh_projection_batch_controlled(
            progress,
            batch,
            // Seeding consumes 21 checkpoints, persistence admission one,
            // the source group one, and the first occurrence one. The next
            // terminal-phase checkpoint therefore fails after that occurrence
            // has been inserted into the still-uncommitted candidate.
            ExecutionControl::default().with_work_limit(24),
        )
        .await
        .expect_err("terminal persistence must honor its execution budget");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::ProjectionReceipts,
            )
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::Occurrences,
            )
            .await
            .unwrap(),
        0,
        "the occurrence inserted before cancellation must roll back with the transaction"
    );
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::RefreshProgress,
            )
            .await
            .unwrap(),
        0,
        "projection progress must not advance when the candidate transaction rolls back"
    );
    assert_eq!(
        store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
}

#[tokio::test]
async fn cancellation_at_final_precommit_checkpoint_rolls_back_all_projection_writes() {
    const WORK_LIMIT: usize = 4_096;

    let (_successful_tmp, successful_runtime, successful_progress, successful_batch) =
        ready_single_observation_projection("session.projector.precommit-meter").await;
    let successful_store = temporal_store(&successful_runtime);
    let successful_control = ExecutionControl::default().with_work_limit(WORK_LIMIT);
    let meter = successful_control.clone();
    successful_store
        .persist_session_refresh_projection_batch_controlled(
            successful_progress,
            successful_batch,
            successful_control,
        )
        .await
        .unwrap();
    let mut unused_work = 0usize;
    while meter.checkpoint().is_ok() {
        unused_work += 1;
    }
    let used_work = WORK_LIMIT.checked_sub(unused_work).unwrap();
    assert!(used_work > 1 && used_work < WORK_LIMIT);

    let (_cancelled_tmp, cancelled_runtime, cancelled_progress, cancelled_batch) =
        ready_single_observation_projection("session.projector.precommit-cancel").await;
    let cancelled_store = temporal_store(&cancelled_runtime);
    let session_id = cancelled_progress.session_id().clone();
    let error = cancelled_store
        .persist_session_refresh_projection_batch_controlled(
            cancelled_progress,
            cancelled_batch,
            ExecutionControl::default().with_work_limit(used_work - 1),
        )
        .await
        .expect_err("the final checkpoint must reject cancellation before commit");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));
    for kind in [
        SessionTemporalFixtureCountV1::ProjectionReceipts,
        SessionTemporalFixtureCountV1::Occurrences,
        SessionTemporalFixtureCountV1::RefreshProgress,
    ] {
        assert_eq!(
            cancelled_runtime
                .session_temporal_fixture_count_for_test(HostAdmissionScope::Profile, kind)
                .await
                .unwrap(),
            0,
            "all candidate writes before the final checkpoint must roll back"
        );
    }
    let database = cancelled_runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM session_derived_evidence WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    drop(transaction);
    assert_eq!(
        cancelled_store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
}

#[tokio::test]
async fn cancellation_at_completion_precommit_rolls_back_activation_and_terminal_receipt() {
    const WORK_LIMIT: usize = 4_096;
    // Sync point, not a performance budget: one less than the successful
    // completion's checkpoint count so cancellation fires on the pre-commit
    // checkpoint after activation and the terminal receipt. The measured
    // count includes identity-index cancellation polls during relation load.
    const COMPLETION_PRECOMMIT_WORK_LIMIT: usize = 80;

    let (_successful_tmp, successful_runtime, successful_request, _) =
        ready_single_observation_completion("session.projector.completion-meter").await;
    let successful_control = ExecutionControl::default().with_work_limit(WORK_LIMIT);
    let meter = successful_control.clone();
    temporal_store(&successful_runtime)
        .complete_session_refresh(successful_request, successful_control)
        .await
        .unwrap();
    let mut unused_work = 0usize;
    while meter.checkpoint().is_ok() {
        unused_work += 1;
    }
    let used_work = WORK_LIMIT.checked_sub(unused_work).unwrap();
    assert_eq!(
        used_work,
        COMPLETION_PRECOMMIT_WORK_LIMIT + 1,
        "the cancellation budget below must expire only after finalization writes"
    );

    let (_cancelled_tmp, cancelled_runtime, cancelled_request, candidate_generation) =
        ready_single_observation_completion("session.projector.completion-cancel").await;
    let session_id = cancelled_request.session_id().clone();
    let operation_id = cancelled_request.operation_id().clone();
    let error = temporal_store(&cancelled_runtime)
        .complete_session_refresh(
            cancelled_request,
            ExecutionControl::default().with_work_limit(COMPLETION_PRECOMMIT_WORK_LIMIT),
        )
        .await
        .expect_err("completion must checkpoint after finalization writes");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));

    let database = cancelled_runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let mut generation_rows = transaction
        .query(
            "SELECT state FROM session_temporal_generations
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id.as_str(), candidate_generation],
        )
        .await
        .unwrap();
    assert_eq!(
        generation_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "building"
    );
    drop(generation_rows);
    let mut operation_rows = transaction
        .query(
            "SELECT state FROM session_refresh_operations
             WHERE session_id = ?1 AND operation_id = ?2",
            params![session_id.as_str(), operation_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        operation_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "running"
    );
    drop(operation_rows);
    drop(transaction);
    assert_eq!(
        cancelled_runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::RefreshReceipts,
            )
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn cancellation_during_active_generation_seed_rolls_back_copied_rows() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.cancelled-seed");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    Box::pin(persist_fixture(&runtime, first, first_write)).await;
    let begin = store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    store
        .persist_session_refresh_projection_batch(progress, batch)
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let progress = recovery.progress().unwrap();
    store
        .complete_session_refresh(
            SessionRefreshCompletionRequestV1::new(
                begin.operation_id().clone(),
                session_id.clone(),
                progress.frontier(),
                *progress.coverage(),
            )
            .unwrap(),
            ExecutionControl::default(),
        )
        .await
        .unwrap();

    let (second, second_write) = fixture_observation(&session_id, 1, None, true);
    Box::pin(persist_fixture(&runtime, second, second_write)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 1).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    let candidate_generation = i64::try_from(batch.generation().value()).unwrap();
    let error = store
        .persist_session_refresh_projection_batch_controlled(
            progress,
            batch,
            // Admission and the first pre-copy checkpoint succeed; the
            // post-copy checkpoint cancels after session_turns was copied.
            ExecutionControl::default().with_work_limit(2),
        )
        .await
        .expect_err("cancellation during active-generation seeding must abort the transaction");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));

    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let transaction = database.begin_write_transaction().await.unwrap();
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM session_turns
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id.as_str(), candidate_generation],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    drop(transaction);
    assert_eq!(
        store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap()
            .restart_state(),
        SessionRefreshRestartStateV1::BeginProjection
    );
}

#[tokio::test]
async fn copied_from_lineage_is_not_auto_emitted_by_materializer() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.copied-from");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    Box::pin(persist_fixture(&runtime, first, first_write)).await;
    let (second, second_write) = fixture_observation(
        &session_id,
        1,
        Some((AnchorProvenanceRelationV2::CopiedFrom, first_anchor)),
        false,
    );
    Box::pin(persist_fixture(&runtime, second, second_write)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.occurrences().len(), 2);
    assert!(batch.copies().is_empty());
    assert!(batch.assertions().is_empty());
    assert_eq!(progress.committed_records(), batch.item_count() as u64);
}

#[tokio::test]
async fn relation_derivation_backs_off_to_the_total_batch_limit() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.derived-limit");
    // Every observation after the first supersedes its predecessor, so each of
    // those occurrences derives one typed assertion edge on top of its own
    // occurrence record. 501 observations therefore derive 1001 records, one
    // past `MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS`, and the materializer
    // must back off to a 500-observation prefix.
    let mut previous_anchor = None;
    for ordinal in 0..501 {
        let lineage = previous_anchor
            .take()
            .map(|anchor| (AnchorProvenanceRelationV2::Supersedes, anchor));
        let (observation, write) = fixture_observation(&session_id, ordinal, lineage, ordinal > 0);
        previous_anchor = Some(
            derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
                .unwrap(),
        );
        Box::pin(persist_fixture(&runtime, observation, write)).await;
    }
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(501, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (first_progress, first_batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_batch.occurrences().len(), 500);
    assert!(first_batch.copies().is_empty());
    assert_eq!(first_batch.assertions().len(), 499);
    assert_eq!(first_batch.item_count(), 999);
    assert_eq!(first_progress.frontier().committed_through(), 500);
    assert_eq!(first_progress.committed_records(), 999);
    store
        .persist_session_refresh_projection_batch(first_progress, first_batch)
        .await
        .unwrap();

    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (second_progress, second_batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_batch.occurrences().len(), 1);
    assert!(second_batch.copies().is_empty());
    assert_eq!(second_batch.assertions().len(), 1);
    assert_eq!(second_batch.item_count(), 2);
    assert_eq!(second_progress.frontier().committed_through(), 501);
    assert_eq!(second_progress.committed_records(), 1001);
    store
        .persist_session_refresh_projection_batch(second_progress, second_batch)
        .await
        .unwrap();
}

#[test]
fn assertion_identity_includes_both_anchors() {
    let session_id = fixture_session("session.projector.assertion-identity");
    let (first, _) = fixture_observation(&session_id, 0, None, false);
    let (second, _) = fixture_observation(&session_id, 1, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    let second_anchor =
        derive_exact_observation_anchor_id(second.scope(), second.observation_id()).unwrap();
    let first_id = derived_temporal_assertion_id(
        &first_anchor,
        TemporalAssertionKindV1::Supports,
        &first_anchor,
    );
    let second_id = derived_temporal_assertion_id(
        &first_anchor,
        TemporalAssertionKindV1::Supports,
        &second_anchor,
    );
    let third_id = derived_temporal_assertion_id(
        &second_anchor,
        TemporalAssertionKindV1::Supports,
        &first_anchor,
    );
    assert_ne!(first_id, second_id);
    assert_ne!(first_id, third_id);
    assert!(first_id.starts_with("sha256:"));
    assert_eq!(first_id.len(), 71);
}

#[tokio::test]
async fn parent_resolver_rejects_ambiguous_session_message_ids() {
    let mut resolver = ParentMessageResolver::default();
    resolver.register("message.shared", "occurrence.a");
    resolver.register("message.shared", "occurrence.b");
    let error = resolver
        .reject_ambiguity("test parent ambiguity")
        .expect_err("duplicate message ids must be rejected");
    let detail = format!("{error:?}");
    assert!(
        detail.contains("message.shared") || detail.contains("resolves to 2 occurrences"),
        "{detail}"
    );
}

#[tokio::test]
async fn parent_resolver_pages_live_sized_observation_history() {
    let directory = TempDir::new().unwrap();
    let connection = TestConnection::open(&directory.path().join("resolver-pages.db"));
    connection
        .execute_batch(
            "CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_observation_effects (
                observation_id TEXT PRIMARY KEY,
                observation_sequence INTEGER NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                output_count INTEGER NOT NULL
             );
             CREATE INDEX idx_session_temporal_observation_effects_session
                ON session_temporal_observation_effects(session_id, observation_sequence);",
        )
        .await
        .unwrap();
    let session_id = fixture_session("session.projector.paged-parent-resolver");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    let encoded = serde_json::to_string(&observation).unwrap();
    connection
        .execute(
            "WITH RECURSIVE fixture(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM fixture WHERE value < 10001
             )
             INSERT INTO observations (observation_id, observation_json)
             SELECT printf('observation.%05d', value), ?1 FROM fixture",
            params![encoded],
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                 observation_id, observation_sequence, session_id, output_count
             )
             SELECT observation_id, sequence, ?1, 1 FROM observations",
            params![session_id.as_str()],
        )
        .await
        .unwrap();

    let resolver = canonical_parent_message_resolver(
        &*connection,
        session_id.as_str(),
        10001,
        "test paged parent resolver",
        None,
        false,
    )
    .await
    .unwrap();

    assert!(resolver.resolve("message.projector.0").is_some());
}

#[tokio::test]
async fn parent_resolver_has_bounded_cancellable_session_traversal() {
    const UNRELATED_OBSERVATIONS: i64 = 2_048;

    let directory = TempDir::new().unwrap();
    let connection = TestConnection::open(&directory.path().join("resolver-session-bound.db"));
    connection
        .execute_batch(
            "CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                observation_json TEXT NOT NULL
             );
             CREATE TABLE session_temporal_observation_effects (
                observation_id TEXT PRIMARY KEY,
                observation_sequence INTEGER NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                output_count INTEGER NOT NULL
             );
             CREATE INDEX idx_session_temporal_observation_effects_session
                ON session_temporal_observation_effects(session_id, observation_sequence);",
        )
        .await
        .unwrap();
    let unrelated_session = fixture_session("session.projector.unrelated-history");
    let (unrelated, _) = fixture_observation(&unrelated_session, 0, None, false);
    let unrelated_json = serde_json::to_string(&unrelated).unwrap();
    connection
        .execute(
            "WITH RECURSIVE fixture(value) AS (
                 SELECT 1
                 UNION ALL
                 SELECT value + 1 FROM fixture WHERE value < ?2
             )
             INSERT INTO observations (observation_id, observation_json)
             SELECT printf('observation.unrelated.%05d', value), ?1 FROM fixture",
            params![unrelated_json, UNRELATED_OBSERVATIONS],
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                 observation_id, observation_sequence, session_id, output_count
             )
             SELECT observation_id, sequence, ?1, 1 FROM observations",
            params![unrelated_session.as_str()],
        )
        .await
        .unwrap();

    let session_id = fixture_session("session.projector.session-bound-parent-resolver");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    connection
        .execute(
            "INSERT INTO observations (observation_id, observation_json) VALUES (?1, ?2)",
            params![
                observation.observation_id().as_str(),
                serde_json::to_string(&observation).unwrap(),
            ],
        )
        .await
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                 observation_id, observation_sequence, session_id, output_count
             )
             SELECT observation_id, sequence, ?2, 1
             FROM observations WHERE observation_id = ?1",
            params![observation.observation_id().as_str(), session_id.as_str()],
        )
        .await
        .unwrap();

    let counted = QueryCountingConnection::new(&connection);
    let resolver = canonical_parent_message_resolver(
        &counted,
        session_id.as_str(),
        u64::try_from(UNRELATED_OBSERVATIONS + 1).unwrap(),
        "test session-bounded parent resolver",
        None,
        false,
    )
    .await
    .unwrap();

    assert!(resolver.resolve("message.projector.0").is_some());
    assert!(
        counted.query_count() <= 2,
        "one relevant history page plus the terminal probe is sufficient, but {} queries visited unrelated profile history",
        counted.query_count()
    );

    let control = ExecutionControl::default().with_work_limit(1);
    let error = canonical_parent_message_resolver(
        &connection,
        session_id.as_str(),
        u64::try_from(UNRELATED_OBSERVATIONS + 1).unwrap(),
        "test cancellable parent resolver",
        Some(&control),
        false,
    )
    .await
    .expect_err("the resolver must checkpoint while visiting its first row");
    assert!(matches!(error, SessionStoreError::BudgetExceeded { .. }));
}

#[test]
fn parent_resolver_registers_same_batch_effects_only_after_derivation() {
    let mut resolver = ParentMessageResolver::default();
    assert_eq!(resolver.resolve("message.reemitted"), None);

    resolver.register("message.reemitted", "occurrence.first-effect");
    assert_eq!(
        resolver.resolve("message.reemitted"),
        Some("occurrence.first-effect")
    );
}

#[test]
fn parent_resolver_prefers_a_persisted_cross_batch_predecessor() {
    let mut resolver = ParentMessageResolver::default();
    resolver.register("message.reemitted", "occurrence.persisted-predecessor");
    assert_eq!(
        resolver.resolve("message.reemitted"),
        Some("occurrence.persisted-predecessor")
    );

    resolver.register("message.reemitted", "occurrence.aaa-current-effect");
    assert_eq!(
        resolver.resolve("message.reemitted"),
        Some("occurrence.persisted-predecessor")
    );
}

#[tokio::test]
async fn explicit_copy_survives_reconstruction_in_the_native_relation_graph() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let session_id = fixture_session("session.projector.copy-bitemporal");
    let (first, first_write) = fixture_observation(&session_id, 0, None, false);
    let first_anchor =
        derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
    Box::pin(persist_fixture(&runtime, first, first_write)).await;
    let (second, second_write) = fixture_observation(
        &session_id,
        1,
        Some((AnchorProvenanceRelationV2::CopiedFrom, first_anchor.clone())),
        false,
    );
    Box::pin(persist_fixture(&runtime, second, second_write)).await;
    store
        .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .unwrap();
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    let (progress, batch) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
        .unwrap();
    // Explicit copy topology is supplied by the typed projection record and
    // must remain reconstructable from the canonical anchor lineage.
    assert!(batch.copies().is_empty());
    let copy = LogicalCopyRecordV1 {
        occurrence_id: batch.occurrences()[1].occurrence_id.clone(),
        copied_from_occurrence_id: batch.occurrences()[0].occurrence_id.clone(),
        proof: CopyProofV1::ExplicitAnchorAssertion {
            source_occurrence_id: batch.occurrences()[0].occurrence_id.clone(),
            assertion_anchor_id: first_anchor,
        },
        knowledge_at: batch.occurrences()[1].knowledge_at,
        valid_time: batch.occurrences()[1].valid_time,
    };
    let expected_copy = copy.clone();
    let batch = SessionTemporalProjectionBatchV1::new(
        batch.session_id().clone(),
        batch.generation(),
        batch.watermarks().clone(),
        batch.occurrences().to_vec(),
        vec![copy],
        batch.assertions().to_vec(),
    )
    .unwrap()
    .with_checkpoint(
        batch.batch_ordinal(),
        batch.source_through(),
        batch.projection_through(),
    )
    .unwrap();
    let mut coverage = *progress.coverage();
    coverage.visible += 1;
    let source_coverage = progress.source_coverage().cloned();
    let mut progress = SessionRefreshProgressV1::new(
        progress.operation_id().clone(),
        progress.session_id().clone(),
        progress.frontier(),
        coverage,
        progress.committed_batches(),
        progress.committed_records() + 1,
        progress.updated_at(),
    );
    if let Some(source_coverage) = source_coverage {
        progress = progress.with_source_coverage(source_coverage);
    }
    assert_eq!(batch.item_count(), 3);

    store
        .persist_session_refresh_projection_batch(progress, batch.clone())
        .await
        .unwrap();
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .unwrap();
    let snapshot = database.read_snapshot().await.unwrap();
    let (scope, relation_store) =
        SessionTemporalRegisteredDb::session_relation_store(database).unwrap();
    let projection = super::super::relation_projection::reconstruct_session_relation_projection(
        &snapshot,
        &scope,
        &session_id,
        batch.generation(),
        100,
        100,
        Arc::new(NeverCancelled),
    )
    .await
    .unwrap();
    relation_store.replace(&projection).unwrap();
    let loaded = relation_store
        .load_projection(
            &scope,
            &session_id,
            batch.generation().value(),
            100,
            100,
            Arc::new(NeverCancelled),
        )
        .unwrap();
    assert_eq!(
        loaded.logical_copies,
        vec![crate::relations::LogicalCopyRelation {
            occurrence_id: expected_copy.occurrence_id,
            copied_from_occurrence_id: expected_copy.copied_from_occurrence_id,
            proof: expected_copy.proof,
            knowledge_at: expected_copy.knowledge_at,
            valid_time: expected_copy.valid_time,
        }]
    );
}

#[tokio::test]
async fn multi_batch_refresh_progress_survives_restart_under_guard() {
    const OBSERVATION_COUNT: u64 = 501;

    let tmp = TempDir::new().unwrap();
    let session_id = fixture_session("session.projector.multi-batch-guard");
    let operation_id;
    {
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
            .await
            .unwrap();
        let store = temporal_store(&runtime);
        let (first, first_write) = fixture_observation(&session_id, 0, None, false);
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        Box::pin(persist_fixture(&runtime, first, first_write)).await;
        for ordinal in 1..OBSERVATION_COUNT {
            let (observation, write) = fixture_observation(
                &session_id,
                ordinal,
                Some((AnchorProvenanceRelationV2::Supersedes, first_anchor.clone())),
                false,
            );
            Box::pin(persist_fixture(&runtime, observation, write)).await;
        }
        let begin = store
            .begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(OBSERVATION_COUNT, 0).unwrap(),
            ))
            .await
            .unwrap();
        operation_id = begin.operation_id().clone();
        let recovery = store
            .session_refresh_recovery(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = store
            .materialize_session_temporal_refresh_batch_for_test(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert!(batch.item_count() > 0);
        assert!(progress.frontier().committed_through() > 0);
        assert!(progress.frontier().committed_through() < OBSERVATION_COUNT);
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }

    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let store = temporal_store(&runtime);
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    match recovery.restart_state() {
        SessionRefreshRestartStateV1::ResumeProjection { .. }
        | SessionRefreshRestartStateV1::ReadyToComplete => {}
        state @ SessionRefreshRestartStateV1::BeginProjection => {
            panic!("unexpected restart state after first batch: {state:?}")
        }
    }
    if let Some((progress, batch)) = store
        .materialize_session_temporal_refresh_batch_for_test(&recovery)
        .await
        .unwrap()
    {
        store
            .persist_session_refresh_projection_batch(progress, batch)
            .await
            .unwrap();
    }
    let recovery = store
        .session_refresh_recovery(&session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovery.restart_state(),
        SessionRefreshRestartStateV1::ReadyToComplete
    );
    let progress = recovery.progress().unwrap();
    let receipt = store
        .complete_session_refresh(
            SessionRefreshCompletionRequestV1::new(
                operation_id,
                session_id,
                progress.frontier(),
                *progress.coverage(),
            )
            .unwrap(),
            ExecutionControl::default(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(
        runtime
            .session_temporal_fixture_count_for_test(
                HostAdmissionScope::Profile,
                SessionTemporalFixtureCountV1::RefreshProgress,
            )
            .await
            .unwrap(),
        progress.committed_batches() as i64
    );
}

/// Seeds the receipt and observation rows that
/// `session_temporal_observation_effects` requires: its insert guard aborts
/// unless `(observation_id, observation_sequence, receipt_id)` already names a
/// committed observation.
async fn seed_effect_observation(
    conn: &impl crate::handle::SessionTemporalExec,
    observation: &DurableObservationV1,
) -> u64 {
    let receipt = observation.receipt();
    conn.execute(
        "INSERT INTO sanitization_receipts
         (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            receipt.receipt().receipt_id().as_str(),
            receipt.receipt().sanitizer_version().as_str(),
            observation.payload_reference().digest().as_str(),
            serde_json::to_string(receipt).unwrap(),
        ],
    )
    .await
    .unwrap();
    let cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().ordering_domain(),
        observation.identity().position().end(),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations
         (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            observation.observation_id().as_str(),
            observation.payload_reference().digest().as_str(),
            receipt.receipt().receipt_id().as_str(),
            serde_json::to_string(observation).unwrap(),
            serde_json::to_string(&cursor).unwrap(),
        ],
    )
    .await
    .unwrap();
    let mut rows = conn
        .query(
            "SELECT sequence FROM observations WHERE observation_id = ?1",
            params![observation.observation_id().as_str()],
        )
        .await
        .unwrap();
    let sequence = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    u64::try_from(sequence).unwrap()
}

async fn recorded_effect(
    conn: &TestConnection,
    observation: &DurableObservationV1,
) -> Option<(i64, String, String, i64)> {
    let mut rows = conn
        .query(
            "SELECT observation_sequence, session_id, effect_digest, output_count
             FROM session_temporal_observation_effects WHERE observation_id = ?1",
            params![observation.observation_id().as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap()?;
    Some((
        row.get::<i64>(0).unwrap(),
        row.get::<String>(1).unwrap(),
        row.get::<String>(2).unwrap(),
        row.get::<i64>(3).unwrap(),
    ))
}

async fn open_effect_store(name: &str) -> (TempDir, TestConnection) {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join(format!("{name}.db"));
    drop(
        open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap(),
    );
    (directory, TestConnection::open(&database_path))
}

/// Case 1 — fresh insert. The durable tuple is exactly the derived one.
#[tokio::test]
async fn canonical_effect_insert_persists_the_derived_tuple() {
    let (_directory, connection) = open_effect_store("effect-fresh-insert").await;
    let session_id = fixture_session("session.projector.effect-fresh");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    let sequence = seed_effect_observation(&connection, &observation).await;
    let effect = ObservationProjection::Skipped(ProjectionSkipReason::NonConversationalRecord);

    record_canonical_observation_effect(&connection, sequence, &observation, &effect)
        .await
        .unwrap();

    let (recorded_sequence, recorded_session, digest, output_count) =
        recorded_effect(&connection, &observation)
            .await
            .expect("fresh insert records one effect row");
    assert_eq!(recorded_sequence, i64::try_from(sequence).unwrap());
    assert_eq!(recorded_session, session_id.as_str());
    assert_eq!(output_count, 0);
    assert!(digest.starts_with("sha256:"), "{digest}");
}

/// Case 2 — idempotent replay. Re-projecting an observation at or below the
/// checkpoint conflicts on the primary key, and the conflict branch's
/// field-by-field comparison must converge instead of erroring.
#[tokio::test]
async fn canonical_effect_replay_converges_on_an_identical_row() {
    let (_directory, connection) = open_effect_store("effect-identical-replay").await;
    let session_id = fixture_session("session.projector.effect-replay");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    let sequence = seed_effect_observation(&connection, &observation).await;
    let effect = ObservationProjection::Skipped(ProjectionSkipReason::NonConversationalRecord);

    record_canonical_observation_effect(&connection, sequence, &observation, &effect)
        .await
        .unwrap();
    let first = recorded_effect(&connection, &observation).await.unwrap();

    record_canonical_observation_effect(&connection, sequence, &observation, &effect)
        .await
        .unwrap();

    assert_eq!(
        recorded_effect(&connection, &observation).await,
        Some(first)
    );
}

/// Case 3 — conflict with a divergent payload. The durable row satisfies the
/// insert guard (same observation, sequence, and receipt) yet disagrees on the
/// projected effect, so the conflict-only read-back must still reject it.
#[tokio::test]
async fn canonical_effect_replay_rejects_a_divergent_durable_row() {
    let (_directory, connection) = open_effect_store("effect-divergent-row").await;
    let session_id = fixture_session("session.projector.effect-divergent");
    let (observation, _) = fixture_observation(&session_id, 0, None, false);
    let sequence = seed_effect_observation(&connection, &observation).await;
    connection
        .execute(
            "INSERT INTO session_temporal_observation_effects (
                observation_id, observation_sequence, session_id, receipt_id,
                effect_digest, output_count, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
            params![
                observation.observation_id().as_str(),
                i64::try_from(sequence).unwrap(),
                session_id.as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                7_i64,
            ],
        )
        .await
        .unwrap();
    let effect = ObservationProjection::Skipped(ProjectionSkipReason::NonConversationalRecord);

    let error = record_canonical_observation_effect(&connection, sequence, &observation, &effect)
        .await
        .expect_err("a divergent durable effect must not be accepted as a replay");

    assert!(
        matches!(error, ProjectionStoreError::ProvenanceCollision),
        "{error:?}"
    );
    assert_eq!(
        recorded_effect(&connection, &observation).await,
        Some((
            i64::try_from(sequence).unwrap(),
            session_id.as_str().to_owned(),
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            7,
        ))
    );
}

const HISTORY_ONLY_EFFECTS: u64 = 24;

const HEAD_GROUPING_SET_SQL: &str = "
    SELECT COUNT(*)
    FROM session_temporal_observation_effects AS effect
    LEFT JOIN session_refresh_operations AS running
      ON running.session_id = effect.session_id
     AND running.state = 'running'
    WHERE running.session_id IS NULL
";

const FILTERED_DISCOVERY_SQL: &str = "
    WITH active AS (
        SELECT session_id, frozen_watermarks_json
        FROM session_temporal_generations
        WHERE state = 'active'
    )
    SELECT COUNT(*)
    FROM session_temporal_observation_effects AS effect
    LEFT JOIN active ON active.session_id = effect.session_id
    LEFT JOIN session_refresh_operations AS running
      ON running.session_id = effect.session_id
     AND running.state = 'running'
    WHERE running.session_id IS NULL
      AND effect.output_count > 0
      AND effect.observation_sequence > COALESCE(
            CAST(json_extract(
                active.frozen_watermarks_json,
                '$.projection_frontier'
            ) AS INTEGER),
            0
      )
";

async fn count_effects(runtime: &HostAdmissionTestRuntimeV1, sql: &str) -> u64 {
    let snapshot = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database")
        .read_snapshot()
        .await
        .expect("effect-count snapshot");
    let mut rows = snapshot.query(sql, ()).await.expect("effect-count query");
    let value: i64 = rows
        .next()
        .await
        .expect("effect-count row")
        .expect("effect-count missing row")
        .get(0)
        .expect("effect-count column");
    u64::try_from(value).expect("effect-count fits u64")
}

async fn seed_output_session_with_history_only_effects(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &SessionId,
    history_only: u64,
) {
    let (observation, write) = fixture_observation(session_id, 0, None, false);
    Box::pin(persist_fixture(runtime, observation, write)).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database");
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("history-only effect transaction");
    for ordinal in 1..=history_only {
        let (observation, _) = fixture_observation(session_id, ordinal, None, false);
        let sequence = seed_effect_observation(&transaction, &observation).await;
        record_canonical_observation_effect(
            &transaction,
            sequence,
            &observation,
            &ObservationProjection::Skipped(ProjectionSkipReason::NonConversationalRecord),
        )
        .await
        .expect("history-only effect");
    }
    transaction
        .commit()
        .await
        .expect("commit history-only effects");
}

/// HEAD grouped every historical effect for a pending session. Discovery must
/// visit only output-producing rows past the frontier, not the history-only
/// prefix on the same session.
#[tokio::test]
async fn explicit_discovery_visits_only_output_effects_past_frontier() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let session_id = fixture_session("session.projector.filtered-discovery");
    Box::pin(seed_output_session_with_history_only_effects(
        &runtime,
        &session_id,
        HISTORY_ONLY_EFFECTS,
    ))
    .await;
    let grouping_set = count_effects(&runtime, HEAD_GROUPING_SET_SQL).await;
    let filtered = count_effects(&runtime, FILTERED_DISCOVERY_SQL).await;
    assert!(
        grouping_set >= filtered.saturating_add(HISTORY_ONLY_EFFECTS),
        "HEAD grouping set {grouping_set} must include the {HISTORY_ONLY_EFFECTS} history-only rows plus filtered {filtered}"
    );
    assert_eq!(filtered, 1, "one output-producing effect is pending");

    let pending = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database")
        .pending_session_temporal_refresh_page_result(128, 1, None)
        .await
        .unwrap()
        .into_parts()
        .0;

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].session_id(), &session_id);
}

#[tokio::test]
async fn explicit_discovery_rediscovery_is_bounded_and_non_mutating() {
    let tmp = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path())
        .await
        .unwrap();
    let healthy_session_id = fixture_session("session.projector.missing-native-relation.0-healthy");
    let session_id = fixture_session("session.projector.missing-native-relation.a");
    let second_session_id = fixture_session("session.projector.missing-native-relation.b");
    for (session, ordinal) in [
        (&healthy_session_id, 5_000),
        (&session_id, 10_000),
        (&second_session_id, 20_000),
    ] {
        let (observation, write) = fixture_observation(session, ordinal, None, false);
        Box::pin(persist_fixture(&runtime, observation, write)).await;
        temporal_store(&runtime)
            .materialize_pending_session_refresh_for_test(session)
            .await
            .expect("seed active temporal generation");
    }

    let db = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("profile registered database");
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("missing relation receipt fixture transaction");
    assert_eq!(
        transaction
            .execute(
                "DELETE FROM session_relation_receipts WHERE session_id IN (?1, ?2)",
                params![session_id.as_str(), second_session_id.as_str()],
            )
            .await
            .expect("remove relation receipt"),
        2
    );
    transaction
        .commit()
        .await
        .expect("commit missing relation receipt fixture");
    let pending_session_id = fixture_session("session.projector.pending-during-relation-repair");
    let (pending_observation, pending_write) =
        fixture_observation(&pending_session_id, 30_000, None, false);
    Box::pin(persist_fixture(
        &runtime,
        pending_observation,
        pending_write,
    ))
    .await;

    let mut cursor = None;
    let mut discovered = Vec::new();
    let mut pending_pages = 0usize;
    let mut active_rows_scanned = 0usize;
    let mut pages = 0usize;
    loop {
        let page = db
            .pending_session_temporal_refresh_page_result(2, 1, cursor.as_ref())
            .await
            .expect("discover missing native relation projection");
        active_rows_scanned = active_rows_scanned.saturating_add(page.active_rows_scanned());
        let (requests, scanned_through, has_more) = page.into_parts();
        pages = pages.saturating_add(1);
        for request in requests {
            if request.session_id() == &pending_session_id {
                pending_pages = pending_pages.saturating_add(1);
                continue;
            }
            assert!(
                request.target_frontier().is_complete(),
                "relation repair rebuilds the committed generation without fabricating source work"
            );
            discovered.push(request.session_id().clone());
        }
        if !has_more {
            break;
        }
        cursor = scanned_through;
    }

    assert_eq!(
        discovered,
        [session_id.clone(), second_session_id.clone()],
        "the healthy prefix is skipped once while both missing receipts are discovered in order"
    );
    assert_eq!(
        pages, 4,
        "three bounded pages visit the rows and one empty page proves the wrapped end"
    );
    assert_eq!(
        active_rows_scanned, 3,
        "cursor paging must visit each active row exactly once per sweep"
    );
    assert_eq!(
        pending_pages, 4,
        "a full pending-effect lane must not consume the reserved active scan slot"
    );
    let snapshot = db.read_snapshot().await.expect("relation receipt snapshot");
    let mut rows = snapshot
        .query(
            "SELECT
                 (SELECT COUNT(*) FROM session_temporal_generations
                  WHERE session_id IN (?1, ?2, ?3) AND state = 'active'),
                 (SELECT COUNT(*) FROM session_relation_receipts
                  WHERE session_id IN (?1, ?2, ?3))",
            params![
                healthy_session_id.as_str(),
                session_id.as_str(),
                second_session_id.as_str()
            ],
        )
        .await
        .expect("query rediscovery state");
    let row = rows
        .next()
        .await
        .expect("read rediscovery state")
        .expect("rediscovery state row");
    assert_eq!(row.get::<i64>(0).expect("active generation count"), 3);
    assert_eq!(
        row.get::<i64>(1).expect("relation receipt count"),
        1,
        "read-only discovery must not fabricate an applied relation receipt"
    );

    let mut plan_rows = snapshot
        .query(
            "EXPLAIN QUERY PLAN
             SELECT active.session_id
             FROM session_temporal_generations AS active
             WHERE active.state = 'active'
               AND active.session_id > ?1
             ORDER BY active.session_id
             LIMIT ?2",
            params!["", 2_i64],
        )
        .await
        .expect("plan bounded missing-relation discovery");
    let mut plan = Vec::new();
    while let Some(row) = plan_rows.next().await.expect("missing-relation plan row") {
        plan.push(
            row.get::<String>(3)
                .expect("missing-relation plan detail")
                .to_ascii_uppercase(),
        );
    }
    assert!(
        plan.iter()
            .any(|detail| detail.contains("IDX_SESSION_TEMPORAL_GENERATIONS_ONE_ACTIVE")),
        "repair discovery must walk active sessions in indexed key order: {plan:?}"
    );
    assert!(
        plan.iter()
            .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
        "repair discovery must stop at the arm-local limit without sorting all active sessions: \
         {plan:?}"
    );
}

use super::*;
use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    ComponentVersion, ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId,
};
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};
use tracedecay_store::{AnchoredObservationWrite, ObservationStore, ObservationWrite};

fn objects_of<'a>(
    receipts: impl IntoIterator<Item = &'a tracedecay_store::ObservationCommitReceipt>,
) -> BTreeSet<SourceNativeObjectIdV1> {
    receipts
        .into_iter()
        .map(|receipt| {
            host_source_object(receipt.observation())
                .unwrap()
                .native_object()
                .clone()
        })
        .collect()
}

struct Fixture {
    retained: RuntimeExternalSourceStore,
    observations: tracedecay_global_db::GlobalDbObservationStore,
    _database: Database,
    _authority: DatabaseAuthority,
    _directory: TempDir,
}

impl Fixture {
    async fn open() -> Self {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("sessions.db");
        Self::open_at(path, TestDatabaseRuntimeMode::Initialize, directory).await
    }

    async fn open_at(
        path: std::path::PathBuf,
        mode: TestDatabaseRuntimeMode,
        directory: TempDir,
    ) -> Self {
        tracedecay_global_db::register_registered_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "cline retained source cutover").unwrap();
        let (database, _) = Database::publish_registered_test_runtime(
            &path,
            &authority,
            mode,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();
        Self {
            retained: RuntimeExternalSourceStore::new(database.runtime_client()),
            observations: tracedecay_global_db::GlobalDbObservationStore::new(database.clone()),
            _database: database,
            _authority: authority,
            _directory: directory,
        }
    }

    async fn persist(
        &self,
        observation: DurableObservationV1,
    ) -> tracedecay_store::ObservationCommitReceipt {
        let cursor = self
            .observations
            .get_source_cursor(observation.source(), observation.scope())
            .await
            .unwrap();
        let next = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            observation.identity().generation(),
            observation.identity().ordering_domain(),
            observation.identity().position().end(),
        )
        .unwrap();
        let generation = ProjectionGenerationId::new("projection.cline-retained-test").unwrap();
        let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
            &observation,
            "cline-retained-test",
        )
        .unwrap();
        let anchor = tracedecay_store::build_observation_retrieval_anchor(
            &observation,
            generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        let write = AnchoredObservationWrite::new(
            ObservationWrite::new(observation, cursor, next).unwrap(),
            anchor,
            generation,
        )
        .unwrap();
        self.observations
            .persist_observation(write)
            .await
            .unwrap()
            .receipt()
            .clone()
    }
}

fn observation(
    stream: ClineTranscriptStream,
    split: bool,
    order: u64,
    native: &str,
    changed: bool,
) -> DurableObservationV1 {
    let provider = ProviderId::new("cline").unwrap();
    let session = SessionId::new("retained-task").unwrap();
    let source = if split {
        stream.source_identity(provider, session).unwrap()
    } else {
        ObservationSourceIdentityV1::for_provider(provider, session).unwrap()
    };
    let range = ObservationSourceRangeV1::new(order, order + 1).unwrap();
    let native_payload = match stream {
        ClineTranscriptStream::ApiHistory => json!({
            "role": "assistant", "text": if changed { "changed fact" } else { "retained API response" },
            "ordinal": order,
        }),
        ClineTranscriptStream::UiMessages => json!({
            "kind": "usage", "ordinal": order,
            "usage": { "input_tokens": if changed { 9 } else { 7 } },
        }),
    };
    let envelope = tracedecay_sessions::runtime::snapshot_observation::canonical_snapshot_envelope(
        &native_payload,
        "cline",
        "retained-task",
        native,
        range,
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(reference.digest().as_str()).unwrap(),
            ComponentVersion::new("sanitizer.cline-retained-test").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(reference),
    )
    .unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(if split { 2 } else { 1 }).unwrap(),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            ObservationId::new(native).unwrap(),
        )
        .unwrap(),
        receipt,
        RetentionClass::new("transcript.cline.v1").unwrap(),
        payload,
    )
    .unwrap()
}

#[tokio::test]
async fn fresh_cline_stream_appends_share_one_task_without_object_duplicates() {
    let fixture = Fixture::open().await;
    let mut binding = None;
    let mut receipts = Vec::new();
    for (stream, ordinal, native) in [
        (ClineTranscriptStream::UiMessages, 0, "ui-1"),
        (ClineTranscriptStream::ApiHistory, 0, "api-1"),
        (ClineTranscriptStream::ApiHistory, 1, "api-2"),
        (ClineTranscriptStream::UiMessages, 1, "ui-2"),
    ] {
        let receipt = fixture
            .persist(observation(stream, true, ordinal, native, false))
            .await;
        let current_binding = host_source_authority(&receipt, fixture.retained.runtime.binding())
            .unwrap()
            .2;
        if let Some(binding) = &binding {
            assert_eq!(binding, &current_binding);
        }
        binding = Some(current_binding);
        fixture
            .retained
            .capture_host_observation(&receipt)
            .await
            .unwrap();
        fixture
            .retained
            .capture_host_observation(&receipt)
            .await
            .unwrap();
        receipts.push(receipt);
    }
    let binding = binding.unwrap();
    let objects = objects_of(&receipts);
    assert_eq!(objects.len(), 4);
    let pending = fixture
        .retained
        .read_state(binding.clone(), objects.clone())
        .await
        .unwrap()
        .unwrap();
    for object in &objects {
        assert!(pending.observed_object(object).unwrap().is_some());
    }
    assert!(pending.projected_objects().is_empty());
    // Capture persists a pending projection. The daemon's admission drain
    // owns replay; a direct store fixture must run that same bounded step.
    let replay = fixture
        .retained
        .drain_host_projection_replay(
            4,
            &tracedecay_sessions::observation::ObservationCancellation::default(),
        )
        .await
        .unwrap();
    assert!(replay.projected > 0);
    assert!(!replay.deferred);
    let state = fixture
        .retained
        .read_state(binding, objects.clone())
        .await
        .unwrap()
        .unwrap();
    for object in &objects {
        let observed = state.observed_object(object).unwrap();
        assert!(observed.is_some());
        assert_eq!(state.projected_objects().get(object), observed);
    }
    assert_eq!(state.projected_objects().len(), 4);
    assert_eq!(state.source_frontier().partitions().len(), 1);
}

#[tokio::test]
async fn a_drainer_whose_head_another_drainer_projected_past_continues() {
    let fixture = Fixture::open().await;
    for (stream, ordinal, native) in [
        (ClineTranscriptStream::UiMessages, 0, "ui-1"),
        (ClineTranscriptStream::ApiHistory, 0, "api-1"),
    ] {
        let receipt = fixture
            .persist(observation(stream, true, ordinal, native, false))
            .await;
        fixture
            .retained
            .capture_host_observation(&receipt)
            .await
            .unwrap();
    }
    let loser_head = fixture
        .retained
        .read_pending_projection(None)
        .await
        .unwrap()
        .expect("the loser read the queue head");
    let winner = fixture
        .retained
        .drain_host_projection_replay(
            4,
            &tracedecay_sessions::observation::ObservationCancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!((winner.projected, winner.deferred), (2, false));

    let projected = fixture
        .retained
        .project_pending_head(&loser_head, host_external_source_projector().unwrap())
        .await
        .expect("a head another drainer already projected is not a failure");
    assert!(!projected, "the loser did not project the head itself");
}

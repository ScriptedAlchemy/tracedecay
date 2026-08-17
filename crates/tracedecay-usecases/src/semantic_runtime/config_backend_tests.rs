use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotId};
use tracedecay_domain::{
    ChunkerRevision, ComponentRevision, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, PrivacyDomainId,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, VectorGenerationIdV1};

use crate::semantic_runtime::SemanticCurrentLinkedActivationV1;
use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;

use crate::config::retrieval::{SemanticCompatibilityPinsV1, SemanticResourceRequirementV1};

fn pin(label: &str, digest_byte: char) -> SemanticConfigurationPinV1 {
    SemanticConfigurationPinV1 {
        revision_id: ConfigurationRevisionId::try_from(format!("configuration.{label}"))
            .expect("revision"),
        snapshot_id: ConfigurationSnapshotId::try_from(format!("snapshot.{label}"))
            .expect("snapshot"),
        effective_behavior_digest: ManifestDigest::new(format!(
            "sha256:{}",
            digest_byte.to_string().repeat(64)
        ))
        .expect("digest"),
    }
}

fn generation(digest_byte: char) -> VectorGenerationIdV1 {
    VectorGenerationIdV1::new(
        ManifestDigest::new(format!("sha256:{}", digest_byte.to_string().repeat(64)))
            .expect("digest"),
    )
}

fn revision(label: &str) -> ConfigurationRevisionId {
    ConfigurationRevisionId::try_from(format!("configuration.{label}")).expect("revision")
}

fn typed_id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("typed fixture identity")
}

fn resources() -> SemanticResourceRequirementV1 {
    SemanticResourceRequirementV1 {
        model_bytes: 10,
        tokenizer_bytes: 5,
        resident_bytes: 20,
        threads: 2,
        max_concurrent_sessions: 1,
        batch_size: 4,
        sequence_length: 128,
        load_deadline_ms: 1_000,
    }
}

fn compatibility(digest_byte: char) -> SemanticCompatibilityPinsV1 {
    let artifact = ManifestDigest::new(format!("sha256:{}", digest_byte.to_string().repeat(64)))
        .expect("artifact digest");
    let projection = EmbeddingProjectionKeyV1 {
        model_artifact_digest: artifact.clone(),
        tokenizer_digest: ManifestDigest::new(format!("sha256:{}", "2".repeat(64)))
            .expect("tokenizer digest"),
        config_digest: ManifestDigest::new(format!("sha256:{}", "3".repeat(64)))
            .expect("config digest"),
        query_instruction_digest: None,
        document_instruction_digest: None,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 128,
        inference_batch_size: 8,
        inference_batch_bytes: 4 * 1024,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "runtime.rollback-test.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 4,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: typed_id::<ChunkerRevision>("chunker.rollback-test.v1"),
        privacy_domain: typed_id::<PrivacyDomainId>("privacy.rollback-test.v1"),
        privacy_key_epoch: 1,
    }
    .admit()
    .expect("admitted projection");
    let vector_generation_id = generation(digest_byte);
    SemanticCompatibilityPinsV1 {
        implementation_revision: ComponentRevision::new("semantic.rollback-test.v1")
            .expect("implementation revision"),
        fusion_revision: ComponentRevision::new("fusion.rollback-test.v1")
            .expect("fusion revision"),
        artifact_manifest_digest: artifact,
        runtime_compatibility_digest: ManifestDigest::new(format!("sha256:{}", "4".repeat(64)))
            .expect("runtime digest"),
        search_index_key: tracedecay_domain::SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .expect("search index key"),
        calibration: SemanticCalibrationProfileV1 {
            calibration_profile_id: typed_id("calibration.rollback-test.v1"),
            cohort_digest: ManifestDigest::new(format!("sha256:{}", "5".repeat(64)))
                .expect("cohort digest"),
            projection_key: projection.projection_key().clone(),
            vector_generation: vector_generation_id.clone(),
            capability_manifest_digest: ManifestDigest::new(format!("sha256:{}", "6".repeat(64)))
                .expect("capability digest"),
            maximum_distance_micros: 2_000_000,
            minimum_margin_micros: 0,
        },
        projection,
        vector_generation_id,
        resources: resources(),
    }
}

struct RecordingInspector {
    calls: Arc<AtomicUsize>,
    cold_offline_ready: bool,
}

impl SemanticRuntimeGenerationInspectorV1 for RecordingInspector {
    fn inspect_generation<'a>(
        &'a self,
        required: &'a SemanticCompatibilityPinsV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<
            crate::semantic_runtime::SemanticExecutableGenerationLeaseV1,
            SemanticRuntimeBackendErrorV1,
        >,
    > {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let cold_offline_ready = self.cold_offline_ready;
        Box::pin(async move {
            let evidence = crate::semantic_runtime::SemanticExecutableGenerationV1::new(
                required.clone(),
                resources(),
                cold_offline_ready,
                cold_offline_ready,
            )
            .expect("valid executable evidence");
            Ok(crate::semantic_runtime::SemanticExecutableGenerationLeaseV1::new(evidence, ()))
        })
    }
}

struct ObservationReadFailureConfiguration {
    current: SemanticCurrentLinkedActivationV1,
    failure: SemanticConfigurationBackendErrorV1,
}

impl SemanticRetrievalConfigurationPortV1 for ObservationReadFailureConfiguration {
    fn current_activation<'a>(
        &'a self,
        _configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<Option<SemanticCurrentLinkedActivationV1>, SemanticConfigurationBackendErrorV1>,
    > {
        let current = self.current.clone();
        Box::pin(async move { Ok(Some(current)) })
    }

    fn prepare_activation<'a>(
        &'a self,
        _command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async { Err(SemanticConfigurationBackendErrorV1::Rejected) })
    }

    fn prepare_rollback<'a>(
        &'a self,
        _command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async { Err(SemanticConfigurationBackendErrorV1::Rejected) })
    }

    fn commit_linked_transition<'a>(
        &'a self,
        _transition: &'a SemanticConfigurationTransitionV1,
        _receipt: Option<&'a SemanticActivationReceiptV1>,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticLinkedTransitionV1, SemanticConfigurationBackendErrorV1>,
    > {
        Box::pin(async { Err(SemanticConfigurationBackendErrorV1::Rejected) })
    }

    fn committed_profile_state<'a>(
        &'a self,
        _linked: &'a SemanticLinkedTransitionV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<CommittedRetrievalProfileStateV1, SemanticConfigurationBackendErrorV1>,
    > {
        let failure = self.failure;
        Box::pin(async move { Err(failure) })
    }
}

struct AcceptingObserver;

impl RetrievalProfileActivationObserverV1 for AcceptingObserver {
    fn activation_committed(
        &self,
        _committed: CommittedRetrievalProfileStateV1,
    ) -> SemanticRuntimeFuture<
        '_,
        Result<(), super::super::RetrievalProfileActivationObserverErrorV1>,
    > {
        Box::pin(async { Ok(()) })
    }
}

fn observed_activation_fixture(
    digest_byte: char,
) -> (
    SemanticConfigurationPinV1,
    SemanticCompatibilityPinsV1,
    SemanticCurrentLinkedActivationV1,
    SemanticLinkedTransitionV1,
) {
    let previous = pin("observation.previous", '7');
    let configuration = pin("observation.current", '8');
    let compatibility = compatibility(digest_byte);
    let command = SemanticActivationCommandV1::new(
        previous.clone(),
        super::super::SemanticActivationRequestV1::new(
            compatibility.vector_generation_id.clone(),
            None,
            None,
        )
        .expect("activation request"),
    )
    .expect("activation command");
    let receipt = SemanticActivationReceiptV1::issue_transition(
        &command,
        configuration.clone(),
        UtcMicros(20),
    )
    .expect("activation receipt");
    let current = SemanticCurrentLinkedActivationV1::new(receipt.clone(), compatibility.clone())
        .expect("current linked activation");
    let linked = SemanticLinkedTransitionV1 {
        epoch: 2,
        transition_digest: pin("observation.transition", '9').effective_behavior_digest,
        activation_receipt_digest: Some(receipt.receipt_digest),
        audit: crate::config::retrieval::RetrievalProfileAuditEventV1 {
            event_id: pin("observation.audit", 'a').effective_behavior_digest,
            actor_id: typed_id("actor.observation-test"),
            operation: RetrievalProfileAuditOperationV1::Activate,
            prior_active_profile_id: typed_id("profile.observation.previous"),
            resulting_active_profile_id: typed_id("profile.observation.current"),
            prior_active_digest: pin("observation.profile.previous", 'b').effective_behavior_digest,
            resulting_active_digest: pin("observation.profile.current", 'c')
                .effective_behavior_digest,
            evaluation_anchor: typed_id("anchor.observation-test"),
            freshness_vector_digest: pin("observation.freshness", 'd').effective_behavior_digest,
            base_revision: previous.revision_id,
            result_revision: configuration.revision_id.clone(),
            occurred_at: UtcMicros(20),
        },
    };
    (configuration, compatibility, current, linked)
}

#[tokio::test]
async fn exact_observation_read_failure_keeps_later_status_degraded() {
    for failure in [
        SemanticConfigurationBackendErrorV1::Unavailable,
        SemanticConfigurationBackendErrorV1::Rejected,
    ] {
        let (configuration, compatibility, current, linked) = observed_activation_fixture('e');
        let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new_with_activation_observer(
            ObservationReadFailureConfiguration { current, failure },
            RecordingInspector {
                calls: Arc::new(AtomicUsize::new(0)),
                cold_offline_ready: true,
            },
            Arc::new(AcceptingObserver),
        );

        let (ticket, observed) = backend
            .observe_committed_activation(&linked)
            .await
            .expect("committed transition reserves an observation");
        assert!(!observed);
        backend.record_observation(
            ticket,
            configuration.clone(),
            Some(compatibility.vector_generation_id.clone()),
            observed,
        );

        assert!(matches!(
            backend
                .status(&configuration)
                .await
                .expect("typed degraded status"),
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(ref generation),
                reason: SemanticFallbackReasonV1::RuntimeFailure,
            } if generation == &compatibility.vector_generation_id
        ));
    }
}

#[test]
fn delayed_older_failure_cannot_replace_newer_failure() {
    let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new((), ());
    let older_result = revision("older");
    let newer_result = revision("newer");
    let older = backend
        .reserve_observation(
            1,
            &older_result,
            &pin("older-transition", '1').effective_behavior_digest,
        )
        .expect("older reservation");
    let newer = backend
        .reserve_observation(
            2,
            &newer_result,
            &pin("newer-transition", '2').effective_behavior_digest,
        )
        .expect("newer reservation");
    let newer_pin = pin("newer", 'b');
    let newer_generation = generation('c');

    backend.record_observation(
        newer,
        newer_pin.clone(),
        Some(newer_generation.clone()),
        false,
    );
    backend.record_observation(older, pin("older", 'a'), Some(generation('a')), false);

    let state = backend
        .observation_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let failure = state.failure.as_ref().expect("newer failure retained");
    assert_eq!(failure.configuration, newer_pin);
    assert_eq!(failure.generation, Some(newer_generation));
}

#[test]
fn delayed_older_failure_cannot_replace_newer_success() {
    let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new((), ());
    let older_result = revision("success-older");
    let newer_result = revision("success-newer");
    let older = backend
        .reserve_observation(
            1,
            &older_result,
            &pin("success-older-transition", '3').effective_behavior_digest,
        )
        .expect("older reservation");
    let newer = backend
        .reserve_observation(
            2,
            &newer_result,
            &pin("success-newer-transition", '4').effective_behavior_digest,
        )
        .expect("newer reservation");
    let newer_pin = pin("success-newer", 'd');

    backend.record_observation(newer, newer_pin, Some(generation('d')), true);
    backend.record_observation(
        older,
        pin("older-failure", 'e'),
        Some(generation('e')),
        false,
    );

    assert!(
        backend
            .observation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .is_none()
    );
}

#[test]
fn exact_reconciliation_success_clears_only_the_matching_observation_failure() {
    let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new((), ());
    let result_revision = revision("reconciliation");
    let transition_digest = pin("reconciliation-transition", '6').effective_behavior_digest;
    let configuration = pin("reconciliation", '7');
    let generation = generation('8');
    let failed = backend
        .reserve_observation(4, &result_revision, &transition_digest)
        .expect("initial observation");
    backend.record_observation(
        failed,
        configuration.clone(),
        Some(generation.clone()),
        false,
    );

    let recovered = backend
        .reserve_observation(4, &result_revision, &transition_digest)
        .expect("exact durable transition may reconcile");
    backend.record_observation(recovered, configuration, Some(generation), true);

    assert!(
        backend
            .observation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .is_none(),
        "only the exact current reconciliation ticket clears degradation"
    );
}

#[test]
fn observation_sequence_overflow_advances_epoch_without_skipping_observer() {
    let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new((), ());
    {
        let mut state = backend
            .observation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.next_ticket = ActivationObservationTicketV1 {
            epoch: 7,
            sequence: u64::MAX,
        };
    }

    let ticket = backend
        .reserve_observation(
            8,
            &revision("overflow-result"),
            &pin("overflow-transition", '7').effective_behavior_digest,
        )
        .expect("overflow reservation");

    assert_eq!(
        ticket,
        ActivationObservationTicketV1 {
            epoch: 8,
            sequence: 0,
        }
    );
    assert_eq!(
        backend
            .observation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .current_transition
            .as_ref()
            .map(|transition| transition.ticket),
        Some(ticket)
    );
}

#[test]
fn older_transition_cannot_become_current_when_observation_starts_after_newer_commit() {
    let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new((), ());
    let first_result = revision("reordered-first");
    let older_result = revision("reordered-older");
    let newer_result = revision("reordered-newer");
    backend
        .reserve_observation(
            1,
            &first_result,
            &pin("reordered-first-transition", '7').effective_behavior_digest,
        )
        .expect("first committed transition");
    let newer = backend
        .reserve_observation(
            3,
            &newer_result,
            &pin("reordered-newer-transition", '8').effective_behavior_digest,
        )
        .expect("newer committed transition");

    assert!(
        backend
            .reserve_observation(
                2,
                &older_result,
                &pin("reordered-older-transition", '9').effective_behavior_digest,
            )
            .is_none(),
        "an older durable transition cannot supersede the newer desired result"
    );

    let newer_pin = pin("reordered-newer", 'a');
    backend.record_observation(newer, newer_pin.clone(), Some(generation('a')), false);
    assert_eq!(
        backend
            .observation_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .as_ref()
            .map(|failure| &failure.configuration),
        Some(&newer_pin)
    );
}

#[test]
fn disabled_rollback_still_requires_the_former_active_generation() {
    assert_eq!(
        unique_rollback_requirements(None, Some(&"generation.g1")),
        vec![&"generation.g1"]
    );
}

#[tokio::test]
async fn disabled_rollback_fails_when_former_active_is_not_cold_offline_ready() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new(
        (),
        RecordingInspector {
            calls: Arc::clone(&calls),
            cold_offline_ready: false,
        },
    );
    let former_active = compatibility('a');

    let result = backend
        .verify_rollback_generations(None, Some(&former_active))
        .await;

    assert_eq!(
        result.err(),
        Some(SemanticRuntimeBackendErrorV1::Unavailable)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn identical_active_and_rollback_generation_is_leased_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new(
        (),
        RecordingInspector {
            calls: Arc::clone(&calls),
            cold_offline_ready: true,
        },
    );
    let generation = compatibility('b');

    let leases = backend
        .verify_rollback_generations(Some(&generation), Some(&generation))
        .await
        .expect("exact rollback generation");

    assert_eq!(leases.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

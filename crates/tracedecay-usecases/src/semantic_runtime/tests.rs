use std::collections::BTreeMap;
use std::sync::Mutex;

use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
use tracedecay_domain::{ManifestDigest, UtcMicros, VectorGenerationIdV1};

use super::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticActivationRequestV1,
    SemanticConfigurationPinV1, SemanticConfigurationSnapshotSourceV1, SemanticFallbackReasonV1,
    SemanticRollbackCommandV1, SemanticRollbackReceiptV1, SemanticRollbackRequestV1,
    SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1, SemanticRuntimeControlErrorV1,
    SemanticRuntimeFuture, SemanticRuntimeOwnerV1, SemanticRuntimeRouteV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};
use crate::configuration::{
    ConfigurationCurrentStateV1, ConfigurationError, ConfigurationOperationFuture,
};

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn generation(byte: char) -> VectorGenerationIdV1 {
    VectorGenerationIdV1::new(digest(byte))
}

fn configuration() -> ConfigurationCurrentStateV1 {
    ConfigurationCurrentStateV1 {
        revision_id: ConfigurationRevisionId::try_from("configuration.revision.1".to_owned())
            .unwrap(),
        snapshot: ConfigurationSnapshotV1::new(BTreeMap::default(), BTreeMap::default()).unwrap(),
    }
}

#[derive(Clone)]
struct StaticConfiguration {
    current: Result<ConfigurationCurrentStateV1, ConfigurationError>,
}

impl SemanticConfigurationSnapshotSourceV1 for StaticConfiguration {
    fn current_configuration(
        &self,
    ) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        let current = self.current.clone();
        Box::pin(async move { current })
    }
}

struct ReceiptWithoutPromotionRuntime {
    state: Mutex<SemanticRuntimeStateV1>,
}

impl SemanticRuntimeBackendV1 for ReceiptWithoutPromotionRuntime {
    fn status<'a>(
        &'a self,
        _configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRuntimeStateV1, SemanticRuntimeBackendErrorV1>>
    {
        let state = self.state.lock().unwrap().clone();
        Box::pin(async move { Ok(state) })
    }

    fn activate<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticActivationReceiptV1, SemanticRuntimeBackendErrorV1>>
    {
        let receipt = SemanticActivationReceiptV1::issue(command, UtcMicros(10)).unwrap();
        Box::pin(async move { Ok(receipt) })
    }

    fn rollback<'a>(
        &'a self,
        _command: &'a super::SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<super::SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1>,
    > {
        Box::pin(async { Err(SemanticRuntimeBackendErrorV1::Rejected) })
    }
}

#[test]
fn only_a_current_receipt_routes_to_semantic_search() {
    let configuration = configuration();
    let pin = SemanticConfigurationPinV1::from_current(&configuration).unwrap();
    let request = SemanticActivationRequestV1::new(generation('a'), None, None).unwrap();
    let command = SemanticActivationCommandV1::new(pin.clone(), request).unwrap();
    let receipt = SemanticActivationReceiptV1::issue(&command, UtcMicros(10)).unwrap();

    let current = SemanticRuntimeStatusV1::new(
        Some(pin.clone()),
        SemanticRuntimeStateV1::Current {
            receipt: receipt.clone(),
        },
    );
    assert_eq!(
        current.route(),
        SemanticRuntimeRouteV1::Semantic {
            generation: generation('a'),
            activation_receipt_digest: receipt.receipt_digest,
        }
    );

    for state in [
        SemanticRuntimeStateV1::Unavailable {
            reason: SemanticFallbackReasonV1::RuntimeUnavailable,
        },
        SemanticRuntimeStateV1::Indexing {
            target_generation: generation('b'),
            completed_units: 3,
            total_units: 10,
        },
        SemanticRuntimeStateV1::Degraded {
            active_generation: Some(generation('a')),
            reason: SemanticFallbackReasonV1::RuntimeFailure,
        },
        SemanticRuntimeStateV1::Rollback {
            from_generation: generation('a'),
            target_generation: generation('c'),
        },
        SemanticRuntimeStateV1::SelectedNotDownloaded {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            artifact_digest: "a".repeat(64),
        },
        SemanticRuntimeStateV1::Downloading {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            artifact_digest: "a".repeat(64),
            bytes_received: 1,
            bytes_total: 2,
        },
        SemanticRuntimeStateV1::Verifying {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            artifact_digest: "a".repeat(64),
        },
        SemanticRuntimeStateV1::Installed {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            artifact_digest: "a".repeat(64),
        },
        SemanticRuntimeStateV1::Loading {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            artifact_digest: "a".repeat(64),
        },
        SemanticRuntimeStateV1::Failed {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            artifact_digest: "a".repeat(64),
            detail: "download failed".to_owned(),
            retryable: true,
        },
    ] {
        let status = SemanticRuntimeStatusV1::new(Some(pin.clone()), state);
        assert!(matches!(
            status.route(),
            SemanticRuntimeRouteV1::LexicalFallback { .. }
        ));
    }
}

#[test]
fn incomplete_stale_failed_and_incompatible_generations_are_omitted() {
    let pin = SemanticConfigurationPinV1::from_current(&configuration()).unwrap();
    for reason in [
        SemanticFallbackReasonV1::ArtifactUnavailable,
        SemanticFallbackReasonV1::IncompatibleRuntime,
        SemanticFallbackReasonV1::ResourceCeilingExceeded,
        SemanticFallbackReasonV1::CorruptArtifact,
        SemanticFallbackReasonV1::RuntimeFailure,
    ] {
        let status = SemanticRuntimeStatusV1::new(
            Some(pin.clone()),
            SemanticRuntimeStateV1::Degraded {
                active_generation: Some(generation('a')),
                reason,
            },
        );
        assert_eq!(
            status.route(),
            SemanticRuntimeRouteV1::LexicalFallback { reason }
        );
    }
}

#[test]
fn rollback_receipt_explicitly_restores_the_retained_generation() {
    let pin = SemanticConfigurationPinV1::from_current(&configuration()).unwrap();
    let command = SemanticRollbackCommandV1::new(
        pin.clone(),
        SemanticRollbackRequestV1::new(generation('b'), generation('a'), generation('b')).unwrap(),
    )
    .unwrap();
    let receipt = SemanticRollbackReceiptV1::issue(&command, UtcMicros(20)).unwrap();

    assert_eq!(receipt.from_generation, generation('a'));
    assert_eq!(receipt.target_generation, Some(generation('b')));
    let restored_activation = receipt.restored_activation.unwrap();
    assert_eq!(
        restored_activation.previous_active_generation,
        Some(generation('a'))
    );
    let restored_receipt_digest = restored_activation.receipt_digest.clone();
    assert_eq!(
        SemanticRuntimeStatusV1::new(
            Some(pin),
            SemanticRuntimeStateV1::Current {
                receipt: restored_activation,
            },
        )
        .route(),
        SemanticRuntimeRouteV1::Semantic {
            generation: generation('b'),
            activation_receipt_digest: restored_receipt_digest,
        }
    );
}

#[test]
fn semantic_off_rollback_has_no_fabricated_activation_receipt() {
    let pin = SemanticConfigurationPinV1::from_current(&configuration()).unwrap();
    let command = SemanticRollbackCommandV1::new(
        pin,
        SemanticRollbackRequestV1::disable(generation('a')).unwrap(),
    )
    .unwrap();

    let receipt = SemanticRollbackReceiptV1::issue(&command, UtcMicros(20)).unwrap();

    assert_eq!(receipt.from_generation, generation('a'));
    assert_eq!(receipt.target_generation, None);
    assert_eq!(receipt.restored_activation, None);
    receipt.validate_for(&command).unwrap();
}

#[tokio::test]
async fn activation_receipt_cannot_silently_promote_an_indexing_runtime() {
    let configuration = configuration();
    let runtime = ReceiptWithoutPromotionRuntime {
        state: Mutex::new(SemanticRuntimeStateV1::Indexing {
            target_generation: generation('a'),
            completed_units: 4,
            total_units: 10,
        }),
    };
    let owner = SemanticRuntimeOwnerV1::new(
        StaticConfiguration {
            current: Ok(configuration),
        },
        runtime,
    );

    let error = owner
        .activate(SemanticActivationRequestV1::new(generation('a'), None, None).unwrap())
        .await
        .unwrap_err();

    assert_eq!(error, SemanticRuntimeControlErrorV1::PromotionNotObserved);
    assert!(matches!(
        owner.status().await.route(),
        SemanticRuntimeRouteV1::LexicalFallback { .. }
    ));
}

#[tokio::test]
async fn startup_observes_indexing_without_waiting_for_semantic_activation() {
    let runtime = ReceiptWithoutPromotionRuntime {
        state: Mutex::new(SemanticRuntimeStateV1::Indexing {
            target_generation: generation('b'),
            completed_units: 2,
            total_units: 8,
        }),
    };
    let owner = SemanticRuntimeOwnerV1::new(
        StaticConfiguration {
            current: Ok(configuration()),
        },
        runtime,
    );

    let status = owner.status().await;
    assert!(matches!(
        status.state,
        SemanticRuntimeStateV1::Indexing {
            completed_units: 2,
            total_units: 8,
            ..
        }
    ));
    assert_eq!(
        status.route(),
        SemanticRuntimeRouteV1::LexicalFallback {
            reason: SemanticFallbackReasonV1::Indexing
        }
    );
}

mod config_backend_tests {
    use tracedecay_domain::{
        ChunkerRevision, ComponentRevision, EmbeddingDeviceClassV1, EmbeddingMetricV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, PrivacyDomainId, UtcMicros,
    };

    use super::*;
    use crate::config::retrieval::{SemanticCompatibilityPinsV1, SemanticResourceRequirementV1};
    use crate::semantic_runtime::{
        SemanticCurrentLinkedActivationV1, SemanticExecutableGenerationV1,
    };
    use tracedecay_query::retrieval::semantic::SemanticCalibrationProfileV1;

    fn typed_id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn resources() -> SemanticResourceRequirementV1 {
        SemanticResourceRequirementV1 {
            model_bytes: 10,
            tokenizer_bytes: 5,
            resident_bytes: 20,
            threads: 2,
            batch_size: 4,
            sequence_length: 128,
            load_deadline_ms: 1_000,
        }
    }

    fn projection(artifact: ManifestDigest) -> tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: artifact,
            tokenizer_digest: digest('2'),
            config_digest: digest('3'),
            query_instruction_digest: None,
            document_instruction_digest: None,
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 128,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "runtime.pass.v1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 4,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: typed_id::<ChunkerRevision>("chunker.pass.v1"),
            privacy_domain: typed_id::<PrivacyDomainId>("privacy.pass.v1"),
            privacy_key_epoch: 1,
        }
        .admit()
        .unwrap()
    }

    fn pins(byte: char) -> SemanticCompatibilityPinsV1 {
        let artifact = digest(byte);
        let projection = projection(artifact.clone());
        let vector_generation_id = generation(byte);
        SemanticCompatibilityPinsV1 {
            implementation_revision: ComponentRevision::new("semantic.pass.v1").unwrap(),
            fusion_revision: ComponentRevision::new("fusion.semantic.pass.v1").unwrap(),
            artifact_manifest_digest: artifact.clone(),
            runtime_compatibility_digest: digest('4'),
            search_index_key: tracedecay_domain::SemanticSearchIndexProfileV1::exact_flat_v1()
                .and_then(|profile| profile.index_key())
                .unwrap(),
            calibration: SemanticCalibrationProfileV1 {
                calibration_profile_id: typed_id("calibration.semantic.pass.v1"),
                cohort_digest: digest('5'),
                projection_key: projection.projection_key().clone(),
                vector_generation: vector_generation_id.clone(),
                capability_manifest_digest: digest('6'),
                maximum_distance_micros: 2_000_000,
                minimum_margin_micros: 0,
            },
            projection,
            vector_generation_id,
            resources: resources(),
        }
    }

    #[test]
    fn current_activation_rejects_shifted_calibration_projection() {
        let target = pins('a');
        let configuration =
            SemanticConfigurationPinV1::from_current(&configuration()).expect("configuration pin");
        let request =
            SemanticActivationRequestV1::new(target.vector_generation_id.clone(), None, None)
                .expect("activation request");
        let command = SemanticActivationCommandV1::new(configuration, request).expect("command");
        let receipt = SemanticActivationReceiptV1::issue(&command, UtcMicros(10))
            .expect("activation receipt");
        let mut shifted = target;
        shifted.calibration.projection_key = pins('b').projection.projection_key().clone();

        assert_eq!(
            SemanticCurrentLinkedActivationV1::new(receipt, shifted),
            Err(crate::semantic_runtime::SemanticRuntimeContractErrorV1::InvalidCompatibility)
        );
    }

    #[test]
    fn tampered_generation_evidence_fails_closed() {
        let target = pins('a');
        let mut evidence =
            SemanticExecutableGenerationV1::new(target.clone(), resources(), true, true).unwrap();
        evidence.rollback_executable = false;

        assert_eq!(
            evidence.validate_for(&target, false),
            Err(super::super::SemanticRuntimeContractErrorV1::InvalidCompatibility)
        );
    }
}

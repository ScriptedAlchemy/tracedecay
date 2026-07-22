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
use crate::application::configuration::{
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
        snapshot: ConfigurationSnapshotV1::new(Default::default(), Default::default()).unwrap(),
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
    assert_eq!(receipt.target_generation, generation('b'));
    assert_eq!(
        receipt.restored_activation.previous_active_generation,
        Some(generation('a'))
    );
    let restored_receipt_digest = receipt.restored_activation.receipt_digest.clone();
    assert_eq!(
        SemanticRuntimeStatusV1::new(
            Some(pin),
            SemanticRuntimeStateV1::Current {
                receipt: receipt.restored_activation,
            },
        )
        .route(),
        SemanticRuntimeRouteV1::Semantic {
            generation: generation('b'),
            activation_receipt_digest: restored_receipt_digest,
        }
    );
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

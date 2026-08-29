use tracedecay_domain::VectorGenerationIdV1;
use tracedecay_semantic::{
    SemanticModelLifecycleStateV1, SemanticModelLifecycleStatusV1,
    SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
    SemanticRuntimeStatusProjectionV1,
};

use super::super::ports::{
    SemanticActivationReceiptV1, SemanticConfigurationPinV1, SemanticFallbackReasonV1,
    SemanticRuntimeStateV1, SemanticRuntimeStatusV1,
};

/// Map daemon schedule projection into the application/Doctor status shape.
///
/// Indexing never blocks exact/lexical/graph; the route remains lexical until
/// [`SemanticRuntimeStateV1::Current`]. A scheduler `Current` pointer is not
/// Ready by itself: Ready requires the durable activation receipt remount
/// reattached from the configuration store. A missing or mismatched receipt
/// stays [`SemanticFallbackReasonV1::InvalidRuntimeStatus`] — never a
/// synthesized receipt or empty success.
pub fn application_status_from_projection(
    projection: &SemanticRuntimeStatusProjectionV1,
    configuration: Option<SemanticConfigurationPinV1>,
    activation_receipt: Option<SemanticActivationReceiptV1>,
) -> SemanticRuntimeStatusV1 {
    match &projection.status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticRuntimeStatusV1::new(
            configuration,
            SemanticRuntimeStateV1::Unavailable {
                reason: projection
                    .degraded_reason
                    .unwrap_or(SemanticFallbackReasonV1::RuntimeUnavailable),
            },
        ),
        SemanticRuntimeScheduleStatusV1::Indexing {
            completed_units,
            total_units,
            ..
        } => SemanticRuntimeStatusV1::new(
            configuration,
            SemanticRuntimeStateV1::Indexing {
                completed_units: *completed_units,
                total_units: *total_units,
            },
        ),
        SemanticRuntimeScheduleStatusV1::Failed {
            reason,
            prior_generation,
        } => SemanticRuntimeStatusV1::new(
            configuration,
            SemanticRuntimeStateV1::Degraded {
                active_generation: prior_generation
                    .clone()
                    .or_else(|| projection.prior_generation.clone()),
                reason: match reason {
                    SemanticRuntimeScheduleFailureV1::Artifact => {
                        SemanticFallbackReasonV1::ArtifactUnavailable
                    }
                    SemanticRuntimeScheduleFailureV1::Cancelled
                    | SemanticRuntimeScheduleFailureV1::DeadlineExceeded => {
                        SemanticFallbackReasonV1::RuntimeUnavailable
                    }
                    SemanticRuntimeScheduleFailureV1::Runtime
                    | SemanticRuntimeScheduleFailureV1::Projection
                    | SemanticRuntimeScheduleFailureV1::ProjectionDetail(_)
                    | SemanticRuntimeScheduleFailureV1::Publication
                    | SemanticRuntimeScheduleFailureV1::PublicationDetail(_) => {
                        SemanticFallbackReasonV1::RuntimeFailure
                    }
                },
            },
        ),
        SemanticRuntimeScheduleStatusV1::Current { generation } => {
            ready_or_typed_missing_receipt(generation, configuration, activation_receipt)
        }
    }
}

fn ready_or_typed_missing_receipt(
    generation: &VectorGenerationIdV1,
    configuration: Option<SemanticConfigurationPinV1>,
    activation_receipt: Option<SemanticActivationReceiptV1>,
) -> SemanticRuntimeStatusV1 {
    if let Some(receipt) = activation_receipt
        && receipt.validate().is_ok()
        && receipt.activated_generation == *generation
        && configuration
            .as_ref()
            .is_none_or(|pin| receipt.configuration == *pin)
    {
        return SemanticRuntimeStatusV1::new(
            configuration.or_else(|| Some(receipt.configuration.clone())),
            SemanticRuntimeStateV1::Current { receipt },
        );
    }
    SemanticRuntimeStatusV1::new(
        configuration,
        SemanticRuntimeStateV1::Degraded {
            active_generation: Some(generation.clone()),
            reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
        },
    )
}

/// Map lifecycle state into the Doctor/status semantic runtime state surface.
///
/// This is the sole lifecycle→runtime mapping. Callers must not invent a
/// second translation of acquisition or failure states.
pub fn lifecycle_to_runtime_state(state: &SemanticModelLifecycleStateV1) -> SemanticRuntimeStateV1 {
    match state {
        SemanticModelLifecycleStateV1::SelectedNotDownloaded {
            model_id,
            artifact_digest,
            ..
        } => SemanticRuntimeStateV1::SelectedNotDownloaded {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        SemanticModelLifecycleStateV1::Downloading {
            model_id,
            artifact_digest,
            bytes_received,
            bytes_total,
            ..
        } => SemanticRuntimeStateV1::Downloading {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
            bytes_received: *bytes_received,
            bytes_total: *bytes_total,
        },
        SemanticModelLifecycleStateV1::Verifying {
            model_id,
            artifact_digest,
            ..
        } => SemanticRuntimeStateV1::Verifying {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        // Lifecycle Ready means the package is locally complete. Semantic
        // search influence still requires a Current activation receipt.
        SemanticModelLifecycleStateV1::Installed {
            model_id,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            model_id,
            artifact_digest,
            ..
        } => SemanticRuntimeStateV1::Installed {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        // Vector-generation Indexing requires a configuration pin +
        // generation id. Acquisition-phase indexing is reported as Loading
        // until the semantic owner publishes an activation receipt.
        SemanticModelLifecycleStateV1::Loading {
            model_id,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Indexing {
            model_id,
            artifact_digest,
            ..
        } => SemanticRuntimeStateV1::Loading {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
        },
        SemanticModelLifecycleStateV1::Failed {
            model_id,
            artifact_digest,
            detail,
            retryable,
            ..
        } => SemanticRuntimeStateV1::Failed {
            model_id: model_id.clone(),
            artifact_digest: artifact_digest.clone(),
            detail: detail.clone(),
            retryable: *retryable,
        },
    }
}

/// Compose seated scheduler status with the model-lifecycle owner.
///
/// A generic unavailable fallback may yield to a more specific lifecycle
/// observation. A mounted-but-broken runtime (degraded, artifact, failure)
/// keeps its own error — lifecycle never overwrites that truth.
pub fn resolve_semantic_application_status(
    scheduler: Option<SemanticRuntimeStatusV1>,
    lifecycle: Option<&SemanticModelLifecycleStatusV1>,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    match (scheduler, lifecycle) {
        (Some(scheduler), Some(lifecycle)) => {
            prefer_lifecycle_over_generic_unavailable(scheduler, lifecycle)
        }
        (Some(scheduler), None) => scheduler,
        (None, Some(lifecycle)) => status_from_lifecycle(lifecycle, configuration),
        (None, None) => {
            let reason = if configuration.is_none() {
                SemanticFallbackReasonV1::ConfigurationUnavailable
            } else {
                SemanticFallbackReasonV1::RuntimeUnavailable
            };
            SemanticRuntimeStatusV1::new(
                configuration,
                SemanticRuntimeStateV1::Unavailable { reason },
            )
        }
    }
}

/// Replace only a generic unavailable scheduler view with lifecycle truth.
pub fn prefer_lifecycle_over_generic_unavailable(
    scheduler: SemanticRuntimeStatusV1,
    lifecycle: &SemanticModelLifecycleStatusV1,
) -> SemanticRuntimeStatusV1 {
    if !is_generic_unavailable(&scheduler.state) {
        return scheduler;
    }
    match lifecycle.state.as_ref() {
        Some(state) => {
            SemanticRuntimeStatusV1::new(scheduler.configuration, lifecycle_to_runtime_state(state))
        }
        None if is_deliberate_disablement(lifecycle) => SemanticRuntimeStatusV1::new(
            scheduler.configuration,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::ConfigurationUnavailable,
            },
        ),
        None => scheduler,
    }
}

fn status_from_lifecycle(
    lifecycle: &SemanticModelLifecycleStatusV1,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    match lifecycle.state.as_ref() {
        Some(state) => {
            SemanticRuntimeStatusV1::new(configuration, lifecycle_to_runtime_state(state))
        }
        None => SemanticRuntimeStatusV1::new(
            configuration,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::ConfigurationUnavailable,
            },
        ),
    }
}

fn is_generic_unavailable(state: &SemanticRuntimeStateV1) -> bool {
    matches!(
        state,
        SemanticRuntimeStateV1::Unavailable {
            reason: SemanticFallbackReasonV1::RuntimeUnavailable
                | SemanticFallbackReasonV1::ConfigurationUnavailable
        }
    )
}

fn is_deliberate_disablement(lifecycle: &SemanticModelLifecycleStatusV1) -> bool {
    lifecycle.selected_model.is_none() && lifecycle.state.is_none()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
    use tracedecay_semantic::SemanticModelRemediationV1;

    use super::*;
    use crate::configuration::ConfigurationCurrentStateV1;
    use crate::semantic_runtime::SemanticRuntimeRouteV1;

    fn pin() -> SemanticConfigurationPinV1 {
        SemanticConfigurationPinV1::from_current(&ConfigurationCurrentStateV1 {
            revision_id: ConfigurationRevisionId::try_from("configuration.revision.1".to_owned())
                .unwrap(),
            snapshot: ConfigurationSnapshotV1::new(BTreeMap::default(), BTreeMap::default())
                .unwrap(),
        })
        .unwrap()
    }

    fn digest() -> String {
        "a".repeat(64)
    }

    fn lifecycle_status(
        selected_model: Option<&str>,
        state: Option<SemanticModelLifecycleStateV1>,
    ) -> SemanticModelLifecycleStatusV1 {
        SemanticModelLifecycleStatusV1 {
            selected_model: selected_model.map(str::to_owned),
            auto_download: false,
            catalog_model_ids: Vec::new(),
            state,
            remediation: SemanticModelRemediationV1 {
                retry: false,
                remove: false,
                rollback: false,
            },
            semantics_omitted: true,
        }
    }

    fn generic_unavailable(
        configuration: Option<SemanticConfigurationPinV1>,
    ) -> SemanticRuntimeStatusV1 {
        SemanticRuntimeStatusV1::new(
            configuration,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::RuntimeUnavailable,
            },
        )
    }

    fn downloading() -> SemanticModelLifecycleStateV1 {
        SemanticModelLifecycleStateV1::Downloading {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            revision: "rev".to_owned(),
            artifact_digest: digest(),
            bytes_received: 10,
            bytes_total: 100,
        }
    }

    fn failed() -> SemanticModelLifecycleStateV1 {
        SemanticModelLifecycleStateV1::Failed {
            model_id: "JinaEmbeddingsV2BaseCode".to_owned(),
            revision: "rev".to_owned(),
            artifact_digest: digest(),
            detail: "connection refused to unroutable endpoint".to_owned(),
            retryable: true,
        }
    }

    #[test]
    fn generic_unavailable_yields_to_lifecycle_downloading() {
        let status = prefer_lifecycle_over_generic_unavailable(
            generic_unavailable(Some(pin())),
            &lifecycle_status(Some("JinaEmbeddingsV2BaseCode"), Some(downloading())),
        );

        assert_eq!(status.validate(), Ok(()));
        assert_eq!(
            status.route(),
            SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::Downloading,
            }
        );
        match &status.state {
            SemanticRuntimeStateV1::Downloading {
                model_id,
                bytes_received,
                bytes_total,
                ..
            } => {
                assert_eq!(model_id, "JinaEmbeddingsV2BaseCode");
                assert_eq!(*bytes_received, 10);
                assert_eq!(*bytes_total, 100);
            }
            other => panic!("expected downloading, got {other:?}"),
        }
    }

    #[test]
    fn generic_unavailable_yields_to_lifecycle_failed() {
        let status = prefer_lifecycle_over_generic_unavailable(
            generic_unavailable(Some(pin())),
            &lifecycle_status(Some("JinaEmbeddingsV2BaseCode"), Some(failed())),
        );

        assert_eq!(status.validate(), Ok(()));
        assert_eq!(
            status.route(),
            SemanticRuntimeRouteV1::LexicalFallback {
                reason: SemanticFallbackReasonV1::ModelFailed,
            }
        );
        match &status.state {
            SemanticRuntimeStateV1::Failed {
                detail, retryable, ..
            } => {
                assert_eq!(detail, "connection refused to unroutable endpoint");
                assert!(retryable);
            }
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn mounted_runtime_failure_is_not_replaced_by_lifecycle() {
        let broken = SemanticRuntimeStatusV1::new(
            Some(pin()),
            SemanticRuntimeStateV1::Degraded {
                active_generation: None,
                reason: SemanticFallbackReasonV1::RuntimeFailure,
            },
        );

        let status = prefer_lifecycle_over_generic_unavailable(
            broken.clone(),
            &lifecycle_status(Some("JinaEmbeddingsV2BaseCode"), Some(downloading())),
        );

        assert_eq!(status, broken);
    }

    #[test]
    fn disablement_keeps_the_configuration_pin() {
        let status = resolve_semantic_application_status(
            Some(generic_unavailable(Some(pin()))),
            Some(&lifecycle_status(None, None)),
            Some(pin()),
        );

        assert_eq!(status.validate(), Ok(()));
        assert!(status.configuration.is_some());
        assert!(matches!(
            status.state,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::ConfigurationUnavailable,
            }
        ));
    }

    #[test]
    fn missing_configuration_has_no_pin() {
        let status = resolve_semantic_application_status(None, None, None);

        assert_eq!(status.validate(), Ok(()));
        assert!(status.configuration.is_none());
        assert!(matches!(
            status.state,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::ConfigurationUnavailable,
            }
        ));
    }
}

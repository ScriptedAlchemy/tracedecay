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
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use tracedecay_domain::{
        ActorId, CalibrationProfileId, ChunkerRevision, ComponentRevision, DiversityPolicy,
        DiversityPolicyId, EmbeddingDeviceClassV1, EmbeddingMetricV1, EmbeddingNormalizationV1,
        EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
        EmbeddingTruncationSideV1, FusionProfile, FusionProfileId, PrivacyDomainId,
        RetrievalBudget, RetrieverKind, UtcMicros, canonical_sha256,
    };

    use super::*;
    use crate::application::semantic_runtime::{
        ConfigurationLinkedSemanticRuntimeBackendV1, SemanticConfigurationBackendErrorV1,
        SemanticConfigurationTransitionV1, SemanticCurrentLinkedActivationV1,
        SemanticExecutableGenerationV1, SemanticLinkedTransitionV1,
        SemanticRetrievalConfigurationPortV1, SemanticRuntimeGenerationInspectorV1,
    };
    use crate::config::retrieval::{
        AcceptedRetrievalProfileV1, PassingRetrievalEvaluationV1, RetrievalCompatibilityPinsV1,
        RetrievalProfileAuditEventV1, RetrievalProfileAuditOperationV1, RetrievalProfileCasV1,
        RetrievalRuntimeCompatibilityV1, SemanticCompatibilityPinsV1,
        SemanticResourceRequirementV1,
    };
    use crate::query::retrieval::semantic::SemanticCalibrationProfileV1;
    use crate::search_eval::{
        DirectEvaluationReportV1, DirectEvaluationStatusV1, DirectProfileEvaluationV1,
        DirectQualityMetricsV1, DirectRatioMetricV1, OptionalStageMeasurementV1,
        OptionalStageMeasurementsV1,
    };

    fn typed_id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn passing_report() -> DirectEvaluationReportV1 {
        let empty_ratio = || DirectRatioMetricV1 {
            numerator: 0,
            denominator: 0,
            ppm: 0,
        };
        let row = |partition: &str| DirectProfileEvaluationV1 {
            profile_id: "semantic.pass.v1".to_owned(),
            partition: partition.to_owned(),
            query_count: 0,
            failed_queries: 0,
            fallback_stable: true,
            fallback_matches_expected: true,
            cancellation_bounded: true,
            offline: true,
            resource_status: DirectEvaluationStatusV1::Pass,
            optional_stages: OptionalStageMeasurementsV1 {
                semantic: OptionalStageMeasurementV1::NotRequested,
                rerank: OptionalStageMeasurementV1::NotRequested,
            },
            quality: DirectQualityMetricsV1 {
                relevant_query_count: 0,
                recall_at_10: empty_ratio(),
                precision_at_10: empty_ratio(),
                mean_reciprocal_rank_ppm: 0,
                ndcg_at_10_ppm: 0,
                duplicate_rate: empty_ratio(),
                protected_recall_at_10: empty_ratio(),
                strata: Vec::new(),
                worst_stratum: None,
            },
            status: DirectEvaluationStatusV1::Pass,
            queries: Vec::new(),
        };
        DirectEvaluationReportV1 {
            command: "compare".to_owned(),
            status: DirectEvaluationStatusV1::Pass,
            workload_digest: "workload-pass".to_owned(),
            corpus_digest: "corpus-pass".to_owned(),
            fixture_source_repository_commit: "commit-pass".to_owned(),
            fixture_source_repository_tree: "tree-pass".to_owned(),
            profiles: vec![row("train"), row("validation")],
        }
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
        let configuration = pin("configuration.revision.1", '8');
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
            Err(
                crate::application::semantic_runtime::SemanticRuntimeContractErrorV1::InvalidCompatibility
            )
        );
    }

    fn accepted_profile(semantic: SemanticCompatibilityPinsV1) -> AcceptedRetrievalProfileV1 {
        let evaluation =
            PassingRetrievalEvaluationV1::from_report(&passing_report(), "semantic.pass.v1")
                .unwrap();
        let anchor = evaluation.evaluation_anchor().clone();
        let lanes = [
            RetrieverKind::ExactLiteral,
            RetrieverKind::Lexical,
            RetrieverKind::Graph,
            RetrieverKind::Semantic,
        ];
        let profile = FusionProfile {
            profile_id: typed_id::<FusionProfileId>("profile.semantic.pass.v1"),
            evaluation_result_anchor: anchor.clone(),
            calibrations: lanes
                .into_iter()
                .map(|lane| {
                    (
                        lane,
                        typed_id::<CalibrationProfileId>(&format!(
                            "calibration.{}.pass.v1",
                            lane.as_str()
                        )),
                    )
                })
                .collect(),
            score_domain_calibrations: BTreeMap::new(),
            weights_micros: lanes.into_iter().map(|lane| (lane, 1_000_000)).collect(),
            diversity_policy_id: typed_id::<DiversityPolicyId>("diversity.pass.v1"),
            rerank_policy_id: None,
            retrieval_budget: RetrievalBudget {
                max_candidates_per_lane: 8,
                max_fused_candidates: 8,
                max_hydrated_results: 4,
                max_hydration_bytes: 4_096,
                deadline_micros: None,
            },
        };
        let diversity = DiversityPolicy {
            policy_id: profile.diversity_policy_id.clone(),
            evaluation_result_anchor: Some(anchor),
            per_source_namespace: None,
            per_source_instance: None,
            per_repository: None,
            per_file: None,
            per_session_or_thread: None,
            per_copy_cluster: None,
            per_evidence_role: None,
        };
        AcceptedRetrievalProfileV1::new(
            profile,
            diversity,
            None,
            RetrievalCompatibilityPinsV1 {
                semantic: Some(semantic),
                rerank: None,
            },
            evaluation,
        )
        .unwrap()
    }

    fn pin(revision: &str, byte: char) -> SemanticConfigurationPinV1 {
        SemanticConfigurationPinV1 {
            revision_id: ConfigurationRevisionId::try_from(revision.to_owned()).unwrap(),
            snapshot_id: typed_id(&format!("configuration.snapshot.{revision}")),
            effective_behavior_digest: digest(byte),
        }
    }

    fn runtime_for(semantic: SemanticCompatibilityPinsV1) -> RetrievalRuntimeCompatibilityV1 {
        RetrievalRuntimeCompatibilityV1 {
            retrieval_ceiling: RetrievalBudget {
                max_candidates_per_lane: 8,
                max_fused_candidates: 8,
                max_hydrated_results: 4,
                max_hydration_bytes: 4_096,
                deadline_micros: None,
            },
            semantic: Some(semantic),
            semantic_ceiling: Some(resources()),
            rerank: None,
            rerank_ceiling: None,
        }
    }

    fn transition(
        prior: Option<SemanticCompatibilityPinsV1>,
        target: SemanticCompatibilityPinsV1,
    ) -> SemanticConfigurationTransitionV1 {
        let accepted = accepted_profile(target.clone());
        SemanticConfigurationTransitionV1::activation(
            pin("configuration.revision.1", '8'),
            pin("configuration.revision.2", '9'),
            typed_id("profile.lexical.pass.v1"),
            &accepted,
            &runtime_for(target),
            RetrievalProfileCasV1 {
                expected_configuration_revision: ConfigurationRevisionId::try_from(
                    "configuration.revision.1".to_owned(),
                )
                .unwrap(),
                expected_active_digest: digest('6'),
                expected_rollback_digest: None,
            },
            prior,
            None,
            UtcMicros(20),
        )
        .unwrap()
    }

    fn audit_for(
        transition: &SemanticConfigurationTransitionV1,
        operation: RetrievalProfileAuditOperationV1,
    ) -> RetrievalProfileAuditEventV1 {
        let actor_id = typed_id::<ActorId>("actor.semantic.config");
        let freshness_vector_digest = digest('7');
        let occurred_at = UtcMicros(20);
        let event_id = canonical_sha256(&(
            "tracedecay.retrieval.profile-audit.v1",
            &actor_id,
            &operation,
            &transition.prior_active_profile_id,
            &transition.result_active_profile_id,
            &transition.prior_active_profile_digest,
            &transition.result_active_profile_digest,
            &transition.evaluation_anchor,
            &freshness_vector_digest,
            &transition.base_configuration.revision_id,
            &transition.result_configuration.revision_id,
            occurred_at,
        ))
        .unwrap();
        RetrievalProfileAuditEventV1 {
            event_id,
            actor_id,
            operation,
            prior_active_profile_id: transition.prior_active_profile_id.clone(),
            resulting_active_profile_id: transition.result_active_profile_id.clone(),
            prior_active_digest: transition.prior_active_profile_digest.clone(),
            resulting_active_digest: transition.result_active_profile_digest.clone(),
            evaluation_anchor: transition.evaluation_anchor.clone(),
            freshness_vector_digest,
            base_revision: transition.base_configuration.revision_id.clone(),
            result_revision: transition.result_configuration.revision_id.clone(),
            occurred_at,
        }
    }

    struct ConfigurationPort {
        transition: SemanticConfigurationTransitionV1,
        current: Mutex<Option<SemanticCurrentLinkedActivationV1>>,
        fail_commit: bool,
    }

    impl SemanticRetrievalConfigurationPortV1 for ConfigurationPort {
        fn current_activation<'a>(
            &'a self,
            _configuration: &'a SemanticConfigurationPinV1,
        ) -> SemanticRuntimeFuture<
            'a,
            Result<Option<SemanticCurrentLinkedActivationV1>, SemanticConfigurationBackendErrorV1>,
        > {
            let current = self.current.lock().unwrap().clone();
            Box::pin(async move { Ok(current) })
        }

        fn prepare_activation<'a>(
            &'a self,
            _command: &'a SemanticActivationCommandV1,
        ) -> SemanticRuntimeFuture<
            'a,
            Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
        > {
            let transition = self.transition.clone();
            Box::pin(async move { Ok(transition) })
        }

        fn prepare_rollback<'a>(
            &'a self,
            _command: &'a SemanticRollbackCommandV1,
        ) -> SemanticRuntimeFuture<
            'a,
            Result<SemanticConfigurationTransitionV1, SemanticConfigurationBackendErrorV1>,
        > {
            let transition = self.transition.clone();
            Box::pin(async move { Ok(transition) })
        }

        fn commit_linked_transition<'a>(
            &'a self,
            transition: &'a SemanticConfigurationTransitionV1,
            receipt: Option<&'a SemanticActivationReceiptV1>,
        ) -> SemanticRuntimeFuture<
            'a,
            Result<SemanticLinkedTransitionV1, SemanticConfigurationBackendErrorV1>,
        > {
            if self.fail_commit {
                return Box::pin(async { Err(SemanticConfigurationBackendErrorV1::Conflict) });
            }
            let linked = SemanticLinkedTransitionV1::new(
                transition,
                receipt,
                audit_for(transition, transition.operation.clone()),
            )
            .unwrap();
            let receipt = receipt.unwrap();
            *self.current.lock().unwrap() = Some(
                SemanticCurrentLinkedActivationV1::new(
                    receipt.clone(),
                    transition.result_active_semantic.clone().unwrap(),
                )
                .unwrap(),
            );
            Box::pin(async move { Ok(linked) })
        }
    }

    struct RuntimeInspector {
        evidence: SemanticExecutableGenerationV1,
    }

    impl SemanticRuntimeGenerationInspectorV1 for RuntimeInspector {
        fn inspect_generation<'a>(
            &'a self,
            _required: &'a SemanticCompatibilityPinsV1,
        ) -> SemanticRuntimeFuture<
            'a,
            Result<SemanticExecutableGenerationV1, SemanticRuntimeBackendErrorV1>,
        > {
            let evidence = self.evidence.clone();
            Box::pin(async move { Ok(evidence) })
        }
    }

    #[tokio::test]
    async fn passing_fastembed_profile_activation_links_config_audit_and_runtime_receipt() {
        let target = pins('a');
        let transition = transition(None, target.clone());
        let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new(
            ConfigurationPort {
                transition,
                current: Mutex::new(None),
                fail_commit: false,
            },
            RuntimeInspector {
                evidence: SemanticExecutableGenerationV1::new(
                    target.clone(),
                    resources(),
                    true,
                    true,
                )
                .unwrap(),
            },
        );
        let command = SemanticActivationCommandV1::new(
            pin("configuration.revision.1", '8'),
            SemanticActivationRequestV1::new(target.vector_generation_id.clone(), None, None)
                .unwrap(),
        )
        .unwrap();

        let receipt = backend.activate(&command).await.unwrap();
        let activation_digest = receipt.receipt_digest.clone();

        assert_eq!(receipt.configuration, pin("configuration.revision.2", '9'));
        assert_eq!(receipt.activated_generation, target.vector_generation_id);
        let state = backend.status(&receipt.configuration).await.unwrap();
        assert!(matches!(
            &state,
            SemanticRuntimeStateV1::Current { receipt: current } if current == &receipt
        ));
        assert_eq!(
            SemanticRuntimeStatusV1::new(Some(receipt.configuration.clone()), state).route(),
            SemanticRuntimeRouteV1::Semantic {
                generation: receipt.activated_generation,
                activation_receipt_digest: activation_digest,
            }
        );
    }

    #[tokio::test]
    async fn artifact_or_runtime_mismatch_fails_closed_before_config_commit() {
        let target = pins('a');
        let transition = transition(None, target.clone());
        let incompatible = pins('b');
        let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new(
            ConfigurationPort {
                transition,
                current: Mutex::new(None),
                fail_commit: false,
            },
            RuntimeInspector {
                evidence: SemanticExecutableGenerationV1::new(
                    incompatible,
                    resources(),
                    true,
                    true,
                )
                .unwrap(),
            },
        );
        let command = SemanticActivationCommandV1::new(
            pin("configuration.revision.1", '8'),
            SemanticActivationRequestV1::new(target.vector_generation_id, None, None).unwrap(),
        )
        .unwrap();

        assert_eq!(
            backend.activate(&command).await,
            Err(SemanticRuntimeBackendErrorV1::Rejected)
        );
        assert!(
            backend
                .status(&pin("configuration.revision.1", '8'))
                .await
                .unwrap()
                .eq(&SemanticRuntimeStateV1::Unavailable {
                    reason: SemanticFallbackReasonV1::ArtifactUnavailable
                })
        );
    }

    #[tokio::test]
    async fn rollback_requires_a_cold_offline_executable_target() {
        let current = pins('a');
        let target = pins('b');
        let accepted = accepted_profile(target.clone());
        let transition = SemanticConfigurationTransitionV1::rollback(
            pin("configuration.revision.2", '8'),
            pin("configuration.revision.3", '9'),
            typed_id("profile.semantic.current.v1"),
            &accepted,
            &runtime_for(target.clone()),
            RetrievalProfileCasV1 {
                expected_configuration_revision: ConfigurationRevisionId::try_from(
                    "configuration.revision.2".to_owned(),
                )
                .unwrap(),
                expected_active_digest: digest('6'),
                expected_rollback_digest: Some(digest('5')),
            },
            current.clone(),
            Some(target.clone()),
            "runtime failure".to_owned(),
            UtcMicros(20),
        )
        .unwrap();
        let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new(
            ConfigurationPort {
                transition,
                current: Mutex::new(None),
                fail_commit: false,
            },
            RuntimeInspector {
                evidence: SemanticExecutableGenerationV1::new(
                    target.clone(),
                    resources(),
                    false,
                    true,
                )
                .unwrap(),
            },
        );
        let command = SemanticRollbackCommandV1::new(
            pin("configuration.revision.2", '8'),
            SemanticRollbackRequestV1::new(
                target.vector_generation_id.clone(),
                current.vector_generation_id,
                target.vector_generation_id,
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            backend.rollback(&command).await,
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        );
    }

    #[test]
    fn tampered_generation_evidence_and_transition_fail_closed() {
        let target = pins('a');
        let mut evidence =
            SemanticExecutableGenerationV1::new(target.clone(), resources(), true, true).unwrap();
        evidence.rollback_executable = false;
        assert_eq!(
            evidence.validate_for(&target, false),
            Err(super::super::SemanticRuntimeContractErrorV1::InvalidCompatibility)
        );

        let mut transition = transition(None, target);
        transition.result_configuration.effective_behavior_digest = digest('1');
        assert_eq!(
            transition.validate(),
            Err(super::super::SemanticRuntimeContractErrorV1::InvalidTransition)
        );
    }

    #[tokio::test]
    async fn concurrent_configuration_cas_conflict_publishes_no_receipt() {
        let target = pins('a');
        let backend = ConfigurationLinkedSemanticRuntimeBackendV1::new(
            ConfigurationPort {
                transition: transition(None, target.clone()),
                current: Mutex::new(None),
                fail_commit: true,
            },
            RuntimeInspector {
                evidence: SemanticExecutableGenerationV1::new(
                    target.clone(),
                    resources(),
                    true,
                    true,
                )
                .unwrap(),
            },
        );
        let command = SemanticActivationCommandV1::new(
            pin("configuration.revision.1", '8'),
            SemanticActivationRequestV1::new(target.vector_generation_id, None, None).unwrap(),
        )
        .unwrap();

        assert_eq!(
            backend.activate(&command).await,
            Err(SemanticRuntimeBackendErrorV1::Conflict)
        );
        assert!(matches!(
            backend
                .status(&pin("configuration.revision.1", '8'))
                .await
                .unwrap(),
            SemanticRuntimeStateV1::Unavailable { .. }
        ));
    }
}

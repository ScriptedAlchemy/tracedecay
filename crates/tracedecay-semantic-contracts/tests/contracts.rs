use std::path::PathBuf;

use tracedecay_domain::{
    EmbeddingDeviceClassV1, EmbeddingDocumentCompositionV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingTruncationSideV1,
    ManifestDigest, host_cpu_target,
};
use tracedecay_semantic_contracts::configuration::{
    DEFAULT_FASTEMBED_MODEL_ID, SemanticConfig, SemanticFallbackReasonV1, SemanticProfileSelection,
    SemanticResourceCeilings,
};
use tracedecay_semantic_contracts::lifecycle::{
    SemanticModelLifecycleStateV1, SemanticModelLifecycleStatusV1, SemanticModelRemediationV1,
};
use tracedecay_semantic_contracts::manifest::{
    ArtifactMemberPinV1, ArtifactMemberRoleV1, ArtifactPackageMemberV1, ArtifactProfileKindV1,
    MODEL_ARTIFACT_MANIFEST_SCHEMA_V1, ManifestValidationErrorV1, ModelArtifactManifestPayloadV1,
    ModelArtifactManifestV1, PlatformTargetV1, ResourceCeilingV1, RuntimeCompatibilityV1,
    Sha256DigestHex, TruncationPolicyV1, UpstreamSourceV1,
};
use tracedecay_semantic_contracts::runtime_status::{
    SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
    SemanticRuntimeStatusProjectionV1,
};

fn digest(character: char) -> Sha256DigestHex {
    Sha256DigestHex::new(character.to_string().repeat(64)).expect("valid digest")
}

fn sample_manifest() -> ModelArtifactManifestV1 {
    ModelArtifactManifestV1 {
        payload: ModelArtifactManifestPayloadV1 {
            schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_owned(),
            artifact_id: "semantic-fixture".to_owned(),
            profile_kind: ArtifactProfileKindV1::Embedding,
            spdx_license: "MIT".to_owned(),
            model_member: ArtifactMemberPinV1 {
                digest: digest('a'),
                byte_length: 10,
            },
            tokenizer_digest: digest('b'),
            config_digest: digest('c'),
            query_instruction_digest: None,
            document_instruction_digest: None,
            members: vec![
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Model,
                    path: "model.onnx".to_owned(),
                    digest: digest('a'),
                    byte_length: 10,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Tokenizer,
                    path: "tokenizer.json".to_owned(),
                    digest: digest('b'),
                    byte_length: 5,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Config,
                    path: "config.json".to_owned(),
                    digest: digest('c'),
                    byte_length: 2,
                },
            ],
            dimensions: 384,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            pooling: EmbeddingPoolingV1::Mean,
            truncation: TruncationPolicyV1 {
                side: EmbeddingTruncationSideV1::Right,
                max_length: 512,
            },
            precision: EmbeddingPrecisionV1::Fp32,
            runtime: RuntimeCompatibilityV1 {
                runtime: "fastembed-ort".to_owned(),
                build_revision: "ort-fixture".to_owned(),
                platforms: vec![PlatformTargetV1 {
                    os: "linux".to_owned(),
                    arch: "x86_64".to_owned(),
                }],
            },
            device: EmbeddingDeviceClassV1::Cpu,
            resource_ceiling: ResourceCeilingV1 {
                max_model_bytes: 20,
                max_tokenizer_bytes: 10,
                max_resident_bytes: 100,
                max_threads: 4,
                max_batch_size: 32,
                max_sequence_length: 512,
                load_deadline_ms: 30_000,
            },
            upstream: UpstreamSourceV1 {
                name: "fixture/model".to_owned(),
                version: "1".to_owned(),
                revision: "immutable-revision".to_owned(),
            },
        },
    }
}

#[test]
fn semantic_config_preserves_omitted_defaults_and_explicit_null_disabling() {
    let omitted: SemanticConfig = serde_json::from_str("{}").expect("omitted configuration");
    assert_eq!(
        omitted.selected_model.as_deref(),
        Some(DEFAULT_FASTEMBED_MODEL_ID)
    );
    assert!(omitted.auto_download);
    assert_eq!(omitted.resources, SemanticResourceCeilings::default());

    let disabled: SemanticConfig =
        serde_json::from_str(r#"{"selected_model":null}"#).expect("disabled configuration");
    assert_eq!(disabled.selected_model, None);
    assert!(disabled.auto_download);
}

#[test]
fn semantic_config_serialization_preserves_contract_field_order() {
    let encoded = serde_json::to_string(&SemanticConfig::default()).expect("configuration JSON");
    assert!(encoded.starts_with(concat!(
        r#"{"selected_model":"JinaEmbeddingsV2BaseCode","#,
        r#""auto_download":true,"active_profile":null,"rollback_profile":null,"resources":{"#
    )));
    let model = encoded.find(r#""max_model_bytes":"#).expect("model field");
    let tokenizer = encoded
        .find(r#""max_tokenizer_bytes":"#)
        .expect("tokenizer field");
    let resident = encoded
        .find(r#""max_resident_bytes":"#)
        .expect("resident field");
    assert!(model < tokenizer && tokenizer < resident);
    assert!(
        encoded.ends_with(r#","load_deadline_ms":30000},"document_composition":"sanitized_text"}"#)
    );
}

#[test]
fn semantic_config_without_a_document_composition_selects_sanitized_text() {
    let legacy = r#"{"selected_model":"JinaEmbeddingsV2BaseCode","auto_download":true,"active_profile":null,"rollback_profile":null,"resources":{"max_model_bytes":734003200,"max_tokenizer_bytes":67108864,"max_resident_bytes":2147483648,"max_threads":4,"max_concurrent_sessions":16,"max_batch_size":32,"max_sequence_length":512,"load_deadline_ms":30000}}"#;
    let config: SemanticConfig = serde_json::from_str(legacy).expect("persisted configuration");
    assert_eq!(
        config.document_composition,
        EmbeddingDocumentCompositionV1::SanitizedText
    );

    let header: SemanticConfig = serde_json::from_str(&legacy.replace(
        r#""load_deadline_ms":30000}}"#,
        r#""load_deadline_ms":30000},"document_composition":"symbol_context_header"}"#,
    ))
    .expect("configuration selecting the header composition");
    assert_eq!(
        header.document_composition,
        EmbeddingDocumentCompositionV1::SymbolContextHeader
    );
    header
        .validate()
        .expect("header composition is a valid selection");
}

#[test]
fn semantic_resource_defaults_preserve_host_derived_runtime_widths() {
    let total_cores = host_cpu_target(usize::MAX);
    let shared_cpu_budget = if total_cores <= 8 {
        total_cores
    } else {
        total_cores / 2
    };
    let resources = SemanticResourceCeilings::default();

    assert_eq!(resources.max_model_bytes, 700 * 1024 * 1024);
    assert_eq!(resources.max_tokenizer_bytes, 64 * 1024 * 1024);
    assert_eq!(resources.max_resident_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(
        resources.max_threads,
        u32::try_from(total_cores.max(1))
            .unwrap_or(u32::MAX)
            .min(12)
    );
    assert_eq!(
        resources.max_concurrent_sessions,
        u32::try_from((shared_cpu_budget / 4).max(1)).unwrap_or(1)
    );
}

#[test]
fn semantic_config_retains_profile_and_resource_validation_failures() {
    let mut config = SemanticConfig::default();
    config.resources.max_threads = 0;
    assert!(config.validate().is_err());

    config.resources = SemanticResourceCeilings::default();
    config.active_profile = Some(SemanticProfileSelection {
        profile_id: "fixture".to_owned(),
        accepted_profile_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
            .expect("manifest digest"),
        artifact_digest: "b".repeat(64),
        artifact_path: PathBuf::from("../relative/model"),
    });
    assert!(config.validate().is_err());
}

#[test]
fn lifecycle_state_keeps_internal_tag_and_snake_case_shape() {
    let state = SemanticModelLifecycleStateV1::Indexing {
        model_id: "fixture".to_owned(),
        revision: "revision".to_owned(),
        artifact_digest: "a".repeat(64),
        install_path: PathBuf::from("/models/fixture"),
        completed_units: 2,
        total_units: 5,
    };
    assert_eq!(
        serde_json::to_string(&state).expect("lifecycle JSON"),
        format!(
            concat!(
                r#"{{"state":"indexing","model_id":"fixture","revision":"revision","#,
                r#""artifact_digest":"{}","install_path":"/models/fixture","#,
                r#""completed_units":2,"total_units":5}}"#
            ),
            "a".repeat(64)
        )
    );
    assert!(state.omits_semantics());
    assert_eq!(state.model_id(), "fixture");
}

#[test]
fn lifecycle_status_rejects_unknown_envelope_fields() {
    let status = SemanticModelLifecycleStatusV1 {
        selected_model: Some("fixture".to_owned()),
        auto_download: true,
        catalog_model_ids: vec!["fixture".to_owned()],
        state: None,
        remediation: SemanticModelRemediationV1 {
            retry: false,
            remove: false,
            rollback: false,
        },
        semantics_omitted: true,
    };
    let mut value = serde_json::to_value(status).expect("status value");
    value
        .as_object_mut()
        .expect("status object")
        .insert("unknown".to_owned(), serde_json::Value::Bool(true));
    assert!(serde_json::from_value::<SemanticModelLifecycleStatusV1>(value).is_err());
}

#[test]
fn runtime_projection_preserves_nested_tags_and_snake_case_reasons() {
    let projection = SemanticRuntimeStatusProjectionV1 {
        status: SemanticRuntimeScheduleStatusV1::Failed {
            reason: SemanticRuntimeScheduleFailureV1::ArtifactDetail("missing".to_owned()),
            prior_generation: None,
        },
        degraded_reason: Some(SemanticFallbackReasonV1::ArtifactUnavailable),
        prior_generation: None,
    };
    assert_eq!(
        serde_json::to_string(&projection).expect("runtime status JSON"),
        concat!(
            r#"{"status":{"state":"failed","reason":{"artifact_detail":"missing"},"#,
            r#""prior_generation":null},"degraded_reason":"artifact_unavailable","#,
            r#""prior_generation":null}"#
        )
    );
}

#[test]
fn manifest_profile_kind_accepts_shipped_pascal_case_aliases() {
    let embedding: ArtifactProfileKindV1 =
        serde_json::from_str(r#""Embedding""#).expect("embedding alias");
    let reranker: ArtifactProfileKindV1 =
        serde_json::from_str(r#""Reranker""#).expect("reranker alias");
    assert_eq!(embedding, ArtifactProfileKindV1::Embedding);
    assert_eq!(reranker, ArtifactProfileKindV1::Reranker);
    assert_eq!(
        serde_json::to_string(&embedding).expect("embedding JSON"),
        r#""embedding""#
    );
}

#[test]
fn manifest_canonical_bytes_are_exact_and_digest_the_same_bytes() {
    let manifest = sample_manifest();
    let canonical = manifest.to_canonical_bytes().expect("canonical bytes");
    let expected = format!(
        concat!(
            r#"{{"payload":{{"schema":"tracedecay.model-artifact-manifest.v1","#,
            r#""artifact_id":"semantic-fixture","profile_kind":"embedding","spdx_license":"MIT","#,
            r#""model_member":{{"digest":"{a}","byte_length":10}},"tokenizer_digest":"{b}","#,
            r#""config_digest":"{c}","query_instruction_digest":null,"document_instruction_digest":null,"#,
            r#""members":[{{"role":"model","path":"model.onnx","digest":"{a}","byte_length":10}},"#,
            r#"{{"role":"tokenizer","path":"tokenizer.json","digest":"{b}","byte_length":5}},"#,
            r#"{{"role":"config","path":"config.json","digest":"{c}","byte_length":2}}],"#,
            r#""dimensions":384,"metric":"cosine","normalization":"l2","pooling":"mean","#,
            r#""truncation":{{"side":"right","max_length":512}},"precision":"fp32","#,
            r#""runtime":{{"runtime":"fastembed-ort","build_revision":"ort-fixture","#,
            r#""platforms":[{{"os":"linux","arch":"x86_64"}}]}},"device":"cpu","#,
            r#""resource_ceiling":{{"max_model_bytes":20,"max_tokenizer_bytes":10,"#,
            r#""max_resident_bytes":100,"max_threads":4,"max_batch_size":32,"#,
            r#""max_sequence_length":512,"load_deadline_ms":30000}},"#,
            r#""upstream":{{"name":"fixture/model","version":"1","#,
            r#""revision":"immutable-revision"}}}}}}"#
        ),
        a = "a".repeat(64),
        b = "b".repeat(64),
        c = "c".repeat(64),
    );
    assert_eq!(canonical, expected.as_bytes());
    assert_eq!(
        manifest.canonical_digest(),
        Sha256DigestHex::of_bytes(expected.as_bytes())
    );
    assert_eq!(
        ModelArtifactManifestV1::parse(&canonical).expect("canonical parse"),
        manifest
    );
}

#[test]
fn manifest_retains_structural_and_canonical_failure_validation() {
    let mut traversal = sample_manifest();
    traversal.payload.members[0].path = "../model.onnx".to_owned();
    assert_eq!(
        traversal.validate(),
        Err(ManifestValidationErrorV1::InvalidPackageMember)
    );

    let mut duplicate = sample_manifest();
    duplicate.payload.members[1].path = duplicate.payload.members[0].path.clone();
    assert_eq!(
        duplicate.validate(),
        Err(ManifestValidationErrorV1::DuplicatePackageMember)
    );

    assert!(Sha256DigestHex::new("A".repeat(64)).is_err());

    let canonical = sample_manifest()
        .to_canonical_bytes()
        .expect("canonical bytes");
    let mut padded = vec![b' '];
    padded.extend_from_slice(&canonical);
    assert!(matches!(
        ModelArtifactManifestV1::parse(&padded),
        Err(ManifestValidationErrorV1::NonCanonicalEncoding(_))
    ));
}

#[test]
fn manifest_rejects_invalid_scalar_contract_fields() {
    let mut bad_schema = sample_manifest();
    bad_schema.payload.schema = "tracedecay.model-artifact-manifest.v0".to_owned();
    assert!(matches!(
        bad_schema.validate(),
        Err(ManifestValidationErrorV1::UnsupportedSchema(_))
    ));

    let mut empty_license = sample_manifest();
    empty_license.payload.spdx_license = "  ".to_owned();
    assert_eq!(
        empty_license.validate(),
        Err(ManifestValidationErrorV1::EmptyField {
            field: "spdx_license".to_owned()
        })
    );

    let mut zero_dimensions = sample_manifest();
    zero_dimensions.payload.dimensions = 0;
    assert_eq!(
        zero_dimensions.validate(),
        Err(ManifestValidationErrorV1::ZeroDimensions)
    );

    let mut zero_ceiling = sample_manifest();
    zero_ceiling.payload.resource_ceiling.max_threads = 0;
    assert_eq!(
        zero_ceiling.validate(),
        Err(ManifestValidationErrorV1::ZeroResourceCeiling {
            field: "max_threads".to_owned()
        })
    );

    let mut low_ceiling = sample_manifest();
    low_ceiling.payload.resource_ceiling.max_model_bytes =
        low_ceiling.payload.model_member.byte_length - 1;
    assert_eq!(
        low_ceiling.validate(),
        Err(ManifestValidationErrorV1::CeilingBelowDeclaredModelBytes)
    );
}

#[test]
fn manifest_rejects_incomplete_or_inconsistent_package_identity() {
    let mut no_platform = sample_manifest();
    no_platform.payload.runtime.platforms.clear();
    assert_eq!(
        no_platform.validate(),
        Err(ManifestValidationErrorV1::NoSupportedPlatforms)
    );

    let mut no_tokenizer = sample_manifest();
    no_tokenizer
        .payload
        .members
        .retain(|member| member.role != ArtifactMemberRoleV1::Tokenizer);
    assert_eq!(
        no_tokenizer.validate(),
        Err(ManifestValidationErrorV1::MissingPackageMembers)
    );

    let mut inconsistent = sample_manifest();
    inconsistent.payload.members[0].digest = digest('d');
    assert_eq!(
        inconsistent.validate(),
        Err(ManifestValidationErrorV1::InconsistentPackageMembers)
    );
}

#[test]
fn manifest_parse_rejects_unknown_fields_at_every_frozen_level() {
    let canonical = sample_manifest()
        .to_canonical_bytes()
        .expect("canonical bytes");
    let mut root: serde_json::Value = serde_json::from_slice(&canonical).expect("manifest value");
    root.as_object_mut()
        .expect("root object")
        .insert("unsigned_extension".to_owned(), serde_json::json!(true));
    assert!(
        ModelArtifactManifestV1::parse(&serde_json::to_vec(&root).expect("unknown root field"))
            .is_err()
    );

    let mut nested: serde_json::Value = serde_json::from_slice(&canonical).expect("manifest value");
    nested["payload"]["runtime"]
        .as_object_mut()
        .expect("runtime object")
        .insert("ambient_cache".to_owned(), serde_json::json!(true));
    assert!(
        ModelArtifactManifestV1::parse(&serde_json::to_vec(&nested).expect("unknown nested field"))
            .is_err()
    );
}

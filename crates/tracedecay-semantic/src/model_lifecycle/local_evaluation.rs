use super::manifest::{
    ArtifactMemberPinV1, ArtifactPackageMemberV1, ArtifactProfileKindV1, DeviceClassV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
    MODEL_ARTIFACT_MANIFEST_SCHEMA_V1, ModelArtifactManifestPayloadV1, PlatformTargetV1,
    ResourceCeilingV1, RuntimeCompatibilityV1, SemanticMetricV1, Sha256DigestHex,
    TruncationPolicyV1, TruncationSideV1, UpstreamSourceV1,
};

/// Import the production embedding package into an explicit evaluator-owned
/// lifecycle root. This path never consults a profile, daemon registry,
/// ambient model cache, or network source.
pub fn open_local_semantic_evaluation_lifecycle(
    lifecycle_root: &Path,
    package_directory: &Path,
    resources: SemanticResourceCeilings,
    imported_at_unix: u64,
) -> Result<Arc<SemanticModelLifecycleOwnerV1>, ModelLifecycleErrorV1> {
    let owner = SemanticModelLifecycleOwnerV1::open_default(lifecycle_root)?;
    let model = owner
        .catalog()
        .get(DEFAULT_FASTEMBED_MODEL_ID)
        .ok_or(CatalogErrorV1::MissingDefault)?;
    let manifest = local_evaluation_manifest(model, resources)?;
    let package_view = tempfile::Builder::new()
        .prefix(".semantic-evaluation-package-")
        .tempdir_in(lifecycle_root)
        .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
    for member in model.members.values() {
        let source = package_directory.join(&member.path);
        let destination = package_view.path().join(&member.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        }
        fs::copy(source, destination).map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
    }
    owner.import_local_artifact(
        DEFAULT_FASTEMBED_MODEL_ID,
        &manifest,
        package_view.path(),
        imported_at_unix,
    )?;
    Ok(Arc::new(owner))
}

fn local_evaluation_manifest(
    model: &CatalogedFastEmbedModelV1,
    resources: SemanticResourceCeilings,
) -> Result<ModelArtifactManifestV1, ModelLifecycleErrorV1> {
    if resources.max_sequence_length < model.max_length {
        return Err(ModelLifecycleErrorV1::VerificationFailed);
    }
    let role = |name: &str| match name {
        "model" => Ok(ArtifactMemberRoleV1::Model),
        "tokenizer" => Ok(ArtifactMemberRoleV1::Tokenizer),
        "config" => Ok(ArtifactMemberRoleV1::Config),
        "special_tokens_map" => Ok(ArtifactMemberRoleV1::SpecialTokensMap),
        "tokenizer_config" => Ok(ArtifactMemberRoleV1::TokenizerConfig),
        _ => Err(ModelLifecycleErrorV1::VerificationFailed),
    };
    let members = model
        .members
        .iter()
        .map(|(name, pin)| {
            Ok(ArtifactPackageMemberV1 {
                role: role(name)?,
                path: pin.path.clone(),
                digest: Sha256DigestHex::new(pin.sha256.clone())
                    .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?,
                byte_length: pin.length,
            })
        })
        .collect::<Result<Vec<_>, ModelLifecycleErrorV1>>()?;
    let member = |role| {
        members
            .iter()
            .find(|member| member.role == role)
            .ok_or(ModelLifecycleErrorV1::VerificationFailed)
    };
    let model_member = member(ArtifactMemberRoleV1::Model)?;
    let manifest = ModelArtifactManifestV1 {
        payload: ModelArtifactManifestPayloadV1 {
            schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_owned(),
            artifact_id: model.model_id.clone(),
            profile_kind: ArtifactProfileKindV1::Embedding,
            spdx_license: model.source.license.clone(),
            model_member: ArtifactMemberPinV1 {
                digest: model_member.digest.clone(),
                byte_length: model_member.byte_length,
            },
            tokenizer_digest: member(ArtifactMemberRoleV1::Tokenizer)?.digest.clone(),
            config_digest: member(ArtifactMemberRoleV1::Config)?.digest.clone(),
            query_instruction_digest: None,
            document_instruction_digest: None,
            members,
            dimensions: model.expected_dimensions,
            metric: SemanticMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            pooling: EmbeddingPoolingV1::Mean,
            truncation: TruncationPolicyV1 {
                side: TruncationSideV1::Right,
                max_length: model.max_length,
            },
            precision: EmbeddingPrecisionV1::Fp32,
            runtime: RuntimeCompatibilityV1 {
                runtime: "fastembed-ort".to_owned(),
                build_revision: "fastembed-v5".to_owned(),
                platforms: vec![PlatformTargetV1 {
                    os: std::env::consts::OS.to_owned(),
                    arch: std::env::consts::ARCH.to_owned(),
                }],
            },
            device: DeviceClassV1::Cpu,
            resource_ceiling: ResourceCeilingV1 {
                max_model_bytes: resources.max_model_bytes,
                max_tokenizer_bytes: resources.max_tokenizer_bytes,
                max_resident_bytes: resources.max_resident_bytes,
                max_threads: resources.max_threads,
                max_batch_size: resources.max_batch_size,
                max_sequence_length: resources.max_sequence_length,
                load_deadline_ms: resources.load_deadline_ms,
            },
            upstream: UpstreamSourceV1 {
                name: model.model_code.clone(),
                version: model.source.revision.clone(),
                revision: model.source.revision.clone(),
            },
        },
    };
    verify_catalog_manifest(model, &manifest)?;
    Ok(manifest)
}

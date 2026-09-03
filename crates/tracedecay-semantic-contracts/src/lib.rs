#![forbid(unsafe_code)]

pub mod configuration;
pub mod lifecycle;
pub mod manifest;
pub mod runtime_status;

pub use configuration::{
    DEFAULT_FASTEMBED_MODEL_ID, MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID, RerankCompatibilityPinsV1,
    SemanticConfig, SemanticFallbackReasonV1, SemanticProfileSelection, SemanticResourceCeilings,
};
pub use lifecycle::{
    RerankerArtifactLifecycleStatusV1, SemanticLifecycleVerifiedReadyEventV1,
    SemanticModelLifecycleStateV1, SemanticModelLifecycleStatusV1, SemanticModelRemediationV1,
};
pub use manifest::{
    ArtifactMemberPinV1, ArtifactMemberRoleV1, ArtifactPackageMemberV1, ArtifactProfileKindV1,
    MODEL_ARTIFACT_MANIFEST_SCHEMA_V1, ManifestValidationErrorV1, ModelArtifactManifestPayloadV1,
    ModelArtifactManifestV1, PlatformTargetV1, ResourceCeilingV1, RuntimeCompatibilityV1,
    Sha256DigestHex, TruncationPolicyV1, UpstreamSourceV1,
};
pub use runtime_status::{
    SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
    SemanticRuntimeStatusProjectionV1,
};

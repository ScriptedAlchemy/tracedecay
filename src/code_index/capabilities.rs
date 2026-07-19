//! Capability-manifest emission port (Plan 25): the mandatory base
//! capability manifest pins code generation, chunk schema/chunker and
//! language-descriptor revisions, available grains and exact-term fields,
//! supported languages, graph edge-authority classes, privacy domain/key
//! epoch, source coverage, exclusions, partial states, and manifest digest.
//!
//! Consumers must reject a missing, incompatible, mixed-generation, or
//! unauthorized base manifest before candidate production. Plan 31's
//! optional semantic manifest augments this base; its absence cannot block
//! authorized lexical/graph retrieval.

use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, CodeIndexCapabilityManifestV1, ProjectionKeyV1,
};

/// Capability-emission failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CapabilityEmissionErrorV1 {
    #[error("the generation manifest is not sealed")]
    GenerationNotSealed,
    #[error("the generation manifest mixes snapshots or generations")]
    MixedGeneration,
    #[error("the privacy domain or key epoch is not authorized for this consumer")]
    UnauthorizedPrivacyDomain,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// The capability-manifest emitter contract (Plan 25:
/// `src/code_index/capabilities.rs` emits `CodeIndexCapabilityManifestV1`).
pub trait CodeIndexCapabilityEmitter {
    /// Emit the base capability manifest for one sealed generation.
    fn emit(
        &self,
        generation: &CodeGenerationManifestV1,
    ) -> Result<CodeIndexCapabilityManifestV1, CapabilityEmissionErrorV1>;
}

/// The consumer-side validation contract for a base manifest (Plan 25:
/// reject missing, incompatible, mixed-generation, or unauthorized
/// manifests before candidate production).
pub trait CodeIndexCapabilityValidator {
    /// Validate that `manifest` authorizes candidate production under
    /// `projection` for `generation`.
    fn validate_for_candidates(
        &self,
        generation: &CodeGenerationId,
        projection: &ProjectionKeyV1,
        manifest: &CodeIndexCapabilityManifestV1,
    ) -> Result<(), CapabilityEmissionErrorV1>;
}

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

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, CodeGenerationManifestV1, CodeIndexCapabilityManifestV1,
    CodeSearchChunkGrainV1, CoverageSummaryV1, DomainError, EdgeAuthorityV1,
    ExactTechnicalTermKindV1, ExtractorRevision, GrammarRevision, LanguageId,
    LanguageRegistryRevision, ManifestDigest, PrivacyDomainId, ProjectionKeyV1, ProjectionKindV1,
    SanitizationReceiptId, SanitizerRevision, canonical_sha256,
};

use super::languages::LanguageRegistry;

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

/// Domain separator for the generation seal's expected digest (Plan 25: the
/// seal is computed over every generation field except the seal itself).
pub const GENERATION_SEAL_SEPARATOR: &str = "tracedecay.code-generation-seal.v1";

/// The chunk schema revision pinned by this implementation.
pub const CHUNK_SCHEMA_REVISION_V1: &str = "code-search-chunk.v1";

/// The exact-term kinds the query chunker emits (Plan 25 extraction evidence).
pub const BASE_EXACT_TERM_KINDS: &[ExactTechnicalTermKindV1] = &[
    ExactTechnicalTermKindV1::WholeSymbol,
    ExactTechnicalTermKindV1::QualifiedName,
    ExactTechnicalTermKindV1::Path,
    ExactTechnicalTermKindV1::CompilerErrorCode,
    ExactTechnicalTermKindV1::CompilerErrorText,
    ExactTechnicalTermKindV1::RuntimeErrorCode,
    ExactTechnicalTermKindV1::RuntimeErrorText,
    ExactTechnicalTermKindV1::CliFlag,
    ExactTechnicalTermKindV1::ToolName,
    ExactTechnicalTermKindV1::ConfigurationKey,
    ExactTechnicalTermKindV1::CommitIdentifier,
];

/// The edge-authority classes query tree-sitter extraction declares: edges
/// derived purely from syntax are `SyntaxExact`; unresolved constructs are
/// `UnknownUnsupported` evidence, never upgraded.
pub const BASE_EDGE_AUTHORITY_CLASSES: &[EdgeAuthorityV1] = &[
    EdgeAuthorityV1::SyntaxExact,
    EdgeAuthorityV1::UnknownUnsupported,
];

#[derive(Serialize)]
struct SealPayload<'a> {
    separator: &'static str,
    generation_id: &'a CodeGenerationId,
    snapshot_digest: &'a ManifestDigest,
    invalidation_digest: &'a ManifestDigest,
    registry_revision: &'a LanguageRegistryRevision,
    grammar_revisions: &'a [(LanguageId, GrammarRevision)],
    extractor_revisions: &'a [(LanguageId, ExtractorRevision)],
    sanitizer_revision: &'a SanitizerRevision,
    chunker_revision: &'a ChunkerRevision,
    privacy_domain: &'a PrivacyDomainId,
    privacy_key_epoch: u64,
    parent_generation: &'a Option<CodeGenerationId>,
}

#[derive(Serialize)]
struct LegacySealPayload<'a> {
    separator: &'static str,
    generation_id: &'a CodeGenerationId,
    snapshot_digest: &'a ManifestDigest,
    registry_revision: &'a LanguageRegistryRevision,
    grammar_revisions: &'a [(LanguageId, GrammarRevision)],
    extractor_revisions: &'a [(LanguageId, ExtractorRevision)],
    sanitizer_revision: &'a SanitizerRevision,
    chunker_revision: &'a ChunkerRevision,
    privacy_domain: &'a PrivacyDomainId,
    privacy_key_epoch: u64,
    parent_generation: &'a Option<CodeGenerationId>,
}

/// The canonical expected digest a generation planner must seal (Plan 25:
/// seal the generation before handing rows and the expected digest to the
/// store publication port). The emitter verifies this digest before
/// emitting capabilities for the generation.
pub fn expected_seal_digest(
    generation: &CodeGenerationManifestV1,
) -> Result<ManifestDigest, DomainError> {
    if generation.uses_legacy_v1_identity()? {
        return canonical_sha256(&LegacySealPayload {
            separator: GENERATION_SEAL_SEPARATOR,
            generation_id: &generation.generation_id,
            snapshot_digest: &generation.snapshot_digest,
            registry_revision: &generation.registry_revision,
            grammar_revisions: &generation.grammar_revisions,
            extractor_revisions: &generation.extractor_revisions,
            sanitizer_revision: &generation.sanitizer_revision,
            chunker_revision: &generation.chunker_revision,
            privacy_domain: &generation.privacy_domain,
            privacy_key_epoch: generation.privacy_key_epoch,
            parent_generation: &generation.parent_generation,
        });
    }
    canonical_sha256(&SealPayload {
        separator: GENERATION_SEAL_SEPARATOR,
        generation_id: &generation.generation_id,
        snapshot_digest: &generation.snapshot_digest,
        invalidation_digest: &generation.invalidation_digest,
        registry_revision: &generation.registry_revision,
        grammar_revisions: &generation.grammar_revisions,
        extractor_revisions: &generation.extractor_revisions,
        sanitizer_revision: &generation.sanitizer_revision,
        chunker_revision: &generation.chunker_revision,
        privacy_domain: &generation.privacy_domain,
        privacy_key_epoch: generation.privacy_key_epoch,
        parent_generation: &generation.parent_generation,
    })
}

/// Recompute the canonical digest of one base capability manifest (the
/// digest field itself is excluded from the hashed bytes; the digest
/// algorithm and domain separator are owned by the domain contract).
pub fn capability_manifest_digest(
    manifest: &CodeIndexCapabilityManifestV1,
) -> Result<ManifestDigest, DomainError> {
    manifest.compute_digest()
}

/// The base capability-manifest emitter. Everything the manifest pins that
/// the generation manifest does not already carry — descriptor revisions,
/// supported languages, available grains, exact-term kinds, edge-authority
/// classes, source coverage, and sanitization receipts — is supplied at
/// construction, so one emitter describes one generation's indexing
/// authority.
pub struct BaseCapabilityEmitter<R: LanguageRegistry> {
    registry: R,
    chunk_schema_revision: String,
    exact_term_kinds: Vec<ExactTechnicalTermKindV1>,
    edge_authority_classes: Vec<EdgeAuthorityV1>,
    source_coverage: CoverageSummaryV1,
    sanitization_receipts: Vec<SanitizationReceiptId>,
}

impl<R: LanguageRegistry> BaseCapabilityEmitter<R> {
    /// Create an emitter with the query base pins: chunk schema
    /// `code-search-chunk.v1`, the chunker's exact-term kinds, and the
    /// tree-sitter edge-authority classes.
    pub fn new(
        registry: R,
        source_coverage: CoverageSummaryV1,
        sanitization_receipts: Vec<SanitizationReceiptId>,
    ) -> Self {
        Self {
            registry,
            chunk_schema_revision: CHUNK_SCHEMA_REVISION_V1.to_owned(),
            exact_term_kinds: BASE_EXACT_TERM_KINDS.to_vec(),
            edge_authority_classes: BASE_EDGE_AUTHORITY_CLASSES.to_vec(),
            source_coverage,
            sanitization_receipts,
        }
    }

    /// Override the declared exact-term kinds (must match chunker output).
    #[must_use]
    pub fn with_exact_term_kinds(mut self, kinds: Vec<ExactTechnicalTermKindV1>) -> Self {
        self.exact_term_kinds = kinds;
        self
    }

    /// Override the declared edge-authority classes.
    #[must_use]
    pub fn with_edge_authority_classes(mut self, classes: Vec<EdgeAuthorityV1>) -> Self {
        self.edge_authority_classes = classes;
        self
    }
}

impl<R: LanguageRegistry> CodeIndexCapabilityEmitter for BaseCapabilityEmitter<R> {
    fn emit(
        &self,
        generation: &CodeGenerationManifestV1,
    ) -> Result<CodeIndexCapabilityManifestV1, CapabilityEmissionErrorV1> {
        match generation.validate() {
            Ok(()) => {}
            Err(DomainError::SelfSupersession) => {
                return Err(CapabilityEmissionErrorV1::MixedGeneration);
            }
            Err(error) => {
                return Err(CapabilityEmissionErrorV1::Contract(error.to_string()));
            }
        }
        let expected = expected_seal_digest(generation)
            .map_err(|error| CapabilityEmissionErrorV1::Contract(error.to_string()))?;
        if generation.seal.expected_digest != expected {
            return Err(CapabilityEmissionErrorV1::GenerationNotSealed);
        }

        let descriptors = self.registry.descriptors();
        if generation.registry_revision != self.registry.registry_revision()
            || generation.grammar_revisions.len() != descriptors.len()
            || generation.extractor_revisions.len() != descriptors.len()
            || generation.grammar_revisions.iter().zip(&descriptors).any(
                |((language, revision), descriptor)| {
                    language != &descriptor.language || revision != &descriptor.grammar_revision
                },
            )
            || generation.extractor_revisions.iter().zip(&descriptors).any(
                |((language, revision), descriptor)| {
                    language != &descriptor.language || revision != &descriptor.extractor_revision
                },
            )
        {
            return Err(CapabilityEmissionErrorV1::MixedGeneration);
        }
        let supported_languages: Vec<LanguageId> = descriptors
            .iter()
            .map(|descriptor| descriptor.language.clone())
            .collect();
        let language_descriptor_revisions: Vec<_> = descriptors
            .iter()
            .map(|descriptor| descriptor.descriptor_revision.clone())
            .collect();
        let mut available_grains = vec![
            CodeSearchChunkGrainV1::SymbolSignature,
            CodeSearchChunkGrainV1::SymbolBody,
            CodeSearchChunkGrainV1::FilePreamble,
            CodeSearchChunkGrainV1::FileWindow,
        ];
        if descriptors
            .iter()
            .any(|descriptor| descriptor.stable_member_spans)
        {
            available_grains.push(CodeSearchChunkGrainV1::SymbolMember);
        }
        available_grains.sort();
        available_grains.dedup();

        // Canonical sorted-unique form is a domain invariant of the
        // manifest; normalize the configurable lists before digesting.
        let mut exact_term_kinds = self.exact_term_kinds.clone();
        exact_term_kinds.sort();
        exact_term_kinds.dedup();
        let mut edge_authority_classes = self.edge_authority_classes.clone();
        edge_authority_classes.sort();
        edge_authority_classes.dedup();
        let mut sanitization_receipts = self.sanitization_receipts.clone();
        sanitization_receipts.sort();
        sanitization_receipts.dedup();

        // Stage the manifest with the generation seal as a placeholder
        // digest, then replace it with the canonical manifest digest (the
        // digest field is excluded from the hashed bytes).
        let mut manifest = CodeIndexCapabilityManifestV1 {
            generation_id: generation.generation_id.clone(),
            chunk_schema_revision: self.chunk_schema_revision.clone(),
            chunker_revision: generation.chunker_revision.clone(),
            language_descriptor_revisions,
            available_grains,
            exact_term_kinds,
            supported_languages,
            edge_authority_classes,
            privacy_domain: generation.privacy_domain.clone(),
            privacy_key_epoch: generation.privacy_key_epoch,
            source_coverage: self.source_coverage,
            sanitization_receipts,
            manifest_digest: generation.seal.expected_digest.clone(),
        };
        manifest.manifest_digest = capability_manifest_digest(&manifest)
            .map_err(|error| CapabilityEmissionErrorV1::Contract(error.to_string()))?;
        manifest
            .validate()
            .map_err(|error| CapabilityEmissionErrorV1::Contract(error.to_string()))?;
        Ok(manifest)
    }
}

/// The consumer-side base-manifest validator. Authorization is pinned at
/// construction: the expected chunk schema revision and the authorized
/// privacy domains with their maximum key epochs.
pub struct BaseCapabilityValidator {
    chunk_schema_revision: String,
    /// Privacy domain identity -> maximum authorized key epoch.
    authorized_privacy_domains: BTreeMap<String, u64>,
}

impl BaseCapabilityValidator {
    /// Create a validator for the query base chunk schema.
    pub fn new() -> Self {
        Self {
            chunk_schema_revision: CHUNK_SCHEMA_REVISION_V1.to_owned(),
            authorized_privacy_domains: BTreeMap::new(),
        }
    }

    /// Authorize one privacy domain up to `max_key_epoch` (inclusive).
    #[must_use]
    pub fn authorize_privacy_domain(
        mut self,
        domain: &PrivacyDomainId,
        max_key_epoch: u64,
    ) -> Self {
        self.authorized_privacy_domains
            .insert(domain.as_str().to_owned(), max_key_epoch);
        self
    }
}

impl Default for BaseCapabilityValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeIndexCapabilityValidator for BaseCapabilityValidator {
    fn validate_for_candidates(
        &self,
        generation: &CodeGenerationId,
        projection: &ProjectionKeyV1,
        manifest: &CodeIndexCapabilityManifestV1,
    ) -> Result<(), CapabilityEmissionErrorV1> {
        if &manifest.generation_id != generation {
            return Err(CapabilityEmissionErrorV1::MixedGeneration);
        }
        if projection.kind == ProjectionKindV1::Embedding {
            // Plan 31's semantic manifest augments this base; the base
            // manifest alone never authorizes embedding projections.
            return Err(CapabilityEmissionErrorV1::Contract(
                "embedding projections require the semantic capability manifest".to_owned(),
            ));
        }
        // Structural and digest validation (including the recomputed
        // manifest digest) is owned by the domain contract.
        manifest
            .validate()
            .map_err(|error| CapabilityEmissionErrorV1::Contract(error.to_string()))?;
        if manifest.chunk_schema_revision != self.chunk_schema_revision {
            return Err(CapabilityEmissionErrorV1::Contract(format!(
                "chunk schema revision {} is not compatible with {}",
                manifest.chunk_schema_revision, self.chunk_schema_revision
            )));
        }
        match self
            .authorized_privacy_domains
            .get(manifest.privacy_domain.as_str())
        {
            Some(max_epoch) if manifest.privacy_key_epoch <= *max_epoch => Ok(()),
            _ => Err(CapabilityEmissionErrorV1::UnauthorizedPrivacyDomain),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{GenerationSealV1, UtcMicros};

    use crate::languages::StaticLanguageRegistry;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn generation_manifest() -> CodeGenerationManifestV1 {
        let registry = StaticLanguageRegistry::new();
        let grammar_revisions = registry
            .descriptors()
            .iter()
            .map(|descriptor| {
                (
                    descriptor.language.clone(),
                    descriptor.grammar_revision.clone(),
                )
            })
            .collect();
        let extractor_revisions = registry
            .descriptors()
            .iter()
            .map(|descriptor| {
                (
                    descriptor.language.clone(),
                    descriptor.extractor_revision.clone(),
                )
            })
            .collect();
        let mut manifest = CodeGenerationManifestV1 {
            project_id: tracedecay_domain::ProjectId::new("project.fixture").expect("valid id"),
            generation_id: CodeGenerationId::new("generation.v1.aaaaaaaa.00000001")
                .expect("valid id"),
            snapshot_digest: digest('a'),
            invalidation_digest: digest('c'),
            registry_revision: registry.registry_revision(),
            grammar_revisions,
            extractor_revisions,
            sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("valid id"),
            chunker_revision: ChunkerRevision::new("chunker.v1").expect("valid id"),
            privacy_domain: PrivacyDomainId::new("privacy.fixture").expect("valid id"),
            privacy_key_epoch: 7,
            parent_generation: None,
            seal: GenerationSealV1 {
                expected_digest: digest('b'),
                sealed_at: UtcMicros(1_000_000),
                planner: tracedecay_domain::ComponentVersion::new("planner.v1").expect("valid id"),
            },
        };
        manifest.invalidation_digest = manifest
            .expected_legacy_invalidation_digest()
            .expect("legacy invalidation digest computes");
        manifest.seal.expected_digest =
            expected_seal_digest(&manifest).expect("seal digest computes");
        manifest
    }

    fn coverage() -> CoverageSummaryV1 {
        CoverageSummaryV1 {
            files_eligible: 10,
            files_excluded: 1,
            files_partial: 2,
            files_unsupported: 1,
            ranges_excluded: 3,
            ranges_unsupported: 2,
        }
    }

    fn receipts() -> Vec<SanitizationReceiptId> {
        vec![SanitizationReceiptId::new("receipt.one").expect("valid id")]
    }

    fn emitter() -> BaseCapabilityEmitter<StaticLanguageRegistry> {
        BaseCapabilityEmitter::new(StaticLanguageRegistry::new(), coverage(), receipts())
    }

    fn projection(kind: ProjectionKindV1) -> ProjectionKeyV1 {
        ProjectionKeyV1 {
            kind,
            schema_revision: "projection.v1".to_owned(),
            profile_digest: digest('e'),
        }
    }

    #[test]
    fn emit_pins_the_frozen_base_manifest_fields() {
        let generation = generation_manifest();
        let manifest = emitter().emit(&generation).expect("emission succeeds");

        assert_eq!(manifest.generation_id, generation.generation_id);
        assert_eq!(manifest.chunk_schema_revision, CHUNK_SCHEMA_REVISION_V1);
        assert_eq!(manifest.chunker_revision, generation.chunker_revision);
        assert_eq!(manifest.privacy_domain, generation.privacy_domain);
        assert_eq!(manifest.privacy_key_epoch, 7);
        assert_eq!(manifest.source_coverage, coverage());
        assert_eq!(manifest.sanitization_receipts, receipts());
        assert_eq!(manifest.exact_term_kinds, BASE_EXACT_TERM_KINDS);
        assert_eq!(manifest.edge_authority_classes, BASE_EDGE_AUTHORITY_CLASSES);
        assert!(
            manifest
                .supported_languages
                .contains(&LanguageId::new("rust").expect("valid id"))
        );
        // Member grain is available because compiled descriptors identify
        // stable member spans.
        assert!(
            manifest
                .available_grains
                .contains(&CodeSearchChunkGrainV1::SymbolMember)
        );
        // Canonical grain order.
        let mut sorted = manifest.available_grains.clone();
        sorted.sort();
        assert_eq!(manifest.available_grains, sorted);
        // The manifest digest recomputes over the manifest minus the digest.
        assert_eq!(
            capability_manifest_digest(&manifest).expect("digest recomputes"),
            manifest.manifest_digest
        );
    }

    #[test]
    fn emission_is_deterministic_and_serde_round_trips() {
        let generation = generation_manifest();
        let first = emitter().emit(&generation).expect("first emission");
        let second = emitter().emit(&generation).expect("second emission");
        assert_eq!(first, second);

        let bytes = serde_json::to_vec(&first).expect("serialize");
        let decoded: CodeIndexCapabilityManifestV1 =
            serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(first, decoded);
        assert_eq!(decoded.manifest_digest, first.manifest_digest);
    }

    #[test]
    fn emit_rejects_unsealed_and_mixed_generations() {
        let mut unsealed = generation_manifest();
        unsealed.seal.expected_digest = digest('f');
        assert_eq!(
            emitter().emit(&unsealed),
            Err(CapabilityEmissionErrorV1::GenerationNotSealed)
        );

        let mut mixed = generation_manifest();
        mixed.parent_generation = Some(mixed.generation_id.clone());
        // Recompute the integrity inputs so only self-supersession is wrong.
        mixed.invalidation_digest = mixed
            .expected_legacy_invalidation_digest()
            .expect("mixed invalidation digest");
        mixed.seal.expected_digest = expected_seal_digest(&mixed).expect("seal");
        assert_eq!(
            emitter().emit(&mixed),
            Err(CapabilityEmissionErrorV1::MixedGeneration)
        );
    }

    #[test]
    fn validator_accepts_an_authorized_base_manifest_round_trip() {
        let generation = generation_manifest();
        let manifest = emitter().emit(&generation).expect("emission succeeds");
        let validator =
            BaseCapabilityValidator::new().authorize_privacy_domain(&generation.privacy_domain, 7);

        validator
            .validate_for_candidates(
                &generation.generation_id,
                &projection(ProjectionKindV1::Lexical),
                &manifest,
            )
            .expect("lexical projection authorized");
        validator
            .validate_for_candidates(
                &generation.generation_id,
                &projection(ProjectionKindV1::Graph),
                &manifest,
            )
            .expect("graph projection authorized");
    }

    #[test]
    fn validator_rejects_wrong_generation_tampering_and_unauthorized_domains() {
        let generation = generation_manifest();
        let manifest = emitter().emit(&generation).expect("emission succeeds");
        let validator =
            BaseCapabilityValidator::new().authorize_privacy_domain(&generation.privacy_domain, 7);

        // Wrong generation.
        let other = CodeGenerationId::new("generation.other").expect("valid id");
        assert_eq!(
            validator.validate_for_candidates(
                &other,
                &projection(ProjectionKindV1::Lexical),
                &manifest
            ),
            Err(CapabilityEmissionErrorV1::MixedGeneration)
        );

        // Tampered manifest (coverage edited after the digest was pinned).
        let mut tampered = manifest.clone();
        tampered.source_coverage.files_eligible = 11;
        assert!(matches!(
            validator.validate_for_candidates(
                &generation.generation_id,
                &projection(ProjectionKindV1::Lexical),
                &tampered,
            ),
            Err(CapabilityEmissionErrorV1::Contract(_))
        ));

        // Embedding projections are not authorized by the base manifest.
        assert!(matches!(
            validator.validate_for_candidates(
                &generation.generation_id,
                &projection(ProjectionKindV1::Embedding),
                &manifest,
            ),
            Err(CapabilityEmissionErrorV1::Contract(_))
        ));

        // Unauthorized privacy domain.
        let no_domains = BaseCapabilityValidator::new();
        assert_eq!(
            no_domains.validate_for_candidates(
                &generation.generation_id,
                &projection(ProjectionKindV1::Lexical),
                &manifest,
            ),
            Err(CapabilityEmissionErrorV1::UnauthorizedPrivacyDomain)
        );

        // Key epoch above the authorized maximum.
        let stale_epoch =
            BaseCapabilityValidator::new().authorize_privacy_domain(&generation.privacy_domain, 6);
        assert_eq!(
            stale_epoch.validate_for_candidates(
                &generation.generation_id,
                &projection(ProjectionKindV1::Lexical),
                &manifest,
            ),
            Err(CapabilityEmissionErrorV1::UnauthorizedPrivacyDomain)
        );
    }
}

//! Generation, intake, extraction, lineage, and test-attribution contracts
//! (Plan 25: "Sanitized intake", "Generations and incremental reuse",
//! "Identity and lineage", "Diagnostics and tests").
//!
//! These are storage-neutral logical records. The index stores only typed
//! references to Plan 35's `GenerationDiagnosticV1` contract (owned by
//! `crates/tracedecay-domain/src/diagnostics.rs`, delivered by the query/12
//! diagnostic-persistence authority packet) — never a duplicate diagnostic
//! record.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::id::{
    CommitId, ManifestDigest, PrivacyDomainId, ProjectId, RefId, RepositoryId, RetrievalAnchorId,
    SanitizationReceiptId, WorktreeId,
};
use crate::research::time::UtcMicros;
use crate::research::{DomainError, canonical_sha256};

use super::identity::{
    ChunkerRevision, CodeGenerationId, CodeSearchChunkId, ContentDigest, ExtractorRevision,
    FileOccurrenceId, GrammarRevision, LanguageDescriptorRevision, LanguageId,
    LanguageRegistryRevision, SanitizerRevision, SourceSpan, SymbolOccurrenceId,
};
use super::language::EdgeAuthorityV1;

/// One receipt-bound sanitized repository snapshot (Plan 25: the only legal
/// intake). Carries repository, checkout, worktree, ref, source revision,
/// sanitizer revision, and content identity. Missing, stale, mixed-snapshot,
/// or unsanitized input is rejected before parsing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SanitizedCodeSnapshotV1 {
    pub repository: RepositoryId,
    pub worktree: Option<WorktreeId>,
    pub reference: Option<RefId>,
    pub source_revision: Option<CommitId>,
    pub sanitizer_revision: SanitizerRevision,
    pub sanitization_receipts: Vec<SanitizationReceiptId>,
    pub content_identity: ContentDigest,
    pub captured_at: UtcMicros,
    pub files: Vec<SanitizedCodeFileV1>,
}

impl SanitizedCodeSnapshotV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository.validate()?;
        if let Some(worktree) = &self.worktree {
            worktree.validate()?;
        }
        if let Some(reference) = &self.reference {
            reference.validate()?;
        }
        if let Some(source_revision) = &self.source_revision {
            source_revision.validate()?;
        }
        self.sanitizer_revision.validate()?;
        self.content_identity.validate()?;
        if self.sanitization_receipts.is_empty() {
            return Err(DomainError::Empty {
                field: "snapshot sanitization receipts",
            });
        }
        if self
            .sanitization_receipts
            .windows(2)
            .any(|receipts| receipts[0] >= receipts[1])
        {
            return Err(DomainError::NonCanonical {
                field: "snapshot sanitization receipt order",
            });
        }

        let mut occurrence_ids = BTreeSet::new();
        let mut logical_paths = BTreeSet::new();
        for file in &self.files {
            file.validate()?;
            if !occurrence_ids.insert(&file.file_occurrence_id) {
                return Err(DomainError::DuplicateId {
                    field: "snapshot file occurrence",
                });
            }
            if !logical_paths.insert(&file.logical_path) {
                return Err(DomainError::DuplicateId {
                    field: "snapshot logical path",
                });
            }
        }
        if self.files.windows(2).any(|files| {
            (&files[0].logical_path, &files[0].file_occurrence_id)
                >= (&files[1].logical_path, &files[1].file_occurrence_id)
        }) {
            return Err(DomainError::NonCanonical {
                field: "snapshot file order",
            });
        }
        Ok(())
    }
}

/// One sanitized file inside a snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SanitizedCodeFileV1 {
    pub file_occurrence_id: FileOccurrenceId,
    pub logical_path: String,
    pub language: Option<LanguageId>,
    pub content_digest: ContentDigest,
    pub disposition: SnapshotFileDispositionV1,
}

impl SanitizedCodeFileV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.file_occurrence_id.validate()?;
        self.content_digest.validate()?;
        if let Some(language) = &self.language {
            language.validate()?;
        }
        if self.logical_path.is_empty() {
            return Err(DomainError::Empty {
                field: "snapshot logical path",
            });
        }
        if self.logical_path.trim() != self.logical_path
            || self.logical_path.starts_with('/')
            || self.logical_path.contains('\\')
            || self.logical_path.chars().any(char::is_control)
            || self
                .logical_path
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        {
            return Err(DomainError::NonCanonical {
                field: "snapshot logical path",
            });
        }
        if self.disposition == SnapshotFileDispositionV1::Present && self.language.is_none() {
            return Err(DomainError::UnknownReference {
                field: "present snapshot file language",
            });
        }
        Ok(())
    }
}

/// Explicit handling of deletions, renames, ignored, binary, generated, and
/// unsupported-language files (Plan 25).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFileDispositionV1 {
    Present,
    Deleted,
    Renamed,
    Ignored,
    Binary,
    Generated,
    UnsupportedLanguage,
}

/// A snapshot that passed intake validation: receipt-bound, single-snapshot,
/// and sanitized. Constructed only by `CodeIndexIntake::validate` in
/// `src/code_index/intake.rs` (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ValidatedCodeSnapshotV1 {
    pub snapshot: SanitizedCodeSnapshotV1,
    pub intake_digest: ManifestDigest,
    pub validated_at: UtcMicros,
}

/// One file drawn from a validated snapshot, the extractor input unit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ValidatedCodeFileV1 {
    /// The planned immutable generation this extraction input belongs to.
    pub generation_id: CodeGenerationId,
    pub file: SanitizedCodeFileV1,
    pub snapshot_digest: ManifestDigest,
    pub sanitized_bytes: Vec<u8>,
}

/// Why intake rejected a snapshot (Plan 25: reject before parsing).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "rejection", content = "detail", rename_all = "snake_case")]
pub enum IntakeRejectionV1 {
    MissingReceipt,
    UnsanitizedInput,
    StaleSnapshot,
    MixedSnapshot,
    IncompatibleSanitizerRevision,
}

/// The sealed manifest of one immutable logical generation (Plan 25:
/// generations are planned, sealed, digested, and never mutated after
/// publication).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeGenerationManifestV1 {
    pub project_id: ProjectId,
    pub generation_id: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    /// Canonical digest of every input that controls incremental invalidation
    /// and therefore the generation's immutable publication fence.
    pub invalidation_digest: ManifestDigest,
    pub registry_revision: LanguageRegistryRevision,
    pub grammar_revisions: Vec<(LanguageId, GrammarRevision)>,
    pub extractor_revisions: Vec<(LanguageId, ExtractorRevision)>,
    pub sanitizer_revision: SanitizerRevision,
    pub chunker_revision: ChunkerRevision,
    pub privacy_domain: PrivacyDomainId,
    pub privacy_key_epoch: u64,
    pub parent_generation: Option<CodeGenerationId>,
    pub seal: GenerationSealV1,
}

const LEGACY_GENERATION_INVALIDATION_DIGEST_DOMAIN: &str =
    "tracedecay.code-generation-legacy-v1-migration.v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeGenerationManifestWireV1 {
    project_id: ProjectId,
    generation_id: CodeGenerationId,
    snapshot_digest: ManifestDigest,
    #[serde(default)]
    invalidation_digest: Option<ManifestDigest>,
    registry_revision: LanguageRegistryRevision,
    grammar_revisions: Vec<(LanguageId, GrammarRevision)>,
    extractor_revisions: Vec<(LanguageId, ExtractorRevision)>,
    sanitizer_revision: SanitizerRevision,
    chunker_revision: ChunkerRevision,
    privacy_domain: PrivacyDomainId,
    privacy_key_epoch: u64,
    parent_generation: Option<CodeGenerationId>,
    seal: GenerationSealV1,
}

impl<'de> Deserialize<'de> for CodeGenerationManifestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CodeGenerationManifestWireV1::deserialize(deserializer)?;
        let needs_legacy_migration = wire.invalidation_digest.is_none();
        let mut manifest = Self {
            project_id: wire.project_id,
            generation_id: wire.generation_id,
            snapshot_digest: wire.snapshot_digest,
            invalidation_digest: wire
                .invalidation_digest
                .unwrap_or_else(zero_manifest_digest),
            registry_revision: wire.registry_revision,
            grammar_revisions: wire.grammar_revisions,
            extractor_revisions: wire.extractor_revisions,
            sanitizer_revision: wire.sanitizer_revision,
            chunker_revision: wire.chunker_revision,
            privacy_domain: wire.privacy_domain,
            privacy_key_epoch: wire.privacy_key_epoch,
            parent_generation: wire.parent_generation,
            seal: wire.seal,
        };
        if needs_legacy_migration {
            if !manifest
                .uses_legacy_v1_identity()
                .map_err(serde::de::Error::custom)?
            {
                return Err(serde::de::Error::missing_field("invalidation_digest"));
            }
            manifest.invalidation_digest = manifest
                .expected_legacy_invalidation_digest()
                .map_err(serde::de::Error::custom)?;
        }
        Ok(manifest)
    }
}

fn zero_manifest_digest() -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
        .expect("zero sha256 digest is canonical")
}

/// The seal applied before rows and the expected digest are handed to the
/// store publication port (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationSealV1 {
    pub expected_digest: ManifestDigest,
    pub sealed_at: UtcMicros,
    pub planner: GenerationPlannerIdV1,
}

/// Identity of the deterministic generation planner that produced a seal.
pub type GenerationPlannerIdV1 = crate::research::id::ComponentVersion;

/// The output of one language extractor for one validated file (Plan 25:
/// stable canonical rows and digests for identical input, registry, and
/// extractor revisions on every supported host; parse errors and unsupported
/// constructs are preserved as evidence).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtractionBatchV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub language: LanguageId,
    pub descriptor_revision: LanguageDescriptorRevision,
    pub grammar_revision: GrammarRevision,
    pub extractor_revision: ExtractorRevision,
    pub content_digest: ContentDigest,
    pub parse_outcome: ParseOutcomeV1,
    pub parsed_ranges: Vec<SourceSpan>,
    pub error_ranges: Vec<SourceSpan>,
    pub unsupported_ranges: Vec<SourceSpan>,
    pub coverage: ExtractionCoverageV1,
    pub rows_digest: ManifestDigest,
}

/// Parse outcome; bounded traversal or extraction caps propagate as partial
/// (Plan 25). Extraction never invents successful structure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "outcome", content = "detail", rename_all = "snake_case")]
pub enum ParseOutcomeV1 {
    Complete,
    Partial { reason: String },
    TimedOut,
    Cancelled,
    Failed { reason: String },
}

/// Extraction coverage and ambiguity evidence (Plan 25: canonical raw
/// quantifier inputs; no universal quality score is defined here).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtractionCoverageV1 {
    pub parsed_bytes: u64,
    pub error_bytes: u64,
    pub unsupported_bytes: u64,
    pub symbols_extracted: u64,
    pub relations_extracted: u64,
    pub ambiguity_count: u64,
}

/// Why extraction failed (the typed error half of the `LanguageExtractor`
/// port result).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "failure", content = "detail", rename_all = "snake_case")]
pub enum ExtractionFailureV1 {
    GrammarUnavailable { language: LanguageId },
    ParseFailed { detail: String },
    Cancelled,
    TimedOut,
    IncompatibleDescriptor { detail: String },
}

/// One recorded relationship edge with its authority class (Plan 25: every
/// graph path preserves its weakest edge authority; unresolved dispatch
/// cannot become semantic fact).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRelationEdgeV1 {
    pub from_occurrence: SymbolOccurrenceId,
    pub to_occurrence: SymbolOccurrenceId,
    pub kind: RelationEdgeKindV1,
    pub authority: EdgeAuthorityV1,
    pub evidence_span: SourceSpan,
}

/// Canonical relation kinds.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RelationEdgeKindV1 {
    Calls,
    Uses,
    TypeOf,
    Contains,
    Implements,
    Extends,
    Annotates,
}

/// One lineage candidate for a symbol across generations (Plan 25: record
/// rename, move, split, merge, and structural-continuity candidates with
/// method, evidence, confidence kind, alternatives, and abstention; ambiguous
/// lineage stays explicit and never silently merges unrelated symbols).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SymbolLineageCandidateV1 {
    pub prior_occurrence: SymbolOccurrenceId,
    pub current_occurrence: SymbolOccurrenceId,
    pub kind: LineageKindV1,
    pub method: LineageMethodV1,
    pub evidence: LineageEvidenceV1,
    pub confidence: LineageConfidenceKindV1,
    pub alternatives: Vec<SymbolOccurrenceId>,
    pub abstention: Option<LineageAbstentionV1>,
}

/// The lineage relation kinds.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LineageKindV1 {
    Unchanged,
    Renamed,
    Moved,
    Split,
    Merged,
    StructuralContinuity,
}

/// How a lineage candidate was derived. Tree-sitter object reuse, path,
/// line, qualified-name similarity, or embedding similarity never proves
/// lineage (Plan 25).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LineageMethodV1 {
    ExactIdentityTuple,
    StructuralBoundaryMatch,
    ContentDigestMatch,
    QualifiedStructureMatch,
    DeclaredAbstention,
}

/// Evidence supporting a lineage candidate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LineageEvidenceV1 {
    pub prior_generation: CodeGenerationId,
    pub current_generation: CodeGenerationId,
    pub prior_digest: Option<ContentDigest>,
    pub current_digest: Option<ContentDigest>,
    pub evidence_digest: ManifestDigest,
}

/// Confidence kind; kept as a kind, not a scalar score (Plan 25 preserves
/// raw evidence and does not define a universal quality score).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LineageConfidenceKindV1 {
    Exact,
    Structural,
    Ambiguous,
    Abstained,
}

/// An explicit lineage abstention with its reason.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LineageAbstentionV1 {
    pub reason: String,
    pub candidate_count: u32,
}

/// A typed reference to Plan 35's generation-bound diagnostic contract. The
/// diagnostic record itself is owned by
/// `crates/tracedecay-domain/src/diagnostics.rs` (query/12 authority packet);
/// the index stores only anchor-bound references (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationDiagnosticAttachmentV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
    /// Plan 13 anchor addressing the Plan-35-owned diagnostic record.
    pub diagnostic_anchor: RetrievalAnchorId,
    pub content_digest: ContentDigest,
}

/// Test-attribution evidence for one generation (Plan 25: map test
/// definitions and runs to the generation, source revision, and candidate
/// production symbols they cover; no candidate mode proves execution,
/// correctness, or universal safety).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationTestAttributionV1 {
    pub generation_id: CodeGenerationId,
    pub source_revision: Option<CommitId>,
    pub test_occurrence: SymbolOccurrenceId,
    pub covered_occurrences: Vec<SymbolOccurrenceId>,
    pub evidence_class: TestAttributionEvidenceClassV1,
    pub attribution_revision: crate::research::id::ComponentVersion,
}

/// The declared attribution evidence classes (Plan 05/Plan 25:
/// `conservative_dependency_candidates`, `observed_coverage_candidates`,
/// `predictive_ranked_candidates`, stale evidence, or
/// `unknown_unsupported`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TestAttributionEvidenceClassV1 {
    ConservativeDependencyCandidates,
    ObservedCoverageCandidates,
    PredictiveRankedCandidates,
    StaleEvidence,
    UnknownUnsupported,
}

/// A chunk-to-generation binding asserted by the index (Plan 25: every
/// eligible chunk names exactly one code generation and file occurrence).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ChunkGenerationBindingV1 {
    pub chunk_id: CodeSearchChunkId,
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
}

impl CodeGenerationManifestV1 {
    pub fn uses_legacy_v1_identity(&self) -> Result<bool, DomainError> {
        Ok(matches!(
            generation_identity_kind(&self.generation_id)?,
            GenerationIdentityKind::Legacy
        ))
    }

    pub fn expected_legacy_invalidation_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            LEGACY_GENERATION_INVALIDATION_DIGEST_DOMAIN,
            &self.project_id,
            &self.generation_id,
            &self.snapshot_digest,
            &self.registry_revision,
            &self.grammar_revisions,
            &self.extractor_revisions,
            &self.sanitizer_revision,
            &self.chunker_revision,
            &self.privacy_domain,
            self.privacy_key_epoch,
            &self.parent_generation,
        ))
    }

    /// A manifest is single-generation: it names exactly one generation and
    /// at most one parent (Plan 25: mixed-generation manifests are rejected
    /// before publication).
    pub fn validate(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.generation_id.validate()?;
        self.snapshot_digest.validate()?;
        self.invalidation_digest.validate()?;
        self.registry_revision.validate()?;
        self.sanitizer_revision.validate()?;
        self.chunker_revision.validate()?;
        self.privacy_domain.validate()?;
        self.seal.expected_digest.validate()?;
        self.seal.planner.validate()?;
        match generation_identity_kind(&self.generation_id)? {
            GenerationIdentityKind::Legacy => {
                if self.invalidation_digest != self.expected_legacy_invalidation_digest()? {
                    return Err(DomainError::DigestMismatch);
                }
            }
            GenerationIdentityKind::Fingerprinted(fingerprint) => {
                let expected = crate::canonical_text::sha256_hex_body(
                    self.invalidation_digest.as_str(),
                    "generation invalidation digest",
                )?;
                if fingerprint != expected {
                    return Err(DomainError::DigestMismatch);
                }
            }
        }
        if self.parent_generation.as_ref() == Some(&self.generation_id) {
            return Err(DomainError::SelfSupersession);
        }
        if let Some(parent_generation) = &self.parent_generation {
            parent_generation.validate()?;
            generation_identity_kind(parent_generation)?;
        }
        validate_language_revisions(
            &self.grammar_revisions,
            "generation grammar revisions",
            |revision| revision.validate(),
        )?;
        validate_language_revisions(
            &self.extractor_revisions,
            "generation extractor revisions",
            |revision| revision.validate(),
        )?;
        if self
            .grammar_revisions
            .iter()
            .map(|(language, _)| language)
            .ne(self
                .extractor_revisions
                .iter()
                .map(|(language, _)| language))
        {
            return Err(DomainError::SnapshotMismatch {
                field: "generation language revision sets",
            });
        }
        Ok(())
    }
}

enum GenerationIdentityKind<'a> {
    Legacy,
    Fingerprinted(&'a str),
}

fn generation_identity_kind(
    generation_id: &CodeGenerationId,
) -> Result<GenerationIdentityKind<'_>, DomainError> {
    let mut parts = generation_id.as_str().split('.');
    let scheme = parts.next();
    let version = parts.next();
    let discriminator = parts.next();
    let sequence = parts.next();
    let fingerprint = parts.next();
    if scheme != Some("generation")
        || version != Some("v1")
        || parts.next().is_some()
        || discriminator.is_none_or(|value| !crate::canonical_text::is_lowercase_hex(value, 8))
        || sequence.is_none_or(|value| {
            value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(DomainError::NonCanonical {
            field: "code generation identity",
        });
    }
    match fingerprint {
        None => Ok(GenerationIdentityKind::Legacy),
        Some(value) if crate::canonical_text::is_lowercase_hex(value, 64) => {
            Ok(GenerationIdentityKind::Fingerprinted(value))
        }
        Some(_) => Err(DomainError::NonCanonical {
            field: "code generation identity fingerprint",
        }),
    }
}

fn validate_language_revisions<T>(
    revisions: &[(LanguageId, T)],
    field: &'static str,
    validate_revision: impl Fn(&T) -> Result<(), DomainError>,
) -> Result<(), DomainError> {
    for (language, revision) in revisions {
        language.validate()?;
        validate_revision(revision)?;
    }
    if revisions.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn snapshot() -> SanitizedCodeSnapshotV1 {
        SanitizedCodeSnapshotV1 {
            repository: id("repository.fixture"),
            worktree: Some(id("worktree.fixture")),
            reference: Some(id("ref.main")),
            source_revision: Some(id("commit.abc123")),
            sanitizer_revision: id("sanitizer.v1"),
            sanitization_receipts: vec![id("receipt.a"), id("receipt.b")],
            content_identity: id(&digest('a')),
            captured_at: UtcMicros(10),
            files: vec![
                SanitizedCodeFileV1 {
                    file_occurrence_id: id("file.a"),
                    logical_path: "src/a.rs".to_owned(),
                    language: Some(id("rust")),
                    content_digest: id(&digest('b')),
                    disposition: SnapshotFileDispositionV1::Present,
                },
                SanitizedCodeFileV1 {
                    file_occurrence_id: id("file.b"),
                    logical_path: "src/b.rs".to_owned(),
                    language: Some(id("rust")),
                    content_digest: id(&digest('c')),
                    disposition: SnapshotFileDispositionV1::Present,
                },
            ],
        }
    }

    fn generation_manifest() -> CodeGenerationManifestV1 {
        let mut manifest = CodeGenerationManifestV1 {
            project_id: id("project.fixture"),
            generation_id: id("generation.v1.aaaaaaaa.00000002"),
            snapshot_digest: id(&digest('a')),
            invalidation_digest: id(&digest('b')),
            registry_revision: id("registry.v1"),
            grammar_revisions: vec![
                (id("go"), id("grammar.go.v1")),
                (id("rust"), id("grammar.rust.v1")),
            ],
            extractor_revisions: vec![
                (id("go"), id("extractor.go.v1")),
                (id("rust"), id("extractor.rust.v1")),
            ],
            sanitizer_revision: id("sanitizer.v1"),
            chunker_revision: id("chunker.v1"),
            privacy_domain: id("privacy.fixture"),
            privacy_key_epoch: 1,
            parent_generation: Some(id("generation.v1.aaaaaaaa.00000001")),
            seal: GenerationSealV1 {
                expected_digest: id(&digest('d')),
                sealed_at: UtcMicros(20),
                planner: id("planner.v1"),
            },
        };
        manifest.invalidation_digest = manifest
            .expected_legacy_invalidation_digest()
            .expect("legacy invalidation digest");
        manifest
    }

    #[test]
    fn sanitized_snapshot_requires_canonical_receipts_files_and_paths() {
        snapshot().validate().expect("canonical snapshot");

        let mut duplicate_receipt = snapshot();
        duplicate_receipt
            .sanitization_receipts
            .push(id("receipt.b"));
        assert!(duplicate_receipt.validate().is_err());

        let mut reordered_files = snapshot();
        reordered_files.files.reverse();
        assert!(reordered_files.validate().is_err());

        let mut noncanonical_path = snapshot();
        noncanonical_path.files[0].logical_path = "./src/a.rs".to_owned();
        assert!(noncanonical_path.validate().is_err());
    }

    #[test]
    fn generation_manifest_requires_matching_canonical_language_revisions() {
        generation_manifest()
            .validate()
            .expect("canonical generation manifest");

        let mut reordered = generation_manifest();
        reordered.grammar_revisions.reverse();
        assert!(reordered.validate().is_err());

        let mut mismatched = generation_manifest();
        mismatched.extractor_revisions.pop();
        assert!(mismatched.validate().is_err());
    }
}

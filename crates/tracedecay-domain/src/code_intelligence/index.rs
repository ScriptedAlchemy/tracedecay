//! Generation, snapshot, relation, and test-attribution contracts shared
//! across the workspace.
//!
//! These are storage-neutral logical records. The index stores only typed
//! references to `GenerationDiagnosticV1` (`crate::diagnostics`) — never a
//! duplicate diagnostic record.
//!
//! Intake-rejection, extraction, and lineage records are owned by
//! `tracedecay-code-index`, whose ports produce and consume them, so edits to
//! those shapes do not invalidate every crate that depends on this one.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::id::{
    CommitId, ManifestDigest, PrivacyDomainId, ProjectId, RefId, RepositoryId, RetrievalAnchorId,
    SanitizationReceiptId, WorktreeId,
};
use crate::research::time::UtcMicros;
use crate::research::{DomainError, canonical_sha256};

use super::identity::{
    ChunkerRevision, CodeGenerationId, ContentDigest, ExtractorRevision, FileOccurrenceId,
    GrammarRevision, LanguageId, LanguageRegistryRevision, SanitizerRevision, SourceSpan,
    SymbolOccurrenceId,
};
use super::language::EdgeAuthorityV1;
use super::search::CodeGenerationSourceCommitmentsV1;

/// One receipt-bound sanitized repository snapshot — the only legal intake.
/// Carries repository, checkout, worktree, ref, source revision,
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
        validate_code_logical_path(&self.logical_path)?;
        if self.disposition == SnapshotFileDispositionV1::Present && self.language.is_none() {
            return Err(DomainError::UnknownReference {
                field: "present snapshot file language",
            });
        }
        Ok(())
    }
}

/// Validate the canonical repository-relative logical-path grammar shared by
/// sanitized snapshot files and production admission evidence.
pub fn validate_code_logical_path(logical_path: &str) -> Result<(), DomainError> {
    if logical_path.is_empty() {
        return Err(DomainError::Empty {
            field: "snapshot logical path",
        });
    }
    if logical_path.trim() != logical_path
        || logical_path.starts_with('/')
        || logical_path.contains('\\')
        || logical_path.chars().any(char::is_control)
        || logical_path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(DomainError::NonCanonical {
            field: "snapshot logical path",
        });
    }
    Ok(())
}

/// Explicit handling of deletions, renames, ignored, binary, generated, and
/// unsupported-language files.
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

/// The sealed manifest of one immutable logical generation. Generations are
/// planned, sealed, digested, and never mutated after publication.
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
    /// Source identities computed after canonical chunk materialization and
    /// authenticated by `seal.expected_digest`. `None` exists only while the
    /// production planner is materializing that source or when historical
    /// bytes predate source commitments; published readers reject it typed.
    pub source_commitments: Option<CodeGenerationSourceCommitmentsV1>,
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
    source_commitments: Option<CodeGenerationSourceCommitmentsV1>,
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
            invalidation_digest: match wire.invalidation_digest {
                Some(digest) => digest,
                None => ManifestDigest::zero().map_err(serde::de::Error::custom)?,
            },
            registry_revision: wire.registry_revision,
            grammar_revisions: wire.grammar_revisions,
            extractor_revisions: wire.extractor_revisions,
            sanitizer_revision: wire.sanitizer_revision,
            chunker_revision: wire.chunker_revision,
            privacy_domain: wire.privacy_domain,
            privacy_key_epoch: wire.privacy_key_epoch,
            parent_generation: wire.parent_generation,
            source_commitments: wire.source_commitments,
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

/// The seal applied before rows and the expected digest are handed to the
/// store publication port.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationSealV1 {
    pub expected_digest: ManifestDigest,
    pub sealed_at: UtcMicros,
    pub planner: GenerationPlannerIdV1,
}

/// Identity of the deterministic generation planner that produced a seal.
pub type GenerationPlannerIdV1 = crate::research::id::ComponentVersion;

/// One recorded relationship edge with its authority class. Every graph path
/// preserves its weakest edge authority; unresolved dispatch cannot become
/// semantic fact.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRelationEdgeV1 {
    pub from_occurrence: SymbolOccurrenceId,
    pub to_occurrence: SymbolOccurrenceId,
    pub kind: RelationEdgeKindV1,
    pub authority: EdgeAuthorityV1,
    pub evidence_span: SourceSpan,
}

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
    Returns,
    Receives,
}

/// A typed reference to the generation-bound diagnostic contract. The
/// diagnostic record itself is owned by `crate::diagnostics`; the index
/// stores only anchor-bound references.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationDiagnosticAttachmentV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
    /// Retrieval anchor addressing the diagnostic record.
    pub diagnostic_anchor: RetrievalAnchorId,
    pub content_digest: ContentDigest,
}

/// Test-attribution evidence for one generation. Map test definitions and
/// runs to the generation, source revision, and candidate production symbols
/// they cover; no candidate mode proves execution, correctness, or
/// universal safety.
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

/// The declared attribution evidence classes
/// (`conservative_dependency_candidates`, `observed_coverage_candidates`,
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
    /// at most one parent. Mixed-generation manifests are rejected before
    /// publication.
    pub fn validate(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.generation_id.validate()?;
        self.snapshot_digest.validate()?;
        self.invalidation_digest.validate()?;
        self.registry_revision.validate()?;
        self.sanitizer_revision.validate()?;
        self.chunker_revision.validate()?;
        self.privacy_domain.validate()?;
        if let Some(commitments) = &self.source_commitments {
            commitments.validate()?;
        }
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
            source_commitments: None,
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

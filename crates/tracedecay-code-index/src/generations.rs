//! Immutable generation planning and sealing (Plan 25, "Generations and
//! incremental reuse").
//!
//! One immutable logical generation is planned from one fenced, receipt-bound
//! snapshot. The planner mints monotonic per-repository generation identity,
//! pins the registry/grammar/extractor/sanitizer/chunker/privacy inputs, and
//! seals the manifest with [`expected_seal_digest`] — the exact rule the
//! capability emitter verifies — before rows and the expected digest are
//! handed to the store publication port. Planning is pure: the seal timestamp
//! is an input, so identical inputs produce an identical seal, and re-sealing
//! a sealed manifest reproduces its digest.
//!
//! Incremental reuse is planned, not assumed: given the prior sealed manifest,
//! the prior snapshot, and the current validated snapshot, the planner emits
//! the minimal re-extraction plan. Content identity digests are the reuse
//! authority — a file carries forward only when its content, grammar,
//! extractor, sanitizer, and chunker inputs all match; capture-declared
//! change hints are recorded as evidence but never override a digest.
//! Incompatible schema, grammar, extractor, sanitizer, chunker, or privacy
//! inputs force a full rebuild with typed triggers instead of disguising all
//! chunks as ordinary edits.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ChunkGenerationBindingV1, ChunkerRevision, CodeGenerationId, CodeGenerationManifestV1,
    CodeSearchChunkId, CodeSearchChunkV1, CodeSearchDocumentV1, ComponentVersion, ContentDigest,
    ExtractorRevision, FileOccurrenceId, GenerationSealV1, GrammarRevision, LanguageId,
    LanguageRegistryRevision, ManifestDigest, PrivacyDomainId, RepositoryId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SnapshotFileDispositionV1, UtcMicros, ValidatedCodeSnapshotV1,
    canonical_sha256,
};

use super::capabilities::expected_seal_digest;
use super::intake::INTAKE_DIGEST_SEPARATOR;
use super::languages::LanguageRegistry;

/// Identity of this deterministic generation planner, recorded on every seal.
pub const GENERATION_PLANNER_ID: &str = "code-index-generation-planner.v1";

/// Domain separator for per-repository generation-identity discriminators.
pub const GENERATION_ID_SEPARATOR: &str = "tracedecay.code-generation-id.v1";

/// Domain separator for immutable generation invalidation inputs.
pub const GENERATION_INVALIDATION_SEPARATOR: &str = "tracedecay.code-generation-invalidation.v1";

/// Generation-planning failures.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenerationPlanningErrorV1 {
    #[error("the parent generation manifest is not sealed")]
    UnsealedParent,
    #[error("the parent generation identity is foreign to this repository planner")]
    ForeignParentIdentity,
    #[error("no language descriptor is registered for {0}")]
    RegistryMiss(LanguageId),
    #[error("the prior snapshot does not match the prior generation manifest")]
    PriorSnapshotMismatch,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// Why an incremental plan escalated to a full rebuild (Plan 25: incompatible
/// schema, grammar, identity, or privacy changes force a full rebuild with a
/// declared reason).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "trigger", content = "detail", rename_all = "snake_case")]
pub enum RebuildTriggerV1 {
    /// A present language's grammar revision changed.
    GrammarRevision(LanguageId),
    /// A present language's extractor revision changed.
    ExtractorRevision(LanguageId),
    /// The sanitizer revision changed.
    SanitizerRevision,
    /// The chunker revision changed.
    ChunkerRevision,
    /// The privacy domain changed.
    PrivacyDomain,
    /// The privacy key epoch changed.
    PrivacyKeyEpoch,
}

/// What the next generation does with one logical path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", content = "detail", rename_all = "snake_case")]
pub enum FileExtractionActionV1 {
    /// Content identity matched the prior generation: the current file
    /// occurrence reuses the prior extraction evidence by identity digest.
    CarryForward {
        /// The file occurrence in the current snapshot.
        file_occurrence_id: FileOccurrenceId,
        /// The prior file occurrence whose extraction evidence is reused.
        prior_file_occurrence_id: FileOccurrenceId,
        /// The shared content identity digest.
        content_digest: ContentDigest,
    },
    /// The file must be (re-)extracted: new, changed, or previously not
    /// present as extractable source.
    ReExtract { file: SanitizedCodeFileV1 },
    /// The file was extractable in the prior generation and is gone (or no
    /// longer present) in the current snapshot.
    Deleted {
        prior_file_occurrence_id: FileOccurrenceId,
    },
}

/// The planned disposition of one logical path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileExtractionPlanV1 {
    pub logical_path: String,
    pub action: FileExtractionActionV1,
}

/// The minimal re-extraction plan from one prior generation to the current
/// snapshot (Plan 25: reparse only changed sanitized content or descriptor
/// inputs). Content identity digests are the reuse authority; the capture-
/// declared changed-file set is recorded as evidence only.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationIncrementPlanV1 {
    pub prior_generation: CodeGenerationId,
    /// Empty when the plan is incremental; non-empty triggers mean every
    /// present file re-extracts (a declared full rebuild).
    pub rebuild_triggers: Vec<RebuildTriggerV1>,
    /// Complete canonical invalidation inputs. Capture hints are deliberately
    /// excluded because they are evidence, not reuse authority.
    pub invalidation_digest: ManifestDigest,
    /// Canonically ordered by logical path.
    pub files: Vec<FileExtractionPlanV1>,
    /// The capture-declared changed logical paths, recorded as evidence.
    /// Digest comparison, not this hint, decides reuse.
    pub capture_changed_files: Vec<String>,
    pub carried_forward: u64,
    pub reextract: u64,
    pub deleted: u64,
}

impl GenerationIncrementPlanV1 {
    /// Whether this plan is a declared full rebuild.
    pub fn is_full_rebuild(&self) -> bool {
        !self.rebuild_triggers.is_empty()
    }
}

#[derive(Serialize)]
struct GenerationInvalidationDigestInput<'a> {
    domain: &'static str,
    repository: &'a RepositoryId,
    parent_generation: Option<&'a CodeGenerationId>,
    parent_publication_fence: Option<&'a ManifestDigest>,
    snapshot_digest: &'a ManifestDigest,
    registry_revision: &'a LanguageRegistryRevision,
    grammar_revisions: &'a [(LanguageId, GrammarRevision)],
    extractor_revisions: &'a [(LanguageId, ExtractorRevision)],
    sanitizer_revision: &'a tracedecay_domain::SanitizerRevision,
    chunker_revision: &'a ChunkerRevision,
    privacy_domain: &'a PrivacyDomainId,
    privacy_key_epoch: u64,
    planner: &'a ComponentVersion,
    rebuild_triggers: &'a [RebuildTriggerV1],
}

/// The deterministic generation planner. One planner is bound to one
/// repository, one language registry, and one pinned chunker/privacy
/// configuration; generation identity is monotonic within that binding.
pub struct GenerationPlanner<R: LanguageRegistry> {
    repository: RepositoryId,
    registry: R,
    chunker_revision: ChunkerRevision,
    privacy_domain: PrivacyDomainId,
    privacy_key_epoch: u64,
    planner: ComponentVersion,
}

impl<R: LanguageRegistry> GenerationPlanner<R> {
    /// Create a planner for one repository with pinned chunker and privacy
    /// inputs.
    pub fn new(
        repository: RepositoryId,
        registry: R,
        chunker_revision: ChunkerRevision,
        privacy_domain: PrivacyDomainId,
        privacy_key_epoch: u64,
    ) -> Self {
        Self {
            repository,
            registry,
            chunker_revision,
            privacy_domain,
            privacy_key_epoch,
            planner: ComponentVersion::new(GENERATION_PLANNER_ID)
                .expect("generation planner identity is canonical"),
        }
    }

    /// The repository this planner mints generation identity for.
    pub fn repository(&self) -> &RepositoryId {
        &self.repository
    }

    /// The language registry backing revision pins.
    pub fn registry(&self) -> &R {
        &self.registry
    }

    /// The per-repository discriminator binding generation identity to this
    /// planner's repository.
    fn discriminator(&self) -> Result<String, GenerationPlanningErrorV1> {
        repository_discriminator(&self.repository)
    }

    /// Mint the next monotonic, collision-resistant generation identity:
    /// `generation.v1.<repo>.<sequence>.<invalidation-digest>`. The parent
    /// must carry this planner's repository discriminator.
    pub fn next_generation_id(
        &self,
        parent: Option<&CodeGenerationId>,
        invalidation_digest: &ManifestDigest,
    ) -> Result<CodeGenerationId, GenerationPlanningErrorV1> {
        invalidation_digest
            .validate()
            .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))?;
        let discriminator = self.discriminator()?;
        let sequence = match parent {
            None => 1,
            Some(parent) => {
                let (_, parent_sequence) = parse_minted_generation_id(parent)
                    .filter(|(minted, _)| *minted == discriminator)
                    .ok_or(GenerationPlanningErrorV1::ForeignParentIdentity)?;
                parent_sequence.checked_add(1).ok_or_else(|| {
                    GenerationPlanningErrorV1::Contract("generation sequence overflow".to_owned())
                })?
            }
        };
        let fingerprint = invalidation_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                GenerationPlanningErrorV1::Contract(
                    "invalidation digest is not a sha256 manifest digest".to_owned(),
                )
            })?;
        CodeGenerationId::new(format!(
            "generation.v1.{discriminator}.{sequence:08}.{fingerprint}"
        ))
        .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))
    }

    /// Plan and seal one immutable generation from one validated snapshot.
    ///
    /// `sealed_at` is an input (never a wall-clock read), so identical inputs
    /// produce an identical manifest and seal. When `parent` is supplied it
    /// must be sealed and minted by this planner's repository chain; the new
    /// manifest records it as `parent_generation`.
    pub fn plan_generation(
        &self,
        snapshot: &ValidatedCodeSnapshotV1,
        parent: Option<&CodeGenerationManifestV1>,
        sealed_at: UtcMicros,
    ) -> Result<CodeGenerationManifestV1, GenerationPlanningErrorV1> {
        self.plan_generation_with_invalidation(snapshot, parent, &BTreeSet::new(), sealed_at)
    }

    /// Plan and seal one immutable generation while binding every inferred and
    /// explicitly declared rebuild cause into its identity and publication
    /// fence.
    pub fn plan_generation_with_invalidation(
        &self,
        snapshot: &ValidatedCodeSnapshotV1,
        parent: Option<&CodeGenerationManifestV1>,
        invalidations: &BTreeSet<RebuildTriggerV1>,
        sealed_at: UtcMicros,
    ) -> Result<CodeGenerationManifestV1, GenerationPlanningErrorV1> {
        if snapshot.snapshot.repository != self.repository {
            return Err(GenerationPlanningErrorV1::Contract(
                "snapshot repository does not match the planner repository".to_owned(),
            ));
        }
        if let Some(parent) = parent {
            self.verified_parent(parent)?;
        }
        let parent_generation = parent.map(|parent| parent.generation_id.clone());
        let (grammar_revisions, extractor_revisions) =
            self.language_revisions(&snapshot.snapshot)?;
        let registry_revision = self.registry.registry_revision();
        let mut rebuild_triggers = parent
            .map(|parent| self.rebuild_triggers(parent, &snapshot.snapshot))
            .unwrap_or_default();
        rebuild_triggers.extend(invalidations.iter().cloned());
        rebuild_triggers.sort();
        rebuild_triggers.dedup();
        let invalidation_digest = self.invalidation_digest(
            parent,
            snapshot,
            &registry_revision,
            &grammar_revisions,
            &extractor_revisions,
            &rebuild_triggers,
        )?;
        let generation_id =
            self.next_generation_id(parent_generation.as_ref(), &invalidation_digest)?;

        let mut manifest = CodeGenerationManifestV1 {
            generation_id,
            snapshot_digest: snapshot.intake_digest.clone(),
            invalidation_digest,
            registry_revision,
            grammar_revisions,
            extractor_revisions,
            sanitizer_revision: snapshot.snapshot.sanitizer_revision.clone(),
            chunker_revision: self.chunker_revision.clone(),
            privacy_domain: self.privacy_domain.clone(),
            privacy_key_epoch: self.privacy_key_epoch,
            parent_generation,
            seal: GenerationSealV1 {
                expected_digest: placeholder_digest(),
                sealed_at,
                planner: self.planner.clone(),
            },
        };
        manifest.seal.expected_digest = expected_seal_digest(&manifest)
            .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))?;
        manifest
            .validate()
            .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))?;
        Ok(manifest)
    }

    /// Plan the minimal re-extraction work from one prior sealed generation
    /// to the current validated snapshot (Plan 25: reuse file and symbol
    /// results only when content, grammar, extractor, identity, and sanitizer
    /// inputs match).
    ///
    /// `changed_files` is the capture-declared changed logical-path set. It is
    /// recorded as evidence; content identity digests alone decide reuse, so a
    /// hinted-but-identical file carries forward and an unhinted-but-changed
    /// file still re-extracts.
    pub fn plan_increment(
        &self,
        prior_manifest: &CodeGenerationManifestV1,
        prior_snapshot: &SanitizedCodeSnapshotV1,
        current: &ValidatedCodeSnapshotV1,
        changed_files: &BTreeSet<String>,
    ) -> Result<GenerationIncrementPlanV1, GenerationPlanningErrorV1> {
        self.plan_increment_with_invalidation(
            prior_manifest,
            prior_snapshot,
            current,
            changed_files,
            &BTreeSet::new(),
        )
    }

    /// Plan an increment with additional conservative invalidations supplied
    /// by the owning application boundary. Schema/identity revisions and
    /// quarantined corruption are not inferable from a sanitized snapshot,
    /// so callers must declare them explicitly. Declared reasons are merged
    /// with descriptor, sanitizer, chunker, and privacy incompatibilities.
    pub fn plan_increment_with_invalidation(
        &self,
        prior_manifest: &CodeGenerationManifestV1,
        prior_snapshot: &SanitizedCodeSnapshotV1,
        current: &ValidatedCodeSnapshotV1,
        changed_files: &BTreeSet<String>,
        invalidations: &BTreeSet<RebuildTriggerV1>,
    ) -> Result<GenerationIncrementPlanV1, GenerationPlanningErrorV1> {
        let prior_generation = self.verified_parent(prior_manifest)?;
        let prior_snapshot_digest = canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, prior_snapshot))
            .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))?;
        if prior_manifest.snapshot_digest != prior_snapshot_digest {
            return Err(GenerationPlanningErrorV1::PriorSnapshotMismatch);
        }

        let mut rebuild_triggers = self.rebuild_triggers(prior_manifest, &current.snapshot);
        rebuild_triggers.extend(invalidations.iter().cloned());
        rebuild_triggers.sort();
        rebuild_triggers.dedup();
        let (grammar_revisions, extractor_revisions) =
            self.language_revisions(&current.snapshot)?;
        let registry_revision = self.registry.registry_revision();
        let invalidation_digest = self.invalidation_digest(
            Some(prior_manifest),
            current,
            &registry_revision,
            &grammar_revisions,
            &extractor_revisions,
            &rebuild_triggers,
        )?;
        let full_rebuild = !rebuild_triggers.is_empty();

        let mut plans: Vec<FileExtractionPlanV1> = Vec::new();
        let mut carried_forward = 0_u64;
        let mut reextract = 0_u64;
        let mut deleted = 0_u64;
        let prior_files_by_path: BTreeMap<&str, &SanitizedCodeFileV1> = prior_snapshot
            .files
            .iter()
            .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
            .map(|file| (file.logical_path.as_str(), file))
            .collect();
        let current_paths: BTreeSet<&str> = current
            .snapshot
            .files
            .iter()
            .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
            .map(|file| file.logical_path.as_str())
            .collect();

        for file in &current.snapshot.files {
            if file.disposition != SnapshotFileDispositionV1::Present {
                continue;
            }
            let prior_file = prior_files_by_path.get(file.logical_path.as_str()).copied();
            let action = match prior_file {
                Some(prior) if !full_rebuild && prior.content_digest == file.content_digest => {
                    carried_forward += 1;
                    FileExtractionActionV1::CarryForward {
                        file_occurrence_id: file.file_occurrence_id.clone(),
                        prior_file_occurrence_id: prior.file_occurrence_id.clone(),
                        content_digest: file.content_digest.clone(),
                    }
                }
                _ => {
                    reextract += 1;
                    FileExtractionActionV1::ReExtract { file: file.clone() }
                }
            };
            plans.push(FileExtractionPlanV1 {
                logical_path: file.logical_path.clone(),
                action,
            });
        }
        for prior in &prior_snapshot.files {
            if prior.disposition != SnapshotFileDispositionV1::Present {
                continue;
            }
            if !current_paths.contains(prior.logical_path.as_str()) {
                deleted += 1;
                plans.push(FileExtractionPlanV1 {
                    logical_path: prior.logical_path.clone(),
                    action: FileExtractionActionV1::Deleted {
                        prior_file_occurrence_id: prior.file_occurrence_id.clone(),
                    },
                });
            }
        }
        plans.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));

        Ok(GenerationIncrementPlanV1 {
            prior_generation,
            rebuild_triggers,
            invalidation_digest,
            files: plans,
            capture_changed_files: changed_files.iter().cloned().collect(),
            carried_forward,
            reextract,
            deleted,
        })
    }

    /// Verify a claimed parent/prior manifest: it must validate, carry this
    /// planner's minted identity, and be sealed with the published rule.
    fn verified_parent(
        &self,
        parent: &CodeGenerationManifestV1,
    ) -> Result<CodeGenerationId, GenerationPlanningErrorV1> {
        parent
            .validate()
            .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))?;
        let discriminator = self.discriminator()?;
        if parse_minted_generation_id(&parent.generation_id)
            .is_none_or(|(minted, _)| minted != discriminator)
        {
            return Err(GenerationPlanningErrorV1::ForeignParentIdentity);
        }
        let expected = expected_seal_digest(parent)
            .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))?;
        if parent.seal.expected_digest != expected {
            return Err(GenerationPlanningErrorV1::UnsealedParent);
        }
        Ok(parent.generation_id.clone())
    }

    /// Collect the grammar/extractor revision pins for the languages present
    /// as extractable source in one snapshot, in canonical language order.
    #[allow(clippy::type_complexity)]
    fn language_revisions(
        &self,
        snapshot: &SanitizedCodeSnapshotV1,
    ) -> Result<
        (
            Vec<(LanguageId, GrammarRevision)>,
            Vec<(LanguageId, ExtractorRevision)>,
        ),
        GenerationPlanningErrorV1,
    > {
        let mut languages = BTreeSet::new();
        for file in &snapshot.files {
            if file.disposition == SnapshotFileDispositionV1::Present {
                let language = file.language.clone().ok_or_else(|| {
                    GenerationPlanningErrorV1::Contract(
                        "present snapshot file without language".to_owned(),
                    )
                })?;
                languages.insert(language);
            }
        }
        let mut grammar_revisions = Vec::with_capacity(languages.len());
        let mut extractor_revisions = Vec::with_capacity(languages.len());
        for language in languages {
            let descriptor = self
                .registry
                .descriptor(&language)
                .ok_or_else(|| GenerationPlanningErrorV1::RegistryMiss(language.clone()))?;
            grammar_revisions.push((language.clone(), descriptor.grammar_revision.clone()));
            extractor_revisions.push((language, descriptor.extractor_revision.clone()));
        }
        Ok((grammar_revisions, extractor_revisions))
    }

    fn invalidation_digest(
        &self,
        parent: Option<&CodeGenerationManifestV1>,
        snapshot: &ValidatedCodeSnapshotV1,
        registry_revision: &LanguageRegistryRevision,
        grammar_revisions: &[(LanguageId, GrammarRevision)],
        extractor_revisions: &[(LanguageId, ExtractorRevision)],
        rebuild_triggers: &[RebuildTriggerV1],
    ) -> Result<ManifestDigest, GenerationPlanningErrorV1> {
        canonical_sha256(&GenerationInvalidationDigestInput {
            domain: GENERATION_INVALIDATION_SEPARATOR,
            repository: &self.repository,
            parent_generation: parent.map(|parent| &parent.generation_id),
            parent_publication_fence: parent.map(|parent| &parent.seal.expected_digest),
            snapshot_digest: &snapshot.intake_digest,
            registry_revision,
            grammar_revisions,
            extractor_revisions,
            sanitizer_revision: &snapshot.snapshot.sanitizer_revision,
            chunker_revision: &self.chunker_revision,
            privacy_domain: &self.privacy_domain,
            privacy_key_epoch: self.privacy_key_epoch,
            planner: &self.planner,
            rebuild_triggers,
        })
        .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))
    }

    /// Field-specific invalidation (Plan 25): grammar, extractor, sanitizer,
    /// chunker, or privacy changes on present inputs force a full rebuild.
    /// A registry-revision change that leaves every present language's
    /// grammar and extractor revisions untouched forces nothing.
    fn rebuild_triggers(
        &self,
        prior_manifest: &CodeGenerationManifestV1,
        current: &SanitizedCodeSnapshotV1,
    ) -> Vec<RebuildTriggerV1> {
        let mut triggers = Vec::new();
        if current.sanitizer_revision != prior_manifest.sanitizer_revision {
            triggers.push(RebuildTriggerV1::SanitizerRevision);
        }
        if self.chunker_revision != prior_manifest.chunker_revision {
            triggers.push(RebuildTriggerV1::ChunkerRevision);
        }
        if self.privacy_domain != prior_manifest.privacy_domain {
            triggers.push(RebuildTriggerV1::PrivacyDomain);
        }
        if self.privacy_key_epoch != prior_manifest.privacy_key_epoch {
            triggers.push(RebuildTriggerV1::PrivacyKeyEpoch);
        }
        for file in &current.files {
            if file.disposition != SnapshotFileDispositionV1::Present {
                continue;
            }
            let Some(language) = &file.language else {
                continue;
            };
            let Some(descriptor) = self.registry.descriptor(language) else {
                continue;
            };
            let prior_grammar = prior_manifest
                .grammar_revisions
                .iter()
                .find(|(pinned, _)| pinned == language)
                .map(|(_, revision)| revision);
            if prior_grammar.is_some_and(|pinned| *pinned != descriptor.grammar_revision) {
                triggers.push(RebuildTriggerV1::GrammarRevision(language.clone()));
            }
            let prior_extractor = prior_manifest
                .extractor_revisions
                .iter()
                .find(|(pinned, _)| pinned == language)
                .map(|(_, revision)| revision);
            if prior_extractor.is_some_and(|pinned| *pinned != descriptor.extractor_revision) {
                triggers.push(RebuildTriggerV1::ExtractorRevision(language.clone()));
            }
        }
        triggers.sort();
        triggers.dedup();
        triggers
    }
}

/// The per-repository discriminator segment of minted generation identity.
fn repository_discriminator(
    repository: &RepositoryId,
) -> Result<String, GenerationPlanningErrorV1> {
    let digest = canonical_sha256(&(GENERATION_ID_SEPARATOR, repository.as_str()))
        .map_err(|error| GenerationPlanningErrorV1::Contract(error.to_string()))?;
    Ok(digest
        .as_str()
        .trim_start_matches("sha256:")
        .chars()
        .take(8)
        .collect())
}

/// Parse an identity minted by this planner. Legacy
/// `generation.v1.<repo>.<seq>` parents remain accepted; current identities
/// include a SHA-256 invalidation fingerprint suffix.
fn parse_minted_generation_id(generation_id: &CodeGenerationId) -> Option<(String, u64)> {
    let mut parts = generation_id.as_str().split('.');
    let scheme = parts.next()?;
    let version = parts.next()?;
    let discriminator = parts.next()?;
    let sequence = parts.next()?;
    let fingerprint = parts.next();
    if scheme != "generation" || version != "v1" || parts.next().is_some() {
        return None;
    }
    if fingerprint.is_some_and(|fingerprint| {
        fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return None;
    }
    Some((discriminator.to_owned(), sequence.parse().ok()?))
}

/// A well-formed placeholder digest, replaced by the computed seal before the
/// manifest is returned. The seal payload excludes the seal itself, so the
/// placeholder never influences the computed digest.
fn placeholder_digest() -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
        .expect("a zeroed sha256 digest is canonical")
}

/// Generation-aware join failures (Plan 25: consumers reject mixed-generation
/// inputs before candidate production).
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenerationJoinErrorV1 {
    #[error("the generation manifest is not sealed")]
    UnsealedGeneration,
    #[error("a document or chunk belongs to a different generation or file occurrence")]
    CrossGeneration,
    #[error("chunk {0} is not declared by the generation document")]
    UndeclaredChunk(CodeSearchChunkId),
    #[error("chunk {0} is joined more than once")]
    DuplicateChunk(CodeSearchChunkId),
}

/// Join one generation-bound document's chunks to a sealed generation
/// manifest, producing the canonical chunk-to-generation bindings (Plan 25:
/// every eligible chunk names exactly one code generation and file
/// occurrence). Cross-generation documents or chunks, undeclared chunks, and
/// duplicates are typed rejections — never silently joined.
pub fn join_chunks_to_generation(
    generation: &CodeGenerationManifestV1,
    document: &CodeSearchDocumentV1,
    chunks: &[CodeSearchChunkV1],
) -> Result<Vec<ChunkGenerationBindingV1>, GenerationJoinErrorV1> {
    generation
        .validate()
        .map_err(|_| GenerationJoinErrorV1::UnsealedGeneration)?;
    let expected =
        expected_seal_digest(generation).map_err(|_| GenerationJoinErrorV1::UnsealedGeneration)?;
    if generation.seal.expected_digest != expected {
        return Err(GenerationJoinErrorV1::UnsealedGeneration);
    }
    if document.generation_id != generation.generation_id {
        return Err(GenerationJoinErrorV1::CrossGeneration);
    }

    let declared: BTreeSet<&CodeSearchChunkId> = document.chunk_ids.iter().collect();
    let mut seen = BTreeSet::new();
    let mut bindings = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.anchor.generation_id != generation.generation_id
            || chunk.anchor.file_occurrence_id != document.file_occurrence_id
        {
            return Err(GenerationJoinErrorV1::CrossGeneration);
        }
        if !declared.contains(&chunk.id) {
            return Err(GenerationJoinErrorV1::UndeclaredChunk(chunk.id.clone()));
        }
        if !seen.insert(chunk.id.clone()) {
            return Err(GenerationJoinErrorV1::DuplicateChunk(chunk.id.clone()));
        }
        bindings.push(ChunkGenerationBindingV1 {
            chunk_id: chunk.id.clone(),
            generation_id: generation.generation_id.clone(),
            file_occurrence_id: document.file_occurrence_id.clone(),
        });
    }
    bindings.sort();
    Ok(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        BoundedSanitizedText, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
        CodeSearchEligibilityV1, LanguageDescriptorRevision, PolicyRevisionId,
        SanitizationReceiptId, SensitivityDecision, SensitivityLevelV1, SourceSpan,
    };

    use crate::capabilities::{BaseCapabilityEmitter, CodeIndexCapabilityEmitter};
    use crate::languages::StaticLanguageRegistry;
    use tracedecay_domain::{CoverageSummaryV1, LanguageDescriptorV1};

    fn digest(byte: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid digest")
    }

    fn manifest_digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("valid digest")
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn repository() -> RepositoryId {
        id("repository.fixture")
    }

    fn rust_descriptor() -> LanguageDescriptorV1 {
        LanguageDescriptorV1 {
            language: id("rust"),
            descriptor_revision: id("descriptor.rust.v1"),
            grammar_revision: id("grammar.tree-sitter.rust.v1"),
            extractor_revision: id("extractor.rust.v1"),
            aliases: vec!["rs".to_owned(), "rust".to_owned()],
            extensions: vec!["rs".to_owned()],
            root_markers: vec!["Cargo.toml".to_owned()],
            expando: tracedecay_domain::ExpandoBehaviorV1::MarkGenerated,
            stable_member_spans: true,
            capabilities: tracedecay_domain::LanguageCapabilitySetV1::default(),
        }
    }

    fn registry() -> StaticLanguageRegistry {
        StaticLanguageRegistry::from_descriptors(vec![rust_descriptor()])
    }

    fn present_file(occurrence: &str, path: &str, content: char) -> SanitizedCodeFileV1 {
        SanitizedCodeFileV1 {
            file_occurrence_id: id(occurrence),
            logical_path: path.to_owned(),
            language: Some(id("rust")),
            content_digest: digest(content),
            disposition: SnapshotFileDispositionV1::Present,
        }
    }

    fn snapshot(files: Vec<SanitizedCodeFileV1>) -> SanitizedCodeSnapshotV1 {
        SanitizedCodeSnapshotV1 {
            repository: repository(),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: id("sanitizer.v1"),
            sanitization_receipts: vec![id("receipt.a")],
            content_identity: digest('f'),
            captured_at: UtcMicros(1_000),
            files,
        }
    }

    fn validated(snapshot: SanitizedCodeSnapshotV1) -> ValidatedCodeSnapshotV1 {
        let intake_digest =
            canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, &snapshot)).expect("intake digest");
        ValidatedCodeSnapshotV1 {
            snapshot,
            intake_digest,
            validated_at: UtcMicros(2_000),
        }
    }

    fn planner() -> GenerationPlanner<StaticLanguageRegistry> {
        GenerationPlanner::new(
            repository(),
            registry(),
            id("chunker.v1"),
            id("privacy.fixture"),
            7,
        )
    }

    fn two_file_snapshot() -> SanitizedCodeSnapshotV1 {
        snapshot(vec![
            present_file("file.a", "src/a.rs", 'a'),
            present_file("file.b", "src/b.rs", 'b'),
        ])
    }

    #[test]
    fn generation_identity_is_monotonic_per_repository() {
        let snapshot = validated(two_file_snapshot());
        let planner = planner();

        let genesis = planner
            .plan_generation(&snapshot, None, UtcMicros(3_000))
            .expect("genesis generation");
        let child = planner
            .plan_generation(&snapshot, Some(&genesis), UtcMicros(4_000))
            .expect("child generation");
        let grandchild = planner
            .plan_generation(&snapshot, Some(&child), UtcMicros(5_000))
            .expect("grandchild generation");

        assert!(genesis.generation_id.as_str().contains(".00000001."));
        assert!(child.generation_id.as_str().contains(".00000002."));
        assert!(grandchild.generation_id.as_str().contains(".00000003."));
        // Zero-padded sequences keep lexicographic order aligned with the
        // monotonic sequence.
        assert!(genesis.generation_id < child.generation_id);
        assert!(child.generation_id < grandchild.generation_id);
        assert_eq!(
            child.parent_generation.as_ref(),
            Some(&genesis.generation_id)
        );
        assert_eq!(
            grandchild.parent_generation.as_ref(),
            Some(&child.generation_id)
        );

        // A different repository has its own discriminator and its own
        // monotonic sequence starting at one.
        let other_planner = GenerationPlanner::new(
            id("repository.other"),
            registry(),
            id("chunker.v1"),
            id("privacy.fixture"),
            7,
        );
        let other_snapshot = validated(SanitizedCodeSnapshotV1 {
            repository: id("repository.other"),
            ..two_file_snapshot()
        });
        let other_genesis = other_planner
            .plan_generation(&other_snapshot, None, UtcMicros(3_000))
            .expect("other repository genesis");
        assert!(other_genesis.generation_id.as_str().contains(".00000001."));
        assert_ne!(other_genesis.generation_id, genesis.generation_id);
    }

    #[test]
    fn seal_is_deterministic_and_resealing_reproduces_the_digest() {
        let snapshot = validated(two_file_snapshot());
        let planner = planner();

        let first = planner
            .plan_generation(&snapshot, None, UtcMicros(3_000))
            .expect("first planning");
        let second = planner
            .plan_generation(&snapshot, None, UtcMicros(3_000))
            .expect("second planning");
        // Identical inputs (including the supplied seal timestamp) produce an
        // identical manifest and seal.
        assert_eq!(first, second);

        // The seal satisfies the published emitter rule, and re-sealing the
        // sealed manifest reproduces the identical digest.
        let expected = expected_seal_digest(&first).expect("seal digest recomputes");
        assert_eq!(first.seal.expected_digest, expected);

        // The seal covers generation inputs: a different parent chain or
        // snapshot produces a different digest.
        let child = planner
            .plan_generation(&snapshot, Some(&first), UtcMicros(3_000))
            .expect("child generation");
        assert_ne!(child.seal.expected_digest, first.seal.expected_digest);
    }

    #[test]
    fn emitter_accepts_planner_sealed_generations() {
        let snapshot = validated(two_file_snapshot());
        let planner = planner();
        let generation = planner
            .plan_generation(&snapshot, None, UtcMicros(3_000))
            .expect("sealed generation");

        let emitter = BaseCapabilityEmitter::new(
            registry(),
            CoverageSummaryV1 {
                files_eligible: 2,
                files_excluded: 0,
                files_partial: 0,
                files_unsupported: 0,
                ranges_excluded: 0,
                ranges_unsupported: 0,
            },
            vec![SanitizationReceiptId::new("receipt.a").expect("valid id")],
        );
        // The capability emitter's seal verification is the acceptance gate:
        // it emits only for a correctly sealed generation.
        let manifest = emitter.emit(&generation).expect("emission succeeds");
        assert_eq!(manifest.generation_id, generation.generation_id);
        assert_eq!(manifest.chunker_revision, generation.chunker_revision);
    }

    #[test]
    fn planning_rejects_unsealed_and_foreign_parents() {
        let snapshot = validated(two_file_snapshot());
        let planner = planner();
        let genesis = planner
            .plan_generation(&snapshot, None, UtcMicros(3_000))
            .expect("genesis");

        // Unsealed parent: the seal no longer matches the manifest inputs.
        let mut unsealed = genesis.clone();
        unsealed.seal.expected_digest = manifest_digest('9');
        assert_eq!(
            planner.plan_generation(&snapshot, Some(&unsealed), UtcMicros(4_000)),
            Err(GenerationPlanningErrorV1::UnsealedParent)
        );

        // Foreign parent: an identity this planner never minted.
        let mut foreign = genesis.clone();
        foreign.generation_id = id("generation.v1.00000002.00000001");
        foreign.parent_generation = None;
        foreign.invalidation_digest = foreign
            .expected_legacy_invalidation_digest()
            .expect("foreign invalidation digest");
        foreign.seal.expected_digest = expected_seal_digest(&foreign).expect("foreign reseal");
        assert_eq!(
            planner.plan_generation(&snapshot, Some(&foreign), UtcMicros(4_000)),
            Err(GenerationPlanningErrorV1::ForeignParentIdentity)
        );

        // Another repository's minted identity is also foreign here.
        let other_planner = GenerationPlanner::new(
            id("repository.other"),
            registry(),
            id("chunker.v1"),
            id("privacy.fixture"),
            7,
        );
        let other_snapshot = validated(SanitizedCodeSnapshotV1 {
            repository: id("repository.other"),
            ..two_file_snapshot()
        });
        let other_genesis = other_planner
            .plan_generation(&other_snapshot, None, UtcMicros(3_000))
            .expect("other genesis");
        assert_eq!(
            planner.plan_generation(&snapshot, Some(&other_genesis), UtcMicros(4_000)),
            Err(GenerationPlanningErrorV1::ForeignParentIdentity)
        );
    }

    #[test]
    fn increment_plan_carries_forward_unchanged_files_by_identity_digest() {
        let planner = planner();
        let prior_snapshot = two_file_snapshot();
        let prior_validated = validated(prior_snapshot.clone());
        let prior_manifest = planner
            .plan_generation(&prior_validated, None, UtcMicros(3_000))
            .expect("prior generation");

        // src/a.rs unchanged, src/b.rs content changed, src/c.rs new.
        let current = validated(snapshot(vec![
            present_file("file.a2", "src/a.rs", 'a'),
            present_file("file.b2", "src/b.rs", 'c'),
            present_file("file.c", "src/c.rs", 'd'),
        ]));
        let changed: BTreeSet<String> = ["src/b.rs".to_owned(), "src/c.rs".to_owned()]
            .into_iter()
            .collect();
        let plan = planner
            .plan_increment(&prior_manifest, &prior_snapshot, &current, &changed)
            .expect("increment plan");

        assert!(!plan.is_full_rebuild());
        assert_eq!(plan.prior_generation, prior_manifest.generation_id);
        assert_eq!(plan.carried_forward, 1);
        assert_eq!(plan.reextract, 2);
        assert_eq!(plan.deleted, 0);
        assert_eq!(
            plan.capture_changed_files,
            vec!["src/b.rs".to_owned(), "src/c.rs".to_owned()]
        );

        let carry = &plan.files[0];
        assert_eq!(carry.logical_path, "src/a.rs");
        match &carry.action {
            FileExtractionActionV1::CarryForward {
                file_occurrence_id,
                prior_file_occurrence_id,
                content_digest,
            } => {
                assert_eq!(file_occurrence_id.as_str(), "file.a2");
                assert_eq!(prior_file_occurrence_id.as_str(), "file.a");
                assert_eq!(*content_digest, digest('a'));
            }
            other => panic!("expected carry-forward, got {other:?}"),
        }
        assert!(matches!(
            plan.files[1].action,
            FileExtractionActionV1::ReExtract { .. }
        ));
        assert!(matches!(
            plan.files[2].action,
            FileExtractionActionV1::ReExtract { .. }
        ));

        // src/b.rs removed and src/a.rs unchanged: one deletion, one carry.
        let shrunk = validated(snapshot(vec![present_file("file.a3", "src/a.rs", 'a')]));
        let shrink_plan = planner
            .plan_increment(&prior_manifest, &prior_snapshot, &shrunk, &BTreeSet::new())
            .expect("shrink plan");
        assert_eq!(shrink_plan.carried_forward, 1);
        assert_eq!(shrink_plan.deleted, 1);
        assert!(matches!(
            shrink_plan.files[1].action,
            FileExtractionActionV1::Deleted {
                ref prior_file_occurrence_id
            } if prior_file_occurrence_id.as_str() == "file.b"
        ));
    }

    #[test]
    fn increment_plan_digest_authority_overrides_capture_hints() {
        let planner = planner();
        let prior_snapshot = two_file_snapshot();
        let prior_validated = validated(prior_snapshot.clone());
        let prior_manifest = planner
            .plan_generation(&prior_validated, None, UtcMicros(3_000))
            .expect("prior generation");

        // Capture hints src/a.rs changed (it did not) and misses src/b.rs
        // (it did). Digest comparison, not the hint, decides.
        let current = validated(snapshot(vec![
            present_file("file.a2", "src/a.rs", 'a'),
            present_file("file.b2", "src/b.rs", 'c'),
        ]));
        let hinted: BTreeSet<String> = ["src/a.rs".to_owned()].into_iter().collect();
        let plan = planner
            .plan_increment(&prior_manifest, &prior_snapshot, &current, &hinted)
            .expect("increment plan");
        let unhinted = planner
            .plan_increment(&prior_manifest, &prior_snapshot, &current, &BTreeSet::new())
            .expect("unhinted increment plan");

        assert_eq!(plan.carried_forward, 1);
        assert_eq!(plan.reextract, 1);
        assert_eq!(plan.invalidation_digest, unhinted.invalidation_digest);
        assert!(matches!(
            plan.files[0].action,
            FileExtractionActionV1::CarryForward { .. }
        ));
        assert!(matches!(
            plan.files[1].action,
            FileExtractionActionV1::ReExtract { .. }
        ));
    }

    #[test]
    fn incompatible_inputs_force_a_declared_full_rebuild() {
        let planner = planner();
        let prior_snapshot = two_file_snapshot();
        let prior_validated = validated(prior_snapshot.clone());
        let prior_manifest = planner
            .plan_generation(&prior_validated, None, UtcMicros(3_000))
            .expect("prior generation");
        let current = validated(two_file_snapshot());

        // Chunker revision bump: a declared full rebuild, nothing carried.
        let rechunked = GenerationPlanner::new(
            repository(),
            registry(),
            id("chunker.v2"),
            id("privacy.fixture"),
            7,
        );
        let plan = rechunked
            .plan_increment(&prior_manifest, &prior_snapshot, &current, &BTreeSet::new())
            .expect("rebuild plan");
        assert!(plan.is_full_rebuild());
        assert_eq!(
            plan.rebuild_triggers,
            vec![RebuildTriggerV1::ChunkerRevision]
        );
        assert_eq!(plan.carried_forward, 0);
        assert_eq!(plan.reextract, 2);

        // Grammar revision bump on a present language.
        let mut bumped = rust_descriptor();
        bumped.grammar_revision = id("grammar.tree-sitter.rust.v2");
        let regrammared = GenerationPlanner::new(
            repository(),
            StaticLanguageRegistry::from_descriptors(vec![bumped]),
            id("chunker.v1"),
            id("privacy.fixture"),
            7,
        );
        let plan = regrammared
            .plan_increment(&prior_manifest, &prior_snapshot, &current, &BTreeSet::new())
            .expect("rebuild plan");
        assert_eq!(
            plan.rebuild_triggers,
            vec![RebuildTriggerV1::GrammarRevision(id("rust"))]
        );

        // Privacy epoch bump.
        let reprivated = GenerationPlanner::new(
            repository(),
            registry(),
            id("chunker.v1"),
            id("privacy.fixture"),
            8,
        );
        let plan = reprivated
            .plan_increment(&prior_manifest, &prior_snapshot, &current, &BTreeSet::new())
            .expect("rebuild plan");
        assert_eq!(
            plan.rebuild_triggers,
            vec![RebuildTriggerV1::PrivacyKeyEpoch]
        );
    }

    fn chunk(id_str: &str, generation: &CodeGenerationId, file: &str) -> CodeSearchChunkV1 {
        CodeSearchChunkV1 {
            id: id(id_str),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation.clone(),
                file_occurrence_id: id(file),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 4,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: digest('a'),
            language_descriptor_revision: LanguageDescriptorRevision::new("descriptor.rust.v1")
                .expect("valid id"),
            chunker_revision: id("chunker.v1"),
            sanitizer_revision: id("sanitizer.v1"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.v1").expect("valid id"),
            },
            exact_terms: vec![],
            subtokens: vec![],
            sanitized_text: BoundedSanitizedText::new("text").expect("bounded text"),
        }
    }

    #[test]
    fn generation_aware_join_binds_only_matching_generation_chunks() {
        let planner = planner();
        let snapshot = validated(two_file_snapshot());
        let generation = planner
            .plan_generation(&snapshot, None, UtcMicros(3_000))
            .expect("sealed generation");
        let document = CodeSearchDocumentV1 {
            generation_id: generation.generation_id.clone(),
            file_occurrence_id: id("file.a"),
            content_digest: digest('a'),
            eligibility: CodeSearchEligibilityV1::Eligible,
            chunk_ids: vec![id("chunk.v1.one"), id("chunk.v1.two")],
        };
        // Input order is not canonical; bindings come back canonically
        // ordered by chunk identity.
        let chunks = vec![
            chunk("chunk.v1.two", &generation.generation_id, "file.a"),
            chunk("chunk.v1.one", &generation.generation_id, "file.a"),
        ];
        let bindings =
            join_chunks_to_generation(&generation, &document, &chunks).expect("join succeeds");
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].chunk_id.as_str(), "chunk.v1.one");
        assert_eq!(bindings[1].chunk_id.as_str(), "chunk.v1.two");
        for binding in &bindings {
            assert_eq!(binding.generation_id, generation.generation_id);
            assert_eq!(binding.file_occurrence_id.as_str(), "file.a");
        }
    }

    #[test]
    fn generation_aware_join_rejects_cross_generation_undeclared_and_duplicate() {
        let planner = planner();
        let snapshot = validated(two_file_snapshot());
        let generation = planner
            .plan_generation(&snapshot, None, UtcMicros(3_000))
            .expect("sealed generation");
        let other_generation = planner
            .plan_generation(&snapshot, Some(&generation), UtcMicros(4_000))
            .expect("other sealed generation");
        let document = CodeSearchDocumentV1 {
            generation_id: generation.generation_id.clone(),
            file_occurrence_id: id("file.a"),
            content_digest: digest('a'),
            eligibility: CodeSearchEligibilityV1::Eligible,
            chunk_ids: vec![id("chunk.v1.one")],
        };
        let matching = chunk("chunk.v1.one", &generation.generation_id, "file.a");

        // A chunk anchored to another generation never joins.
        let foreign_chunk = chunk("chunk.v1.one", &other_generation.generation_id, "file.a");
        assert_eq!(
            join_chunks_to_generation(&generation, &document, &[foreign_chunk]),
            Err(GenerationJoinErrorV1::CrossGeneration)
        );

        // A document from another generation never joins.
        let foreign_document = CodeSearchDocumentV1 {
            generation_id: other_generation.generation_id.clone(),
            ..document.clone()
        };
        assert_eq!(
            join_chunks_to_generation(
                &generation,
                &foreign_document,
                std::slice::from_ref(&matching)
            ),
            Err(GenerationJoinErrorV1::CrossGeneration)
        );

        // A chunk the document does not declare never joins.
        let undeclared = chunk("chunk.v1.two", &generation.generation_id, "file.a");
        assert_eq!(
            join_chunks_to_generation(&generation, &document, &[undeclared]),
            Err(GenerationJoinErrorV1::UndeclaredChunk(id("chunk.v1.two")))
        );

        // A chunk joined twice is a duplicate.
        assert_eq!(
            join_chunks_to_generation(
                &generation,
                &document,
                &[matching.clone(), matching.clone()]
            ),
            Err(GenerationJoinErrorV1::DuplicateChunk(id("chunk.v1.one")))
        );

        // An unsealed generation never joins.
        let mut unsealed = generation.clone();
        unsealed.seal.expected_digest = manifest_digest('9');
        assert_eq!(
            join_chunks_to_generation(&unsealed, &document, &[matching]),
            Err(GenerationJoinErrorV1::UnsealedGeneration)
        );
    }
}

//! Production write side for generation-bound diagnostics (Plan 35,
//! "Universal managed diagnostics").
//!
//! [`crate::diagnostics_store::DiagnosticsStore`] persists diagnostics and
//! [`crate::lsp_runtime::DiagnosticsStoreLspFeedbackProjection`]
//! reads them back by anchor, but before this module nothing in production
//! ever wrote a record: `publish_clean_generation` had test-only call sites,
//! so the LSP Problems projection resolved no anchor and published nothing for
//! any finding.
//!
//! The store's publication contract is deliberately snapshot-shaped: one clean
//! generation is published exactly once, atomically, and clears every prior
//! current record. Producers therefore cannot append to a published
//! generation. [`CleanGenerationDiagnosticSnapshotBuilderV1`] is the
//! corresponding aggregation point: every producer contributes into one
//! builder for one clean generation, and the builder performs the single
//! atomic publication.
//!
//! Reference-only discipline: a contribution carries the producer's bounded
//! notice text plus canonical identity. Source bodies, logs, diffs, and
//! provider payloads are never copied into a diagnostic record; consumers
//! reach evidence through the authorized expansion path instead.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;

use tracedecay_domain::{
    CodeGenerationId, CommitId, ComponentVersion, ContentDigest, DiagnosticEvidenceClassV1,
    DiagnosticProducerKindV1, DiagnosticProvenanceV1, DiagnosticRecordStateV1,
    DiagnosticSeverityV1, FileOccurrenceId, GenerationDiagnosticV1, ProviderId, RefId,
    RepositoryId, RetrievalAnchorId, SourceSpan, SymbolOccurrenceId, UtcMicros, WorktreeId,
};
#[cfg(test)]
use tracedecay_lsp::DiagnosticSource;

use crate::diagnostics_store::DiagnosticsStore;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

/// Cataloged production diagnostic producers.
///
/// Each pillar fixes its own producer kind, `ProviderId`, and evidence class,
/// so identical findings from different pillars stay distinct records (Plan
/// 35, "Merge and publication semantics") and the LSP `source` field can name
/// the real producer instead of a single blanket `tracedecay`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub enum DiagnosticPillarV1 {
    /// `cargo check` / `clippy` output parsed by [`crate::diagnose`].
    Compiler,
    /// PR13 GitHub review advisory findings.
    GitHubReview,
    /// PR13 CI failure localization advisory findings.
    CiLocalization,
    /// PR13 proximity advisory findings.
    Proximity,
}

impl DiagnosticPillarV1 {
    /// Canonical provider identity persisted in `provenance.producer`. The LSP
    /// gateway maps exactly these strings onto its `source` field.
    #[must_use]
    pub const fn provider(self) -> &'static str {
        match self {
            Self::Compiler => "tracedecay",
            Self::GitHubReview => "tracedecay-github",
            Self::CiLocalization => "tracedecay-ci",
            Self::Proximity => "tracedecay-proximity",
        }
    }

    #[must_use]
    pub const fn producer_kind(self) -> DiagnosticProducerKindV1 {
        match self {
            // Compiler findings are relayed verbatim from the upstream
            // toolchain; TraceDecay only re-addresses them.
            Self::Compiler => DiagnosticProducerKindV1::UpstreamCompiler,
            // GitHub review and CI findings originate outside TraceDecay in an
            // authorized, cataloged provider.
            Self::GitHubReview | Self::CiLocalization => {
                DiagnosticProducerKindV1::AuthorizedExternalAnalyzer
            }
            // Proximity findings are derived from TraceDecay's own graph.
            Self::Proximity => DiagnosticProducerKindV1::TracedecayStructural,
        }
    }

    #[must_use]
    pub const fn evidence_class(self) -> DiagnosticEvidenceClassV1 {
        match self {
            Self::Compiler | Self::GitHubReview | Self::CiLocalization => {
                DiagnosticEvidenceClassV1::ProducerReported
            }
            Self::Proximity => DiagnosticEvidenceClassV1::DerivedStructural,
        }
    }

    fn provider_id(self) -> Result<ProviderId> {
        ProviderId::new(self.provider().to_owned()).map_err(|error| {
            contract(format!(
                "diagnostic pillar {self:?} has an invalid provider identity: {error}"
            ))
        })
    }
}

/// Immutable identity every record in one clean-generation snapshot shares.
///
/// The caller owns these values; the builder never invents repository,
/// worktree, reference, or revision identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanGenerationDiagnosticScopeV1 {
    pub generation_id: CodeGenerationId,
    pub repository: RepositoryId,
    pub worktree: Option<WorktreeId>,
    pub reference: Option<RefId>,
    pub source_revision: Option<CommitId>,
    /// Revision of the analyzing component, part of canonical identity.
    pub analyzer_revision: ComponentVersion,
    /// Revision of the configuration the producers ran under.
    pub configuration_revision: ComponentVersion,
    pub collected_at: UtcMicros,
}

/// The code-index generation authority's identity for one project root.
///
/// Every file identity `TraceDecay` publishes has exactly one mint: the
/// code-index scheduler, which derives `file.daemon.<digest>` from
/// `(repository, worktree, logical path, content digest)`. A producer that
/// invented its own file identity — a repository-relative path, say — would
/// publish records the LSP feedback projection can only refuse, because that
/// projection compares a record's `file_occurrence_id` against the saved-edit
/// cycle's impact target, which is minted by the same authority.
///
/// This type carries that authority across the boundary so a producer
/// *resolves* identity instead of minting it. A file the code index does not
/// know is not resolvable here, and the honest outcome is a typed skip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeIndexPublicationIdentityV1 {
    generation_id: CodeGenerationId,
    sealed_at: UtcMicros,
    repository: RepositoryId,
    worktree: Option<WorktreeId>,
    reference: Option<RefId>,
    source_revision: Option<CommitId>,
    files: BTreeMap<String, (FileOccurrenceId, ContentDigest)>,
}

impl CodeIndexPublicationIdentityV1 {
    /// Builds the identity from one complete code-index generation. Only the
    /// generation authority may call this; `files` is keyed by the generation's
    /// own logical paths.
    #[must_use]
    pub fn new(
        generation_id: CodeGenerationId,
        sealed_at: UtcMicros,
        repository: RepositoryId,
        worktree: Option<WorktreeId>,
        reference: Option<RefId>,
        source_revision: Option<CommitId>,
        files: impl IntoIterator<Item = (String, FileOccurrenceId, ContentDigest)>,
    ) -> Self {
        Self {
            generation_id,
            sealed_at,
            repository,
            worktree,
            reference,
            source_revision,
            files: files
                .into_iter()
                .map(|(path, file, digest)| (path, (file, digest)))
                .collect(),
        }
    }

    #[must_use]
    pub const fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    #[must_use]
    pub const fn repository(&self) -> &RepositoryId {
        &self.repository
    }

    #[must_use]
    pub const fn worktree(&self) -> Option<&WorktreeId> {
        self.worktree.as_ref()
    }

    #[must_use]
    pub const fn reference(&self) -> Option<&RefId> {
        self.reference.as_ref()
    }

    #[must_use]
    pub const fn source_revision(&self) -> Option<&CommitId> {
        self.source_revision.as_ref()
    }

    /// The authority's identity for one logical path, or `None` when the
    /// code-index generation does not contain that file.
    #[must_use]
    pub fn file(&self, logical_path: &str) -> Option<(&FileOccurrenceId, &ContentDigest)> {
        self.files
            .get(logical_path)
            .map(|(file, digest)| (file, digest))
    }

    #[must_use]
    pub fn logical_path(&self, occurrence: &FileOccurrenceId) -> Option<&str> {
        self.files
            .iter()
            .find_map(|(path, (file, _))| (file == occurrence).then_some(path.as_str()))
    }

    /// The clean-generation scope a producer publishes under. The generation
    /// and repository identity are the code index's, never the producer's.
    #[must_use]
    pub fn publication_scope(
        &self,
        analyzer_revision: ComponentVersion,
        configuration_revision: ComponentVersion,
    ) -> CleanGenerationDiagnosticScopeV1 {
        CleanGenerationDiagnosticScopeV1 {
            generation_id: self.generation_id.clone(),
            repository: self.repository.clone(),
            worktree: self.worktree.clone(),
            reference: self.reference.clone(),
            source_revision: self.source_revision.clone(),
            analyzer_revision,
            configuration_revision,
            // The snapshot is immutable per code generation. Binding evidence
            // time to the generation seal makes identical re-publication
            // converge instead of conflicting on wall-clock invocation time.
            collected_at: self.sealed_at,
        }
    }
}

pub type CodeIndexPublicationIdentityFuture<'a> =
    Pin<Box<dyn Future<Output = Option<CodeIndexPublicationIdentityV1>> + Send + 'a>>;

/// Type-erased access to the code-index generation authority.
///
/// The daemon owns the only production implementation
/// (`CodeIndexSchedulerRegistryV1`). A caller that cannot reach the daemon —
/// a direct, non-daemon MCP server — has no resolver, and the correct outcome
/// there is to publish nothing rather than to guess an identity.
pub trait CodeIndexPublicationIdentityPortV1: Send + Sync {
    fn resolve(&self, project_root: PathBuf) -> CodeIndexPublicationIdentityFuture<'_>;
}

/// Normalizes a producer-reported path onto the code index's logical-path
/// namespace (repository-relative, `/`-separated).
///
/// Returns `None` for a path that escapes the project root; such a path has no
/// logical identity in this generation and must not be resolved to one.
#[must_use]
pub fn code_index_logical_path(project_root: &Path, reported: &str) -> Option<String> {
    let candidate = Path::new(reported);
    let relative = if candidate.is_absolute() {
        candidate.strip_prefix(project_root).ok()?
    } else {
        candidate
    };
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let logical = relative.to_str()?.replace('\\', "/");
    (!logical.is_empty()).then_some(logical)
}

/// One producer's finding, addressed exactly.
///
/// `anchor` is the stable finding identity the feedback cycle carries as
/// `retrieval_anchor_id`; the LSP projection resolves the published record
/// through it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticContributionV1 {
    pub anchor: RetrievalAnchorId,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub span: SourceSpan,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
    pub code: String,
    pub severity: DiagnosticSeverityV1,
    /// Bounded notice text only. Never a source body, log excerpt, diff, or
    /// provider payload.
    pub message: String,
}

/// Why a contribution was refused. Refusals are typed and returned to the
/// caller; a producer never silently drops a finding it meant to publish.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticContributionRejectionV1 {
    /// Two contributions claimed the same anchor within one snapshot.
    DuplicateAnchor { anchor: String },
    /// The record failed `GenerationDiagnosticV1::validate`.
    InvalidRecord { anchor: String, reason: String },
}

impl std::fmt::Display for DiagnosticContributionRejectionV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateAnchor { anchor } => {
                write!(f, "diagnostic anchor {anchor} was contributed twice")
            }
            Self::InvalidRecord { anchor, reason } => {
                write!(f, "diagnostic contribution {anchor} is invalid: {reason}")
            }
        }
    }
}

/// Aggregates every producer's findings for one clean generation and performs
/// the single atomic publication the store's contract requires.
pub struct CleanGenerationDiagnosticSnapshotBuilderV1 {
    scope: CleanGenerationDiagnosticScopeV1,
    records: BTreeMap<String, GenerationDiagnosticV1>,
}

impl CleanGenerationDiagnosticSnapshotBuilderV1 {
    #[must_use]
    pub const fn new(scope: CleanGenerationDiagnosticScopeV1) -> Self {
        Self {
            scope,
            records: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn generation(&self) -> &CodeGenerationId {
        &self.scope.generation_id
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Adds one producer finding to the pending snapshot.
    ///
    /// Every field of canonical identity comes from the scope or the
    /// contribution; the message digest is recomputed so the record validates
    /// against its own sanitized text.
    pub fn contribute(
        &mut self,
        pillar: DiagnosticPillarV1,
        contribution: DiagnosticContributionV1,
    ) -> std::result::Result<(), DiagnosticContributionRejectionV1> {
        let key = contribution.anchor.as_str().to_owned();
        if self.records.contains_key(&key) {
            return Err(DiagnosticContributionRejectionV1::DuplicateAnchor { anchor: key });
        }
        let producer = pillar.provider_id().map_err(|error| {
            DiagnosticContributionRejectionV1::InvalidRecord {
                anchor: key.clone(),
                reason: error.to_string(),
            }
        })?;
        let mut record = GenerationDiagnosticV1 {
            diagnostic_anchor: contribution.anchor,
            generation_id: self.scope.generation_id.clone(),
            repository: self.scope.repository.clone(),
            worktree: self.scope.worktree.clone(),
            reference: self.scope.reference.clone(),
            source_revision: self.scope.source_revision.clone(),
            file_occurrence_id: contribution.file_occurrence_id,
            content_digest: contribution.content_digest,
            span: contribution.span,
            symbol_occurrence_id: contribution.symbol_occurrence_id,
            code: contribution.code,
            severity: contribution.severity,
            message: contribution.message,
            // Replaced below; the canonical digest is derived, never supplied.
            message_digest: tracedecay_domain::ManifestDigest::new(format!(
                "sha256:{}",
                "0".repeat(64)
            ))
            .map_err(|error| DiagnosticContributionRejectionV1::InvalidRecord {
                anchor: key.clone(),
                reason: error.to_string(),
            })?,
            provenance: DiagnosticProvenanceV1 {
                producer_kind: pillar.producer_kind(),
                producer,
                analyzer_revision: self.scope.analyzer_revision.clone(),
                configuration_revision: self.scope.configuration_revision.clone(),
                sanitization_receipt: None,
            },
            evidence_class: pillar.evidence_class(),
            collected_at: self.scope.collected_at,
            state: DiagnosticRecordStateV1::Current,
        };
        record.message_digest = record.compute_message_digest().map_err(|error| {
            DiagnosticContributionRejectionV1::InvalidRecord {
                anchor: key.clone(),
                reason: error.to_string(),
            }
        })?;
        record
            .validate()
            .map_err(|error| DiagnosticContributionRejectionV1::InvalidRecord {
                anchor: key.clone(),
                reason: error.to_string(),
            })?;
        self.records.insert(key, record);
        Ok(())
    }

    /// The pending records, ordered by anchor.
    #[must_use]
    pub fn records(&self) -> Vec<GenerationDiagnosticV1> {
        self.records.values().cloned().collect()
    }

    /// Publishes the aggregated snapshot as this clean generation's single
    /// atomic publication. Returns `(inserted, cleared)`.
    ///
    /// Republishing an identical snapshot converges (the store treats it as a
    /// no-op), so a repeated production cycle over an unchanged generation is
    /// safe.
    pub async fn publish(&self, store: &DiagnosticsStore<'_>) -> Result<(u64, u64)> {
        store
            .publish_clean_generation(&self.scope.generation_id, &self.records())
            .await
    }
}

fn contract(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}

/// Maps a parsed compiler diagnostic onto a contribution.
///
/// The compiler's own code (`E0308`, `clippy::redundant_closure`) is
/// preserved; when a diagnostic carried no code the severity name is used so
/// the record still has a stable, non-empty code. Only the compiler's bounded
/// message is copied — never the rendered snippet.
pub fn compiler_contribution_v1(
    diagnostic: &crate::diagnose::Diagnostic,
    anchor: RetrievalAnchorId,
    file_occurrence_id: FileOccurrenceId,
    content_digest: ContentDigest,
    span: SourceSpan,
    symbol_occurrence_id: Option<SymbolOccurrenceId>,
) -> DiagnosticContributionV1 {
    DiagnosticContributionV1 {
        anchor,
        file_occurrence_id,
        content_digest,
        span,
        symbol_occurrence_id,
        code: diagnostic
            .code
            .clone()
            .unwrap_or_else(|| compiler_severity_code(diagnostic.severity).to_owned()),
        severity: compiler_severity(diagnostic.severity),
        message: bounded_notice(&diagnostic.message),
    }
}

const fn compiler_severity(severity: crate::diagnose::Severity) -> DiagnosticSeverityV1 {
    match severity {
        crate::diagnose::Severity::Error => DiagnosticSeverityV1::Error,
        crate::diagnose::Severity::Warning => DiagnosticSeverityV1::Warning,
        crate::diagnose::Severity::Note => DiagnosticSeverityV1::Information,
        crate::diagnose::Severity::Help => DiagnosticSeverityV1::Hint,
    }
}

const fn compiler_severity_code(severity: crate::diagnose::Severity) -> &'static str {
    match severity {
        crate::diagnose::Severity::Error => "error",
        crate::diagnose::Severity::Warning => "warning",
        crate::diagnose::Severity::Note => "note",
        crate::diagnose::Severity::Help => "help",
    }
}

/// Builds a GitHub review contribution from the pillar's own immutable anchor.
///
/// [`tracedecay_domain::feedback::GitHubReviewImmutableAnchorV1`] already
/// carries the exact file, content digest, and span the diagnostics store
/// needs, so no identity is invented here. An anchor without a span cannot be
/// projected into an LSP range and is refused rather than published at an
/// arbitrary offset.
///
/// `notice` is the bounded advisory notice text; the review body itself stays
/// behind the authorized expansion path and is never copied in.
pub fn github_review_contribution_v1(
    anchor: &tracedecay_domain::feedback::GitHubReviewImmutableAnchorV1,
    code: &str,
    severity: DiagnosticSeverityV1,
    notice: &str,
) -> Option<DiagnosticContributionV1> {
    Some(DiagnosticContributionV1 {
        anchor: anchor.retrieval_anchor_id.clone(),
        file_occurrence_id: anchor.file.clone(),
        content_digest: anchor.content_digest.clone(),
        span: anchor.span?,
        symbol_occurrence_id: anchor.symbol.clone(),
        code: code.to_owned(),
        severity,
        message: bounded_notice(notice),
    })
}

/// Builds an advisory contribution for a pillar whose anchor identity is
/// assembled by the caller (CI localization, proximity).
///
/// Unlike GitHub review anchors, `CiExactCodeEvidenceV1` and proximity
/// candidates do not carry a `(file, content_digest, span)` triple directly;
/// the caller resolves those from its own generation evidence and passes them
/// here so no identity is invented inside the publication layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticContributionAnchorV1 {
    pub anchor: RetrievalAnchorId,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub span: SourceSpan,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
}

pub fn advisory_contribution_v1(
    anchor: DiagnosticContributionAnchorV1,
    code: &str,
    severity: DiagnosticSeverityV1,
    notice: &str,
) -> DiagnosticContributionV1 {
    DiagnosticContributionV1 {
        anchor: anchor.anchor,
        file_occurrence_id: anchor.file_occurrence_id,
        content_digest: anchor.content_digest,
        span: anchor.span,
        symbol_occurrence_id: anchor.symbol_occurrence_id,
        code: code.to_owned(),
        severity,
        message: bounded_notice(notice),
    }
}

/// A parsed compiler diagnostic resolved against real file content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCompilerDiagnosticV1 {
    pub diagnostic: crate::diagnose::Diagnostic,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub span: SourceSpan,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
}

/// Outcome of one production compiler-diagnostic publication.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticPublicationReportV1 {
    pub inserted: u64,
    pub cleared: u64,
    pub rejected: Vec<DiagnosticContributionRejectionV1>,
}

const COMPILER_ANCHOR_DOMAIN_V1: &str = "tracedecay.diagnostics.compiler-anchor.v1";

/// Deterministic anchor for one compiler finding.
///
/// Identity is the generation, file, span, and code — so republishing the same
/// unchanged generation converges on the same anchors, and a moved or changed
/// finding gets a new anchor rather than mutating an immutable record.
pub fn compiler_anchor_v1(
    generation: &CodeGenerationId,
    file_occurrence_id: &FileOccurrenceId,
    span: SourceSpan,
    code: &str,
) -> Result<RetrievalAnchorId> {
    let digest = tracedecay_domain::canonical_sha256(&(
        COMPILER_ANCHOR_DOMAIN_V1,
        generation.as_str(),
        file_occurrence_id.as_str(),
        span.start_byte,
        span.end_byte,
        code,
    ))
    .map_err(|error| contract(format!("compiler anchor digest is unavailable: {error}")))?;
    RetrievalAnchorId::new(format!(
        "anchor.diagnostic.compiler.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|error| contract(format!("compiler anchor identity is invalid: {error}")))
}

/// Why one parsed compiler diagnostic could not be resolved into a publishable
/// record. Every refusal names its file, so a producer never reports a bare
/// count for findings it silently dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilerDiagnosticResolutionSkipV1 {
    /// The reported path escapes the project root, so it has no logical
    /// identity in the code-index generation.
    PathOutsideProject { file: String },
    /// The code-index generation does not contain this file, so its
    /// `file.daemon.<digest>` identity does not exist. Guessing one here is
    /// exactly the raw-path mint this resolver exists to remove.
    FileNotInCodeIndex { file: String },
    /// The file on disk no longer matches the content the code-index
    /// generation recorded; publishing against a stale identity would produce
    /// records the LSP projection must refuse.
    ContentDriftFromCodeIndex { file: String },
    /// The file could not be read to compute a span.
    FileUnreadable { file: String },
    /// The reported line does not exist in the resolved content.
    LineOutOfRange { file: String, line: u32 },
}

impl std::fmt::Display for CompilerDiagnosticResolutionSkipV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PathOutsideProject { file } => {
                write!(f, "{file} is outside the project root")
            }
            Self::FileNotInCodeIndex { file } => {
                write!(f, "{file} is not in the current code-index generation")
            }
            Self::ContentDriftFromCodeIndex { file } => {
                write!(f, "{file} drifted from the code-index generation content")
            }
            Self::FileUnreadable { file } => write!(f, "{file} could not be read"),
            Self::LineOutOfRange { file, line } => {
                write!(f, "{file} has no line {line}")
            }
        }
    }
}

/// Resolves parsed compiler diagnostics against the code-index generation
/// authority.
///
/// File identity is *resolved*, never minted: each reported path is normalized
/// onto the generation's logical-path namespace and looked up, so a published
/// record carries the same `file.daemon.<digest>` identity the saved-edit
/// feedback cycle's impact target carries. That is the whole reason the LSP
/// projection can admit these records instead of refusing them with
/// `ImpactTargetFileMismatch`.
///
/// A diagnostic whose file is unknown to the generation, has drifted from it,
/// is unreadable, or whose reported line does not exist is refused with a named
/// reason rather than published under invented identity.
///
/// The span runs from the reported column to the end of the reported line —
/// the honest extent of what `cargo` reports without re-parsing the source.
pub async fn resolve_compiler_diagnostics_v1(
    project_root: &Path,
    identity: &CodeIndexPublicationIdentityV1,
    diagnostics: &[crate::diagnose::Diagnostic],
) -> (
    Vec<ResolvedCompilerDiagnosticV1>,
    Vec<CompilerDiagnosticResolutionSkipV1>,
) {
    let mut contents: BTreeMap<String, Option<(ContentDigest, String)>> = BTreeMap::new();
    let mut resolved = Vec::new();
    let mut skipped = Vec::new();
    for diagnostic in diagnostics {
        let Some(logical_path) = code_index_logical_path(project_root, &diagnostic.file) else {
            skipped.push(CompilerDiagnosticResolutionSkipV1::PathOutsideProject {
                file: diagnostic.file.clone(),
            });
            continue;
        };
        let Some((file_occurrence_id, indexed_digest)) = identity.file(&logical_path) else {
            skipped.push(CompilerDiagnosticResolutionSkipV1::FileNotInCodeIndex {
                file: logical_path,
            });
            continue;
        };
        if !contents.contains_key(&logical_path) {
            let loaded = load_project_file(project_root, &logical_path).await;
            contents.insert(logical_path.clone(), loaded);
        }
        let Some(Some((content_digest, text))) = contents.get(&logical_path) else {
            skipped.push(CompilerDiagnosticResolutionSkipV1::FileUnreadable { file: logical_path });
            continue;
        };
        if content_digest != indexed_digest {
            skipped.push(
                CompilerDiagnosticResolutionSkipV1::ContentDriftFromCodeIndex {
                    file: logical_path,
                },
            );
            continue;
        }
        let Some(span) = line_column_span(text, diagnostic.line, diagnostic.column) else {
            skipped.push(CompilerDiagnosticResolutionSkipV1::LineOutOfRange {
                file: logical_path,
                line: diagnostic.line,
            });
            continue;
        };
        resolved.push(ResolvedCompilerDiagnosticV1 {
            diagnostic: diagnostic.clone(),
            file_occurrence_id: file_occurrence_id.clone(),
            content_digest: indexed_digest.clone(),
            span,
            symbol_occurrence_id: None,
        });
    }
    (resolved, skipped)
}

/// Reads one repository-relative file, refusing paths that escape the root.
async fn load_project_file(project_root: &Path, relative: &str) -> Option<(ContentDigest, String)> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let bytes = tokio::fs::read(project_root.join(path)).await.ok()?;
    let digest = tracedecay_code_index::intake::content_digest(&bytes);
    let text = String::from_utf8(bytes).ok()?;
    Some((digest, text))
}

/// Converts a 1-based line/column into a byte span covering the reported
/// column through the end of that line.
fn line_column_span(text: &str, line: u32, column: u32) -> Option<SourceSpan> {
    if line == 0 {
        return None;
    }
    let mut offset = 0usize;
    for (index, line_text) in text.split_inclusive('\n').enumerate() {
        if index + 1 == line as usize {
            let trimmed = line_text.trim_end_matches(['\n', '\r']);
            let column_offset = trimmed
                .char_indices()
                .nth(column.saturating_sub(1) as usize)
                .map_or(trimmed.len(), |(byte_index, _)| byte_index);
            let start = offset + column_offset;
            let end = offset + trimmed.len();
            return Some(SourceSpan {
                start_byte: start as u64,
                end_byte: end.max(start) as u64,
            });
        }
        offset += line_text.len();
    }
    None
}

/// Publishes compiler diagnostics as one clean-generation snapshot.
///
/// This is the production write path: `tracedecay_diagnose` parses real
/// `cargo check` output and calls here, so the durable store — and therefore
/// the LSP Problems projection — is populated for a real edit cycle.
///
/// Contributions that cannot form a valid record are reported, never silently
/// dropped.
pub async fn publish_compiler_diagnostics_v1(
    store: &DiagnosticsStore<'_>,
    scope: CleanGenerationDiagnosticScopeV1,
    resolved: &[ResolvedCompilerDiagnosticV1],
) -> Result<DiagnosticPublicationReportV1> {
    let generation = scope.generation_id.clone();
    let mut builder = CleanGenerationDiagnosticSnapshotBuilderV1::new(scope);
    let mut rejected = Vec::new();
    for entry in resolved {
        let code = entry
            .diagnostic
            .code
            .clone()
            .unwrap_or_else(|| compiler_severity_code(entry.diagnostic.severity).to_owned());
        let anchor = compiler_anchor_v1(&generation, &entry.file_occurrence_id, entry.span, &code)?;
        let contribution = compiler_contribution_v1(
            &entry.diagnostic,
            anchor,
            entry.file_occurrence_id.clone(),
            entry.content_digest.clone(),
            entry.span,
            entry.symbol_occurrence_id.clone(),
        );
        match builder.contribute(DiagnosticPillarV1::Compiler, contribution) {
            Ok(()) => {}
            // Two compiler diagnostics on the same span with the same code
            // collapse into one record; that is the intended identity, not an
            // error, but every other refusal is reported.
            Err(DiagnosticContributionRejectionV1::DuplicateAnchor { .. }) => {}
            Err(rejection) => rejected.push(rejection),
        }
    }
    let (inserted, cleared) = builder.publish(store).await?;
    Ok(DiagnosticPublicationReportV1 {
        inserted,
        cleared,
        rejected,
    })
}

/// Outcome of the compiler pillar's production publication attempt.
///
/// Every non-published outcome is named. A producer that cannot reach the
/// code-index generation authority publishes nothing and says so, rather than
/// writing records under an identity the LSP feedback projection must refuse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilerDiagnosticPublicationOutcomeV1 {
    /// No resolver at this call site — a direct, non-daemon server.
    CodeIndexIdentityUnavailable,
    /// The resolver exists but has no complete, fresh generation for this root.
    CodeIndexGenerationUnavailable,
    NoResolvableDiagnostics {
        unresolved: Vec<CompilerDiagnosticResolutionSkipV1>,
    },
    Published {
        generation: CodeGenerationId,
        report: DiagnosticPublicationReportV1,
        unresolved: Vec<CompilerDiagnosticResolutionSkipV1>,
    },
    Failed {
        reason: String,
    },
}

/// The compiler pillar's production publication path, end to end.
///
/// This is the exact sequence `tracedecay_diagnose` runs: resolve the
/// code-index generation authority, resolve every parsed diagnostic's identity
/// against it, then publish one clean-generation snapshot under that same
/// generation. Both identities the LSP feedback projection compares —
/// `file_occurrence_id` and `generation_id` — therefore come from the same mint
/// as the saved-edit cycle's impact target.
pub async fn publish_compiler_diagnostics_through_code_index_v1(
    project_root: &Path,
    resolver: Option<&dyn CodeIndexPublicationIdentityPortV1>,
    store: &DiagnosticsStore<'_>,
    parsed: &[crate::diagnose::Diagnostic],
    analyzer_revision: ComponentVersion,
    configuration_revision: ComponentVersion,
) -> CompilerDiagnosticPublicationOutcomeV1 {
    let Some(resolver) = resolver else {
        return CompilerDiagnosticPublicationOutcomeV1::CodeIndexIdentityUnavailable;
    };
    let Some(identity) = resolver.resolve(project_root.to_path_buf()).await else {
        return CompilerDiagnosticPublicationOutcomeV1::CodeIndexGenerationUnavailable;
    };
    let (resolved, unresolved) = if parsed.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        resolve_compiler_diagnostics_v1(project_root, &identity, parsed).await
    };
    if !parsed.is_empty() && resolved.is_empty() {
        return CompilerDiagnosticPublicationOutcomeV1::NoResolvableDiagnostics { unresolved };
    }
    let scope = identity.publication_scope(analyzer_revision, configuration_revision);
    let generation = scope.generation_id.clone();
    if let Err(error) = store.ensure_schema().await {
        return CompilerDiagnosticPublicationOutcomeV1::Failed {
            reason: error.to_string(),
        };
    }
    match publish_compiler_diagnostics_v1(store, scope, &resolved).await {
        Ok(report) => CompilerDiagnosticPublicationOutcomeV1::Published {
            generation,
            report,
            unresolved,
        },
        Err(error) => CompilerDiagnosticPublicationOutcomeV1::Failed {
            reason: error.to_string(),
        },
    }
}

/// Collapses control characters and bounds the notice to the domain limit so
/// a producer's text always satisfies `validate_sanitized_message`.
#[must_use]
pub fn bounded_notice(message: &str) -> String {
    let collapsed: String = message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let collapsed = collapsed.trim();
    if collapsed.is_empty() {
        return "diagnostic reported without a message".to_owned();
    }
    let limit = tracedecay_domain::MAX_DIAGNOSTIC_MESSAGE_BYTES;
    if collapsed.len() <= limit {
        return collapsed.to_owned();
    }
    let mut end = limit;
    while end > 0 && !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    collapsed[..end].to_owned()
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

    pub(crate) fn scope(generation: &str) -> CleanGenerationDiagnosticScopeV1 {
        CleanGenerationDiagnosticScopeV1 {
            generation_id: id(generation),
            repository: id("repository.fixture"),
            worktree: Some(id("worktree.fixture")),
            reference: Some(id("ref.main")),
            source_revision: Some(id("commit.abc123")),
            analyzer_revision: id("analyzer.v1"),
            configuration_revision: id("config.v1"),
            collected_at: UtcMicros(1_700_000_000_000_000),
        }
    }

    fn contribution(anchor: &str) -> DiagnosticContributionV1 {
        DiagnosticContributionV1 {
            anchor: id(anchor),
            file_occurrence_id: id("file.occurrence.1"),
            content_digest: id(&digest('a')),
            span: SourceSpan {
                start_byte: 0,
                end_byte: 8,
            },
            symbol_occurrence_id: None,
            code: "E0308".to_owned(),
            severity: DiagnosticSeverityV1::Error,
            message: "mismatched types".to_owned(),
        }
    }

    #[test]
    fn each_pillar_publishes_its_own_provider_identity() {
        let mut builder = CleanGenerationDiagnosticSnapshotBuilderV1::new(scope("generation.p.1"));
        for (index, pillar) in [
            DiagnosticPillarV1::Compiler,
            DiagnosticPillarV1::GitHubReview,
            DiagnosticPillarV1::CiLocalization,
            DiagnosticPillarV1::Proximity,
        ]
        .into_iter()
        .enumerate()
        {
            builder
                .contribute(pillar, contribution(&format!("anchor.pillar.{index}")))
                .expect("contribution accepted");
        }
        let providers: Vec<String> = builder
            .records()
            .iter()
            .map(|record| record.provenance.producer.as_str().to_owned())
            .collect();
        assert!(providers.contains(&"tracedecay".to_owned()));
        assert!(providers.contains(&"tracedecay-github".to_owned()));
        assert!(providers.contains(&"tracedecay-ci".to_owned()));
        assert!(providers.contains(&"tracedecay-proximity".to_owned()));
    }

    #[test]
    fn duplicate_anchor_is_typed_not_silent() {
        let mut builder = CleanGenerationDiagnosticSnapshotBuilderV1::new(scope("generation.p.2"));
        builder
            .contribute(DiagnosticPillarV1::Compiler, contribution("anchor.dup.1"))
            .expect("first contribution accepted");
        let rejection = builder
            .contribute(
                DiagnosticPillarV1::GitHubReview,
                contribution("anchor.dup.1"),
            )
            .expect_err("duplicate anchor is refused");
        assert_eq!(
            rejection,
            DiagnosticContributionRejectionV1::DuplicateAnchor {
                anchor: "anchor.dup.1".to_owned()
            }
        );
    }

    #[test]
    fn invalid_contribution_is_refused_with_a_reason() {
        let mut builder = CleanGenerationDiagnosticSnapshotBuilderV1::new(scope("generation.p.3"));
        let mut invalid = contribution("anchor.invalid.1");
        invalid.code = String::new();
        let rejection = builder
            .contribute(DiagnosticPillarV1::Compiler, invalid)
            .expect_err("empty code is refused");
        assert!(matches!(
            rejection,
            DiagnosticContributionRejectionV1::InvalidRecord { .. }
        ));
        assert!(builder.is_empty());
    }

    #[test]
    fn compiler_contribution_preserves_code_and_severity() {
        let diagnostic = crate::diagnose::Diagnostic {
            severity: crate::diagnose::Severity::Warning,
            code: Some("clippy::redundant_closure".to_owned()),
            message: "redundant closure".to_owned(),
            file: "src/lib.rs".to_owned(),
            line: 4,
            column: 1,
        };
        let contribution = compiler_contribution_v1(
            &diagnostic,
            id("anchor.compiler.1"),
            id("file.occurrence.1"),
            id(&digest('a')),
            SourceSpan {
                start_byte: 0,
                end_byte: 4,
            },
            None,
        );
        assert_eq!(contribution.code, "clippy::redundant_closure");
        assert_eq!(contribution.severity, DiagnosticSeverityV1::Warning);
        assert_eq!(contribution.message, "redundant closure");
    }

    #[test]
    fn codeless_compiler_diagnostic_still_has_a_stable_code() {
        let diagnostic = crate::diagnose::Diagnostic {
            severity: crate::diagnose::Severity::Error,
            code: None,
            message: "cannot borrow".to_owned(),
            file: "src/lib.rs".to_owned(),
            line: 4,
            column: 1,
        };
        let contribution = compiler_contribution_v1(
            &diagnostic,
            id("anchor.compiler.2"),
            id("file.occurrence.1"),
            id(&digest('a')),
            SourceSpan {
                start_byte: 0,
                end_byte: 4,
            },
            None,
        );
        assert_eq!(contribution.code, "error");
    }

    #[test]
    fn notice_text_is_bounded_and_control_free() {
        let raw = format!("line one\nline two\t{}", "x".repeat(8192));
        let bounded = bounded_notice(&raw);
        assert!(!bounded.chars().any(char::is_control));
        assert!(bounded.len() <= tracedecay_domain::MAX_DIAGNOSTIC_MESSAGE_BYTES);
    }

    /// Every pillar's published record must resolve to its own LSP `source`
    /// through the exact mapping the projection uses. This publishes real
    /// records and reads them back — it never inspects source strings.
    #[tokio::test]
    async fn published_pillar_records_map_to_producer_specific_lsp_sources() {
        let temp = tempfile::tempdir().expect("tempdir");
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &temp.path().join("diagnostics.db"),
        );
        let store = DiagnosticsStore::new_runtime(&conn);
        store.ensure_schema().await.expect("ensure schema");

        let expected = [
            (
                DiagnosticPillarV1::Compiler,
                "anchor.pillar.compiler",
                DiagnosticSource::TraceDecay,
            ),
            (
                DiagnosticPillarV1::GitHubReview,
                "anchor.pillar.github",
                DiagnosticSource::TraceDecayGitHub,
            ),
            (
                DiagnosticPillarV1::CiLocalization,
                "anchor.pillar.ci",
                DiagnosticSource::TraceDecayCi,
            ),
            (
                DiagnosticPillarV1::Proximity,
                "anchor.pillar.proximity",
                DiagnosticSource::TraceDecayProximity,
            ),
        ];

        let mut builder = CleanGenerationDiagnosticSnapshotBuilderV1::new(scope("generation.p.5"));
        for (pillar, anchor, _) in &expected {
            builder
                .contribute(*pillar, contribution(anchor))
                .expect("contribution accepted");
        }
        builder.publish(&store).await.expect("publish snapshot");

        for (_, anchor, source) in &expected {
            let record = store
                .record_by_anchor(&id::<RetrievalAnchorId>(anchor))
                .await
                .expect("read anchor")
                .expect("record present");
            assert_eq!(
                DiagnosticSource::from_producer(record.provenance.producer.as_str()),
                *source,
                "pillar anchor {anchor} projected the wrong LSP source"
            );
        }
    }

    /// A code-index generation identity standing in for the daemon authority,
    /// carrying the same `file.daemon.<digest>` shape the scheduler mints.
    fn code_index_identity(
        generation: &str,
        files: &[(&str, &str, &str)],
    ) -> CodeIndexPublicationIdentityV1 {
        CodeIndexPublicationIdentityV1::new(
            id(generation),
            UtcMicros(1_700_000_000_000_000),
            id("repository.fixture"),
            Some(id("worktree.fixture")),
            Some(id("ref.main")),
            Some(id("commit.abc123")),
            files.iter().map(|(path, file, digest)| {
                ((*path).to_owned(), id(file), id::<ContentDigest>(digest))
            }),
        )
    }

    struct StaticCodeIndexIdentity(CodeIndexPublicationIdentityV1);

    impl CodeIndexPublicationIdentityPortV1 for StaticCodeIndexIdentity {
        fn resolve(&self, _project_root: PathBuf) -> CodeIndexPublicationIdentityFuture<'_> {
            let identity = self.0.clone();
            Box::pin(async move { Some(identity) })
        }
    }

    /// Reachability: calls the exact function the `tracedecay_diagnose` MCP
    /// handler calls, with real parsed `cargo check` text, and asserts the
    /// durable store is populated afterwards with the code-index file identity.
    #[tokio::test]
    async fn production_compiler_path_populates_the_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        tokio::fs::create_dir_all(project_root.join("src"))
            .await
            .expect("create src");
        let source = "fn main() {\n    let x: u32 = \"nope\";\n}\n";
        tokio::fs::write(project_root.join("src/lib.rs"), source)
            .await
            .expect("write source");
        let content_digest = tracedecay_code_index::intake::content_digest(source.as_bytes());

        let cargo_output = "error[E0308]: mismatched types\n  --> src/lib.rs:2:18\n";
        let parsed = crate::diagnose::parse_cargo_output(cargo_output);
        assert_eq!(parsed.len(), 1, "fixture cargo output must parse");

        let identity = code_index_identity(
            "generation.reachability.1",
            &[(
                "src/lib.rs",
                "file.daemon.reachability",
                content_digest.as_str(),
            )],
        );
        let (resolved, skipped) =
            resolve_compiler_diagnostics_v1(&project_root, &identity, &parsed).await;
        assert!(skipped.is_empty(), "unexpected skips: {skipped:?}");
        assert_eq!(resolved.len(), 1);

        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &temp.path().join("diagnostics.db"),
        );
        let store = DiagnosticsStore::new_runtime(&conn);
        store.ensure_schema().await.expect("ensure schema");

        let scope = identity.publication_scope(id("analyzer.v1"), id("config.v1"));
        let generation = identity.generation_id().clone();
        let report = publish_compiler_diagnostics_v1(&store, scope, &resolved)
            .await
            .expect("publish compiler diagnostics");
        assert_eq!(report.inserted, 1);
        assert!(report.rejected.is_empty());

        let current = store
            .current_records(&generation)
            .await
            .expect("read current records");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].code, "E0308");
        assert_eq!(current[0].provenance.producer.as_str(), "tracedecay");
        assert_eq!(
            current[0].file_occurrence_id.as_str(),
            "file.daemon.reachability",
            "file identity must be resolved from the code-index generation"
        );
        assert_eq!(current[0].content_digest, content_digest);
        assert_eq!(
            store
                .current_generation()
                .await
                .expect("current generation"),
            Some(generation)
        );
    }

    #[tokio::test]
    async fn repeated_compiler_publication_converges_on_generation_seal_time() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        tokio::fs::create_dir_all(project_root.join("src"))
            .await
            .expect("create src");
        let source = "fn main() {\n    let x: u32 = \"nope\";\n}\n";
        tokio::fs::write(project_root.join("src/lib.rs"), source)
            .await
            .expect("write source");
        let content_digest = tracedecay_code_index::intake::content_digest(source.as_bytes());
        let resolver = StaticCodeIndexIdentity(code_index_identity(
            "generation.repeat.1",
            &[("src/lib.rs", "file.daemon.repeat", content_digest.as_str())],
        ));
        let parsed = crate::diagnose::parse_cargo_output(
            "error[E0308]: mismatched types\n  --> src/lib.rs:2:18\n",
        );
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &temp.path().join("diagnostics.db"),
        );
        let store = DiagnosticsStore::new_runtime(&conn);

        for attempt in 0..2 {
            let outcome = publish_compiler_diagnostics_through_code_index_v1(
                &project_root,
                Some(&resolver),
                &store,
                &parsed,
                id("analyzer.v1"),
                id("config.v1"),
            )
            .await;
            let CompilerDiagnosticPublicationOutcomeV1::Published { report, .. } = outcome else {
                panic!("publication attempt {attempt} failed: {outcome:?}");
            };
            assert_eq!(report.inserted, u64::from(attempt == 0));
            assert_eq!(report.cleared, 0);
        }
    }

    #[tokio::test]
    async fn empty_compiler_result_publishes_clean_successor_generation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        tokio::fs::create_dir_all(project_root.join("src"))
            .await
            .expect("create src");
        let source = "fn main() {\n    let x: u32 = \"nope\";\n}\n";
        tokio::fs::write(project_root.join("src/lib.rs"), source)
            .await
            .expect("write source");
        let content_digest = tracedecay_code_index::intake::content_digest(source.as_bytes());
        let parsed = crate::diagnose::parse_cargo_output(
            "error[E0308]: mismatched types\n  --> src/lib.rs:2:18\n",
        );
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &temp.path().join("diagnostics.db"),
        );
        let store = DiagnosticsStore::new_runtime(&conn);

        let first = StaticCodeIndexIdentity(code_index_identity(
            "generation.clean.1",
            &[("src/lib.rs", "file.daemon.clean.1", content_digest.as_str())],
        ));
        assert!(matches!(
            publish_compiler_diagnostics_through_code_index_v1(
                &project_root,
                Some(&first),
                &store,
                &parsed,
                id("analyzer.v1"),
                id("config.v1"),
            )
            .await,
            CompilerDiagnosticPublicationOutcomeV1::Published { .. }
        ));

        let second = StaticCodeIndexIdentity(code_index_identity(
            "generation.clean.2",
            &[("src/lib.rs", "file.daemon.clean.2", content_digest.as_str())],
        ));
        let outcome = publish_compiler_diagnostics_through_code_index_v1(
            &project_root,
            Some(&second),
            &store,
            &[],
            id("analyzer.v1"),
            id("config.v1"),
        )
        .await;
        let CompilerDiagnosticPublicationOutcomeV1::Published {
            generation, report, ..
        } = outcome
        else {
            panic!("clean successor was not published: {outcome:?}");
        };
        assert_eq!(generation.as_str(), "generation.clean.2");
        assert_eq!(report.inserted, 0);
        assert_eq!(report.cleared, 1);
        assert!(
            store
                .current_records(&generation)
                .await
                .expect("read clean generation")
                .is_empty()
        );
    }

    /// A file the code-index generation does not contain is refused by name.
    /// Before this, the resolver minted `FileOccurrenceId::new(<raw path>)` and
    /// published a record the LSP projection could only refuse.
    #[tokio::test]
    async fn file_outside_the_code_index_is_named_not_minted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        tokio::fs::create_dir_all(project_root.join("src"))
            .await
            .expect("create src");
        tokio::fs::write(project_root.join("src/other.rs"), "fn other() {}\n")
            .await
            .expect("write source");

        let parsed =
            crate::diagnose::parse_cargo_output("error[E0308]: nope\n  --> src/other.rs:1:1\n");
        let identity = code_index_identity("generation.absent.1", &[]);
        let (resolved, skipped) =
            resolve_compiler_diagnostics_v1(&project_root, &identity, &parsed).await;
        assert!(resolved.is_empty());
        assert_eq!(
            skipped,
            vec![CompilerDiagnosticResolutionSkipV1::FileNotInCodeIndex {
                file: "src/other.rs".to_owned()
            }]
        );
    }

    /// Content that drifted from the indexed generation is refused rather than
    /// published against a stale identity.
    #[tokio::test]
    async fn content_drift_from_the_code_index_is_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path().join("project");
        tokio::fs::create_dir_all(project_root.join("src"))
            .await
            .expect("create src");
        tokio::fs::write(project_root.join("src/lib.rs"), "fn main() {}\n")
            .await
            .expect("write source");

        let parsed =
            crate::diagnose::parse_cargo_output("error[E0308]: nope\n  --> src/lib.rs:1:1\n");
        let identity = code_index_identity(
            "generation.drift.1",
            &[("src/lib.rs", "file.daemon.drift", &digest('a'))],
        );
        let (resolved, skipped) =
            resolve_compiler_diagnostics_v1(&project_root, &identity, &parsed).await;
        assert!(resolved.is_empty());
        assert_eq!(
            skipped,
            vec![
                CompilerDiagnosticResolutionSkipV1::ContentDriftFromCodeIndex {
                    file: "src/lib.rs".to_owned()
                }
            ]
        );
    }

    #[test]
    fn logical_paths_normalize_and_refuse_escapes() {
        let root = Path::new("/project");
        assert_eq!(
            code_index_logical_path(root, "src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            code_index_logical_path(root, "/project/src/lib.rs").as_deref(),
            Some("src/lib.rs")
        );
        assert!(code_index_logical_path(root, "../outside.rs").is_none());
        assert!(code_index_logical_path(root, "/elsewhere/lib.rs").is_none());
    }

    #[test]
    fn github_review_anchor_becomes_a_contribution_without_inventing_identity() {
        let anchor = tracedecay_domain::feedback::GitHubReviewImmutableAnchorV1 {
            repository_id: id("repository.fixture"),
            commit_id: id("commit.abc123"),
            retrieval_anchor_id: id("anchor.github-code.1"),
            file: id("src/lib.rs"),
            content_digest: id(&digest('a')),
            span: Some(SourceSpan {
                start_byte: 12,
                end_byte: 40,
            }),
            symbol: Some(id("symbol.occurrence.1")),
        };
        let contribution = github_review_contribution_v1(
            &anchor,
            "github-review",
            DiagnosticSeverityV1::Information,
            "unresolved review comment on this line",
        )
        .expect("anchor with a span yields a contribution");
        assert_eq!(contribution.anchor, anchor.retrieval_anchor_id);
        assert_eq!(contribution.file_occurrence_id, anchor.file);
        assert_eq!(contribution.content_digest, anchor.content_digest);
        assert_eq!(contribution.span, anchor.span.expect("span"));
        assert_eq!(contribution.symbol_occurrence_id, anchor.symbol);
    }

    #[test]
    fn spanless_github_anchor_is_refused_rather_than_placed_at_an_arbitrary_offset() {
        let anchor = tracedecay_domain::feedback::GitHubReviewImmutableAnchorV1 {
            repository_id: id("repository.fixture"),
            commit_id: id("commit.abc123"),
            retrieval_anchor_id: id("anchor.github-code.2"),
            file: id("src/lib.rs"),
            content_digest: id(&digest('a')),
            span: None,
            symbol: None,
        };
        assert!(
            github_review_contribution_v1(
                &anchor,
                "github-review",
                DiagnosticSeverityV1::Information,
                "unresolved review comment",
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn aggregated_snapshot_publishes_and_reads_back_by_anchor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &temp.path().join("diagnostics.db"),
        );
        let store = DiagnosticsStore::new_runtime(&conn);
        store.ensure_schema().await.expect("ensure schema");

        let mut builder = CleanGenerationDiagnosticSnapshotBuilderV1::new(scope("generation.p.4"));
        builder
            .contribute(
                DiagnosticPillarV1::GitHubReview,
                contribution("anchor.published.1"),
            )
            .expect("contribution accepted");
        let (inserted, _cleared) = builder.publish(&store).await.expect("publish snapshot");
        assert_eq!(inserted, 1);

        let record = store
            .record_by_anchor(&id::<RetrievalAnchorId>("anchor.published.1"))
            .await
            .expect("read anchor")
            .expect("record present");
        assert_eq!(record.provenance.producer.as_str(), "tracedecay-github");
        assert_eq!(
            record.provenance.producer_kind,
            DiagnosticProducerKindV1::AuthorizedExternalAnalyzer
        );
    }
}

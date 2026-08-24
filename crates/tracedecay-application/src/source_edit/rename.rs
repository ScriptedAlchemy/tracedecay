//! Typed Plan 34 rename preview and apply outcome.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::ManifestDigest;

use crate::error::ApplicationContractError;

/// Exact graph-backed symbol identity returned by the published rename preview.
/// These five fields are copied verbatim into `tracedecay_rename_symbol` so a
/// later plan/apply can revalidate the same occurrence instead of a spelling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewNodeV1 {
    pub id: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub name: String,
}

/// Read-only identity preview over one admitted immutable graph generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewResultV1 {
    pub success: bool,
    pub node: RenamePreviewNodeV1,
    pub graph_revision: ManifestDigest,
    pub message: String,
}

/// Exact preview material an apply must echo. Preview calls omit this value;
/// apply rejects any mismatch before publishing a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewAcceptanceV1 {
    pub preview_id: ManifestDigest,
    pub preview_digest: ManifestDigest,
    pub plan_digest: ManifestDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_revision: Option<String>,
    pub graph_revision: ManifestDigest,
}

impl RenamePreviewAcceptanceV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.plan_digest.validate()?;
        self.graph_revision.validate()?;
        if self
            .repository_revision
            .as_ref()
            .is_some_and(|revision| revision.trim().is_empty())
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "rename repository revision",
            });
        }
        Ok(())
    }
}

/// One file a rename changed (or would change), with the exact bound-site count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameFileEditV1 {
    pub file: String,
    pub replaced_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenameSiteDispositionV1 {
    Changed,
    Unchanged,
    Skipped,
    Blocked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenameSiteKindV1 {
    Declaration,
    Import,
    Reexport,
    QualifiedPath,
    UnqualifiedPath,
    Annotation,
    GenericArgument,
    Constructor,
    Pattern,
    TraitDeclaration,
    TraitImplementation,
    ResolvedCall,
    InherentMethod,
    EnumVariant,
    Test,
    Example,
    Documentation,
    ProtectedValue,
    UnresolvedText,
}

/// One exact old-name occurrence and the rename planner's disposition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameSiteV1 {
    pub site_id: String,
    pub kind: RenameSiteKindV1,
    pub disposition: RenameSiteDispositionV1,
    pub file: String,
    pub line: u32,
    pub start_byte: u64,
    pub end_byte: u64,
    pub expected_bytes: String,
    pub replacement_bytes: String,
    pub reason: String,
    /// Canonical source `SymbolOccurrenceId` that attested this site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenameHazardKindV1 {
    InvalidIdentifier,
    StaleEvidence,
    AmbiguousSymbol,
    NamespaceCollision,
    ChangedResolution,
    Shadowing,
    UnsupportedSyntax,
    MacroExpansion,
    GeneratedSource,
    OverlappingSite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameHazardV1 {
    pub kind: RenameHazardKindV1,
    pub blocking: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenameProtectedValueCategoryV1 {
    WireValue,
    SerializedName,
    SqlIdentifier,
    PersistedName,
    SchemaEpoch,
    HashDomain,
    ProtocolName,
    ContractSnapshot,
    StringLiteral,
    ByteLiteral,
}

/// A byte-exact stable value deliberately preserved by the rename.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameProtectedValueV1 {
    pub site_id: String,
    pub file: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub category: RenameProtectedValueCategoryV1,
    pub exact_bytes: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameDispositionCountsV1 {
    pub changed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub blocked: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameImpactV1 {
    pub callers: Vec<String>,
    pub reexports: Vec<String>,
    pub affected_files: Vec<String>,
    pub affected_tests: Vec<String>,
}

/// The same typed payload is serialized by application, CLI, and MCP paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RenameResult {
    pub success: bool,
    /// Digest-addressed identity of the bound symbol plus proposed name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_id: Option<ManifestDigest>,
    /// Exact candidate-state digest that apply must echo as `expected_state`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_digest: Option<ManifestDigest>,
    /// Digest of the complete typed site/hazard/protected-value manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_digest: Option<ManifestDigest>,
    /// Current Git HEAD when the worktree has one; dirty bytes remain bound by
    /// `preview_digest` independently of this revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_revision: Option<String>,
    /// Digest of the admitted generation, exact target, same-name symbols,
    /// and graph edges used to classify bound and protected sites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_revision: Option<ManifestDigest>,
    /// Qualified name of the renamed symbol at plan time.
    pub symbol: String,
    pub old_name: String,
    pub new_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<RenameFileEditV1>,
    pub reference_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<RenameSiteV1>,
    pub dispositions: RenameDispositionCountsV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hazards: Vec<RenameHazardV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_values: Vec<RenameProtectedValueV1>,
    pub impact: RenameImpactV1,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub dry_run: bool,
    /// True only when apply published the accepted edit and post-apply
    /// verification then restored every exact preimage.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub rolled_back: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub message: String,
}

impl RenameResult {
    pub fn bind_preview_digest(&mut self, digest: ManifestDigest) {
        self.preview_digest = Some(digest);
    }

    pub fn mark_verification_rollback(&mut self) {
        self.success = false;
        self.rolled_back = true;
        self.message = "rename verification failed; exact preimages were restored".to_owned();
    }
}

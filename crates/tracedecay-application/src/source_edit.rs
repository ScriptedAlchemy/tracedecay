use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, ExecutableSchemaAuthority,
    IdempotencyContract, LifecycleClass, PrivacyClass, ProfileId, ReceiptContract,
    ReconciliationContract, RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId,
    SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract, TerminalState,
    TerminalStateContract, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;
use crate::source_edit_rollback::{source_edit_rollback_operation, source_edit_rollback_schema};
use crate::{current_bindings, current_bindings_with_slug};

/// `serde` `skip_serializing_if` predicate for default-off flags.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// Result of a single string replacement edit.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct EditResult {
    pub success: bool,
    pub file_path: String,
    pub matched_str: String,
    pub new_str: String,
    /// The exact source text that was replaced. For `replace_symbol` this is
    /// the item's full span, including any leading doc-comment / attribute
    /// block, so callers can recover its docs/attrs if the replacement
    /// dropped them; for `str_replace` it is the matched `old_str` text.
    /// `None` only on a failed edit, where nothing was resolved to replace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_span: Option<String>,
    /// True when this was a dry run: validation, spans, and the resulting
    /// content were all computed, but nothing was written to disk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Bounded preview diff of the would-be change. Populated only on a
    /// successful dry run; `None` for real edits and for failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub message: String,
}

/// Result of a multi-string replacement edit.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MultiEditResult {
    pub success: bool,
    pub file_path: String,
    pub applied_count: usize,
    /// True when this was a dry run: replacements were validated and the
    /// resulting content computed, but nothing was written to disk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Bounded preview diff of the would-be change. Populated only on a
    /// successful dry run; `None` for real edits and for failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub message: String,
}

/// Result of an insert-at operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct InsertResult {
    pub success: bool,
    pub file_path: String,
    pub anchor_line: u32,
    pub content: String,
    pub before: bool,
    /// True when this was a dry run: the insertion point was resolved and the
    /// resulting content computed, but nothing was written to disk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Bounded preview diff of the would-be change. Populated only on a
    /// successful dry run; `None` for real edits and for failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub message: String,
}

/// Result of an ast-grep rewrite operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AstGrepResult {
    pub success: bool,
    pub file_path: String,
    pub pattern: String,
    pub rewrite: String,
    /// True when this was a dry run: the rewrite was resolved (via the built-in
    /// literal fallback or an ast-grep preview run) but nothing was written.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Bounded preview of the would-be change. Populated only on a successful
    /// dry run; `None` for real edits and for failures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub message: String,
}

/// One evidence-based, actionable finding produced by the `move_symbol` impact
/// engine. Each hint points at a concrete file/line and carries a suggestion
/// the caller (or a follow-up refactor) can act on. Hints are derived from graph
/// edges (callers/callees) and parse-level facts (identifiers, `use` lines,
/// module declarations) — never speculative noise.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MoveHint {
    /// Taxonomy tag: `caller_reference`, `dependency_broken`, `import_needed`,
    /// `visibility_required`, `collision`, `module_missing`, `cycle_risk`,
    /// `orphaned_import`, or `cfg_context`.
    pub kind: String,
    /// File the finding concerns (the caller's file, the destination, or the
    /// source), project-relative.
    pub file: String,
    /// 1-based line the finding concerns, when a specific site is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Human-readable description of what the move breaks or affects.
    pub detail: String,
    /// The exact change to make (e.g. a `use` line to add, a path to rewrite, a
    /// visibility to escalate). `None` when no single mechanical fix applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Result of a `move_symbol` operation: the moved span, a dry-run diff of the
/// source + destination files, and — the centerpiece — the impact report of
/// everything the move breaks or that needs attention.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MoveResult {
    pub success: bool,
    /// The resolved symbol that was (or would be) moved, `name (kind)`.
    pub symbol: String,
    pub source_file: String,
    pub dest_file: String,
    /// The exact source span that was moved, including its leading
    /// doc-comment / attribute block. `None` on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_span: Option<String>,
    /// True when this was a dry run: spans, the destination shape, and the
    /// impact report were all computed, but nothing was written to disk.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Combined preview diff of the source (removal) and destination (insertion)
    /// files. Populated on a successful dry run; `None` for real moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// `use` lines auto-inserted at the destination because the moved body's
    /// dependency on them was unambiguous. Reported so the caller sees exactly
    /// what the move added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_imports: Vec<String>,
    /// The impact report — every actionable finding. Empty on a truly clean
    /// move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impact: Vec<MoveHint>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceEditKind {
    StrReplace,
    MultiStrReplace,
    InsertAt,
    AstGrepRewrite,
    ReplaceSymbol,
    InsertAtSymbol,
    MoveSymbol,
    RenameSymbol,
}

impl SourceEditKind {
    pub const fn operation_name(self) -> &'static str {
        match self {
            Self::StrReplace => "str_replace",
            Self::MultiStrReplace => "multi_str_replace",
            Self::InsertAt => "insert_at",
            Self::AstGrepRewrite => "ast_grep_rewrite",
            Self::ReplaceSymbol => "replace_symbol",
            Self::InsertAtSymbol => "insert_at_symbol",
            Self::MoveSymbol => "move_symbol",
            Self::RenameSymbol => "rename_symbol",
        }
    }
}

/// Exact symbol identity a rename apply is bound to. A bare spelling is never
/// sufficient: the apply revalidates every field against the live graph and
/// refuses when any of them drifted since the preview was computed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameSymbolBindingV1 {
    pub node_id: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub old_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_preview: Option<RenamePreviewAcceptanceV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum SourceEditRequest {
    StrReplace {
        path: String,
        old_str: String,
        new_str: String,
        dry_run: bool,
        verify: bool,
    },
    MultiStrReplace {
        path: String,
        replacements: Vec<(String, String)>,
        dry_run: bool,
        verify: bool,
    },
    InsertAt {
        path: String,
        anchor: String,
        content: String,
        before: bool,
        dry_run: bool,
        verify: bool,
    },
    AstGrepRewrite {
        path: String,
        pattern: String,
        rewrite: String,
        dry_run: bool,
        verify: bool,
    },
    ReplaceSymbol {
        symbol: String,
        new_source: String,
        dry_run: bool,
        verify: bool,
    },
    InsertAtSymbol {
        symbol: String,
        content: String,
        position: String,
        dry_run: bool,
        verify: bool,
    },
    MoveSymbol {
        symbol: String,
        dest_file: String,
        dry_run: bool,
        update_references: bool,
    },
    RenameSymbol {
        binding: RenameSymbolBindingV1,
        new_name: String,
        dry_run: bool,
        verify: bool,
    },
}

impl SourceEditRequest {
    pub const fn kind(&self) -> SourceEditKind {
        match self {
            Self::StrReplace { .. } => SourceEditKind::StrReplace,
            Self::MultiStrReplace { .. } => SourceEditKind::MultiStrReplace,
            Self::InsertAt { .. } => SourceEditKind::InsertAt,
            Self::AstGrepRewrite { .. } => SourceEditKind::AstGrepRewrite,
            Self::ReplaceSymbol { .. } => SourceEditKind::ReplaceSymbol,
            Self::InsertAtSymbol { .. } => SourceEditKind::InsertAtSymbol,
            Self::MoveSymbol { .. } => SourceEditKind::MoveSymbol,
            Self::RenameSymbol { .. } => SourceEditKind::RenameSymbol,
        }
    }

    pub const fn dry_run(&self) -> bool {
        match self {
            Self::StrReplace { dry_run, .. }
            | Self::MultiStrReplace { dry_run, .. }
            | Self::InsertAt { dry_run, .. }
            | Self::AstGrepRewrite { dry_run, .. }
            | Self::ReplaceSymbol { dry_run, .. }
            | Self::InsertAtSymbol { dry_run, .. }
            | Self::MoveSymbol { dry_run, .. }
            | Self::RenameSymbol { dry_run, .. } => *dry_run,
        }
    }

    pub const fn verify(&self) -> bool {
        match self {
            Self::StrReplace { verify, .. }
            | Self::MultiStrReplace { verify, .. }
            | Self::InsertAt { verify, .. }
            | Self::AstGrepRewrite { verify, .. }
            | Self::ReplaceSymbol { verify, .. }
            | Self::InsertAtSymbol { verify, .. }
            | Self::RenameSymbol { verify, .. } => *verify,
            Self::MoveSymbol { .. } => false,
        }
    }

    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        match &mut self {
            Self::StrReplace { dry_run: value, .. }
            | Self::MultiStrReplace { dry_run: value, .. }
            | Self::InsertAt { dry_run: value, .. }
            | Self::AstGrepRewrite { dry_run: value, .. }
            | Self::ReplaceSymbol { dry_run: value, .. }
            | Self::InsertAtSymbol { dry_run: value, .. }
            | Self::MoveSymbol { dry_run: value, .. }
            | Self::RenameSymbol { dry_run: value, .. } => *value = dry_run,
        }
        self
    }
}

mod effect_authorization;
mod output;
mod rename;
mod surface_request;

pub use effect_authorization::{
    SourceEditAuthorizationAdmissionV1, SourceEditAuthorizationFuture, SourceEditAuthorizationPort,
    SourceEditEffectProofV1, SourceEditEffectRequestV1, SourceEditReconciliationDispositionV1,
    SourceEditReconciliationRequestV1,
};
pub use output::{
    SourceEditCancelledResultV1, SourceEditDurableEffectPayloadV1, SourceEditEffectUnknownResultV1,
    SourceEditFailedResultV1, SourceEditReconciledResultV1, SourceEditSurfaceOutcomeV1,
    SourceEditSurfaceResultV1, SourceEditTimedOutResultV1,
};
pub use rename::{
    RenameDispositionCountsV1, RenameFileEditV1, RenameHazardKindV1, RenameHazardV1,
    RenameImpactV1, RenamePreviewAcceptanceV1, RenamePreviewNodeV1, RenamePreviewResultV1,
    RenameProtectedValueCategoryV1, RenameProtectedValueV1, RenameResult, RenameSiteDispositionV1,
    RenameSiteKindV1, RenameSiteV1,
};
pub use surface_request::{
    AstGrepRewriteSurfaceRequestV1, InsertAtSurfaceRequestV1, InsertAtSymbolSurfaceRequestV1,
    MoveSymbolSurfaceRequestV1, MultiStrReplaceSurfaceRequestV1, RenamePreviewSurfaceRequestV1,
    RenameSymbolSurfaceRequestV1, ReplaceSymbolSurfaceRequestV1, SourceEditApplyControlV1,
    SourceEditReconcileSurfaceRequestV1, SourceEditRollbackSurfaceRequestV1,
    StrReplaceSurfaceRequestV1,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceEditDiagnosticV1 {
    pub line: u32,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceEditVerificationStateV1 {
    Clean,
    Errors,
    Unavailable,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceEditVerificationV1 {
    pub state: SourceEditVerificationStateV1,
    pub verdict: String,
    pub error_count: usize,
    pub warning_count: usize,
    pub first_errors: Vec<SourceEditDiagnosticV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

const SOURCE_EDIT_KINDS: [SourceEditKind; 8] = [
    SourceEditKind::StrReplace,
    SourceEditKind::MultiStrReplace,
    SourceEditKind::InsertAt,
    SourceEditKind::AstGrepRewrite,
    SourceEditKind::ReplaceSymbol,
    SourceEditKind::InsertAtSymbol,
    SourceEditKind::MoveSymbol,
    SourceEditKind::RenameSymbol,
];

const SOURCE_EDIT_SURFACES: [BindingSurface; 2] = [BindingSurface::Cli, BindingSurface::Mcp];

pub fn source_edit_operation(
    kind: SourceEditKind,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = source_edit_schema(kind, "result")?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(format!(
            "capability.application.source-edit.{}",
            kind.operation_name().replace('_', "-")
        ))?,
        UseCaseId::new(format!(
            "use-case.application.source-edit.{}",
            kind.operation_name().replace('_', "-")
        ))?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

pub fn source_edit_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    let mut descriptors = SOURCE_EDIT_KINDS
        .into_iter()
        .map(|kind| {
            ApplicationHandlerDescriptor::new(
                source_edit_operation(kind)?,
                source_edit_schema(kind, "request")?,
                source_edit_schema(kind, "result")?,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    descriptors.push(ApplicationHandlerDescriptor::new(
        source_edit_reconciliation_operation()?,
        source_edit_reconciliation_schema("request")?,
        source_edit_reconciliation_schema("result")?,
    )?);
    descriptors.push(ApplicationHandlerDescriptor::new(
        source_edit_rollback_operation()?,
        source_edit_rollback_schema("request")?,
        source_edit_rollback_schema("result")?,
    )?);
    Ok(descriptors)
}

pub fn source_edit_catalog_contribution() -> Result<CatalogContributionV1, ApplicationContractError>
{
    let rollback_operation = source_edit_rollback_operation()?;
    let rollback_capability_id = rollback_operation.capability_id().clone();
    let mut capabilities = Vec::with_capacity(SOURCE_EDIT_KINDS.len() + 2);
    let mut bindings =
        Vec::with_capacity((SOURCE_EDIT_KINDS.len() + 2) * SOURCE_EDIT_SURFACES.len());
    for kind in SOURCE_EDIT_KINDS {
        let operation_name = kind.operation_name();
        let capability_id = CapabilityId::new(format!(
            "capability.application.source-edit.{}",
            operation_name.replace('_', "-")
        ))?;
        let (kind_bindings, binding_ids) =
            current_bindings(&capability_id, operation_name, SOURCE_EDIT_SURFACES)?;
        bindings.extend(kind_bindings);
        capabilities.push(CapabilityManifestV1::new(CapabilityManifestInputV1 {
            capability_id,
            use_case_id: UseCaseId::new(format!(
                "use-case.application.source-edit.{}",
                operation_name.replace('_', "-")
            ))?,
            routing: RoutingContractV1::new(
                1,
                format!("Apply {operation_name} source edit"),
                "Preview or apply one project-scoped source edit and optionally verify diagnostics.",
                vec![format!("Use {operation_name} on this project")],
            )?,
            request_schema: source_edit_schema(kind, "request")?,
            result_schema: source_edit_schema(kind, "result")?,
            effect: EffectClass::SourceEdit,
            scope: ScopeRequirement::new(vec![
                ScopeDimension::Project,
                ScopeDimension::Repository,
                ScopeDimension::Worktree,
            ])?,
            authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
            denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
            privacy: PrivacyClass::Sensitive,
            lifecycle: LifecycleClass::Resumable,
            streaming: StreamingContract::Unsupported,
            cancellation: CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeEffect,
                CancellationPoint::EffectInFlight,
                CancellationPoint::AfterCommit,
            ])?,
            deadline: DeadlineContract::new(30_000, DeadlineBehavior::ReturnEffectReceipt)?,
            pagination: None,
            idempotency: IdempotencyContract::Required,
            inverse: if kind == SourceEditKind::MoveSymbol {
                tracedecay_tool_catalog::InverseContract::Capability {
                    capability_id: rollback_capability_id.clone(),
                }
            } else {
                tracedecay_tool_catalog::InverseContract::Unavailable {
                    reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
                }
            },
            authority_revalidation: RevalidationContract::required(vec![
                RevalidationPoint::Authority,
                RevalidationPoint::Scope,
                RevalidationPoint::Policy,
                RevalidationPoint::Configuration,
                RevalidationPoint::ExpectedState,
            ])?,
            reconciliation: ReconciliationContract::Required,
            receipt: ReceiptContract::DurableEffect,
            terminal_states: TerminalStateContract::new(vec![
                TerminalState::Completed,
                TerminalState::Cancelled,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::EffectUnknown,
                TerminalState::Partial,
            ])?,
            availability: AvailabilityContract::Available,
            binding_ids,
            profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
            required_features: Vec::new(),
        })?);
    }
    let reconciliation_operation = source_edit_reconciliation_operation()?;
    let (reconciliation_bindings, reconciliation_binding_ids) = current_bindings_with_slug(
        reconciliation_operation.capability_id(),
        "source_edit_reconcile",
        "source-edit-reconcile",
        SOURCE_EDIT_SURFACES,
    )?;
    bindings.extend(reconciliation_bindings);
    capabilities.push(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: reconciliation_operation.capability_id().clone(),
        use_case_id: reconciliation_operation.use_case_id().clone(),
        routing: RoutingContractV1::new(
            1,
            "Reconcile an uncertain source edit",
            "Confirm the exact committed or rolled-back state of one retained source-edit effect.",
            vec!["Reconcile this retained source edit effect".to_owned()],
        )?,
        request_schema: source_edit_reconciliation_schema("request")?,
        result_schema: source_edit_reconciliation_schema("result")?,
        effect: EffectClass::SourceEdit,
        scope: ScopeRequirement::new(vec![
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
        ])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::Sensitive,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::AfterCommit,
        ])?,
        deadline: DeadlineContract::new(30_000, DeadlineBehavior::ReturnEffectReceipt)?,
        pagination: None,
        idempotency: IdempotencyContract::Required,
        inverse: tracedecay_tool_catalog::InverseContract::Unavailable {
            reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: ReconciliationContract::Required,
        receipt: ReceiptContract::DurableEffect,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Failed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ])?,
        availability: AvailabilityContract::Available,
        binding_ids: reconciliation_binding_ids,
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?);
    let (rollback_bindings, rollback_binding_ids) = current_bindings_with_slug(
        rollback_operation.capability_id(),
        "source_edit_rollback",
        "source-edit-rollback",
        SOURCE_EDIT_SURFACES,
    )?;
    bindings.extend(rollback_bindings);
    capabilities.push(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: rollback_operation.capability_id().clone(),
        use_case_id: rollback_operation.use_case_id().clone(),
        routing: RoutingContractV1::new(
            1,
            "Roll back a completed source edit",
            "Restore the exact retained preimages of one completed source-edit effect.",
            vec!["Roll back this completed source edit effect".to_owned()],
        )?,
        request_schema: source_edit_rollback_schema("request")?,
        result_schema: source_edit_rollback_schema("result")?,
        effect: EffectClass::SourceEdit,
        scope: ScopeRequirement::new(vec![
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
        ])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::Sensitive,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::AfterCommit,
        ])?,
        deadline: DeadlineContract::new(30_000, DeadlineBehavior::ReturnEffectReceipt)?,
        pagination: None,
        idempotency: IdempotencyContract::Required,
        inverse: tracedecay_tool_catalog::InverseContract::Unavailable {
            reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: ReconciliationContract::Required,
        receipt: ReceiptContract::DurableEffect,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Failed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ])?,
        availability: AvailabilityContract::Available,
        binding_ids: rollback_binding_ids,
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?);
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.source-edit")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?;
    let schemas = source_edit_executable_schemas(&contribution)?;
    Ok(contribution.with_executable_schemas(schemas)?)
}

/// SDK schemas are paired with the exact request accepted by each mounted MCP
/// operation and the exact typed result its daemon-owned use case serializes.
fn source_edit_executable_schemas(
    contribution: &CatalogContributionV1,
) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError> {
    macro_rules! schema {
        ($capability:expr, $request:ty, $result:ty) => {
            source_edit_executable_schema::<$request, $result>(
                contribution,
                $capability,
                concat!(
                    "tracedecay_application::source_edit::",
                    stringify!($request)
                ),
                concat!("tracedecay_application::source_edit::", stringify!($result)),
            )?
        };
    }

    let reconciliation = source_edit_reconciliation_operation()?;
    let rollback = source_edit_rollback_operation()?;
    Ok(vec![
        schema!(
            source_edit_operation(SourceEditKind::StrReplace)?.capability_id(),
            StrReplaceSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            source_edit_operation(SourceEditKind::MultiStrReplace)?.capability_id(),
            MultiStrReplaceSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            source_edit_operation(SourceEditKind::InsertAt)?.capability_id(),
            InsertAtSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            source_edit_operation(SourceEditKind::AstGrepRewrite)?.capability_id(),
            AstGrepRewriteSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            source_edit_operation(SourceEditKind::ReplaceSymbol)?.capability_id(),
            ReplaceSymbolSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            source_edit_operation(SourceEditKind::InsertAtSymbol)?.capability_id(),
            InsertAtSymbolSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            source_edit_operation(SourceEditKind::MoveSymbol)?.capability_id(),
            MoveSymbolSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            source_edit_operation(SourceEditKind::RenameSymbol)?.capability_id(),
            RenameSymbolSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            reconciliation.capability_id(),
            SourceEditReconcileSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
        schema!(
            rollback.capability_id(),
            SourceEditRollbackSurfaceRequestV1,
            SourceEditSurfaceResultV1
        ),
    ])
}

fn source_edit_executable_schema<Request, Response>(
    contribution: &CatalogContributionV1,
    capability_id: &CapabilityId,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError>
where
    Request: JsonSchema,
    Response: JsonSchema,
{
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == capability_id)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "source edit executable schema capability",
        })?;
    Ok(ExecutableSchemaAuthority::for_types_at_paths::<
        Request,
        Response,
    >(
        manifest, request_rust_type_path, result_rust_type_path
    )?)
}

pub fn source_edit_reconciliation_operation()
-> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = source_edit_reconciliation_schema("result")?;
    Ok(ApplicationOperation::new(
        CapabilityId::new("capability.application.source-edit.reconcile")?,
        UseCaseId::new("use-case.application.source-edit.reconcile")?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

fn source_edit_reconciliation_schema(suffix: &str) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!("schema.application.source-edit.reconcile.{suffix}"))?,
        1,
    )?)
}

fn source_edit_schema(
    kind: SourceEditKind,
    suffix: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.source-edit.{}.{}",
            kind.operation_name().replace('_', "-"),
            suffix
        ))?,
        1,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_edit_catalog_binds_every_typed_request_to_cli_and_mcp() {
        let contribution = source_edit_catalog_contribution().unwrap();
        assert_eq!(
            contribution.capabilities().len(),
            SOURCE_EDIT_KINDS.len() + 2
        );
        assert_eq!(
            contribution.bindings().len(),
            (SOURCE_EDIT_KINDS.len() + 2) * SOURCE_EDIT_SURFACES.len()
        );
        for capability in contribution.capabilities() {
            assert!(
                contribution
                    .executable_schema(capability.capability_id())
                    .is_some(),
                "{} must have its mounted typed schema",
                capability.capability_id().as_str()
            );
        }
        for kind in SOURCE_EDIT_KINDS {
            let operation = source_edit_operation(kind).unwrap();
            assert_eq!(
                operation.capability_id().as_str(),
                format!(
                    "capability.application.source-edit.{}",
                    kind.operation_name().replace('_', "-")
                )
            );
            for surface in SOURCE_EDIT_SURFACES {
                assert!(contribution.bindings().iter().any(|binding| {
                    binding.surface() == surface
                        && binding.operation().as_str() == kind.operation_name()
                }));
            }
        }
        let reconciliation = source_edit_reconciliation_operation().unwrap();
        assert!(
            contribution
                .capabilities()
                .iter()
                .any(|capability| { capability.capability_id() == reconciliation.capability_id() })
        );
        for surface in SOURCE_EDIT_SURFACES {
            assert!(contribution.bindings().iter().any(|binding| {
                binding.surface() == surface
                    && binding.operation().as_str() == "source_edit_reconcile"
            }));
        }
        let rollback = source_edit_rollback_operation().unwrap();
        let move_operation = source_edit_operation(SourceEditKind::MoveSymbol).unwrap();
        let move_capability = contribution
            .capabilities()
            .iter()
            .find(|capability| capability.capability_id() == move_operation.capability_id())
            .unwrap();
        assert_eq!(
            move_capability.inverse(),
            &tracedecay_tool_catalog::InverseContract::Capability {
                capability_id: rollback.capability_id().clone(),
            }
        );
        assert!(
            contribution
                .capabilities()
                .iter()
                .any(|capability| capability.capability_id() == rollback.capability_id())
        );
        for surface in SOURCE_EDIT_SURFACES {
            assert!(contribution.bindings().iter().any(|binding| {
                binding.surface() == surface
                    && binding.operation().as_str() == "source_edit_rollback"
            }));
        }
    }
}

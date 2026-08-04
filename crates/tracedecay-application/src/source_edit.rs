use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    ManifestDigest, PrivacyDomainId, RetrievalAnchorId, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass,
    PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract, RevalidationContract,
    RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement,
    StreamingContract, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::api_migration::ApiMigrationPlanV1;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::{
    ApplicationProblem, AuthorityReceipt, EffectId, IdempotencyKey, ResultContractRef,
};
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;
use crate::{
    RequestAdmission, RequestContext, ResolvedScope, current_bindings, current_bindings_with_slug,
};

const SOURCE_EDIT_EFFECT_REQUEST_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.source-edit-effect-request.v1";
const SOURCE_EDIT_RECONCILIATION_ATTEMPT_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.source-edit-reconciliation-attempt.v1";

/// `serde` `skip_serializing_if` predicate for default-off flags.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// Result of a single string replacement edit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEditKind {
    StrReplace,
    MultiStrReplace,
    InsertAt,
    AstGrepRewrite,
    ReplaceSymbol,
    InsertAtSymbol,
    MoveSymbol,
    ApiMigrationApply,
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
            Self::ApiMigrationApply => "api_migration_apply",
        }
    }
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
    ApiMigrationApply {
        plan: ApiMigrationPlanV1,
        plan_digest: ManifestDigest,
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
            Self::ApiMigrationApply { .. } => SourceEditKind::ApiMigrationApply,
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
            | Self::ApiMigrationApply { dry_run, .. } => *dry_run,
        }
    }

    pub const fn verify(&self) -> bool {
        match self {
            Self::StrReplace { verify, .. }
            | Self::MultiStrReplace { verify, .. }
            | Self::InsertAt { verify, .. }
            | Self::AstGrepRewrite { verify, .. }
            | Self::ReplaceSymbol { verify, .. }
            | Self::InsertAtSymbol { verify, .. } => *verify,
            Self::MoveSymbol { .. } => false,
            Self::ApiMigrationApply { verify, .. } => *verify,
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
            | Self::ApiMigrationApply { dry_run: value, .. } => *value = dry_run,
        }
        self
    }
}

/// Current sink evidence carried into a durable source-edit receipt.
///
/// The authority receipt is validated separately because it is refreshed at
/// admission and immediately before the effect. These digests bind the other
/// current authorities without persisting credentials or source text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditEffectProofV1 {
    pub policy_digest: ManifestDigest,
    pub configuration_revision_id: ConfigurationRevisionId,
    pub configuration_digest: ManifestDigest,
    pub catalog_revision: u32,
    pub catalog_digest: ManifestDigest,
    pub privacy_domain_id: PrivacyDomainId,
    pub privacy_key_epoch: u64,
    pub privacy_digest: ManifestDigest,
    pub external_proof: Option<RetrievalAnchorId>,
}

impl SourceEditEffectProofV1 {
    pub fn validate_for(
        &self,
        authority: &AuthorityReceipt,
    ) -> Result<(), ApplicationContractError> {
        self.policy_digest.validate()?;
        self.configuration_revision_id.validate()?;
        self.configuration_digest.validate()?;
        if self.catalog_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "source edit effect proof catalog revision",
            });
        }
        self.catalog_digest.validate()?;
        self.privacy_domain_id.validate()?;
        if self.privacy_key_epoch == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "source edit effect proof privacy key epoch",
            });
        }
        self.privacy_digest.validate()?;
        self.external_proof
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)?;
        if self.policy_digest != authority.policy.digest {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit effect proof policy digest",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEditAuthorizationAdmissionV1 {
    pub receipt: AuthorityReceipt,
    pub proof: SourceEditEffectProofV1,
}

impl SourceEditAuthorizationAdmissionV1 {
    pub fn new(
        receipt: AuthorityReceipt,
        proof: SourceEditEffectProofV1,
        scope: &ResolvedScope,
    ) -> Result<Self, ApplicationContractError> {
        let admission = Self { receipt, proof };
        admission.validate_for(scope)?;
        Ok(admission)
    }

    pub fn validate_for(&self, scope: &ResolvedScope) -> Result<(), ApplicationContractError> {
        self.receipt.validate_for(scope)?;
        self.proof.validate_for(&self.receipt)
    }
}

/// Immutable, transport-neutral request for one preview or journaled edit.
///
/// `expected_state` is the caller-observed digest of every file the edit may
/// touch. The concrete edit authority independently captures those files and
/// rejects a mismatch before publishing its durable prepared journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditEffectRequestV1 {
    pub context: RequestContext,
    pub authority: AuthorityReceipt,
    pub edit: SourceEditRequest,
    pub idempotency_key: IdempotencyKey,
    pub expected_state: ManifestDigest,
    pub proof: SourceEditEffectProofV1,
    pub observed_at: UtcMicros,
}

impl SourceEditEffectRequestV1 {
    pub fn input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(
            SOURCE_EDIT_EFFECT_REQUEST_DIGEST_DOMAIN_V1,
            self.context.actor(),
            self.context.scope(),
            &self.edit,
            &self.idempotency_key,
            &self.expected_state,
            &self.proof.external_proof,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.context.validate()?;
        self.authority.validate_for(self.context.scope())?;
        self.expected_state.validate()?;
        self.proof.validate_for(&self.authority)?;
        if let SourceEditRequest::ApiMigrationApply {
            plan, plan_digest, ..
        } = &self.edit
        {
            plan.validate()?;
            plan_digest.validate()?;
            if plan.blocked || plan.plan_digest != *plan_digest {
                return Err(ApplicationContractError::Inconsistent {
                    field: "API migration applicable plan digest",
                });
            }
        }
        let operation = source_edit_operation(self.edit.kind())?;
        if self.context.admission_at(self.observed_at) != RequestAdmission::Admitted {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit request admission",
            });
        }
        if !self
            .context
            .allows(operation.capability_id(), operation.use_case_id())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit request capability binding",
            });
        }
        let grant = self.context.grant();
        if self.authority.grant_id != grant.grant_id
            || self.authority.grant_revision != grant.revision
            || self.authority.grant_digest != grant.digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit request current grant",
            });
        }
        Ok(())
    }
}

/// Explicit conclusion supplied by an authorized reconciliation/inspection
/// operation. The concrete authority independently recaptures every candidate
/// file and accepts only an exact matching state digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "disposition")]
pub enum SourceEditReconciliationDispositionV1 {
    ConfirmCommitted { committed_state: ManifestDigest },
    ConfirmRolledBack,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditReconciliationRequestV1 {
    pub context: RequestContext,
    pub authority: AuthorityReceipt,
    pub kind: SourceEditKind,
    pub effect_id: EffectId,
    pub idempotency_key: IdempotencyKey,
    pub attempt_idempotency_key: IdempotencyKey,
    pub input_digest: ManifestDigest,
    pub disposition: SourceEditReconciliationDispositionV1,
    pub proof: SourceEditEffectProofV1,
    pub observed_at: UtcMicros,
}

impl SourceEditReconciliationRequestV1 {
    pub fn attempt_input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(
            SOURCE_EDIT_RECONCILIATION_ATTEMPT_DIGEST_DOMAIN_V1,
            self.context.actor(),
            self.context.scope(),
            self.kind,
            &self.effect_id,
            &self.idempotency_key,
            &self.attempt_idempotency_key,
            &self.input_digest,
            &self.disposition,
            &self.proof.external_proof,
        ))?)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.context.validate()?;
        self.authority.validate_for(self.context.scope())?;
        self.input_digest.validate()?;
        if self.attempt_idempotency_key == self.idempotency_key {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit reconciliation attempt idempotency key",
            });
        }
        self.proof.validate_for(&self.authority)?;
        if let SourceEditReconciliationDispositionV1::ConfirmCommitted { committed_state } =
            &self.disposition
        {
            committed_state.validate()?;
        }
        let operation = source_edit_reconciliation_operation()?;
        if self.context.admission_at(self.observed_at) != RequestAdmission::Admitted
            || !self
                .context
                .allows(operation.capability_id(), operation.use_case_id())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit reconciliation admission",
            });
        }
        let grant = self.context.grant();
        if self.authority.grant_id != grant.grant_id
            || self.authority.grant_revision != grant.revision
            || self.authority.grant_digest != grant.digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source edit reconciliation current grant",
            });
        }
        Ok(())
    }
}

pub type SourceEditAuthorizationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<SourceEditAuthorizationAdmissionV1, ApplicationProblem>>
            + Send
            + 'a,
    >,
>;

/// Current source-edit authorization. Production adapters must reload their
/// policy/configuration authority for `recheck_effect`; retaining the
/// admission receipt alone is not a recheck.
pub trait SourceEditAuthorizationPort: Send + Sync {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a>;

    fn recheck_effect<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        admission: &'a SourceEditAuthorizationAdmissionV1,
        observed_at: UtcMicros,
    ) -> SourceEditAuthorizationFuture<'a>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceEditDiagnosticV1 {
    pub line: u32,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEditVerificationStateV1 {
    Clean,
    Errors,
    Unavailable,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    SourceEditKind::ApiMigrationApply,
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
    Ok(descriptors)
}

pub fn source_edit_catalog_contribution() -> Result<CatalogContributionV1, ApplicationContractError>
{
    let mut capabilities = Vec::with_capacity(SOURCE_EDIT_KINDS.len() + 1);
    let mut bindings =
        Vec::with_capacity((SOURCE_EDIT_KINDS.len() + 1) * SOURCE_EDIT_SURFACES.len());
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
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.source-edit")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
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
            SOURCE_EDIT_KINDS.len() + 1
        );
        assert_eq!(
            contribution.bindings().len(),
            (SOURCE_EDIT_KINDS.len() + 1) * SOURCE_EDIT_SURFACES.len()
        );
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
    }
}

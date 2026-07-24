use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    ManifestDigest, PrivacyDomainId, RetrievalAnchorId, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogContributionInputV1, CatalogContributionV1, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract,
    LifecycleClass, PrivacyClass, ProfileId, ProtocolRevisionRange, ReceiptContract,
    ReconciliationContract, RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId,
    SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract, SurfaceBindingInputV1,
    SurfaceBindingV1, SurfaceOperationName, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::{
    ApplicationProblem, AuthorityReceipt, EffectId, IdempotencyKey, ResultContractRef,
};
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;
use crate::{RequestAdmission, RequestContext, ResolvedScope};

const SOURCE_EDIT_EFFECT_REQUEST_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.source-edit-effect-request.v1";
const SOURCE_EDIT_RECONCILIATION_ATTEMPT_DIGEST_DOMAIN_V1: &str =
    "tracedecay.application.source-edit-reconciliation-attempt.v1";

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
            | Self::MoveSymbol { dry_run, .. } => *dry_run,
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
            | Self::MoveSymbol { dry_run: value, .. } => *value = dry_run,
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

const SOURCE_EDIT_KINDS: [SourceEditKind; 7] = [
    SourceEditKind::StrReplace,
    SourceEditKind::MultiStrReplace,
    SourceEditKind::InsertAt,
    SourceEditKind::AstGrepRewrite,
    SourceEditKind::ReplaceSymbol,
    SourceEditKind::InsertAtSymbol,
    SourceEditKind::MoveSymbol,
];

const SOURCE_EDIT_SURFACES: [BindingSurface; 2] = [BindingSurface::Cli, BindingSurface::Mcp];

pub fn source_edit_operation(
    kind: SourceEditKind,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = source_edit_schema(kind, "result", 1_048_576)?;
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
                source_edit_schema(kind, "request", 1_048_576)?,
                source_edit_schema(kind, "result", 1_048_576)?,
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
        let mut binding_ids = Vec::with_capacity(SOURCE_EDIT_SURFACES.len());
        for surface in SOURCE_EDIT_SURFACES {
            let binding_id = BindingId::new(format!(
                "binding.{}.{operation_name}.v1",
                source_edit_surface_name(surface)
            ))?;
            bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
                binding_id: binding_id.clone(),
                capability_id: capability_id.clone(),
                surface,
                operation: SurfaceOperationName::new(operation_name)?,
                protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
                required_features: Vec::new(),
                status: BindingStatus::Current,
                alias_of: None,
            })?);
            binding_ids.push(binding_id);
        }
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
            request_schema: source_edit_schema(kind, "request", 1_048_576)?,
            result_schema: source_edit_schema(kind, "result", 1_048_576)?,
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
    let mut reconciliation_binding_ids = Vec::with_capacity(SOURCE_EDIT_SURFACES.len());
    for surface in SOURCE_EDIT_SURFACES {
        let binding_id = BindingId::new(format!(
            "binding.{}.source-edit-reconcile.v1",
            source_edit_surface_name(surface)
        ))?;
        bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
            binding_id: binding_id.clone(),
            capability_id: reconciliation_operation.capability_id().clone(),
            surface,
            operation: SurfaceOperationName::new("source_edit_reconcile")?,
            protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
            required_features: Vec::new(),
            status: BindingStatus::Current,
            alias_of: None,
        })?);
        reconciliation_binding_ids.push(binding_id);
    }
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

fn source_edit_surface_name(surface: BindingSurface) -> &'static str {
    match surface {
        BindingSurface::Cli => "cli",
        BindingSurface::Mcp => "mcp",
        _ => unreachable!("source edits bind only CLI and MCP"),
    }
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
        1_048_576,
    )?)
}

fn source_edit_schema(
    kind: SourceEditKind,
    suffix: &str,
    maximum_bytes: u32,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.source-edit.{}.{}",
            kind.operation_name().replace('_', "-"),
            suffix
        ))?,
        1,
        maximum_bytes,
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

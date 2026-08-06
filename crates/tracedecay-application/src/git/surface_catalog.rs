//! Public read-only Git intelligence and preview/apply surface bindings.
//!
//! Internal `stage_hunks` / `unstage_hunks` / `commit_index` capabilities remain
//! application-only (no surface bindings). Adapters expose only `git_preview`
//! and `git_apply`; query status/diff/history/blame/hunk reads are callable
//! independently and expose no mutation capability.

use schemars::JsonSchema;
use tracedecay_domain::{GitIndexPreviewV1, GitIndexTransactionReceiptV1};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, CatalogValidationError, CodecBindingKey,
    ContributionId, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingV1, IdempotencyContract, LifecycleClass, OperationId, PrivacyClass, ProfileId,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RouteExposureV1, RoutingContractV1, SchemaBodyAuthorityV1, SchemaId, SchemaRef, ScopeDimension,
    ScopeRequirement, SdkExecutableBindingAvailabilityV1, SdkExecutableBindingRegistryV1,
    SdkExecutableBindingV1, SdkTransportBindingV1, ServiceId, StreamingContract,
    SurfaceOperationName, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::git::{
    GitApplySurfaceRequest, GitBlameSurfaceRequest, GitDiffSurfaceRequest,
    GitHistorySurfaceRequest, GitHunksSurfaceRequest, GitPreviewSurfaceRequest, GitReadResultV1,
    GitStatusSurfaceRequest,
};
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;

struct SurfaceSpec {
    capability: &'static str,
    use_case: &'static str,
    request_schema: &'static str,
    result_schema: &'static str,
    operation: &'static str,
    sdk_operation_id: &'static str,
    mcp_tool_name: &'static str,
    effect: EffectClass,
    summary: &'static str,
    description: &'static str,
    example: &'static str,
    surfaces: &'static [BindingSurface],
}

const CLI_MCP_SURFACES: [BindingSurface; 2] = [BindingSurface::Cli, BindingSurface::Mcp];
const TRANSPORT_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];
const SURFACE_SPECS: [SurfaceSpec; 7] = [
    SurfaceSpec {
        capability: "capability.application.git.status",
        use_case: "use-case.application.git.status",
        request_schema: "schema.application.git.status.request",
        result_schema: "schema.application.git.status.result",
        operation: "git_status",
        sdk_operation_id: "operation.application.git.status",
        mcp_tool_name: "tracedecay_git_status",
        effect: EffectClass::Read,
        summary: "Read typed Git status",
        description: "Read bounded typed status for one exact admitted project worktree.",
        example: "Show typed Git status for this project",
        surfaces: &TRANSPORT_SURFACES,
    },
    SurfaceSpec {
        capability: "capability.application.git.diff",
        use_case: "use-case.application.git.diff",
        request_schema: "schema.application.git.diff.request",
        result_schema: "schema.application.git.diff.result",
        operation: "git_diff",
        sdk_operation_id: "operation.application.git.diff",
        mcp_tool_name: "tracedecay_git_diff",
        effect: EffectClass::Read,
        summary: "Read a typed Git diff",
        description: "Read one bounded working-tree, staged, or exact commit-range diff.",
        example: "Show the typed staged Git diff",
        surfaces: &TRANSPORT_SURFACES,
    },
    SurfaceSpec {
        capability: "capability.application.git.history",
        use_case: "use-case.application.git.history",
        request_schema: "schema.application.git.history.request",
        result_schema: "schema.application.git.history.result",
        operation: "git_history",
        sdk_operation_id: "operation.application.git.history",
        mcp_tool_name: "tracedecay_git_history",
        effect: EffectClass::Read,
        summary: "Read bounded Git history",
        description: "Read bounded typed commit history for one exact admitted project worktree.",
        example: "Show recent typed Git history",
        surfaces: &TRANSPORT_SURFACES,
    },
    SurfaceSpec {
        capability: "capability.application.git.blame",
        use_case: "use-case.application.git.blame",
        request_schema: "schema.application.git.blame.request",
        result_schema: "schema.application.git.blame.result",
        operation: "git_blame",
        sdk_operation_id: "operation.application.git.blame",
        mcp_tool_name: "tracedecay_git_blame",
        effect: EffectClass::Read,
        summary: "Read typed Git blame",
        description: "Read bounded typed line provenance for one admitted path.",
        example: "Show typed Git blame for this file",
        surfaces: &TRANSPORT_SURFACES,
    },
    SurfaceSpec {
        capability: "capability.application.git.hunks",
        use_case: "use-case.application.git.hunks",
        request_schema: "schema.application.git.hunks.request",
        result_schema: "schema.application.git.hunks.result",
        operation: "git_hunks",
        sdk_operation_id: "operation.application.git.hunks",
        mcp_tool_name: "tracedecay_git_hunks",
        effect: EffectClass::Read,
        summary: "Read typed Git hunk references",
        description: "Mint bounded HunkRef evidence from one working-tree or staged diff.",
        example: "List typed hunk references for the staged diff",
        surfaces: &TRANSPORT_SURFACES,
    },
    SurfaceSpec {
        capability: "capability.application.git.preview",
        use_case: "use-case.application.git.preview",
        request_schema: "schema.application.git.preview.request",
        result_schema: "schema.application.git.preview.result",
        operation: "git_preview",
        sdk_operation_id: "operation.application.git.preview",
        mcp_tool_name: "tracedecay_git_preview",
        effect: EffectClass::Preview,
        summary: "Preview Git index mutations",
        description: "Build an immutable preview for selected index mutations with CAS evidence.",
        example: "Preview staging these hunks",
        surfaces: &CLI_MCP_SURFACES,
    },
    SurfaceSpec {
        capability: "capability.application.git.apply",
        use_case: "use-case.application.git.apply",
        request_schema: "schema.application.git.apply.request",
        result_schema: "schema.application.git.apply.result",
        operation: "git_apply",
        sdk_operation_id: "operation.application.git.apply",
        mcp_tool_name: "tracedecay_git_apply",
        // Public apply is a facade over preview-bound stage/unstage/commit.
        // The exact Git-index effect class is fixed by the preview identity.
        effect: EffectClass::Administrative,
        summary: "Apply a Git index preview",
        description: "Apply one exact preview identity through daemon-serialized index transactions.",
        example: "Apply the previewed Git index mutation",
        surfaces: &CLI_MCP_SURFACES,
    },
];

/// Catalog contribution for public Git read and preview/apply bindings.
pub fn git_surface_catalog_contribution() -> Result<CatalogContributionV1, ApplicationContractError>
{
    let mut capabilities = Vec::with_capacity(SURFACE_SPECS.len());
    let mut bindings = Vec::new();

    for spec in &SURFACE_SPECS {
        let capability_id = CapabilityId::new(spec.capability)?;
        let (spec_bindings, binding_ids) = current_bindings(
            &capability_id,
            spec.operation,
            spec.surfaces.iter().copied(),
        )?;
        bindings.extend(spec_bindings);
        capabilities.push(capability(spec, capability_id, binding_ids)?);
    }

    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.git-surface")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
}

/// Schema-backed named SDK bindings for every public Git surface.
///
/// Git's generated SDK invokes the mounted MCP tools. The embedded executable
/// records remain internal because no Git HTTP route is being introduced by
/// this SDK projection.
pub fn git_sdk_executable_binding_registry()
-> Result<SdkExecutableBindingRegistryV1, CatalogValidationError> {
    let contribution =
        git_surface_catalog_contribution().map_err(|_| CatalogValidationError::InvalidValue {
            field: "Git SDK catalog",
            reason: "could not build the canonical Git surface contribution",
        })?;
    let bindings = SURFACE_SPECS
        .iter()
        .map(|spec| git_sdk_binding(&contribution, spec))
        .collect::<Result<Vec<_>, _>>()?;
    SdkExecutableBindingRegistryV1::new(bindings)
}

fn git_sdk_binding(
    contribution: &CatalogContributionV1,
    spec: &SurfaceSpec,
) -> Result<SdkExecutableBindingAvailabilityV1, CatalogValidationError> {
    let capability_id =
        CapabilityId::new(spec.capability).map_err(|_| CatalogValidationError::InvalidValue {
            field: "Git SDK capability ID",
            reason: "is not a canonical capability identifier",
        })?;
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == &capability_id)
        .ok_or(CatalogValidationError::InvalidValue {
            field: "Git SDK capability",
            reason: "is missing from the canonical Git contribution",
        })?;
    let binding_id = contribution
        .bindings()
        .iter()
        .find(|binding| {
            binding.capability_id() == manifest.capability_id()
                && binding.surface() == BindingSurface::Mcp
                && binding.operation().as_str() == spec.operation
        })
        .map(|binding| binding.binding_id().clone())
        .ok_or(CatalogValidationError::InvalidCapability {
            capability_id,
            reason: "is missing its mounted MCP binding",
        })?;

    match spec.operation {
        "git_status" => git_mcp_sdk_binding::<GitStatusSurfaceRequest, GitReadResultV1>(
            manifest, spec, binding_id,
        ),
        "git_diff" => git_mcp_sdk_binding::<GitDiffSurfaceRequest, GitReadResultV1>(
            manifest, spec, binding_id,
        ),
        "git_history" => git_mcp_sdk_binding::<GitHistorySurfaceRequest, GitReadResultV1>(
            manifest, spec, binding_id,
        ),
        "git_blame" => git_mcp_sdk_binding::<GitBlameSurfaceRequest, GitReadResultV1>(
            manifest, spec, binding_id,
        ),
        "git_hunks" => git_mcp_sdk_binding::<GitHunksSurfaceRequest, GitReadResultV1>(
            manifest, spec, binding_id,
        ),
        "git_preview" => git_mcp_sdk_binding::<GitPreviewSurfaceRequest, GitIndexPreviewV1>(
            manifest, spec, binding_id,
        ),
        "git_apply" => git_mcp_sdk_binding::<GitApplySurfaceRequest, GitIndexTransactionReceiptV1>(
            manifest, spec, binding_id,
        ),
        _ => Err(CatalogValidationError::InvalidValue {
            field: "Git SDK operation",
            reason: "is absent from the canonical Git schema registry",
        }),
    }
}

fn git_mcp_sdk_binding<Request, Output>(
    manifest: &CapabilityManifestV1,
    spec: &SurfaceSpec,
    binding_id: BindingId,
) -> Result<SdkExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: JsonSchema,
    Output: JsonSchema,
{
    let executable = ExecutableBindingV1::daemon_owned(
        manifest,
        OperationId::new(spec.sdk_operation_id).map_err(|_| {
            CatalogValidationError::InvalidValue {
                field: "Git SDK operation ID",
                reason: "is not a canonical operation identifier",
            }
        })?,
        ServiceId::new("service.application.git").map_err(|_| {
            CatalogValidationError::InvalidValue {
                field: "Git SDK service ID",
                reason: "is not a canonical service identifier",
            }
        })?,
        SchemaBodyAuthorityV1::for_type::<Request>(manifest.request_schema().clone())?,
        SchemaBodyAuthorityV1::for_type::<Output>(manifest.result_schema().clone())?,
        CodecBindingKey::new(format!("codec.application.git.{}.json.v1", spec.operation)).map_err(
            |_| CatalogValidationError::InvalidValue {
                field: "Git SDK codec binding",
                reason: "is not a canonical codec identifier",
            },
        )?,
        RouteExposureV1::Internal,
    )?;
    let binding = SdkExecutableBindingV1::new(
        executable,
        binding_id,
        SurfaceOperationName::new(spec.operation).map_err(|_| {
            CatalogValidationError::InvalidValue {
                field: "Git SDK method",
                reason: "is not a canonical surface operation name",
            }
        })?,
        SdkTransportBindingV1::McpTool {
            tool_name: spec.mcp_tool_name.to_owned(),
        },
    )?;
    Ok(SdkExecutableBindingAvailabilityV1::available(binding))
}

pub fn git_surface_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    SURFACE_SPECS.iter().map(handler_descriptor).collect()
}

fn capability(
    spec: &SurfaceSpec,
    capability_id: CapabilityId,
    binding_ids: Vec<BindingId>,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id: UseCaseId::new(spec.use_case)?,
        routing: RoutingContractV1::new(
            1,
            spec.summary,
            spec.description,
            vec![spec.example.to_owned()],
        )?,
        request_schema: schema(spec.request_schema)?,
        result_schema: schema(spec.result_schema)?,
        effect: spec.effect,
        scope: ScopeRequirement::new(vec![
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
        ])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(cancellation_points(spec.effect))?,
        deadline: DeadlineContract::new(30_000, deadline_behavior(spec.effect))?,
        pagination: None,
        idempotency: if spec.effect.is_effect() {
            IdempotencyContract::Required
        } else {
            IdempotencyContract::NotRequired
        },
        inverse: if spec.effect.is_effect() {
            tracedecay_tool_catalog::InverseContract::Unavailable {
                reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
            }
        } else {
            tracedecay_tool_catalog::InverseContract::NotApplicable
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: if spec.effect.is_effect() {
            ReconciliationContract::Required
        } else {
            ReconciliationContract::NotRequired
        },
        receipt: if spec.effect.is_effect() {
            ReceiptContract::DurableEffect
        } else {
            ReceiptContract::Operation
        },
        terminal_states: TerminalStateContract::new(terminal_states(spec.effect))?,
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?)
}

fn cancellation_points(effect: EffectClass) -> Vec<CancellationPoint> {
    if effect.is_effect() {
        vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::AfterCommit,
        ]
    } else {
        vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ]
    }
}

fn deadline_behavior(effect: EffectClass) -> DeadlineBehavior {
    if effect.is_effect() {
        DeadlineBehavior::ReturnEffectReceipt
    } else {
        DeadlineBehavior::ReturnOperationReceipt
    }
}

fn terminal_states(effect: EffectClass) -> Vec<TerminalState> {
    if effect.is_effect() {
        vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Unavailable,
            TerminalState::Failed,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ]
    } else {
        vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Unavailable,
            TerminalState::Failed,
            TerminalState::Partial,
        ]
    }
}

fn handler_descriptor(
    spec: &SurfaceSpec,
) -> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    let result_schema = schema(spec.result_schema)?;
    ApplicationHandlerDescriptor::new(
        ApplicationOperation::new(
            CapabilityId::new(spec.capability)?,
            UseCaseId::new(spec.use_case)?,
            ResultContractRef::from_schema(&result_schema),
            true,
        ),
        schema(spec.request_schema)?,
        result_schema,
    )
}

fn schema(id: &str) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(SchemaId::new(id)?, 1)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_git_bindings_include_reads_and_exclude_internal_index_steps() {
        let contribution = git_surface_catalog_contribution().expect("contribution");
        let operations: Vec<_> = contribution
            .bindings()
            .iter()
            .map(|binding| binding.operation().as_str().to_owned())
            .collect();
        for expected in [
            "git_status",
            "git_diff",
            "git_history",
            "git_blame",
            "git_hunks",
            "git_preview",
            "git_apply",
        ] {
            assert!(operations.iter().any(|name| name == expected), "{expected}");
        }
        assert!(!operations.iter().any(|name| {
            name.contains("stage_hunks")
                || name.contains("unstage_hunks")
                || name.contains("commit_index")
        }));
        assert!(contribution.bindings().iter().all(|binding| {
            binding.surface() != BindingSurface::Http
                || !matches!(binding.operation().as_str(), "git_preview" | "git_apply")
        }));
        for operation in [
            "git_status",
            "git_diff",
            "git_history",
            "git_blame",
            "git_hunks",
        ] {
            assert!(contribution.bindings().iter().any(|binding| {
                binding.operation().as_str() == operation
                    && binding.surface() == BindingSurface::Http
            }));
        }
    }
}

//! Catalog contracts for the typed configuration control plane.
//!
//! The root runtime owns concrete stores and authorization. This crate keeps
//! the reviewed operation identities, schemas, and surface bindings beside the
//! application feature without importing a transport or persistence adapter.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use tracedecay_domain::configuration::ConfigurationSettlementAuthorityV1;
use tracedecay_domain::configuration::{
    ChangePlanId, ConfigurationAuditEvent, ConfigurationAuditEventId, ConfigurationCandidateV1,
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationReceiptId,
    ConfigurationRevisionId, ConfigurationSnapshotId, ConfigurationValueV1, CredentialKindV1,
    CredentialReferenceId, ProtectedChange, RestartRequirementV1, RollbackModeV1, SettingKey,
    SettingSensitivityV1,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, CodecBindingKey, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableSchemaAuthority, IdempotencyContract, LifecycleClass, OperationId,
    PaginationContract, PrivacyClass, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RouteExposureV1, RoutingContractV1, SchemaId,
    SchemaRef, ScopeDimension, ScopeRequirement, ServiceId, StreamingContract, TerminalState,
    TerminalStateContract, UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::{
    APPLICATION_ADMINISTRATIVE_PROFILE_ID, APPLICATION_DEFAULT_PROFILE_ID, application_profile_ids,
};

mod semantic_lifecycle;
pub use semantic_lifecycle::*;

/// Typed input for the first configuration read migrated through the daemon
/// invocation boundary.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationListRequestV1 {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationGetRequestV1 {
    pub key: SettingKey,
}

/// Typed revision-CAS input for the first configuration write migrated through
/// the daemon invocation boundary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSetRequestV1 {
    pub layer: ConfigurationLayerIdV1,
    pub key: SettingKey,
    pub value: ConfigurationValueV1,
    pub expected_revision: ConfigurationRevisionId,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum ConfigurationDirectMutationRequestV1 {
    Set {
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
        value: ConfigurationValueV1,
    },
    Unset {
        layer: ConfigurationLayerIdV1,
        key: SettingKey,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationUnsetRequestV1 {
    pub layer: ConfigurationLayerIdV1,
    pub key: SettingKey,
    pub expected_revision: ConfigurationRevisionId,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationBatchRequestV1 {
    pub mutations: Vec<ConfigurationDirectMutationRequestV1>,
    pub expected_revision: ConfigurationRevisionId,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationWriteCredentialRequestV1 {
    pub expected_reference_id: Option<CredentialReferenceId>,
    pub kind: CredentialKindV1,
    pub write_handle: String,
    pub expected_revision: ConfigurationRevisionId,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationObservedStateRequestV1 {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationProtectedPreviewRequestV1 {
    pub change: ProtectedChange,
    pub expected_revision: ConfigurationRevisionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationProtectedApplyRequestV1 {
    pub plan_id: ChangePlanId,
    pub expected_base_revision_id: ConfigurationRevisionId,
    pub operation_digest: ManifestDigest,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationRollbackPreviewRequestV1 {
    pub target_revision_id: ConfigurationRevisionId,
    pub mode: RollbackModeV1,
}

pub type ConfigurationRollbackApplyRequestV1 = ConfigurationProtectedApplyRequestV1;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationAuditRequestV1 {
    #[serde(default)]
    pub after_event_id: Option<ConfigurationAuditEventId>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SettingSummary {
    pub key: SettingKey,
    pub sensitivity: SettingSensitivityV1,
    pub restart_requirement: RestartRequirementV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ResolvedSetting {
    pub key: SettingKey,
    pub effective_value: ConfigurationValueV1,
    pub snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: ManifestDigest,
    pub resolution_provenance_digest: ManifestDigest,
    pub candidates: Vec<ConfigurationCandidateV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivationDriftV1 {
    Current,
    NeverActivated,
    PendingRestart,
    ActivationFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ComponentConfigurationState {
    pub component: String,
    pub desired_revision_id: ConfigurationRevisionId,
    pub observed_revision_id: Option<ConfigurationRevisionId>,
    pub last_working_revision_id: Option<ConfigurationRevisionId>,
    pub restart_required: bool,
    pub activation_error_code: Option<String>,
    pub drift: ActivationDriftV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ConfigurationMutationReceipt {
    pub receipt_id: ConfigurationReceiptId,
    pub base_revision_id: ConfigurationRevisionId,
    pub result_revision_id: ConfigurationRevisionId,
    pub snapshot_id: ConfigurationSnapshotId,
    pub operation_digest: ManifestDigest,
    pub settlement_authority: ConfigurationSettlementAuthorityV1,
    pub created_at: UtcMicros,
    pub effective_deadline_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ConfigurationAuditPage {
    pub events: Vec<ConfigurationAuditEvent>,
    pub next_after_event_id: Option<ConfigurationAuditEventId>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "operation", content = "request")]
pub enum ConfigurationWireRequestV1 {
    List(ConfigurationListRequestV1),
    Explain(ConfigurationGetRequestV1),
    Get(ConfigurationGetRequestV1),
    Set(ConfigurationSetRequestV1),
    Unset(ConfigurationUnsetRequestV1),
    Batch(ConfigurationBatchRequestV1),
    WriteCredential(ConfigurationWriteCredentialRequestV1),
    ObservedState(ConfigurationObservedStateRequestV1),
    ProtectedPreview(ConfigurationProtectedPreviewRequestV1),
    ProtectedApply(ConfigurationProtectedApplyRequestV1),
    RollbackPreview(ConfigurationRollbackPreviewRequestV1),
    RollbackApply(ConfigurationRollbackApplyRequestV1),
    Audit(ConfigurationAuditRequestV1),
    SemanticModelRetry(SemanticLifecycleControlRequestV1),
    SemanticModelRemove(SemanticLifecycleControlRequestV1),
    SemanticModelRollback(SemanticLifecycleControlRequestV1),
    SemanticEmbeddingImportLocal(SemanticEmbeddingLocalImportRequestV1),
    SemanticEmbeddingImportConfiguredHttps(SemanticEmbeddingConfiguredHttpsImportRequestV1),
    SemanticRerankerImportLocal(SemanticRerankerLocalImportRequestV1),
    SemanticRerankerImportConfiguredHttps(SemanticRerankerConfiguredHttpsImportRequestV1),
    SemanticRerankerRollback(SemanticRerankerRollbackRequestV1),
}

struct ConfigurationSurfaceSpec {
    name: &'static str,
    summary: &'static str,
    description: &'static str,
    example: &'static str,
    effect: EffectClass,
    paginated: bool,
    maximum_deadline_millis: u64,
    surfaces: &'static [BindingSurface],
}

const CONFIGURATION_SURFACES: [BindingSurface; 4] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
    BindingSurface::Dashboard,
];

const CONFIGURATION_SPECS: [ConfigurationSurfaceSpec; 21] = [
    ConfigurationSurfaceSpec {
        name: "configuration_list",
        summary: "List configuration settings",
        description: "List typed settings visible through the retained configuration authority.",
        example: "List project configuration settings",
        effect: EffectClass::Read,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_explain",
        summary: "Explain effective configuration",
        description: "Explain the resolved value and provenance for one typed setting.",
        example: "Explain this configuration setting",
        effect: EffectClass::Read,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_get",
        summary: "Get effective configuration",
        description: "Read one effective typed configuration setting.",
        example: "Get this configuration setting",
        effect: EffectClass::Read,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_set",
        summary: "Set configuration value",
        description: "Apply one authorized typed configuration value with revision CAS.",
        example: "Set this project configuration value",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_unset",
        summary: "Unset configuration value",
        description: "Remove one authorized typed configuration value with revision CAS.",
        example: "Unset this project configuration value",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_batch",
        summary: "Apply configuration batch",
        description: "Apply one authorized atomic batch of typed configuration mutations.",
        example: "Apply these project configuration changes together",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_write_credential",
        summary: "Write credential reference",
        description: "Resolve an opaque credential handle into write-only reference metadata.",
        example: "Rotate this configuration credential reference",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_observed_state",
        summary: "Read configuration activation state",
        description: "Read desired versus observed component configuration state.",
        example: "Show configuration activation drift",
        effect: EffectClass::Read,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_protected_preview",
        summary: "Preview protected configuration change",
        description: "Create a revalidated redacted preview for a protected configuration change.",
        example: "Preview this protected configuration change",
        effect: EffectClass::Preview,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_protected_apply",
        summary: "Apply protected configuration change",
        description: "Apply an actor-bound protected configuration preview with exact CAS evidence.",
        example: "Apply this approved protected configuration change",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_rollback_preview",
        summary: "Preview configuration rollback",
        description: "Create a forward rollback preview against one historical revision.",
        example: "Preview rollback to this configuration revision",
        effect: EffectClass::Preview,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_rollback_apply",
        summary: "Apply configuration rollback",
        description: "Apply an actor-bound forward rollback preview with exact CAS evidence.",
        example: "Apply this approved configuration rollback",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_audit",
        summary: "Read configuration audit",
        description: "Read reauthorized append-only redacted configuration audit events.",
        example: "Show configuration audit history",
        effect: EffectClass::Read,
        paginated: true,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_model_retry",
        summary: "Retry semantic model lifecycle",
        description: "Retry the selected semantic model through the daemon-owned lifecycle owner.",
        example: "Retry the selected semantic model",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_model_remove",
        summary: "Remove semantic model installation",
        description: "Remove the selected semantic installation through the daemon-owned lifecycle owner.",
        example: "Remove the selected semantic model installation",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_model_rollback",
        summary: "Rollback semantic model",
        description: "Restore the prior verified semantic model through the daemon-owned lifecycle owner.",
        example: "Rollback the semantic model to its verified predecessor",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_embedding_import_local",
        summary: "Import local embedding artifact",
        description: "Verify and install an explicitly selected local embedding artifact.",
        example: "Import this local embedding artifact",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 900_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_embedding_import_configured_https",
        summary: "Import configured HTTPS embedding artifact",
        description: "Verify and install an explicitly configured immutable HTTPS embedding artifact.",
        example: "Import this configured HTTPS embedding artifact",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 900_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_reranker_import_local",
        summary: "Import local reranker artifact",
        description: "Verify and install an explicitly selected local reranker artifact.",
        example: "Import this local reranker artifact",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 900_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_reranker_import_configured_https",
        summary: "Import configured HTTPS reranker artifact",
        description: "Verify and install an explicitly configured immutable HTTPS reranker artifact.",
        example: "Import this configured HTTPS reranker artifact",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 900_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
    ConfigurationSurfaceSpec {
        name: "semantic_reranker_rollback",
        summary: "Rollback reranker artifact",
        description: "Restore the prior verified reranker artifact through the daemon-owned lifecycle owner.",
        example: "Rollback the reranker artifact to its verified predecessor",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
        maximum_deadline_millis: 15_000,
        surfaces: &CONFIGURATION_SURFACES,
    },
];

pub const CONFIGURATION_SURFACE_OPERATION_NAMES: [&str; 21] = [
    "configuration_list",
    "configuration_explain",
    "configuration_get",
    "configuration_set",
    "configuration_unset",
    "configuration_batch",
    "configuration_write_credential",
    "configuration_observed_state",
    "configuration_protected_preview",
    "configuration_protected_apply",
    "configuration_rollback_preview",
    "configuration_rollback_apply",
    "configuration_audit",
    "semantic_model_retry",
    "semantic_model_remove",
    "semantic_model_rollback",
    "semantic_embedding_import_local",
    "semantic_embedding_import_configured_https",
    "semantic_reranker_import_local",
    "semantic_reranker_import_configured_https",
    "semantic_reranker_rollback",
];

pub fn configuration_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(CONFIGURATION_SPECS.len());
    let mut bindings = Vec::with_capacity(CONFIGURATION_SPECS.len() * CONFIGURATION_SURFACES.len());

    for spec in &CONFIGURATION_SPECS {
        let capability_id = CapabilityId::new(capability_id(spec.name))?;
        let (spec_bindings, binding_ids) =
            current_bindings(&capability_id, spec.name, spec.surfaces.iter().copied())?;
        bindings.extend(spec_bindings);
        capabilities.push(capability(spec, capability_id, binding_ids)?);
    }

    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.configuration-surface")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?;
    let schemas = configuration_executable_schemas(&contribution)?;
    Ok(contribution.with_executable_schemas(schemas)?)
}

/// Daemon-owned public HTTP bindings for every shipped configuration use case.
///
/// The contribution above owns both manifest references and generated schema
/// bodies. This registry adds only the concrete daemon service, codec, and
/// externally mounted HTTP path consumed by first-party SDKs.
pub fn configuration_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    let contribution = configuration_surface_catalog_contribution()?;
    let service_id = ServiceId::new("service.application.configuration")?;
    let mut bindings = Vec::with_capacity(CONFIGURATION_SPECS.len());
    for spec in &CONFIGURATION_SPECS {
        let capability_id = CapabilityId::new(capability_id(spec.name))?;
        let manifest = contribution
            .capabilities()
            .binary_search_by(|manifest| manifest.capability_id().cmp(&capability_id))
            .ok()
            .map(|index| &contribution.capabilities()[index])
            .ok_or(ApplicationContractError::Inconsistent {
                field: "configuration executable capability",
            })?;
        let schema = contribution.executable_schema(&capability_id).ok_or(
            ApplicationContractError::Inconsistent {
                field: "configuration executable schema",
            },
        )?;
        let http_binding = contribution
            .bindings()
            .iter()
            .find(|binding| {
                binding.capability_id() == &capability_id
                    && binding.surface() == BindingSurface::Http
            })
            .ok_or(ApplicationContractError::Inconsistent {
                field: "configuration HTTP binding",
            })?;
        let executable = ExecutableBindingV1::daemon_owned(
            manifest,
            OperationId::new(format!("operation.application.{}", spec.name))?,
            service_id.clone(),
            schema.request_schema().clone(),
            schema.result_schema().clone(),
            CodecBindingKey::new(format!(
                "codec.application.configuration.{}.json.v1",
                spec.name
            ))?,
            RouteExposureV1::Public {
                binding_id: http_binding.binding_id().clone(),
                route_path: format!("/application/configuration/{}", spec.name),
            },
        )?;
        bindings.push(ExecutableBindingAvailabilityV1::available(executable));
    }
    Ok(ExecutableBindingRegistryV1::new(bindings)?)
}

fn configuration_executable_schemas(
    contribution: &CatalogContributionV1,
) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError> {
    let mut schemas = Vec::with_capacity(CONFIGURATION_SPECS.len());
    macro_rules! add {
        ($operation:literal, $request:ty, Vec<$result:ident>) => {
            schemas.push(configuration_executable_schema::<$request, Vec<$result>>(
                contribution,
                $operation,
                concat!(
                    "tracedecay_application::configuration::",
                    stringify!($request)
                ),
                concat!(
                    "alloc::vec::Vec<tracedecay_application::configuration::",
                    stringify!($result),
                    ">"
                ),
            )?)
        };
        ($operation:literal, $request:ty, tracedecay_domain::configuration::$result:ident) => {
            schemas.push(configuration_executable_schema::<
                $request,
                tracedecay_domain::configuration::$result,
            >(
                contribution,
                $operation,
                concat!(
                    "tracedecay_application::configuration::",
                    stringify!($request)
                ),
                concat!("tracedecay_domain::configuration::", stringify!($result)),
            )?)
        };
        ($operation:literal, $request:ty, $result:ty) => {
            schemas.push(configuration_executable_schema::<$request, $result>(
                contribution,
                $operation,
                concat!(
                    "tracedecay_application::configuration::",
                    stringify!($request)
                ),
                concat!(
                    "tracedecay_application::configuration::",
                    stringify!($result)
                ),
            )?)
        };
    }
    add!(
        "configuration_list",
        ConfigurationListRequestV1,
        Vec<SettingSummary>
    );
    add!(
        "configuration_explain",
        ConfigurationGetRequestV1,
        ResolvedSetting
    );
    add!(
        "configuration_get",
        ConfigurationGetRequestV1,
        ResolvedSetting
    );
    add!(
        "configuration_set",
        ConfigurationSetRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        "configuration_unset",
        ConfigurationUnsetRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        "configuration_batch",
        ConfigurationBatchRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        "configuration_write_credential",
        ConfigurationWriteCredentialRequestV1,
        tracedecay_domain::configuration::CredentialReferenceMetadataV1
    );
    add!(
        "configuration_observed_state",
        ConfigurationObservedStateRequestV1,
        Vec<ComponentConfigurationState>
    );
    add!(
        "configuration_protected_preview",
        ConfigurationProtectedPreviewRequestV1,
        tracedecay_domain::configuration::ProtectedChangePlan
    );
    add!(
        "configuration_protected_apply",
        ConfigurationProtectedApplyRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        "configuration_rollback_preview",
        ConfigurationRollbackPreviewRequestV1,
        tracedecay_domain::configuration::ProtectedChangePlan
    );
    add!(
        "configuration_rollback_apply",
        ConfigurationRollbackApplyRequestV1,
        ConfigurationMutationReceipt
    );
    add!(
        "configuration_audit",
        ConfigurationAuditRequestV1,
        ConfigurationAuditPage
    );
    add!(
        "semantic_model_retry",
        SemanticLifecycleControlRequestV1,
        SemanticLifecycleOperationResultV1
    );
    add!(
        "semantic_model_remove",
        SemanticLifecycleControlRequestV1,
        SemanticLifecycleOperationResultV1
    );
    add!(
        "semantic_model_rollback",
        SemanticLifecycleControlRequestV1,
        SemanticLifecycleOperationResultV1
    );
    add!(
        "semantic_embedding_import_local",
        SemanticEmbeddingLocalImportRequestV1,
        SemanticLifecycleOperationResultV1
    );
    add!(
        "semantic_embedding_import_configured_https",
        SemanticEmbeddingConfiguredHttpsImportRequestV1,
        SemanticLifecycleOperationResultV1
    );
    add!(
        "semantic_reranker_import_local",
        SemanticRerankerLocalImportRequestV1,
        SemanticLifecycleOperationResultV1
    );
    add!(
        "semantic_reranker_import_configured_https",
        SemanticRerankerConfiguredHttpsImportRequestV1,
        SemanticLifecycleOperationResultV1
    );
    add!(
        "semantic_reranker_rollback",
        SemanticRerankerRollbackRequestV1,
        SemanticLifecycleOperationResultV1
    );
    Ok(schemas)
}

fn configuration_executable_schema<Request, Response>(
    contribution: &CatalogContributionV1,
    operation: &str,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError>
where
    Request: JsonSchema,
    Response: JsonSchema,
{
    let capability_id = CapabilityId::new(capability_id(operation))?;
    let manifest = contribution
        .capabilities()
        .binary_search_by(|manifest| manifest.capability_id().cmp(&capability_id))
        .ok()
        .map(|index| &contribution.capabilities()[index])
        .ok_or(ApplicationContractError::Inconsistent {
            field: "configuration schema capability",
        })?;
    Ok(ExecutableSchemaAuthority::for_types_at_paths::<
        Request,
        Response,
    >(
        manifest, request_rust_type_path, result_rust_type_path
    )?)
}

pub fn configuration_surface_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    CONFIGURATION_SPECS.iter().map(handler_descriptor).collect()
}

pub fn configuration_surface_operation(
    name: &str,
) -> Result<Option<ApplicationOperation>, ApplicationContractError> {
    CONFIGURATION_SPECS
        .iter()
        .find(|spec| spec.name == name)
        .map(application_operation)
        .transpose()
}

fn capability(
    spec: &ConfigurationSurfaceSpec,
    capability_id: CapabilityId,
    binding_ids: Vec<BindingId>,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    let effect = spec.effect;
    let is_effect = effect.is_effect();
    let is_semantic_import = matches!(
        spec.name,
        "semantic_embedding_import_local"
            | "semantic_embedding_import_configured_https"
            | "semantic_reranker_import_local"
            | "semantic_reranker_import_configured_https"
    );
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id: UseCaseId::new(use_case_id(spec.name))?,
        routing: RoutingContractV1::new(
            1,
            spec.summary,
            spec.description,
            vec![spec.example.to_owned()],
        )?,
        request_schema: configuration_surface_request_schema(spec.name)?,
        result_schema: configuration_surface_result_schema(spec.name)?,
        effect,
        scope: ScopeRequirement::new(vec![
            ScopeDimension::ConfigurationLayer,
            ScopeDimension::Project,
        ])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: if is_semantic_import {
            CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeEffect,
                CancellationPoint::EffectInFlight,
            ])?
        } else if is_effect {
            CancellationContract::NotCancellable
        } else {
            CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ])?
        },
        deadline: DeadlineContract::new(
            spec.maximum_deadline_millis,
            if is_effect {
                DeadlineBehavior::ReturnEffectReceipt
            } else {
                DeadlineBehavior::ReturnOperationReceipt
            },
        )?,
        pagination: spec
            .paginated
            .then(|| PaginationContract::new(10, 100, 60_000))
            .transpose()?,
        idempotency: if is_effect {
            IdempotencyContract::Required
        } else {
            IdempotencyContract::NotRequired
        },
        inverse: if is_effect {
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
        reconciliation: if is_effect {
            ReconciliationContract::Required
        } else {
            ReconciliationContract::NotRequired
        },
        receipt: if is_effect {
            ReceiptContract::DurableEffect
        } else {
            ReceiptContract::Operation
        },
        terminal_states: TerminalStateContract::new(if is_effect {
            let mut states = vec![
                TerminalState::Completed,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::EffectUnknown,
                TerminalState::Partial,
            ];
            if is_semantic_import {
                states.push(TerminalState::Cancelled);
            }
            states
        } else {
            vec![
                TerminalState::Completed,
                TerminalState::Cancelled,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::Partial,
            ]
        })?,
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility: application_profile_ids(
            if matches!(
                spec.name,
                "configuration_list"
                    | "configuration_explain"
                    | "configuration_get"
                    | "configuration_observed_state"
                    | "configuration_audit"
            ) {
                &[
                    APPLICATION_DEFAULT_PROFILE_ID,
                    APPLICATION_ADMINISTRATIVE_PROFILE_ID,
                ]
            } else {
                &[APPLICATION_DEFAULT_PROFILE_ID]
            },
        )?,
        required_features: Vec::new(),
    })?)
}

fn handler_descriptor(
    spec: &ConfigurationSurfaceSpec,
) -> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    let result_schema = configuration_surface_result_schema(spec.name)?;
    ApplicationHandlerDescriptor::new(
        application_operation(spec)?,
        configuration_surface_request_schema(spec.name)?,
        result_schema,
    )
}

fn application_operation(
    spec: &ConfigurationSurfaceSpec,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = configuration_surface_result_schema(spec.name)?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(capability_id(spec.name))?,
        UseCaseId::new(use_case_id(spec.name))?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

pub fn configuration_surface_request_schema(
    operation: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    configuration_surface_schema(operation, "request")
}

pub fn configuration_surface_result_schema(
    operation: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    configuration_surface_schema(operation, "result")
}

fn configuration_surface_schema(
    operation: &str,
    direction: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    if !CONFIGURATION_SURFACE_OPERATION_NAMES.contains(&operation) {
        return Err(ApplicationContractError::Inconsistent {
            field: "configuration surface operation",
        });
    }
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.configuration.{operation}.{direction}"
        ))?,
        1,
    )?)
}

fn capability_id(operation: &str) -> String {
    format!(
        "capability.application.configuration.{}",
        operation_suffix(operation)
    )
}

fn use_case_id(operation: &str) -> String {
    format!(
        "use-case.application.configuration.{}",
        operation_suffix(operation)
    )
}

fn operation_suffix(operation: &str) -> &str {
    operation
        .strip_prefix("configuration_")
        .unwrap_or(operation)
}

#[cfg(test)]
mod tests;

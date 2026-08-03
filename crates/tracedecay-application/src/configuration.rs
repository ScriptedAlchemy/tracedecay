//! Catalog contracts for the typed configuration control plane.
//!
//! The root runtime owns concrete stores and authorization. This crate keeps
//! the reviewed operation identities, schemas, and surface bindings beside the
//! application feature without importing a transport or persistence adapter.

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1, SettingKey,
};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass,
    PaginationContract, PrivacyClass, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamingContract, TerminalState, TerminalStateContract,
    UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::{
    APPLICATION_ADMINISTRATIVE_PROFILE_ID, APPLICATION_DEFAULT_PROFILE_ID, application_profile_ids,
};

/// Typed input for the first configuration read migrated through the daemon
/// invocation boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationGetRequestV1 {
    pub key: SettingKey,
}

/// Typed revision-CAS input for the first configuration write migrated through
/// the daemon invocation boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSetRequestV1 {
    pub layer: ConfigurationLayerIdV1,
    pub key: SettingKey,
    pub value: ConfigurationValueV1,
    pub expected_revision: ConfigurationRevisionId,
}

struct ConfigurationSurfaceSpec {
    name: &'static str,
    summary: &'static str,
    description: &'static str,
    example: &'static str,
    effect: EffectClass,
    paginated: bool,
}

const CONFIGURATION_SPECS: [ConfigurationSurfaceSpec; 13] = [
    ConfigurationSurfaceSpec {
        name: "configuration_list",
        summary: "List configuration settings",
        description: "List typed settings visible through the retained configuration authority.",
        example: "List project configuration settings",
        effect: EffectClass::Read,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_explain",
        summary: "Explain effective configuration",
        description: "Explain the resolved value and provenance for one typed setting.",
        example: "Explain this configuration setting",
        effect: EffectClass::Read,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_get",
        summary: "Get effective configuration",
        description: "Read one effective typed configuration setting.",
        example: "Get this configuration setting",
        effect: EffectClass::Read,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_set",
        summary: "Set configuration value",
        description: "Apply one authorized typed configuration value with revision CAS.",
        example: "Set this project configuration value",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_unset",
        summary: "Unset configuration value",
        description: "Remove one authorized typed configuration value with revision CAS.",
        example: "Unset this project configuration value",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_batch",
        summary: "Apply configuration batch",
        description: "Apply one authorized atomic batch of typed configuration mutations.",
        example: "Apply these project configuration changes together",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_write_credential",
        summary: "Write credential reference",
        description: "Resolve an opaque credential handle into write-only reference metadata.",
        example: "Rotate this configuration credential reference",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_observed_state",
        summary: "Read configuration activation state",
        description: "Read desired versus observed component configuration state.",
        example: "Show configuration activation drift",
        effect: EffectClass::Read,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_protected_preview",
        summary: "Preview protected configuration change",
        description: "Create a revalidated redacted preview for a protected configuration change.",
        example: "Preview this protected configuration change",
        effect: EffectClass::Preview,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_protected_apply",
        summary: "Apply protected configuration change",
        description: "Apply an actor-bound protected configuration preview with exact CAS evidence.",
        example: "Apply this approved protected configuration change",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_rollback_preview",
        summary: "Preview configuration rollback",
        description: "Create a forward rollback preview against one historical revision.",
        example: "Preview rollback to this configuration revision",
        effect: EffectClass::Preview,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_rollback_apply",
        summary: "Apply configuration rollback",
        description: "Apply an actor-bound forward rollback preview with exact CAS evidence.",
        example: "Apply this approved configuration rollback",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
    },
    ConfigurationSurfaceSpec {
        name: "configuration_audit",
        summary: "Read configuration audit",
        description: "Read reauthorized append-only redacted configuration audit events.",
        example: "Show configuration audit history",
        effect: EffectClass::Read,
        paginated: true,
    },
];

pub const CONFIGURATION_SURFACE_OPERATION_NAMES: [&str; 13] = [
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
];

const CONFIGURATION_SURFACES: [BindingSurface; 4] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
    BindingSurface::Dashboard,
];

pub fn configuration_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(CONFIGURATION_SPECS.len());
    let mut bindings = Vec::with_capacity(CONFIGURATION_SPECS.len() * CONFIGURATION_SURFACES.len());

    for spec in &CONFIGURATION_SPECS {
        let capability_id = CapabilityId::new(capability_id(spec.name))?;
        let (spec_bindings, binding_ids) =
            current_bindings(&capability_id, spec.name, CONFIGURATION_SURFACES)?;
        bindings.extend(spec_bindings);
        capabilities.push(capability(spec, capability_id, binding_ids)?);
    }

    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.configuration-surface")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
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
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id: UseCaseId::new(use_case_id(spec.name))?,
        routing: RoutingContractV1::new(
            1,
            spec.summary,
            spec.description,
            vec![spec.example.to_owned()],
        )?,
        request_schema: schema(spec.name, "request")?,
        result_schema: schema(spec.name, "result")?,
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
        cancellation: if is_effect {
            CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeEffect,
                CancellationPoint::EffectInFlight,
                CancellationPoint::Reconciling,
                CancellationPoint::AfterCommit,
            ])?
        } else {
            CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ])?
        },
        deadline: DeadlineContract::new(
            15_000,
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
            vec![
                TerminalState::Completed,
                TerminalState::Cancelled,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::EffectUnknown,
                TerminalState::Partial,
            ]
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
    let result_schema = schema(spec.name, "result")?;
    ApplicationHandlerDescriptor::new(
        application_operation(spec)?,
        schema(spec.name, "request")?,
        result_schema,
    )
}

fn application_operation(
    spec: &ConfigurationSurfaceSpec,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = schema(spec.name, "result")?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(capability_id(spec.name))?,
        UseCaseId::new(use_case_id(spec.name))?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

fn schema(operation: &str, direction: &str) -> Result<SchemaRef, ApplicationContractError> {
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
mod tests {
    use super::*;

    #[test]
    fn configuration_surface_keeps_every_retained_operation_callable() {
        let contribution = configuration_surface_catalog_contribution().expect("contribution");
        assert_eq!(contribution.capabilities().len(), CONFIGURATION_SPECS.len());
        assert_eq!(
            contribution.bindings().len(),
            CONFIGURATION_SPECS.len() * CONFIGURATION_SURFACES.len()
        );
        assert!(
            contribution
                .capabilities()
                .iter()
                .all(|capability| capability.availability().is_callable())
        );
    }

    #[test]
    fn configuration_surface_exposes_the_pr14_dashboard_transport() {
        let contribution = configuration_surface_catalog_contribution().expect("contribution");
        let surfaces = contribution
            .bindings()
            .iter()
            .map(|binding| binding.surface())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(
            surfaces,
            std::collections::BTreeSet::from([
                BindingSurface::Cli,
                BindingSurface::Mcp,
                BindingSurface::Http,
                BindingSurface::Dashboard,
            ])
        );
    }

    #[test]
    fn configuration_surface_requires_mounted_project_and_exact_layer_routes() {
        let contribution = configuration_surface_catalog_contribution().expect("contribution");

        for capability in contribution.capabilities() {
            assert!(
                capability
                    .scope()
                    .requires(ScopeDimension::ConfigurationLayer),
                "{} must route through an exact configuration-layer authority",
                capability.capability_id()
            );
            assert!(
                capability.scope().requires(ScopeDimension::Project),
                "{} must not advertise a nonexistent projectless profile route",
                capability.capability_id()
            );
        }
    }

    #[test]
    fn exported_configuration_operation_names_match_the_catalog_specs() {
        assert_eq!(
            CONFIGURATION_SPECS
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            CONFIGURATION_SURFACE_OPERATION_NAMES
        );
    }

    #[test]
    fn invocation_requests_keep_configuration_read_and_cas_inputs_typed() {
        let get = ConfigurationGetRequestV1 {
            key: tracedecay_domain::configuration::SettingKey::new("mcp.tool_timings").unwrap(),
        };
        let set = ConfigurationSetRequestV1 {
            layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Default,
            key: get.key.clone(),
            value: tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true),
            expected_revision: tracedecay_domain::configuration::ConfigurationRevisionId::new(
                "revision.configuration-test",
            )
            .unwrap(),
        };

        assert_eq!(get.key, set.key);
        assert!(matches!(
            set.value,
            tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true)
        ));
    }
}

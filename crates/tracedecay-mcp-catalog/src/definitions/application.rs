use serde_json::{Value, json};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, CatalogContributionV1, CatalogValidationError, OperationId,
};

use super::application_schema::{bound_tagged_union, closed_object_schema};
use super::def;
use crate::{McpCatalogError, ToolDefinition};

/// Guidance carried by the bounded `ConfigurationValueV1` payload on the
/// CAS-gated direct configuration writes.
const CONFIGURATION_VALUE_GUIDANCE: &str = "Read the setting first through \
    tracedecay_configuration_get: that read supplies the CAS \
    expected_revision and shows the exact typed shape to send, and the daemon validates the \
    payload against the canonical ConfigurationValueV1 schema on admission.";

/// Guidance carried by the bounded `ProtectedChange` payload on the CAS-gated
/// protected-change preview.
const PROTECTED_CHANGE_GUIDANCE: &str = "Read the affected setting first through \
    tracedecay_configuration_list or tracedecay_configuration_get: that read supplies the CAS \
    expected_revision and shows the \
    exact typed shape of the current source bindings, access rules, or work topology policy, and \
    the daemon validates the change against the canonical ProtectedChange schema before it \
    returns the redacted preview.";

/// Operations whose shipped MCP definition (input schema and description) is
/// the handwritten `def_*` in [`super`], not a projection of the catalog.
const HANDWRITTEN_DEFINITION_OPERATIONS: &[ApplicationSurfaceOperation] = &[
    ApplicationSurfaceOperation::Context,
    ApplicationSurfaceOperation::Node,
    ApplicationSurfaceOperation::Impact,
    ApplicationSurfaceOperation::Similar,
    ApplicationSurfaceOperation::Redundancy,
    ApplicationSurfaceOperation::RenamePreview,
    ApplicationSurfaceOperation::PortStatus,
    ApplicationSurfaceOperation::PortOrder,
    ApplicationSurfaceOperation::Todos,
    ApplicationSurfaceOperation::TestMap,
    ApplicationSurfaceOperation::TestRisk,
    ApplicationSurfaceOperation::Gini,
    ApplicationSurfaceOperation::DependencyDepth,
    ApplicationSurfaceOperation::Health,
    ApplicationSurfaceOperation::Dsm,
    ApplicationSurfaceOperation::Diagnose,
    ApplicationSurfaceOperation::DeadCode,
    ApplicationSurfaceOperation::Circular,
    ApplicationSurfaceOperation::Hotspots,
    ApplicationSurfaceOperation::UnmountedFiles,
    ApplicationSurfaceOperation::Rank,
    ApplicationSurfaceOperation::Largest,
    ApplicationSurfaceOperation::Coupling,
    ApplicationSurfaceOperation::InheritanceDepth,
    ApplicationSurfaceOperation::Distribution,
    ApplicationSurfaceOperation::Recursion,
    ApplicationSurfaceOperation::Complexity,
    ApplicationSurfaceOperation::DocCoverage,
    ApplicationSurfaceOperation::GodClass,
    ApplicationSurfaceOperation::UnsafePatterns,
    ApplicationSurfaceOperation::Constructors,
    ApplicationSurfaceOperation::FieldSites,
    ApplicationSurfaceOperation::FindExactSymbol,
    ApplicationSurfaceOperation::ByQualifiedName,
    ApplicationSurfaceOperation::Signature,
    ApplicationSurfaceOperation::Derives,
    ApplicationSurfaceOperation::Grep,
    ApplicationSurfaceOperation::AstGrepSearch,
    ApplicationSurfaceOperation::Affected,
    ApplicationSurfaceOperation::DiffContext,
    ApplicationSurfaceOperation::Changelog,
    ApplicationSurfaceOperation::CommitContext,
    ApplicationSurfaceOperation::PrContext,
    ApplicationSurfaceOperation::BranchSearch,
    ApplicationSurfaceOperation::BranchDiff,
    ApplicationSurfaceOperation::BranchList,
    ApplicationSurfaceOperation::StrReplace,
    ApplicationSurfaceOperation::MultiStrReplace,
    ApplicationSurfaceOperation::InsertAt,
    ApplicationSurfaceOperation::AstGrepRewrite,
    ApplicationSurfaceOperation::ReplaceSymbol,
    ApplicationSurfaceOperation::InsertAtSymbol,
    ApplicationSurfaceOperation::MoveSymbol,
    ApplicationSurfaceOperation::RenameSymbol,
    ApplicationSurfaceOperation::SourceEditReconcile,
    ApplicationSurfaceOperation::SourceEditRollback,
    ApplicationSurfaceOperation::FactStoreCurate,
    ApplicationSurfaceOperation::FactStoreAdd,
    ApplicationSurfaceOperation::FactStoreSearch,
    ApplicationSurfaceOperation::FactStoreProbe,
    ApplicationSurfaceOperation::FactStoreRelated,
    ApplicationSurfaceOperation::FactStoreReason,
    ApplicationSurfaceOperation::FactStoreContradict,
    ApplicationSurfaceOperation::FactStoreGet,
    ApplicationSurfaceOperation::FactStoreUpdate,
    ApplicationSurfaceOperation::FactStoreRemove,
    ApplicationSurfaceOperation::FactStoreSupersede,
    ApplicationSurfaceOperation::FactStoreList,
    ApplicationSurfaceOperation::FactFeedback,
    ApplicationSurfaceOperation::MemoryStatus,
    ApplicationSurfaceOperation::SessionRefreshStatus,
    ApplicationSurfaceOperation::SessionRefreshCancel,
    ApplicationSurfaceOperation::SessionRefreshBegin,
    ApplicationSurfaceOperation::MessageSearch,
    ApplicationSurfaceOperation::SessionsFor,
    ApplicationSurfaceOperation::Workflows,
    ApplicationSurfaceOperation::LcmStatus,
    ApplicationSurfaceOperation::LcmDoctor,
    ApplicationSurfaceOperation::LcmLoadSession,
    ApplicationSurfaceOperation::LcmGrep,
    ApplicationSurfaceOperation::LcmDescribe,
    ApplicationSurfaceOperation::LcmExpand,
    ApplicationSurfaceOperation::LcmExpandQuery,
];

/// Project every canonical application handler into its MCP transport view.
///
/// Operation identity comes from `ApplicationHandlerDescriptor`, exposure and
/// human routing text come from the owning catalog contribution, and request
/// shape/effect come from the executable binding. MCP owns only its public tool
/// spelling and protocol metadata.
pub(super) fn application_definitions() -> Result<Vec<ToolDefinition>, McpCatalogError> {
    let handlers = tracedecay_contracts::application_handler_descriptors()
        .map_err(|error| McpCatalogError::Initialization(error.to_string()))?;
    let registry = tracedecay_contracts::mcp_executable_binding_registry()
        .map_err(|error| McpCatalogError::Initialization(error.to_string()))?;
    let contributions = tracedecay_contracts::application_catalog_contributions()
        .map_err(|error| McpCatalogError::Initialization(error.to_string()))?;

    ApplicationSurfaceOperation::ALL
        .into_iter()
        .filter(|operation| !HANDWRITTEN_DEFINITION_OPERATIONS.contains(operation))
        .map(|operation| {
            let descriptor = handlers.for_surface_operation(operation).ok_or_else(|| {
                invalid_application_definition(
                    "application MCP handler",
                    "canonical operation has no handler descriptor",
                )
            })?;
            let operation_id =
                OperationId::new(format!("operation.application.{}", operation.as_str())).map_err(
                    |_| {
                        invalid_application_definition(
                            "application MCP operation ID",
                            "canonical operation name is invalid",
                        )
                    },
                )?;
            let executable = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .ok_or_else(|| {
                    invalid_application_definition(
                        "application MCP executable binding",
                        "canonical operation is not executable",
                    )
                })?;
            if executable.capability_id() != descriptor.operation().capability_id() {
                return Err(invalid_application_definition(
                    "application MCP capability",
                    "handler and executable binding disagree",
                ));
            }
            let manifest = application_manifest(&contributions, executable.capability_id())
                .ok_or_else(|| {
                    invalid_application_definition(
                        "application MCP manifest",
                        "executable binding has no owning capability",
                    )
                })?;
            Ok(ToolDefinition {
                name: operation.mcp_tool_name().to_owned(),
                description: manifest.routing().description().to_owned(),
                input_schema: application_input_schema(
                    operation,
                    executable.request_schema().body(),
                )?,
                annotations: Some(json!({
                    "readOnlyHint": executable.effect().is_read_only(),
                    "title": manifest.routing().name(),
                })),
                // Callers stays loaded: "who calls this" is the most common
                // native reflex after grep and chains straight from a node ID.
                meta: matches!(
                    operation,
                    ApplicationSurfaceOperation::StorageStatus
                        | ApplicationSurfaceOperation::CodeCallers
                )
                .then(|| json!({ "anthropic/alwaysLoad": true })),
            })
        })
        .collect()
}

fn application_input_schema(
    operation: ApplicationSurfaceOperation,
    canonical: &Value,
) -> Result<Value, McpCatalogError> {
    match operation {
        // The configuration writes are revision-CAS gated: the agent reads the
        // setting to obtain `expected_revision`, and that read already shows
        // the typed value. Advertising the complete value union again costs
        // 15–30 KiB per tool in every `tools/list`, so MCP keeps the
        // discriminator and bounds the payload; HTTP/SDK callers keep the full
        // canonical schema.
        ApplicationSurfaceOperation::ConfigurationSet
        | ApplicationSurfaceOperation::ConfigurationBatch => bounded_input_schema(
            canonical,
            "ConfigurationValueV1",
            CONFIGURATION_VALUE_GUIDANCE,
        ),
        ApplicationSurfaceOperation::ConfigurationProtectedPreview => {
            bounded_input_schema(canonical, "ProtectedChange", PROTECTED_CHANGE_GUIDANCE)
        }
        ApplicationSurfaceOperation::DiagnosticsRead => Ok(shipped_diagnostics_input_schema()),
        _ => Ok(canonical.clone()),
    }
}

fn bounded_input_schema(
    canonical: &Value,
    definition: &str,
    payload_guidance: &str,
) -> Result<Value, McpCatalogError> {
    let mut schema = canonical.clone();
    bound_tagged_union(&mut schema, definition, payload_guidance)?;
    Ok(schema)
}

/// `tracedecay_diagnostics` shipped with this flat MCP/CLI input. Keep this
/// one transport projection at the edge while the descriptor, HTTP/SDK
/// schema, handler, and runtime all retain `DiagnosticsPrimitiveRequest`.
fn shipped_diagnostics_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "scope": {
                "type": "string",
                "enum": ["workspace", "file"],
                "description": "Read scope. Default 'workspace'. 'file' requires `path`."
            },
            "path": {
                "type": "string",
                "description": "Project-relative file path when scope='file'."
            },
            "maximum_diagnostics": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1000,
                "description": "Maximum diagnostics returned in this page."
            },
            "cursor": {
                "type": ["string", "null"],
                "minLength": 1,
                "description": "Opaque cursor returned by the prior page."
            }
        }
    })
}

fn application_manifest<'a>(
    contributions: &'a [CatalogContributionV1],
    capability_id: &tracedecay_tool_catalog::CapabilityId,
) -> Option<&'a tracedecay_tool_catalog::CapabilityManifestV1> {
    contributions
        .iter()
        .flat_map(CatalogContributionV1::capabilities)
        .find(|manifest| manifest.capability_id() == capability_id)
}

fn invalid_application_definition(field: &'static str, reason: &'static str) -> McpCatalogError {
    CatalogValidationError::InvalidValue { field, reason }.into()
}

pub(super) fn def_remote_status_read() -> ToolDefinition {
    def(
        "tracedecay_remote_status",
        "Read Remote Brain status",
        "Read the Remote Brain operational plane: listener, enrollment, spool, replay coverage, and failover/recovery state.",
        closed_object_schema(json!({}), &[]),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        HANDWRITTEN_DEFINITION_OPERATIONS, application_definitions, application_input_schema,
    };
    use crate::definitions::{
        add_format_property, add_registered_project_selector_properties,
        get_maximal_tool_definitions, project_input_schema,
    };
    use tracedecay_tool_catalog::{ApplicationSurfaceOperation, OperationId};

    #[test]
    fn diagnostics_definition_preserves_the_shipped_flat_request() {
        let definitions = application_definitions().expect("application definitions");
        let definition = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_diagnostics")
            .expect("shipped diagnostics definition");

        assert_eq!(
            definition.input_schema["properties"]["scope"]["enum"],
            serde_json::json!(["workspace", "file"])
        );
        assert_eq!(
            definition.input_schema["properties"]["path"]["type"],
            "string"
        );
        assert!(
            definitions
                .iter()
                .all(|definition| definition.name != "tracedecay_diagnostics_read")
        );
    }

    #[test]
    fn code_navigation_reads_publish_only_their_short_tool_names() {
        let advertised = get_maximal_tool_definitions().expect("advertised definitions");
        for operation in [
            ApplicationSurfaceOperation::CodeCallers,
            ApplicationSurfaceOperation::CodeCallees,
            ApplicationSurfaceOperation::CodeTypeHierarchy,
            ApplicationSurfaceOperation::CodeSignatureSearch,
            ApplicationSurfaceOperation::CodeImplementations,
        ] {
            let short = operation.mcp_tool_name();
            let canonical = format!("tracedecay_{}", operation.as_str());
            assert_ne!(short, canonical);
            assert_eq!(
                advertised
                    .iter()
                    .filter(|definition| definition.name == short)
                    .count(),
                1,
                "{short}"
            );
            assert!(
                advertised
                    .iter()
                    .all(|definition| definition.name != canonical),
                "{canonical} must not be a second advertisement"
            );
        }
        let callers = advertised
            .iter()
            .find(|definition| definition.name == "tracedecay_callers")
            .expect("callers definition");
        assert_eq!(
            callers.input_schema["required"],
            serde_json::json!(["node_id"])
        );
        assert_eq!(
            callers.meta,
            Some(serde_json::json!({ "anthropic/alwaysLoad": true }))
        );
    }

    #[test]
    fn definitions_project_each_handler_and_executable_once() {
        let handlers =
            tracedecay_contracts::application_handler_descriptors().expect("application handlers");
        let registry = tracedecay_contracts::mcp_executable_binding_registry()
            .expect("application MCP registry");
        let definitions = application_definitions().expect("application definitions");
        let advertised = get_maximal_tool_definitions().expect("advertised definitions");

        for operation in ApplicationSurfaceOperation::ALL {
            let descriptor = handlers
                .for_surface_operation(operation)
                .expect("canonical surface handler");
            let operation_id =
                OperationId::new(format!("operation.application.{}", operation.as_str()))
                    .expect("operation ID");
            let executable = registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .expect("executable binding");

            assert_eq!(
                executable.capability_id(),
                descriptor.operation().capability_id()
            );
            if HANDWRITTEN_DEFINITION_OPERATIONS.contains(&operation) {
                assert!(
                    definitions
                        .iter()
                        .all(|definition| definition.name != operation.mcp_tool_name()),
                    "{operation:?} keeps its handwritten definition"
                );
                assert_eq!(
                    advertised
                        .iter()
                        .filter(|published| published.name == operation.mcp_tool_name())
                        .count(),
                    1,
                    "{operation:?}"
                );
                continue;
            }
            let definition = definitions
                .iter()
                .find(|definition| definition.name == operation.mcp_tool_name())
                .expect("MCP definition");
            let canonical = executable.request_schema().body();
            let expected_schema = match operation {
                // These tools deliberately adapt a shipped request or bound
                // a CAS-gated payload. Every other request must stay typed.
                ApplicationSurfaceOperation::DiagnosticsRead
                | ApplicationSurfaceOperation::ConfigurationSet
                | ApplicationSurfaceOperation::ConfigurationBatch
                | ApplicationSurfaceOperation::ConfigurationProtectedPreview => {
                    application_input_schema(operation, canonical).expect("reviewed MCP projection")
                }
                _ => canonical.clone(),
            };
            assert_eq!(definition.input_schema, expected_schema, "{operation:?}");

            // Check the final tools/list projection too: assembly can add
            // transport arguments or accidentally introduce a duplicate name.
            let mut expected = vec![definition.clone()];
            project_input_schema(&mut expected[0].input_schema);
            add_registered_project_selector_properties(&mut expected);
            add_format_property(&mut expected).expect("transport format schema");
            let published = advertised
                .iter()
                .filter(|published| published.name == definition.name)
                .collect::<Vec<_>>();
            assert_eq!(published.len(), 1, "{}", definition.name);
            assert_eq!(
                published[0].input_schema, expected[0].input_schema,
                "{}",
                definition.name
            );
        }
    }

    #[test]
    fn cas_gated_configuration_writes_bound_the_value_union_to_its_tags() {
        let registry = tracedecay_contracts::mcp_executable_binding_registry()
            .expect("application MCP registry");
        let definitions = application_definitions().expect("application definitions");
        let canonical_body = |operation: &str| {
            let operation_id = OperationId::new(format!("operation.application.{operation}"))
                .expect("operation ID");
            registry
                .get(&operation_id)
                .and_then(|availability| availability.binding())
                .expect("executable binding")
                .request_schema()
                .body()
                .clone()
        };

        for (tool_name, operation, union) in [
            (
                "tracedecay_configuration_set",
                "configuration_set",
                "ConfigurationValueV1",
            ),
            (
                "tracedecay_configuration_batch",
                "configuration_batch",
                "ConfigurationValueV1",
            ),
            (
                "tracedecay_configuration_protected_preview",
                "configuration_protected_preview",
                "ProtectedChange",
            ),
        ] {
            let definition = definitions
                .iter()
                .find(|definition| definition.name == tool_name)
                .expect("configuration write definition");
            let canonical = canonical_body(operation);
            let canonical_tags = canonical["$defs"][union]["oneOf"]
                .as_array()
                .expect("canonical value union")
                .iter()
                .map(|branch| branch["properties"]["kind"]["const"].clone())
                .collect::<Vec<_>>();
            let bounded = &definition.input_schema["$defs"][union];
            assert_eq!(
                bounded["properties"]["kind"]["enum"],
                serde_json::Value::Array(canonical_tags)
            );
            assert_eq!(bounded["required"], serde_json::json!(["kind", "value"]));
            assert!(bounded["properties"]["value"].get("$ref").is_none());
            assert!(
                definition.input_schema["$defs"]
                    .get("WorkTopologyPolicyV1")
                    .is_none(),
                "{tool_name} must not re-advertise the typed value payloads"
            );
            assert_eq!(
                definition.input_schema["$defs"]["ConfigurationRevisionId"],
                canonical["$defs"]["ConfigurationRevisionId"],
                "{tool_name} must keep the definitions its other fields reference"
            );
        }

        let unset = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_configuration_unset")
            .expect("configuration unset definition");
        assert!(
            unset.input_schema["$defs"]
                .get("ConfigurationValueV1")
                .is_none()
        );
    }
}

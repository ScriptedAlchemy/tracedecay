//! Canonical execution contract exposed by every mounted MCP tool.
//!
//! Catalog-owned operations borrow their contract from the immutable
//! application catalog. Root compatibility handlers retain an explicit class
//! on their canonical binding row until they are migrated, so dispatch never
//! reintroduces a name-matching deadline list.

use serde_json::{Value, json};
use tracedecay_application::RetainedSurfaceOperation;
use tracedecay_tool_catalog::{
    AvailabilityContract, CapabilityManifestV1, DeadlineBehavior, EffectClass,
};

use super::ToolDefinition;
use super::binding::{legacy_execution_class, legacy_requires_cooperative_worker_cleanup};

pub(crate) const EXECUTION_METADATA_KEY: &str = "tracedecay/execution";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum McpToolExecutionAvailabilityV1 {
    Available,
    Unavailable { reason_code: String },
}

/// One resolved MCP execution contract.
///
/// The value is created once immediately after tool-name lookup and remains
/// immutable for the full request. `deadline_millis` therefore measures one
/// absolute lifecycle budget, not a fresh timeout at every dispatch layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct McpToolExecutionPolicyV1 {
    availability: McpToolExecutionAvailabilityV1,
    effect: EffectClass,
    deadline_millis: u64,
    deadline_behavior: DeadlineBehavior,
    required_features: Vec<String>,
    cooperative_worker_cleanup: bool,
}

impl McpToolExecutionPolicyV1 {
    pub(crate) fn interactive_read(deadline_millis: u64) -> Self {
        Self {
            availability: McpToolExecutionAvailabilityV1::Available,
            effect: EffectClass::Read,
            deadline_millis,
            deadline_behavior: DeadlineBehavior::ReturnOperationReceipt,
            required_features: Vec::new(),
            cooperative_worker_cleanup: false,
        }
    }

    fn from_manifest(manifest: &CapabilityManifestV1) -> Self {
        let availability = match manifest.availability() {
            AvailabilityContract::Available => McpToolExecutionAvailabilityV1::Available,
            AvailabilityContract::Unavailable { reason } => {
                McpToolExecutionAvailabilityV1::Unavailable {
                    reason_code: match reason {
                        tracedecay_tool_catalog::UnavailabilityReason::NotImplemented => {
                            "not_implemented"
                        }
                        tracedecay_tool_catalog::UnavailabilityReason::ReachedThroughAnotherCapability => {
                            "reached_through_another_capability"
                        }
                    }
                    .to_owned(),
                }
            }
        };
        Self {
            availability,
            effect: manifest.effect(),
            deadline_millis: manifest.deadline().maximum_millis(),
            deadline_behavior: manifest.deadline().behavior(),
            required_features: manifest
                .required_features()
                .iter()
                .map(|feature| feature.as_str().to_owned())
                .collect(),
            cooperative_worker_cleanup: false,
        }
    }

    fn unavailable(reason_code: impl Into<String>) -> Self {
        Self {
            availability: McpToolExecutionAvailabilityV1::Unavailable {
                reason_code: reason_code.into(),
            },
            effect: EffectClass::Read,
            deadline_millis: 120_000,
            deadline_behavior: DeadlineBehavior::RejectBeforeAdmission,
            required_features: Vec::new(),
            cooperative_worker_cleanup: false,
        }
    }

    pub(crate) fn availability(&self) -> &McpToolExecutionAvailabilityV1 {
        &self.availability
    }

    pub(crate) const fn deadline_millis(&self) -> u64 {
        self.deadline_millis
    }

    pub(crate) const fn deadline_behavior(&self) -> DeadlineBehavior {
        self.deadline_behavior
    }

    pub(crate) const fn requires_cooperative_worker_cleanup(&self) -> bool {
        self.cooperative_worker_cleanup
    }

    pub(crate) fn metadata(&self) -> Value {
        let availability = match &self.availability {
            McpToolExecutionAvailabilityV1::Available => json!({ "state": "available" }),
            McpToolExecutionAvailabilityV1::Unavailable { reason_code } => json!({
                "state": "unavailable",
                "reason_code": reason_code,
            }),
        };
        json!({
            "version": 1,
            "availability": availability,
            "effect": self.effect,
            "deadline_ms": self.deadline_millis,
            "deadline_behavior": self.deadline_behavior,
            "required_features": self.required_features,
        })
    }
}

/// Resolve the one policy authority for a mounted tool name.
///
/// Application and retained surfaces stay catalog-owned. Compatibility rows
/// only supply metadata for handlers that have not reached either catalog yet.
/// A catalog composition failure is surfaced as an unavailable policy, never
/// an invented available default.
pub(crate) fn execution_policy_for_tool(tool_name: &str) -> Option<McpToolExecutionPolicyV1> {
    if crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name).is_some()
    {
        return match crate::application_surface::mcp_catalog_capability_for_tool(tool_name) {
            Ok(Some(manifest)) => Some(McpToolExecutionPolicyV1::from_manifest(manifest)),
            Ok(None) => Some(McpToolExecutionPolicyV1::unavailable(
                "application_binding_unavailable",
            )),
            Err(_) => Some(McpToolExecutionPolicyV1::unavailable(
                "application_catalog_unavailable",
            )),
        };
    }
    if RetainedSurfaceOperation::from_name(tool_name).is_some() {
        return match super::handlers::retained_mcp_capability_for_tool(tool_name) {
            Ok(Some(manifest)) => Some(McpToolExecutionPolicyV1::from_manifest(manifest)),
            Ok(None) => Some(McpToolExecutionPolicyV1::unavailable(
                "retained_binding_unavailable",
            )),
            Err(_) => Some(McpToolExecutionPolicyV1::unavailable(
                "retained_catalog_unavailable",
            )),
        };
    }
    let class = legacy_execution_class(tool_name)?;
    let availability = if tool_name == "tracedecay_ast_grep_rewrite" && !super::ast_grep_available()
    {
        McpToolExecutionAvailabilityV1::Unavailable {
            reason_code: "host_dependency_unavailable".to_owned(),
        }
    } else {
        McpToolExecutionAvailabilityV1::Available
    };
    Some(McpToolExecutionPolicyV1 {
        availability,
        effect: class.effect(),
        deadline_millis: class.deadline_millis(),
        deadline_behavior: class.deadline_behavior(),
        required_features: Vec::new(),
        cooperative_worker_cleanup: legacy_requires_cooperative_worker_cleanup(tool_name),
    })
}

/// Attach the same resolved contract that dispatch will enforce to every
/// advertised definition. Existing host metadata (for example
/// `anthropic/alwaysLoad`) is preserved.
pub(crate) fn attach_execution_metadata(definitions: &mut [ToolDefinition]) {
    for definition in definitions {
        let policy = execution_policy_for_tool(&definition.name)
            .unwrap_or_else(|| McpToolExecutionPolicyV1::unavailable("execution_contract_missing"));
        let meta = definition.meta.get_or_insert_with(|| json!({}));
        let Some(meta) = meta.as_object_mut() else {
            *meta = json!({});
            let Some(meta) = meta.as_object_mut() else {
                continue;
            };
            meta.insert(EXECUTION_METADATA_KEY.to_owned(), policy.metadata());
            continue;
        };
        meta.insert(EXECUTION_METADATA_KEY.to_owned(), policy.metadata());
    }
}

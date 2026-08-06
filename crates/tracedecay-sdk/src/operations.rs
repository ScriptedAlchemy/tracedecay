//! Generated typed public operation descriptors. DO NOT EDIT.
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_tool_catalog::{
    DeadlineBehavior, EffectClass, ExecutableUnavailableDispositionV1, IdempotencyContract,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnavailableOperationCapability {
    pub operation: &'static str,
    pub operation_id: &'static str,
    pub disposition: ExecutableUnavailableDispositionV1,
}
pub trait TypedOperation {
    type Request: Serialize;
    type Result: DeserializeOwned;
    const OPERATION_ID: &'static str;
    const ROUTE: &'static str;
    const BINDING_ID: &'static str;
    const EFFECT: EffectClass;
    const IDEMPOTENCY: IdempotencyContract;
    const MAXIMUM_DEADLINE_MILLIS: u64;
    const DEADLINE_BEHAVIOR: DeadlineBehavior;
    const RESULT_SCHEMA_ID: &'static str;
    const RESULT_SCHEMA_REVISION: u32;
}
macro_rules! typed_operation {
    (
        $name:ident, $module:ident, $operation:literal, $route:literal, $binding:literal,
        $effect:expr, $idempotency:expr, $maximum_deadline:literal,
        $deadline_behavior:expr, $schema:literal, $revision:literal
    ) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $name;
        impl TypedOperation for $name {
            type Request = $module::Request;
            type Result = $module::Result;
            const OPERATION_ID: &'static str = $operation;
            const ROUTE: &'static str = $route;
            const BINDING_ID: &'static str = $binding;
            const EFFECT: EffectClass = $effect;
            const IDEMPOTENCY: IdempotencyContract = $idempotency;
            const MAXIMUM_DEADLINE_MILLIS: u64 = $maximum_deadline;
            const DEADLINE_BEHAVIOR: DeadlineBehavior = $deadline_behavior;
            const RESULT_SCHEMA_ID: &'static str = $schema;
            const RESULT_SCHEMA_REVISION: u32 = $revision;
        }
    };
}
pub const UNAVAILABLE_OPERATIONS: &[UnavailableOperationCapability] = &[
    UnavailableOperationCapability {
        operation: "application_affected_tests",
        operation_id: "operation.application.affected_tests",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_ast_grep_rewrite",
        operation_id: "operation.application.ast_grep_rewrite",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_call_chain",
        operation_id: "operation.application.call_chain",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_callees",
        operation_id: "operation.application.code_callees",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_callers",
        operation_id: "operation.application.code_callers",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_declaration",
        operation_id: "operation.application.code_declaration",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_definition",
        operation_id: "operation.application.code_definition",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_exact_occurrence",
        operation_id: "operation.application.code_exact_occurrence",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_facets",
        operation_id: "operation.application.code_facets",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_implementations",
        operation_id: "operation.application.code_implementations",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_phrase_search",
        operation_id: "operation.application.code_phrase_search",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_references",
        operation_id: "operation.application.code_references",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_signature_search",
        operation_id: "operation.application.code_signature_search",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_symbol_search",
        operation_id: "operation.application.code_symbol_search",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_timeline",
        operation_id: "operation.application.code_timeline",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_type_definition",
        operation_id: "operation.application.code_type_definition",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_code_type_hierarchy",
        operation_id: "operation.application.code_type_hierarchy",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_audit",
        operation_id: "operation.application.configuration_audit",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_batch",
        operation_id: "operation.application.configuration_batch",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_explain",
        operation_id: "operation.application.configuration_explain",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_get",
        operation_id: "operation.application.configuration_get",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_list",
        operation_id: "operation.application.configuration_list",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_observed_state",
        operation_id: "operation.application.configuration_observed_state",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_protected_apply",
        operation_id: "operation.application.configuration_protected_apply",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_protected_preview",
        operation_id: "operation.application.configuration_protected_preview",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_rollback_apply",
        operation_id: "operation.application.configuration_rollback_apply",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_rollback_preview",
        operation_id: "operation.application.configuration_rollback_preview",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_set",
        operation_id: "operation.application.configuration_set",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_unset",
        operation_id: "operation.application.configuration_unset",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_configuration_write_credential",
        operation_id: "operation.application.configuration_write_credential",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_budget",
        operation_id: "operation.application.context_scout_budget",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_cancel",
        operation_id: "operation.application.context_scout_cancel",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_capability",
        operation_id: "operation.application.context_scout_capability",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_claim",
        operation_id: "operation.application.context_scout_claim",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_delivery",
        operation_id: "operation.application.context_scout_delivery",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_explain",
        operation_id: "operation.application.context_scout_explain",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_feedback",
        operation_id: "operation.application.context_scout_feedback",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_pause",
        operation_id: "operation.application.context_scout_pause",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_recent",
        operation_id: "operation.application.context_scout_recent",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_resume",
        operation_id: "operation.application.context_scout_resume",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_context_scout_status",
        operation_id: "operation.application.context_scout_status",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_diagnostics_read",
        operation_id: "operation.application.diagnostics_read",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_fact_feedback",
        operation_id: "operation.application.fact_feedback",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_fact_store",
        operation_id: "operation.application.fact_store",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_feedback_advisory_cycle",
        operation_id: "operation.application.feedback_advisory_cycle",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_feedback_diagnostics",
        operation_id: "operation.application.feedback_diagnostics",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_feedback_expand",
        operation_id: "operation.application.feedback_expand",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_feedback_get",
        operation_id: "operation.application.feedback_get",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_feedback_impact",
        operation_id: "operation.application.feedback_impact",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_feedback_list",
        operation_id: "operation.application.feedback_list",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_file_dependents",
        operation_id: "operation.application.file_dependents",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_file_metadata",
        operation_id: "operation.application.file_metadata",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_git_apply",
        operation_id: "operation.application.git_apply",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_git_blame",
        operation_id: "operation.application.git_blame",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_git_diff",
        operation_id: "operation.application.git_diff",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_git_history",
        operation_id: "operation.application.git_history",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_git_hunks",
        operation_id: "operation.application.git_hunks",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_git_preview",
        operation_id: "operation.application.git_preview",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_git_status",
        operation_id: "operation.application.git_status",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_health_delta",
        operation_id: "operation.application.health_delta",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_health_read",
        operation_id: "operation.application.health_read",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_insert_at",
        operation_id: "operation.application.insert_at",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_insert_at_symbol",
        operation_id: "operation.application.insert_at_symbol",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_compress",
        operation_id: "operation.application.lcm_compress",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_describe",
        operation_id: "operation.application.lcm_describe",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_doctor",
        operation_id: "operation.application.lcm_doctor",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_expand",
        operation_id: "operation.application.lcm_expand",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_expand_query",
        operation_id: "operation.application.lcm_expand_query",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_grep",
        operation_id: "operation.application.lcm_grep",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_load_session",
        operation_id: "operation.application.lcm_load_session",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_preflight",
        operation_id: "operation.application.lcm_preflight",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_session_boundary",
        operation_id: "operation.application.lcm_session_boundary",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_lcm_status",
        operation_id: "operation.application.lcm_status",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_memory_status",
        operation_id: "operation.application.memory_status",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_message_search",
        operation_id: "operation.application.message_search",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_module_api",
        operation_id: "operation.application.module_api",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_move_symbol",
        operation_id: "operation.application.move_symbol",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_multi_str_replace",
        operation_id: "operation.application.multi_str_replace",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_qualified_name",
        operation_id: "operation.application.qualified_name",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_replace_symbol",
        operation_id: "operation.application.replace_symbol",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_session_lookup",
        operation_id: "operation.application.session_lookup",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_session_refresh",
        operation_id: "operation.application.session_refresh",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_sessions_for",
        operation_id: "operation.application.sessions_for",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_source_body",
        operation_id: "operation.application.source_body",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_source_edit_reconcile",
        operation_id: "operation.application.source_edit_reconcile",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_source_lines",
        operation_id: "operation.application.source_lines",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_source_outline",
        operation_id: "operation.application.source_outline",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_storage_status",
        operation_id: "operation.application.storage_status",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_str_replace",
        operation_id: "operation.application.str_replace",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_test_results",
        operation_id: "operation.application.test_results",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
    UnavailableOperationCapability {
        operation: "application_workflows",
        operation_id: "operation.application.workflows",
        disposition: ExecutableUnavailableDispositionV1::SchemaUnavailable,
    },
];
#[allow(clippy::all)]
pub mod workflow_definition_history {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Wire request for [`WorkflowDefinitionService::history`].
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionHistoryRequest",
        ///  "description": "Wire request for [`WorkflowDefinitionService::history`].",
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionHistoryRequest {
            pub definition_id: WorkflowDefinitionId,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`ArrayOfWorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "Array_of_WorkflowDefinition",
        ///  "type": "array",
        ///  "items": {
        ///    "$ref": "#/definitions/WorkflowDefinition"
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct ArrayOfWorkflowDefinition(pub ::std::vec::Vec<WorkflowDefinition>);
        impl ::std::ops::Deref for ArrayOfWorkflowDefinition {
            type Target = ::std::vec::Vec<WorkflowDefinition>;
            fn deref(&self) -> &::std::vec::Vec<WorkflowDefinition> {
                &self.0
            }
        }
        impl ::std::convert::From<ArrayOfWorkflowDefinition> for ::std::vec::Vec<WorkflowDefinition> {
            fn from(value: ArrayOfWorkflowDefinition) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::vec::Vec<WorkflowDefinition>> for ArrayOfWorkflowDefinition {
            fn from(value: ::std::vec::Vec<WorkflowDefinition>) -> Self {
                Self(value)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "pinned_catalog_digest",
        ///    "pinned_configuration_digest",
        ///    "pinned_policy_digest",
        ///    "project_id",
        ///    "steps"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "pinned_catalog_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_configuration_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStep"
        ///      }
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinition {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: u64,
            pub pinned_catalog_digest: ManifestDigest,
            pub pinned_configuration_digest: ManifestDigest,
            pub pinned_policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub steps: ::std::vec::Vec<WorkflowStep>,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowFanOut`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "max_width"
        ///  ],
        ///  "properties": {
        ///    "max_width": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowFanOut {
            pub max_width: u32,
        }
        ///Strongly typed canonical identity: `WorkflowOperationRef`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOperationRef`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOperationRef(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOperationRef {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOperationRef> for ::std::string::String {
            fn from(value: WorkflowOperationRef) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOperationRef {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOperationRef {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOperationRef {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowOutputName`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOutputName`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOutputName(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOutputName {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOutputName> for ::std::string::String {
            fn from(value: WorkflowOutputName) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOutputName {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOutputName {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOutputName {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowOutputReference`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "output_name",
        ///    "producer_step_id"
        ///  ],
        ///  "properties": {
        ///    "output_name": {
        ///      "$ref": "#/definitions/WorkflowOutputName"
        ///    },
        ///    "producer_step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowOutputReference {
            pub output_name: WorkflowOutputName,
            pub producer_step_id: WorkflowStepId,
        }
        ///`WorkflowStep`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "inputs",
        ///    "operation",
        ///    "outputs",
        ///    "predecessors",
        ///    "step_id"
        ///  ],
        ///  "properties": {
        ///    "fan_out": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkflowFanOut"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "inputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputReference"
        ///      }
        ///    },
        ///    "operation": {
        ///      "$ref": "#/definitions/WorkflowOperationRef"
        ///    },
        ///    "outputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputName"
        ///      }
        ///    },
        ///    "predecessors": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowStep {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub fan_out: ::std::option::Option<WorkflowFanOut>,
            pub inputs: ::std::vec::Vec<WorkflowOutputReference>,
            pub operation: WorkflowOperationRef,
            pub outputs: ::std::vec::Vec<WorkflowOutputName>,
            pub predecessors: Vec<WorkflowStepId>,
            pub step_id: WorkflowStepId,
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkflowDefinitionHistoryRequest;
    pub type Result = result::ArrayOfWorkflowDefinition;
}
typed_operation!(
    WorkflowDefinitionHistory,
    workflow_definition_history,
    "operation.workflow.definition_history",
    "/application/workflow/definition-history",
    "binding.http.workflow.definition_history",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    "schema.workflow.definition_history.result",
    1
);
#[allow(clippy::all)]
pub mod workflow_diff_definition {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Wire request for [`WorkflowDefinitionService::diff`].
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionDiffRequest",
        ///  "description": "Wire request for [`WorkflowDefinitionService::diff`].",
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "from_version",
        ///    "to_version"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "from_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 1.0
        ///    },
        ///    "to_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 1.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionDiffRequest {
            pub definition_id: WorkflowDefinitionId,
            pub from_version: ::std::num::NonZeroU64,
            pub to_version: ::std::num::NonZeroU64,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`WorkflowDefinitionDiff`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionDiff",
        ///  "type": "object",
        ///  "required": [
        ///    "catalog_changed",
        ///    "changed_steps",
        ///    "configuration_changed",
        ///    "definition_id",
        ///    "from_version",
        ///    "policy_changed",
        ///    "to_version"
        ///  ],
        ///  "properties": {
        ///    "catalog_changed": {
        ///      "type": "boolean"
        ///    },
        ///    "changed_steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "configuration_changed": {
        ///      "type": "boolean"
        ///    },
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "from_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "policy_changed": {
        ///      "type": "boolean"
        ///    },
        ///    "to_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionDiff {
            pub catalog_changed: bool,
            pub changed_steps: Vec<WorkflowStepId>,
            pub configuration_changed: bool,
            pub definition_id: WorkflowDefinitionId,
            pub from_version: u64,
            pub policy_changed: bool,
            pub to_version: u64,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkflowDefinitionDiffRequest;
    pub type Result = result::WorkflowDefinitionDiff;
}
typed_operation!(
    WorkflowDiffDefinition,
    workflow_diff_definition,
    "operation.workflow.diff_definition",
    "/application/workflow/diff-definition",
    "binding.http.workflow.diff_definition",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    "schema.workflow.diff_definition.result",
    1
);
#[allow(clippy::all)]
pub mod workflow_get_definition {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Wire request for [`WorkflowDefinitionService::get`].
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionGetRequest",
        ///  "description": "Wire request for [`WorkflowDefinitionService::get`].",
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 1.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionGetRequest {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: ::std::num::NonZeroU64,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinition",
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "pinned_catalog_digest",
        ///    "pinned_configuration_digest",
        ///    "pinned_policy_digest",
        ///    "project_id",
        ///    "steps"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "pinned_catalog_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_configuration_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStep"
        ///      }
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinition {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: u64,
            pub pinned_catalog_digest: ManifestDigest,
            pub pinned_configuration_digest: ManifestDigest,
            pub pinned_policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub steps: ::std::vec::Vec<WorkflowStep>,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowFanOut`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "max_width"
        ///  ],
        ///  "properties": {
        ///    "max_width": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowFanOut {
            pub max_width: u32,
        }
        ///Strongly typed canonical identity: `WorkflowOperationRef`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOperationRef`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOperationRef(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOperationRef {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOperationRef> for ::std::string::String {
            fn from(value: WorkflowOperationRef) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOperationRef {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOperationRef {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOperationRef {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowOutputName`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOutputName`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOutputName(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOutputName {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOutputName> for ::std::string::String {
            fn from(value: WorkflowOutputName) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOutputName {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOutputName {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOutputName {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowOutputReference`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "output_name",
        ///    "producer_step_id"
        ///  ],
        ///  "properties": {
        ///    "output_name": {
        ///      "$ref": "#/definitions/WorkflowOutputName"
        ///    },
        ///    "producer_step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowOutputReference {
            pub output_name: WorkflowOutputName,
            pub producer_step_id: WorkflowStepId,
        }
        ///`WorkflowStep`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "inputs",
        ///    "operation",
        ///    "outputs",
        ///    "predecessors",
        ///    "step_id"
        ///  ],
        ///  "properties": {
        ///    "fan_out": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkflowFanOut"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "inputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputReference"
        ///      }
        ///    },
        ///    "operation": {
        ///      "$ref": "#/definitions/WorkflowOperationRef"
        ///    },
        ///    "outputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputName"
        ///      }
        ///    },
        ///    "predecessors": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowStep {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub fan_out: ::std::option::Option<WorkflowFanOut>,
            pub inputs: ::std::vec::Vec<WorkflowOutputReference>,
            pub operation: WorkflowOperationRef,
            pub outputs: ::std::vec::Vec<WorkflowOutputName>,
            pub predecessors: Vec<WorkflowStepId>,
            pub step_id: WorkflowStepId,
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkflowDefinitionGetRequest;
    pub type Result = result::WorkflowDefinition;
}
typed_operation!(
    WorkflowGetDefinition,
    workflow_get_definition,
    "operation.workflow.get_definition",
    "/application/workflow/get-definition",
    "binding.http.workflow.get_definition",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    "schema.workflow.get_definition.result",
    1
);
#[allow(clippy::all)]
pub mod workflow_handoff_issue {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        /**Wire request for [`TaskHandoffService::issue`].

        `secret` is the caller-supplied bearer token; the authority persists only
        its digest, never the secret itself.*/
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "TaskHandoffIssueRequest",
        ///  "description": "Wire request for [`TaskHandoffService::issue`].\n\n`secret` is the caller-supplied bearer token; the authority persists only\nits digest, never the secret itself.",
        ///  "type": "object",
        ///  "required": [
        ///    "scope",
        ///    "secret"
        ///  ],
        ///  "properties": {
        ///    "scope": {
        ///      "$ref": "#/definitions/TaskHandoffScope"
        ///    },
        ///    "secret": {
        ///      "type": "string"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffIssueRequest {
            pub scope: TaskHandoffScope,
            pub secret: ::std::string::String,
        }
        ///`TaskHandoffScope`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "from_actor_id",
        ///    "project_id",
        ///    "repository_id",
        ///    "run_id",
        ///    "step_id",
        ///    "task_id",
        ///    "thread_id",
        ///    "to_actor_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 1.0
        ///    },
        ///    "from_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "thread_id": {
        ///      "$ref": "#/definitions/ThreadId"
        ///    },
        ///    "to_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffScope {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: ::std::num::NonZeroU64,
            pub from_actor_id: ActorId,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub run_id: RunId,
            pub step_id: WorkflowStepId,
            pub task_id: TaskId,
            pub thread_id: ThreadId,
            pub to_actor_id: ActorId,
            pub worktree_id: WorktreeId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ThreadId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ThreadId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ThreadId(pub ::std::string::String);
        impl ::std::ops::Deref for ThreadId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ThreadId> for ::std::string::String {
            fn from(value: ThreadId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ThreadId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ThreadId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ThreadId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`TaskHandoffGrant`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "TaskHandoffGrant",
        ///  "type": "object",
        ///  "required": [
        ///    "expires_at",
        ///    "issued_at",
        ///    "scope",
        ///    "token_digest"
        ///  ],
        ///  "properties": {
        ///    "expires_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "issued_at": {
        ///      "$ref": "#/definitions/UtcMicros"
        ///    },
        ///    "scope": {
        ///      "$ref": "#/definitions/TaskHandoffScope"
        ///    },
        ///    "token_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffGrant {
            pub expires_at: UtcMicros,
            pub issued_at: UtcMicros,
            pub scope: TaskHandoffScope,
            pub token_digest: ManifestDigest,
        }
        ///`TaskHandoffScope`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "from_actor_id",
        ///    "project_id",
        ///    "repository_id",
        ///    "run_id",
        ///    "step_id",
        ///    "task_id",
        ///    "thread_id",
        ///    "to_actor_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 1.0
        ///    },
        ///    "from_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "thread_id": {
        ///      "$ref": "#/definitions/ThreadId"
        ///    },
        ///    "to_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffScope {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: ::std::num::NonZeroU64,
            pub from_actor_id: ActorId,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub run_id: RunId,
            pub step_id: WorkflowStepId,
            pub task_id: TaskId,
            pub thread_id: ThreadId,
            pub to_actor_id: ActorId,
            pub worktree_id: WorktreeId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ThreadId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ThreadId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ThreadId(pub ::std::string::String);
        impl ::std::ops::Deref for ThreadId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ThreadId> for ::std::string::String {
            fn from(value: ThreadId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ThreadId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ThreadId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ThreadId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///UTC timestamp represented as microseconds from the Unix epoch.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "UTC timestamp represented as microseconds from the Unix epoch.",
        ///  "type": "integer",
        ///  "format": "int64"
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct UtcMicros(pub i64);
        impl ::std::ops::Deref for UtcMicros {
            type Target = i64;
            fn deref(&self) -> &i64 {
                &self.0
            }
        }
        impl ::std::convert::From<UtcMicros> for i64 {
            fn from(value: UtcMicros) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<i64> for UtcMicros {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for UtcMicros {
            type Err = <i64 as ::std::str::FromStr>::Err;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.parse()?))
            }
        }
        impl ::std::convert::TryFrom<&str> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: &str) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::convert::TryFrom<String> for UtcMicros {
            type Error = <i64 as ::std::str::FromStr>::Err;
            fn try_from(value: String) -> ::std::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::std::fmt::Display for UtcMicros {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::TaskHandoffIssueRequest;
    pub type Result = result::TaskHandoffGrant;
}
typed_operation!(
    WorkflowHandoffIssue,
    workflow_handoff_issue,
    "operation.workflow.handoff_issue",
    "/application/workflow/handoff-issue",
    "binding.http.workflow.handoff_issue",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    "schema.workflow.handoff_issue.result",
    1
);
#[allow(clippy::all)]
pub mod workflow_handoff_redeem {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Wire request for [`TaskHandoffService::redeem`].
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "TaskHandoffRedeemRequest",
        ///  "description": "Wire request for [`TaskHandoffService::redeem`].",
        ///  "type": "object",
        ///  "required": [
        ///    "expected_scope",
        ///    "secret"
        ///  ],
        ///  "properties": {
        ///    "expected_scope": {
        ///      "$ref": "#/definitions/TaskHandoffScope"
        ///    },
        ///    "secret": {
        ///      "type": "string"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffRedeemRequest {
            pub expected_scope: TaskHandoffScope,
            pub secret: ::std::string::String,
        }
        ///`TaskHandoffScope`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "from_actor_id",
        ///    "project_id",
        ///    "repository_id",
        ///    "run_id",
        ///    "step_id",
        ///    "task_id",
        ///    "thread_id",
        ///    "to_actor_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 1.0
        ///    },
        ///    "from_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "thread_id": {
        ///      "$ref": "#/definitions/ThreadId"
        ///    },
        ///    "to_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffScope {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: ::std::num::NonZeroU64,
            pub from_actor_id: ActorId,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub run_id: RunId,
            pub step_id: WorkflowStepId,
            pub task_id: TaskId,
            pub thread_id: ThreadId,
            pub to_actor_id: ActorId,
            pub worktree_id: WorktreeId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ThreadId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ThreadId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ThreadId(pub ::std::string::String);
        impl ::std::ops::Deref for ThreadId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ThreadId> for ::std::string::String {
            fn from(value: ThreadId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ThreadId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ThreadId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ThreadId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed canonical identity: `ActorId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ActorId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ActorId(pub ::std::string::String);
        impl ::std::ops::Deref for ActorId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ActorId> for ::std::string::String {
            fn from(value: ActorId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ActorId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ActorId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ActorId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RepositoryId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RepositoryId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RepositoryId(pub ::std::string::String);
        impl ::std::ops::Deref for RepositoryId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RepositoryId> for ::std::string::String {
            fn from(value: RepositoryId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RepositoryId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RepositoryId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RepositoryId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `RunId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `RunId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct RunId(pub ::std::string::String);
        impl ::std::ops::Deref for RunId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<RunId> for ::std::string::String {
            fn from(value: RunId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for RunId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for RunId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for RunId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        /**Wire response for [`TaskHandoffService::redeem`]: the redeemed scope,
        once and only once, for the caller that actually consumed it.*/
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "TaskHandoffRedeemed",
        ///  "description": "Wire response for [`TaskHandoffService::redeem`]: the redeemed scope,\nonce and only once, for the caller that actually consumed it.",
        ///  "type": "object",
        ///  "required": [
        ///    "scope"
        ///  ],
        ///  "properties": {
        ///    "scope": {
        ///      "$ref": "#/definitions/TaskHandoffScope"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffRedeemed {
            pub scope: TaskHandoffScope,
        }
        ///`TaskHandoffScope`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "from_actor_id",
        ///    "project_id",
        ///    "repository_id",
        ///    "run_id",
        ///    "step_id",
        ///    "task_id",
        ///    "thread_id",
        ///    "to_actor_id",
        ///    "worktree_id"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 1.0
        ///    },
        ///    "from_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "repository_id": {
        ///      "$ref": "#/definitions/RepositoryId"
        ///    },
        ///    "run_id": {
        ///      "$ref": "#/definitions/RunId"
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    },
        ///    "task_id": {
        ///      "$ref": "#/definitions/TaskId"
        ///    },
        ///    "thread_id": {
        ///      "$ref": "#/definitions/ThreadId"
        ///    },
        ///    "to_actor_id": {
        ///      "$ref": "#/definitions/ActorId"
        ///    },
        ///    "worktree_id": {
        ///      "$ref": "#/definitions/WorktreeId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct TaskHandoffScope {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: ::std::num::NonZeroU64,
            pub from_actor_id: ActorId,
            pub project_id: ProjectId,
            pub repository_id: RepositoryId,
            pub run_id: RunId,
            pub step_id: WorkflowStepId,
            pub task_id: TaskId,
            pub thread_id: ThreadId,
            pub to_actor_id: ActorId,
            pub worktree_id: WorktreeId,
        }
        ///Strongly typed canonical identity: `TaskId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `TaskId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct TaskId(pub ::std::string::String);
        impl ::std::ops::Deref for TaskId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<TaskId> for ::std::string::String {
            fn from(value: TaskId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for TaskId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for TaskId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for TaskId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ThreadId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ThreadId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ThreadId(pub ::std::string::String);
        impl ::std::ops::Deref for ThreadId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ThreadId> for ::std::string::String {
            fn from(value: ThreadId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ThreadId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ThreadId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ThreadId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorktreeId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorktreeId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorktreeId(pub ::std::string::String);
        impl ::std::ops::Deref for WorktreeId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorktreeId> for ::std::string::String {
            fn from(value: WorktreeId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorktreeId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorktreeId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorktreeId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::TaskHandoffRedeemRequest;
    pub type Result = result::TaskHandoffRedeemed;
}
typed_operation!(
    WorkflowHandoffRedeem,
    workflow_handoff_redeem,
    "operation.workflow.handoff_redeem",
    "/application/workflow/handoff-redeem",
    "binding.http.workflow.handoff_redeem",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    "schema.workflow.handoff_redeem.result",
    1
);
#[allow(clippy::all)]
pub mod workflow_list_definitions {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Wire request for [`WorkflowDefinitionService::list`].
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionListRequest",
        ///  "description": "Wire request for [`WorkflowDefinitionService::list`].",
        ///  "type": "object",
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionListRequest {}
        impl ::std::default::Default for WorkflowDefinitionListRequest {
            fn default() -> Self {
                Self {}
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///`ArrayOfWorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "Array_of_WorkflowDefinition",
        ///  "type": "array",
        ///  "items": {
        ///    "$ref": "#/definitions/WorkflowDefinition"
        ///  }
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(transparent)]
        pub struct ArrayOfWorkflowDefinition(pub ::std::vec::Vec<WorkflowDefinition>);
        impl ::std::ops::Deref for ArrayOfWorkflowDefinition {
            type Target = ::std::vec::Vec<WorkflowDefinition>;
            fn deref(&self) -> &::std::vec::Vec<WorkflowDefinition> {
                &self.0
            }
        }
        impl ::std::convert::From<ArrayOfWorkflowDefinition> for ::std::vec::Vec<WorkflowDefinition> {
            fn from(value: ArrayOfWorkflowDefinition) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::vec::Vec<WorkflowDefinition>> for ArrayOfWorkflowDefinition {
            fn from(value: ::std::vec::Vec<WorkflowDefinition>) -> Self {
                Self(value)
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "pinned_catalog_digest",
        ///    "pinned_configuration_digest",
        ///    "pinned_policy_digest",
        ///    "project_id",
        ///    "steps"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "pinned_catalog_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_configuration_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStep"
        ///      }
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinition {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: u64,
            pub pinned_catalog_digest: ManifestDigest,
            pub pinned_configuration_digest: ManifestDigest,
            pub pinned_policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub steps: ::std::vec::Vec<WorkflowStep>,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowFanOut`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "max_width"
        ///  ],
        ///  "properties": {
        ///    "max_width": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowFanOut {
            pub max_width: u32,
        }
        ///Strongly typed canonical identity: `WorkflowOperationRef`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOperationRef`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOperationRef(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOperationRef {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOperationRef> for ::std::string::String {
            fn from(value: WorkflowOperationRef) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOperationRef {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOperationRef {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOperationRef {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowOutputName`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOutputName`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOutputName(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOutputName {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOutputName> for ::std::string::String {
            fn from(value: WorkflowOutputName) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOutputName {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOutputName {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOutputName {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowOutputReference`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "output_name",
        ///    "producer_step_id"
        ///  ],
        ///  "properties": {
        ///    "output_name": {
        ///      "$ref": "#/definitions/WorkflowOutputName"
        ///    },
        ///    "producer_step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowOutputReference {
            pub output_name: WorkflowOutputName,
            pub producer_step_id: WorkflowStepId,
        }
        ///`WorkflowStep`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "inputs",
        ///    "operation",
        ///    "outputs",
        ///    "predecessors",
        ///    "step_id"
        ///  ],
        ///  "properties": {
        ///    "fan_out": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkflowFanOut"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "inputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputReference"
        ///      }
        ///    },
        ///    "operation": {
        ///      "$ref": "#/definitions/WorkflowOperationRef"
        ///    },
        ///    "outputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputName"
        ///      }
        ///    },
        ///    "predecessors": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowStep {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub fan_out: ::std::option::Option<WorkflowFanOut>,
            pub inputs: ::std::vec::Vec<WorkflowOutputReference>,
            pub operation: WorkflowOperationRef,
            pub outputs: ::std::vec::Vec<WorkflowOutputName>,
            pub predecessors: Vec<WorkflowStepId>,
            pub step_id: WorkflowStepId,
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkflowDefinitionListRequest;
    pub type Result = result::ArrayOfWorkflowDefinition;
}
typed_operation!(
    WorkflowListDefinitions,
    workflow_list_definitions,
    "operation.workflow.list_definitions",
    "/application/workflow/list-definitions",
    "binding.http.workflow.list_definitions",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    "schema.workflow.list_definitions.result",
    1
);
#[allow(clippy::all)]
pub mod workflow_register_definition {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "pinned_catalog_digest",
        ///    "pinned_configuration_digest",
        ///    "pinned_policy_digest",
        ///    "project_id",
        ///    "steps"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "pinned_catalog_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_configuration_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStep"
        ///      }
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinition {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: u64,
            pub pinned_catalog_digest: ManifestDigest,
            pub pinned_configuration_digest: ManifestDigest,
            pub pinned_policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub steps: ::std::vec::Vec<WorkflowStep>,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Wire request for [`WorkflowDefinitionService::register`].
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionRegisterRequest",
        ///  "description": "Wire request for [`WorkflowDefinitionService::register`].",
        ///  "type": "object",
        ///  "required": [
        ///    "definition"
        ///  ],
        ///  "properties": {
        ///    "definition": {
        ///      "$ref": "#/definitions/WorkflowDefinition"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionRegisterRequest {
            pub definition: WorkflowDefinition,
        }
        ///`WorkflowFanOut`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "max_width"
        ///  ],
        ///  "properties": {
        ///    "max_width": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowFanOut {
            pub max_width: u32,
        }
        ///Strongly typed canonical identity: `WorkflowOperationRef`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOperationRef`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOperationRef(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOperationRef {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOperationRef> for ::std::string::String {
            fn from(value: WorkflowOperationRef) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOperationRef {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOperationRef {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOperationRef {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowOutputName`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOutputName`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOutputName(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOutputName {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOutputName> for ::std::string::String {
            fn from(value: WorkflowOutputName) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOutputName {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOutputName {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOutputName {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowOutputReference`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "output_name",
        ///    "producer_step_id"
        ///  ],
        ///  "properties": {
        ///    "output_name": {
        ///      "$ref": "#/definitions/WorkflowOutputName"
        ///    },
        ///    "producer_step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowOutputReference {
            pub output_name: WorkflowOutputName,
            pub producer_step_id: WorkflowStepId,
        }
        ///`WorkflowStep`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "inputs",
        ///    "operation",
        ///    "outputs",
        ///    "predecessors",
        ///    "step_id"
        ///  ],
        ///  "properties": {
        ///    "fan_out": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkflowFanOut"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "inputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputReference"
        ///      }
        ///    },
        ///    "operation": {
        ///      "$ref": "#/definitions/WorkflowOperationRef"
        ///    },
        ///    "outputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputName"
        ///      }
        ///    },
        ///    "predecessors": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowStep {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub fan_out: ::std::option::Option<WorkflowFanOut>,
            pub inputs: ::std::vec::Vec<WorkflowOutputReference>,
            pub operation: WorkflowOperationRef,
            pub outputs: ::std::vec::Vec<WorkflowOutputName>,
            pub predecessors: Vec<WorkflowStepId>,
            pub step_id: WorkflowStepId,
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinition",
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "pinned_catalog_digest",
        ///    "pinned_configuration_digest",
        ///    "pinned_policy_digest",
        ///    "project_id",
        ///    "steps"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "pinned_catalog_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_configuration_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStep"
        ///      }
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinition {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: u64,
            pub pinned_catalog_digest: ManifestDigest,
            pub pinned_configuration_digest: ManifestDigest,
            pub pinned_policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub steps: ::std::vec::Vec<WorkflowStep>,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowFanOut`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "max_width"
        ///  ],
        ///  "properties": {
        ///    "max_width": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowFanOut {
            pub max_width: u32,
        }
        ///Strongly typed canonical identity: `WorkflowOperationRef`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOperationRef`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOperationRef(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOperationRef {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOperationRef> for ::std::string::String {
            fn from(value: WorkflowOperationRef) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOperationRef {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOperationRef {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOperationRef {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowOutputName`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOutputName`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOutputName(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOutputName {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOutputName> for ::std::string::String {
            fn from(value: WorkflowOutputName) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOutputName {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOutputName {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOutputName {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowOutputReference`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "output_name",
        ///    "producer_step_id"
        ///  ],
        ///  "properties": {
        ///    "output_name": {
        ///      "$ref": "#/definitions/WorkflowOutputName"
        ///    },
        ///    "producer_step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowOutputReference {
            pub output_name: WorkflowOutputName,
            pub producer_step_id: WorkflowStepId,
        }
        ///`WorkflowStep`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "inputs",
        ///    "operation",
        ///    "outputs",
        ///    "predecessors",
        ///    "step_id"
        ///  ],
        ///  "properties": {
        ///    "fan_out": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkflowFanOut"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "inputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputReference"
        ///      }
        ///    },
        ///    "operation": {
        ///      "$ref": "#/definitions/WorkflowOperationRef"
        ///    },
        ///    "outputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputName"
        ///      }
        ///    },
        ///    "predecessors": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowStep {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub fan_out: ::std::option::Option<WorkflowFanOut>,
            pub inputs: ::std::vec::Vec<WorkflowOutputReference>,
            pub operation: WorkflowOperationRef,
            pub outputs: ::std::vec::Vec<WorkflowOutputName>,
            pub predecessors: Vec<WorkflowStepId>,
            pub step_id: WorkflowStepId,
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkflowDefinitionRegisterRequest;
    pub type Result = result::WorkflowDefinition;
}
typed_operation!(
    WorkflowRegisterDefinition,
    workflow_register_definition,
    "operation.workflow.register_definition",
    "/application/workflow/register-definition",
    "binding.http.workflow.register_definition",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    "schema.workflow.register_definition.result",
    1
);
#[allow(clippy::all)]
pub mod workflow_validate_definition {
    pub mod request {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "pinned_catalog_digest",
        ///    "pinned_configuration_digest",
        ///    "pinned_policy_digest",
        ///    "project_id",
        ///    "steps"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "pinned_catalog_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_configuration_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStep"
        ///      }
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinition {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: u64,
            pub pinned_catalog_digest: ManifestDigest,
            pub pinned_configuration_digest: ManifestDigest,
            pub pinned_policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub steps: ::std::vec::Vec<WorkflowStep>,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Wire request for [`WorkflowDefinitionService::validate`].
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionValidateRequest",
        ///  "description": "Wire request for [`WorkflowDefinitionService::validate`].",
        ///  "type": "object",
        ///  "required": [
        ///    "definition"
        ///  ],
        ///  "properties": {
        ///    "definition": {
        ///      "$ref": "#/definitions/WorkflowDefinition"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionValidateRequest {
            pub definition: WorkflowDefinition,
        }
        ///`WorkflowFanOut`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "max_width"
        ///  ],
        ///  "properties": {
        ///    "max_width": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowFanOut {
            pub max_width: u32,
        }
        ///Strongly typed canonical identity: `WorkflowOperationRef`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOperationRef`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOperationRef(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOperationRef {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOperationRef> for ::std::string::String {
            fn from(value: WorkflowOperationRef) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOperationRef {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOperationRef {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOperationRef {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowOutputName`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOutputName`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOutputName(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOutputName {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOutputName> for ::std::string::String {
            fn from(value: WorkflowOutputName) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOutputName {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOutputName {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOutputName {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowOutputReference`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "output_name",
        ///    "producer_step_id"
        ///  ],
        ///  "properties": {
        ///    "output_name": {
        ///      "$ref": "#/definitions/WorkflowOutputName"
        ///    },
        ///    "producer_step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowOutputReference {
            pub output_name: WorkflowOutputName,
            pub producer_step_id: WorkflowStepId,
        }
        ///`WorkflowStep`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "inputs",
        ///    "operation",
        ///    "outputs",
        ///    "predecessors",
        ///    "step_id"
        ///  ],
        ///  "properties": {
        ///    "fan_out": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkflowFanOut"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "inputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputReference"
        ///      }
        ///    },
        ///    "operation": {
        ///      "$ref": "#/definitions/WorkflowOperationRef"
        ///    },
        ///    "outputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputName"
        ///      }
        ///    },
        ///    "predecessors": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowStep {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub fan_out: ::std::option::Option<WorkflowFanOut>,
            pub inputs: ::std::vec::Vec<WorkflowOutputReference>,
            pub operation: WorkflowOperationRef,
            pub outputs: ::std::vec::Vec<WorkflowOutputName>,
            pub predecessors: Vec<WorkflowStepId>,
            pub step_id: WorkflowStepId,
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub mod result {
        /// Error types.
        pub mod error {
            /// Error from a `TryFrom` or `FromStr` implementation.
            pub struct ConversionError(::std::borrow::Cow<'static, str>);
            impl ::std::error::Error for ConversionError {}
            impl ::std::fmt::Display for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Display::fmt(&self.0, f)
                }
            }
            impl ::std::fmt::Debug for ConversionError {
                fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> Result<(), ::std::fmt::Error> {
                    ::std::fmt::Debug::fmt(&self.0, f)
                }
            }
            impl From<&'static str> for ConversionError {
                fn from(value: &'static str) -> Self {
                    Self(value.into())
                }
            }
            impl From<String> for ConversionError {
                fn from(value: String) -> Self {
                    Self(value.into())
                }
            }
        }
        ///Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed algorithm-tagged integrity digest: `ManifestDigest`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ManifestDigest(pub ::std::string::String);
        impl ::std::ops::Deref for ManifestDigest {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ManifestDigest> for ::std::string::String {
            fn from(value: ManifestDigest) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ManifestDigest {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ManifestDigest {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ManifestDigest {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `ProjectId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `ProjectId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct ProjectId(pub ::std::string::String);
        impl ::std::ops::Deref for ProjectId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<ProjectId> for ::std::string::String {
            fn from(value: ProjectId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for ProjectId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for ProjectId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for ProjectId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinition`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "definition_id",
        ///    "definition_version",
        ///    "pinned_catalog_digest",
        ///    "pinned_configuration_digest",
        ///    "pinned_policy_digest",
        ///    "project_id",
        ///    "steps"
        ///  ],
        ///  "properties": {
        ///    "definition_id": {
        ///      "$ref": "#/definitions/WorkflowDefinitionId"
        ///    },
        ///    "definition_version": {
        ///      "type": "integer",
        ///      "format": "uint64",
        ///      "minimum": 0.0
        ///    },
        ///    "pinned_catalog_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_configuration_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "pinned_policy_digest": {
        ///      "$ref": "#/definitions/ManifestDigest"
        ///    },
        ///    "project_id": {
        ///      "$ref": "#/definitions/ProjectId"
        ///    },
        ///    "steps": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStep"
        ///      }
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinition {
            pub definition_id: WorkflowDefinitionId,
            pub definition_version: u64,
            pub pinned_catalog_digest: ManifestDigest,
            pub pinned_configuration_digest: ManifestDigest,
            pub pinned_policy_digest: ManifestDigest,
            pub project_id: ProjectId,
            pub steps: ::std::vec::Vec<WorkflowStep>,
        }
        ///Strongly typed canonical identity: `WorkflowDefinitionId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowDefinitionId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowDefinitionId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowDefinitionId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowDefinitionId> for ::std::string::String {
            fn from(value: WorkflowDefinitionId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowDefinitionId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowDefinitionId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowDefinitionId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowDefinitionValidation`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "title": "WorkflowDefinitionValidation",
        ///  "type": "object",
        ///  "required": [
        ///    "definition"
        ///  ],
        ///  "properties": {
        ///    "definition": {
        ///      "$ref": "#/definitions/WorkflowDefinition"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowDefinitionValidation {
            pub definition: WorkflowDefinition,
        }
        ///`WorkflowFanOut`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "max_width"
        ///  ],
        ///  "properties": {
        ///    "max_width": {
        ///      "type": "integer",
        ///      "format": "uint32",
        ///      "minimum": 0.0
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowFanOut {
            pub max_width: u32,
        }
        ///Strongly typed canonical identity: `WorkflowOperationRef`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOperationRef`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOperationRef(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOperationRef {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOperationRef> for ::std::string::String {
            fn from(value: WorkflowOperationRef) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOperationRef {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOperationRef {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOperationRef {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///Strongly typed canonical identity: `WorkflowOutputName`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowOutputName`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowOutputName(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowOutputName {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowOutputName> for ::std::string::String {
            fn from(value: WorkflowOutputName) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowOutputName {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowOutputName {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowOutputName {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
        ///`WorkflowOutputReference`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "output_name",
        ///    "producer_step_id"
        ///  ],
        ///  "properties": {
        ///    "output_name": {
        ///      "$ref": "#/definitions/WorkflowOutputName"
        ///    },
        ///    "producer_step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowOutputReference {
            pub output_name: WorkflowOutputName,
            pub producer_step_id: WorkflowStepId,
        }
        ///`WorkflowStep`
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "type": "object",
        ///  "required": [
        ///    "inputs",
        ///    "operation",
        ///    "outputs",
        ///    "predecessors",
        ///    "step_id"
        ///  ],
        ///  "properties": {
        ///    "fan_out": {
        ///      "anyOf": [
        ///        {
        ///          "$ref": "#/definitions/WorkflowFanOut"
        ///        },
        ///        {
        ///          "type": "null"
        ///        }
        ///      ]
        ///    },
        ///    "inputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputReference"
        ///      }
        ///    },
        ///    "operation": {
        ///      "$ref": "#/definitions/WorkflowOperationRef"
        ///    },
        ///    "outputs": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowOutputName"
        ///      }
        ///    },
        ///    "predecessors": {
        ///      "type": "array",
        ///      "items": {
        ///        "$ref": "#/definitions/WorkflowStepId"
        ///      },
        ///      "uniqueItems": true
        ///    },
        ///    "step_id": {
        ///      "$ref": "#/definitions/WorkflowStepId"
        ///    }
        ///  },
        ///  "additionalProperties": false
        ///}
        /// ```
        /// </details>
        #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
        #[serde(deny_unknown_fields)]
        pub struct WorkflowStep {
            #[serde(default, skip_serializing_if = "::std::option::Option::is_none")]
            pub fan_out: ::std::option::Option<WorkflowFanOut>,
            pub inputs: ::std::vec::Vec<WorkflowOutputReference>,
            pub operation: WorkflowOperationRef,
            pub outputs: ::std::vec::Vec<WorkflowOutputName>,
            pub predecessors: Vec<WorkflowStepId>,
            pub step_id: WorkflowStepId,
        }
        ///Strongly typed canonical identity: `WorkflowStepId`.
        ///
        /// <details><summary>JSON schema</summary>
        ///
        /// ```json
        ///{
        ///  "description": "Strongly typed canonical identity: `WorkflowStepId`.",
        ///  "type": "string"
        ///}
        /// ```
        /// </details>
        #[derive(
            ::serde::Deserialize,
            ::serde::Serialize,
            Clone,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
        )]
        #[serde(transparent)]
        pub struct WorkflowStepId(pub ::std::string::String);
        impl ::std::ops::Deref for WorkflowStepId {
            type Target = ::std::string::String;
            fn deref(&self) -> &::std::string::String {
                &self.0
            }
        }
        impl ::std::convert::From<WorkflowStepId> for ::std::string::String {
            fn from(value: WorkflowStepId) -> Self {
                value.0
            }
        }
        impl ::std::convert::From<::std::string::String> for WorkflowStepId {
            fn from(value: ::std::string::String) -> Self {
                Self(value)
            }
        }
        impl ::std::str::FromStr for WorkflowStepId {
            type Err = ::std::convert::Infallible;
            fn from_str(value: &str) -> ::std::result::Result<Self, Self::Err> {
                Ok(Self(value.to_string()))
            }
        }
        impl ::std::fmt::Display for WorkflowStepId {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }
    }
    pub type Request = request::WorkflowDefinitionValidateRequest;
    pub type Result = result::WorkflowDefinitionValidation;
}
typed_operation!(
    WorkflowValidateDefinition,
    workflow_validate_definition,
    "operation.workflow.validate_definition",
    "/application/workflow/validate-definition",
    "binding.http.workflow.validate_definition",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    "schema.workflow.validate_definition.result",
    1
);

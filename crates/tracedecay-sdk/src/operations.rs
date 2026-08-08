//! Generated typed public operation descriptors. DO NOT EDIT.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_tool_catalog::{
    CancellationPoint, DeadlineBehavior, EffectClass, ExecutableUnavailableDispositionV1,
    IdempotencyContract, ReceiptContract, ReconciliationContract, TerminalState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnavailableOperationCapability {
    pub operation: &'static str,
    pub operation_id: &'static str,
    pub disposition: ExecutableUnavailableDispositionV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationTransport {
    Http { route: &'static str },
    McpTool { tool_name: &'static str },
}

pub trait TypedOperation {
    type Request: Serialize;
    type Result: DeserializeOwned;

    const OPERATION_ID: &'static str;
    const TRANSPORT: OperationTransport;
    const BINDING_ID: &'static str;
    const EFFECT: EffectClass;
    const IDEMPOTENCY: IdempotencyContract;
    const CANCELLABLE: bool;
    const CANCELLATION_POINTS: &'static [CancellationPoint];
    const MAXIMUM_DEADLINE_MILLIS: u64;
    const DEADLINE_BEHAVIOR: DeadlineBehavior;
    const RECONCILIATION: ReconciliationContract;
    const RECEIPT: ReceiptContract;
    const TERMINAL_STATES: &'static [TerminalState];
    const RESULT_SCHEMA_ID: &'static str;
    const RESULT_SCHEMA_REVISION: u32;
}

macro_rules! typed_operation {
    ($name:ident, $module:ident, $operation:literal, $transport:expr, $binding:literal, $effect:expr, $idempotency:expr, $cancellable:literal, $cancellation_points:expr, $maximum_deadline:literal, $deadline_behavior:expr, $reconciliation:expr, $receipt:expr, $terminal_states:expr, $schema:literal, $revision:literal) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct $name;
        impl TypedOperation for $name {
            type Request = $module::Request;
            type Result = $module::Result;
            const OPERATION_ID: &'static str = $operation;
            const TRANSPORT: OperationTransport = $transport;
            const BINDING_ID: &'static str = $binding;
            const EFFECT: EffectClass = $effect;
            const IDEMPOTENCY: IdempotencyContract = $idempotency;
            const CANCELLABLE: bool = $cancellable;
            const CANCELLATION_POINTS: &'static [CancellationPoint] = $cancellation_points;
            const MAXIMUM_DEADLINE_MILLIS: u64 = $maximum_deadline;
            const DEADLINE_BEHAVIOR: DeadlineBehavior = $deadline_behavior;
            const RECONCILIATION: ReconciliationContract = $reconciliation;
            const RECEIPT: ReceiptContract = $receipt;
            const TERMINAL_STATES: &'static [TerminalState] = $terminal_states;
            const RESULT_SCHEMA_ID: &'static str = $schema;
            const RESULT_SCHEMA_REVISION: u32 = $revision;
        }
    };
}

pub const UNAVAILABLE_OPERATIONS: &[UnavailableOperationCapability] = &[];

pub mod affected_tests {
    pub type Request = tracedecay_application::feedback::FeedbackHandleRequestV1;
    pub type Result = tracedecay_application::feedback::CanonicalAffectedTestsProjectionV1;
}
typed_operation!(
    AffectedTests,
    affected_tests,
    "operation.application.affected_tests",
    OperationTransport::McpTool {
        tool_name: "tracedecay_affected_tests"
    },
    "binding.mcp.affected_tests.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.affected-tests.result",
    1
);

pub mod apply_native_integration {
    pub type Request = tracedecay_application::git::NativeIntegrationApplySurfaceRequest;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    ApplyNativeIntegration,
    apply_native_integration,
    "operation.application.apply_native_integration",
    OperationTransport::McpTool {
        tool_name: "tracedecay_apply_native_integration"
    },
    "binding.mcp.apply_native_integration.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.native-integration.apply.result",
    1
);

pub mod approve_native_integration {
    pub type Request = tracedecay_application::git::NativeIntegrationApproveSurfaceRequest;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    ApproveNativeIntegration,
    approve_native_integration,
    "operation.application.approve_native_integration",
    OperationTransport::McpTool {
        tool_name: "tracedecay_approve_native_integration"
    },
    "binding.mcp.approve_native_integration.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.native-integration.approve.result",
    1
);

pub mod ast_grep_rewrite {
    pub type Request = tracedecay_application::source_edit::AstGrepRewriteSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    AstGrepRewrite,
    ast_grep_rewrite,
    "operation.application.ast_grep_rewrite",
    OperationTransport::McpTool {
        tool_name: "tracedecay_ast_grep_rewrite"
    },
    "binding.mcp.ast_grep_rewrite.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.ast-grep-rewrite.result",
    1
);

pub mod call_chain {
    pub type Request = tracedecay_application::retrieval::CallChainPrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::CallChainPrimitiveResult;
}
typed_operation!(
    CallChain,
    call_chain,
    "operation.application.call_chain",
    OperationTransport::McpTool {
        tool_name: "tracedecay_call_chain"
    },
    "binding.mcp.call_chain.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.call-chain.result",
    1
);

pub mod cancel_native_integration {
    pub type Request = tracedecay_application::git::NativeIntegrationCancelSurfaceRequest;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    CancelNativeIntegration,
    cancel_native_integration,
    "operation.application.cancel_native_integration",
    OperationTransport::McpTool {
        tool_name: "tracedecay_cancel_native_integration"
    },
    "binding.mcp.cancel_native_integration.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.native-integration.cancel.result",
    1
);

pub mod code_callees {
    pub type Request = tracedecay_application::retrieval::CodeRelationRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::SymbolRelationRecord,
    >;
}
typed_operation!(
    CodeCallees,
    code_callees,
    "operation.application.code_callees",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_callees"
    },
    "binding.mcp.code_callees.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.callees.result",
    1
);

pub mod code_callers {
    pub type Request = tracedecay_application::retrieval::GraphRelationRequest;
    pub type Result = tracedecay_application::retrieval::SymbolGraphPage<
        tracedecay_application::retrieval::SymbolRelationRecord,
    >;
}
typed_operation!(
    CodeCallers,
    code_callers,
    "operation.application.code_callers",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_callers"
    },
    "binding.mcp.code_callers.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.code-callers.result",
    1
);

pub mod code_declaration {
    pub type Request = tracedecay_application::retrieval::CodeNavigationRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::SymbolPrimitiveRecord,
    >;
}
typed_operation!(
    CodeDeclaration,
    code_declaration,
    "operation.application.code_declaration",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_declaration"
    },
    "binding.mcp.code_declaration.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.declaration.result",
    1
);

pub mod code_definition {
    pub type Request = tracedecay_application::retrieval::CodeNavigationRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::SymbolPrimitiveRecord,
    >;
}
typed_operation!(
    CodeDefinition,
    code_definition,
    "operation.application.code_definition",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_definition"
    },
    "binding.mcp.code_definition.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.definition.result",
    1
);

pub mod code_exact_occurrence {
    pub type Request = tracedecay_application::retrieval::ExactOccurrenceRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::ExactOccurrenceRecord,
    >;
}
typed_operation!(
    CodeExactOccurrence,
    code_exact_occurrence,
    "operation.application.code_exact_occurrence",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_exact_occurrence"
    },
    "binding.mcp.code_exact_occurrence.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.exact-occurrence.result",
    1
);

pub mod code_facets {
    pub type Request = tracedecay_application::retrieval::CodeFacetRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::CodeFacetRecord,
    >;
}
typed_operation!(
    CodeFacets,
    code_facets,
    "operation.application.code_facets",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_facets"
    },
    "binding.mcp.code_facets.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.facets.result",
    1
);

pub mod code_implementations {
    pub type Request = tracedecay_application::retrieval::ImplementationsRequest;
    pub type Result = tracedecay_application::retrieval::SymbolGraphPage<
        tracedecay_application::retrieval::SymbolRelationRecord,
    >;
}
typed_operation!(
    CodeImplementations,
    code_implementations,
    "operation.application.code_implementations",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_implementations"
    },
    "binding.mcp.code_implementations.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.code-implementations.result",
    1
);

pub mod code_phrase_search {
    pub type Request = tracedecay_application::retrieval::PhraseSearchSurfaceRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::LexicalOccurrenceRecord,
    >;
}
typed_operation!(
    CodePhraseSearch,
    code_phrase_search,
    "operation.application.code_phrase_search",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_phrase_search"
    },
    "binding.mcp.code_phrase_search.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.phrase-search.result",
    1
);

pub mod code_references {
    pub type Request = tracedecay_application::retrieval::CodeNavigationRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::SymbolRelationRecord,
    >;
}
typed_operation!(
    CodeReferences,
    code_references,
    "operation.application.code_references",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_references"
    },
    "binding.mcp.code_references.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.references.result",
    1
);

pub mod code_signature_search {
    pub type Request = tracedecay_application::retrieval::SignatureSearchRequest;
    pub type Result = tracedecay_application::retrieval::SymbolGraphPage<
        tracedecay_application::retrieval::SymbolPrimitiveRecord,
    >;
}
typed_operation!(
    CodeSignatureSearch,
    code_signature_search,
    "operation.application.code_signature_search",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_signature_search"
    },
    "binding.mcp.code_signature_search.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.code-signature-search.result",
    1
);

pub mod code_symbol_search {
    pub type Request = tracedecay_application::retrieval::CodeSymbolSearchSurfaceRequestV1;
    pub type Result = tracedecay_application::retrieval::SymbolGraphPage<
        tracedecay_application::retrieval::SymbolPrimitiveRecord,
    >;
}
typed_operation!(
    CodeSymbolSearch,
    code_symbol_search,
    "operation.application.code_symbol_search",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_symbol_search"
    },
    "binding.mcp.code_symbol_search.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.symbol-search.result",
    1
);

pub mod code_timeline {
    pub type Request = tracedecay_application::retrieval::CodeTimelineRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::CodeTimelineRecord,
    >;
}
typed_operation!(
    CodeTimeline,
    code_timeline,
    "operation.application.code_timeline",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_timeline"
    },
    "binding.mcp.code_timeline.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.timeline.result",
    1
);

pub mod code_type_definition {
    pub type Request = tracedecay_application::retrieval::CodeNavigationRequest;
    pub type Result = tracedecay_application::retrieval::CodeQueryPage<
        tracedecay_application::retrieval::SymbolPrimitiveRecord,
    >;
}
typed_operation!(
    CodeTypeDefinition,
    code_type_definition,
    "operation.application.code_type_definition",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_type_definition"
    },
    "binding.mcp.code_type_definition.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.code-query.type-definition.result",
    1
);

pub mod code_type_hierarchy {
    pub type Request = tracedecay_application::retrieval::TypeHierarchyRequest;
    pub type Result = tracedecay_application::retrieval::SymbolGraphPage<
        tracedecay_application::retrieval::TypeHierarchyRecord,
    >;
}
typed_operation!(
    CodeTypeHierarchy,
    code_type_hierarchy,
    "operation.application.code_type_hierarchy",
    OperationTransport::McpTool {
        tool_name: "tracedecay_code_type_hierarchy"
    },
    "binding.mcp.code_type_hierarchy.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.code-type-hierarchy.result",
    1
);

pub mod application_configuration_audit {
    pub type Request = tracedecay_application::configuration::ConfigurationAuditRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationAuditPage;
}
typed_operation!(
    ApplicationConfigurationAudit,
    application_configuration_audit,
    "operation.application.configuration_audit",
    OperationTransport::Http {
        route: "/application/configuration/configuration_audit"
    },
    "binding.http.configuration_audit.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_audit.result",
    1
);

pub mod application_configuration_batch {
    pub type Request = tracedecay_application::configuration::ConfigurationBatchRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationMutationReceipt;
}
typed_operation!(
    ApplicationConfigurationBatch,
    application_configuration_batch,
    "operation.application.configuration_batch",
    OperationTransport::Http {
        route: "/application/configuration/configuration_batch"
    },
    "binding.http.configuration_batch.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_batch.result",
    1
);

pub mod application_configuration_explain {
    pub type Request = tracedecay_application::configuration::ConfigurationGetRequestV1;
    pub type Result = tracedecay_application::configuration::ResolvedSetting;
}
typed_operation!(
    ApplicationConfigurationExplain,
    application_configuration_explain,
    "operation.application.configuration_explain",
    OperationTransport::Http {
        route: "/application/configuration/configuration_explain"
    },
    "binding.http.configuration_explain.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_explain.result",
    1
);

pub mod application_configuration_get {
    pub type Request = tracedecay_application::configuration::ConfigurationGetRequestV1;
    pub type Result = tracedecay_application::configuration::ResolvedSetting;
}
typed_operation!(
    ApplicationConfigurationGet,
    application_configuration_get,
    "operation.application.configuration_get",
    OperationTransport::Http {
        route: "/application/configuration/configuration_get"
    },
    "binding.http.configuration_get.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_get.result",
    1
);

pub mod application_configuration_list {
    extern crate alloc;
    pub type Request = tracedecay_application::configuration::ConfigurationListRequestV1;
    pub type Result = alloc::vec::Vec<tracedecay_application::configuration::SettingSummary>;
}
typed_operation!(
    ApplicationConfigurationList,
    application_configuration_list,
    "operation.application.configuration_list",
    OperationTransport::Http {
        route: "/application/configuration/configuration_list"
    },
    "binding.http.configuration_list.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_list.result",
    1
);

pub mod application_configuration_observed_state {
    extern crate alloc;
    pub type Request = tracedecay_application::configuration::ConfigurationObservedStateRequestV1;
    pub type Result =
        alloc::vec::Vec<tracedecay_application::configuration::ComponentConfigurationState>;
}
typed_operation!(
    ApplicationConfigurationObservedState,
    application_configuration_observed_state,
    "operation.application.configuration_observed_state",
    OperationTransport::Http {
        route: "/application/configuration/configuration_observed_state"
    },
    "binding.http.configuration_observed_state.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_observed_state.result",
    1
);

pub mod application_configuration_protected_apply {
    pub type Request = tracedecay_application::configuration::ConfigurationProtectedApplyRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationMutationReceipt;
}
typed_operation!(
    ApplicationConfigurationProtectedApply,
    application_configuration_protected_apply,
    "operation.application.configuration_protected_apply",
    OperationTransport::Http {
        route: "/application/configuration/configuration_protected_apply"
    },
    "binding.http.configuration_protected_apply.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_protected_apply.result",
    1
);

pub mod application_configuration_protected_preview {
    pub type Request =
        tracedecay_application::configuration::ConfigurationProtectedPreviewRequestV1;
    pub type Result = tracedecay_domain::configuration::ProtectedChangePlan;
}
typed_operation!(
    ApplicationConfigurationProtectedPreview,
    application_configuration_protected_preview,
    "operation.application.configuration_protected_preview",
    OperationTransport::Http {
        route: "/application/configuration/configuration_protected_preview"
    },
    "binding.http.configuration_protected_preview.v1",
    EffectClass::Preview,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_protected_preview.result",
    1
);

pub mod application_configuration_rollback_apply {
    pub type Request = tracedecay_application::configuration::ConfigurationRollbackApplyRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationMutationReceipt;
}
typed_operation!(
    ApplicationConfigurationRollbackApply,
    application_configuration_rollback_apply,
    "operation.application.configuration_rollback_apply",
    OperationTransport::Http {
        route: "/application/configuration/configuration_rollback_apply"
    },
    "binding.http.configuration_rollback_apply.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_rollback_apply.result",
    1
);

pub mod application_configuration_rollback_preview {
    pub type Request = tracedecay_application::configuration::ConfigurationRollbackPreviewRequestV1;
    pub type Result = tracedecay_domain::configuration::ProtectedChangePlan;
}
typed_operation!(
    ApplicationConfigurationRollbackPreview,
    application_configuration_rollback_preview,
    "operation.application.configuration_rollback_preview",
    OperationTransport::Http {
        route: "/application/configuration/configuration_rollback_preview"
    },
    "binding.http.configuration_rollback_preview.v1",
    EffectClass::Preview,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_rollback_preview.result",
    1
);

pub mod application_configuration_set {
    pub type Request = tracedecay_application::configuration::ConfigurationSetRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationMutationReceipt;
}
typed_operation!(
    ApplicationConfigurationSet,
    application_configuration_set,
    "operation.application.configuration_set",
    OperationTransport::Http {
        route: "/application/configuration/configuration_set"
    },
    "binding.http.configuration_set.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_set.result",
    1
);

pub mod application_configuration_unset {
    pub type Request = tracedecay_application::configuration::ConfigurationUnsetRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationMutationReceipt;
}
typed_operation!(
    ApplicationConfigurationUnset,
    application_configuration_unset,
    "operation.application.configuration_unset",
    OperationTransport::Http {
        route: "/application/configuration/configuration_unset"
    },
    "binding.http.configuration_unset.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_unset.result",
    1
);

pub mod application_configuration_write_credential {
    pub type Request = tracedecay_application::configuration::ConfigurationWriteCredentialRequestV1;
    pub type Result = tracedecay_domain::configuration::CredentialReferenceMetadataV1;
}
typed_operation!(
    ApplicationConfigurationWriteCredential,
    application_configuration_write_credential,
    "operation.application.configuration_write_credential",
    OperationTransport::Http {
        route: "/application/configuration/configuration_write_credential"
    },
    "binding.http.configuration_write_credential.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.configuration_write_credential.result",
    1
);

pub mod application_context_scout_budget {
    pub type Request = tracedecay_application::context_scout::ContextScoutExactAddressRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutBudgetResultV1;
}
typed_operation!(
    ApplicationContextScoutBudget,
    application_context_scout_budget,
    "operation.application.context_scout_budget",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_budget"
    },
    "binding.http.context_scout_budget.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.context-scout-budget.result",
    1
);

pub mod application_context_scout_cancel {
    pub type Request = tracedecay_application::context_scout::ContextScoutCancelRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutMutationResultV1;
}
typed_operation!(
    ApplicationContextScoutCancel,
    application_context_scout_cancel,
    "operation.application.context_scout_cancel",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_cancel"
    },
    "binding.http.context_scout_cancel.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.context-scout-cancel.result",
    1
);

pub mod application_context_scout_capability {
    pub type Request = tracedecay_application::context_scout::ContextScoutExactAddressRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutCapabilityResultV1;
}
typed_operation!(
    ApplicationContextScoutCapability,
    application_context_scout_capability,
    "operation.application.context_scout_capability",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_capability"
    },
    "binding.http.context_scout_capability.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.context-scout-capability.result",
    1
);

pub mod application_context_scout_claim {
    pub type Request = tracedecay_application::context_scout::ContextScoutClaimRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutClaimResultV1;
}
typed_operation!(
    ApplicationContextScoutClaim,
    application_context_scout_claim,
    "operation.application.context_scout_claim",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_claim"
    },
    "binding.http.context_scout_claim.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.context-scout-claim.result",
    1
);

pub mod application_context_scout_delivery {
    pub type Request = tracedecay_application::context_scout::ContextScoutDeliveryRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutMutationResultV1;
}
typed_operation!(
    ApplicationContextScoutDelivery,
    application_context_scout_delivery,
    "operation.application.context_scout_delivery",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_delivery"
    },
    "binding.http.context_scout_delivery.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.context-scout-delivery.result",
    1
);

pub mod application_context_scout_explain {
    pub type Request = tracedecay_application::context_scout::ContextScoutRecentRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutExplanationResultV1;
}
typed_operation!(
    ApplicationContextScoutExplain,
    application_context_scout_explain,
    "operation.application.context_scout_explain",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_explain"
    },
    "binding.http.context_scout_explain.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.context-scout-explain.result",
    1
);

pub mod application_context_scout_feedback {
    pub type Request = tracedecay_application::context_scout::ContextScoutFeedbackRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutMutationResultV1;
}
typed_operation!(
    ApplicationContextScoutFeedback,
    application_context_scout_feedback,
    "operation.application.context_scout_feedback",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_feedback"
    },
    "binding.http.context_scout_feedback.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.context-scout-feedback.result",
    1
);

pub mod application_context_scout_pause {
    pub type Request = tracedecay_application::context_scout::ContextScoutControlRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationMutationReceipt;
}
typed_operation!(
    ApplicationContextScoutPause,
    application_context_scout_pause,
    "operation.application.context_scout_pause",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_pause"
    },
    "binding.http.context_scout_pause.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.context-scout-pause.result",
    1
);

pub mod application_context_scout_recent {
    pub type Request = tracedecay_application::context_scout::ContextScoutRecentRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutRecentResultV1;
}
typed_operation!(
    ApplicationContextScoutRecent,
    application_context_scout_recent,
    "operation.application.context_scout_recent",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_recent"
    },
    "binding.http.context_scout_recent.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.context-scout-recent.result",
    1
);

pub mod application_context_scout_resume {
    pub type Request = tracedecay_application::context_scout::ContextScoutControlRequestV1;
    pub type Result = tracedecay_application::configuration::ConfigurationMutationReceipt;
}
typed_operation!(
    ApplicationContextScoutResume,
    application_context_scout_resume,
    "operation.application.context_scout_resume",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_resume"
    },
    "binding.http.context_scout_resume.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.context-scout-resume.result",
    1
);

pub mod application_context_scout_status {
    pub type Request = tracedecay_application::context_scout::ContextScoutExactAddressRequestV1;
    pub type Result = tracedecay_application::context_scout::ContextScoutStatusResultV1;
}
typed_operation!(
    ApplicationContextScoutStatus,
    application_context_scout_status,
    "operation.application.context_scout_status",
    OperationTransport::Http {
        route: "/application/context-scout/context_scout_status"
    },
    "binding.http.context_scout_status.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.context-scout-status.result",
    1
);

pub mod diagnostics_read {
    pub type Request = tracedecay_application::retrieval::DiagnosticsPrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::DiagnosticsPrimitiveResult;
}
typed_operation!(
    DiagnosticsRead,
    diagnostics_read,
    "operation.application.diagnostics_read",
    OperationTransport::McpTool {
        tool_name: "tracedecay_diagnostics_read"
    },
    "binding.mcp.diagnostics_read.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.diagnostics-read.result",
    1
);

pub mod application_fact_feedback {
    pub type Request = tracedecay_application::retained_surfaces::FactFeedbackRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactFeedbackResultV1;
}
typed_operation!(
    ApplicationFactFeedback,
    application_fact_feedback,
    "operation.application.fact_feedback",
    OperationTransport::Http {
        route: "/application/retained/fact_feedback"
    },
    "binding.http.fact_feedback.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::Reconciling,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-feedback.result",
    1
);

pub mod application_fact_store_add {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreAddRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreAddResultV1;
}
typed_operation!(
    ApplicationFactStoreAdd,
    application_fact_store_add,
    "operation.application.fact_store_add",
    OperationTransport::Http {
        route: "/application/retained/fact_store_add"
    },
    "binding.http.fact_store_add.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::Reconciling,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-add.result",
    1
);

pub mod application_fact_store_contradict {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreContradictRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreContradictResultV1;
}
typed_operation!(
    ApplicationFactStoreContradict,
    application_fact_store_contradict,
    "operation.application.fact_store_contradict",
    OperationTransport::Http {
        route: "/application/retained/fact_store_contradict"
    },
    "binding.http.fact_store_contradict.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-contradict.result",
    1
);

pub mod application_fact_store_get {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreGetRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreGetResultV1;
}
typed_operation!(
    ApplicationFactStoreGet,
    application_fact_store_get,
    "operation.application.fact_store_get",
    OperationTransport::Http {
        route: "/application/retained/fact_store_get"
    },
    "binding.http.fact_store_get.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-get.result",
    1
);

pub mod application_fact_store_list {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreListRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreListResultV1;
}
typed_operation!(
    ApplicationFactStoreList,
    application_fact_store_list,
    "operation.application.fact_store_list",
    OperationTransport::Http {
        route: "/application/retained/fact_store_list"
    },
    "binding.http.fact_store_list.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-list.result",
    1
);

pub mod application_fact_store_probe {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreProbeRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreProbeResultV1;
}
typed_operation!(
    ApplicationFactStoreProbe,
    application_fact_store_probe,
    "operation.application.fact_store_probe",
    OperationTransport::Http {
        route: "/application/retained/fact_store_probe"
    },
    "binding.http.fact_store_probe.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-probe.result",
    1
);

pub mod application_fact_store_reason {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreReasonRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreReasonResultV1;
}
typed_operation!(
    ApplicationFactStoreReason,
    application_fact_store_reason,
    "operation.application.fact_store_reason",
    OperationTransport::Http {
        route: "/application/retained/fact_store_reason"
    },
    "binding.http.fact_store_reason.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-reason.result",
    1
);

pub mod application_fact_store_related {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreRelatedRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreRelatedResultV1;
}
typed_operation!(
    ApplicationFactStoreRelated,
    application_fact_store_related,
    "operation.application.fact_store_related",
    OperationTransport::Http {
        route: "/application/retained/fact_store_related"
    },
    "binding.http.fact_store_related.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-related.result",
    1
);

pub mod application_fact_store_remove {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreRemoveRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreRemoveResultV1;
}
typed_operation!(
    ApplicationFactStoreRemove,
    application_fact_store_remove,
    "operation.application.fact_store_remove",
    OperationTransport::Http {
        route: "/application/retained/fact_store_remove"
    },
    "binding.http.fact_store_remove.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::Reconciling,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-remove.result",
    1
);

pub mod application_fact_store_search {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreSearchRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreSearchResultV1;
}
typed_operation!(
    ApplicationFactStoreSearch,
    application_fact_store_search,
    "operation.application.fact_store_search",
    OperationTransport::Http {
        route: "/application/retained/fact_store_search"
    },
    "binding.http.fact_store_search.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-search.result",
    1
);

pub mod application_fact_store_update {
    pub type Request = tracedecay_application::retained_surfaces::FactStoreUpdateRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::FactStoreUpdateResultV1;
}
typed_operation!(
    ApplicationFactStoreUpdate,
    application_fact_store_update,
    "operation.application.fact_store_update",
    OperationTransport::Http {
        route: "/application/retained/fact_store_update"
    },
    "binding.http.fact_store_update.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::Reconciling,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.retained.fact-store-update.result",
    1
);

pub mod feedback_advisory_cycle {
    pub type Request = tracedecay_application::feedback::FeedbackAdvisoryCycleSurfaceRequestV1;
    pub type Result = tracedecay_application::feedback::FeedbackAdvisoryCycleSurfaceResultV1;
}
typed_operation!(
    FeedbackAdvisoryCycle,
    feedback_advisory_cycle,
    "operation.application.feedback_advisory_cycle",
    OperationTransport::McpTool {
        tool_name: "tracedecay_feedback_advisory_cycle"
    },
    "binding.mcp.feedback_advisory_cycle.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.advisory-cycle.result",
    1
);

pub mod feedback_diagnostics {
    pub type Request = tracedecay_application::feedback::FeedbackHandleRequestV1;
    pub type Result = tracedecay_application::feedback::FeedbackDiagnosticsReadResultV1;
}
typed_operation!(
    FeedbackDiagnostics,
    feedback_diagnostics,
    "operation.application.feedback_diagnostics",
    OperationTransport::McpTool {
        tool_name: "tracedecay_feedback_diagnostics"
    },
    "binding.mcp.feedback_diagnostics.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.diagnostics.result",
    1
);

pub mod feedback_expand {
    pub type Request = tracedecay_application::feedback::FeedbackHandleRequestV1;
    pub type Result = tracedecay_application::feedback::FeedbackExpandResultV1;
}
typed_operation!(
    FeedbackExpand,
    feedback_expand,
    "operation.application.feedback_expand",
    OperationTransport::McpTool {
        tool_name: "tracedecay_feedback_expand"
    },
    "binding.mcp.feedback_expand.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.expand.result",
    1
);

pub mod feedback_get {
    pub type Request = tracedecay_application::feedback::FeedbackHandleRequestV1;
    pub type Result = tracedecay_application::feedback::FeedbackGetResultV1;
}
typed_operation!(
    FeedbackGet,
    feedback_get,
    "operation.application.feedback_get",
    OperationTransport::McpTool {
        tool_name: "tracedecay_feedback_get"
    },
    "binding.mcp.feedback_get.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.get.result",
    1
);

pub mod feedback_impact {
    pub type Request = tracedecay_application::feedback::FeedbackHandleRequestV1;
    pub type Result = tracedecay_application::feedback::CanonicalFeedbackImpactProjectionV1;
}
typed_operation!(
    FeedbackImpact,
    feedback_impact,
    "operation.application.feedback_impact",
    OperationTransport::McpTool {
        tool_name: "tracedecay_feedback_impact"
    },
    "binding.mcp.feedback_impact.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.impact.result",
    1
);

pub mod feedback_list {
    pub type Request = tracedecay_application::feedback::FeedbackHandleRequestV1;
    pub type Result = tracedecay_application::feedback::FeedbackListResultV1;
}
typed_operation!(
    FeedbackList,
    feedback_list,
    "operation.application.feedback_list",
    OperationTransport::McpTool {
        tool_name: "tracedecay_feedback_list"
    },
    "binding.mcp.feedback_list.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.list.result",
    1
);

pub mod file_dependents {
    pub type Request = tracedecay_application::retrieval::FileDependentsPrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::FileDependentsPrimitiveResult;
}
typed_operation!(
    FileDependents,
    file_dependents,
    "operation.application.file_dependents",
    OperationTransport::McpTool {
        tool_name: "tracedecay_file_dependents"
    },
    "binding.mcp.file_dependents.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.file-dependents.result",
    1
);

pub mod file_metadata {
    pub type Request = tracedecay_application::retrieval::FileMetadataPrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::FileMetadataPrimitiveResult;
}
typed_operation!(
    FileMetadata,
    file_metadata,
    "operation.application.file_metadata",
    OperationTransport::McpTool {
        tool_name: "tracedecay_file_metadata"
    },
    "binding.mcp.file_metadata.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.file-metadata.result",
    1
);

pub mod git_apply {
    pub type Request = tracedecay_application::git::GitApplySurfaceRequest;
    pub type Result = tracedecay_domain::GitIndexTransactionReceiptV1;
}
typed_operation!(
    GitApply,
    git_apply,
    "operation.application.git_apply",
    OperationTransport::McpTool {
        tool_name: "tracedecay_git_apply"
    },
    "binding.mcp.git_apply.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.git.apply.result",
    1
);

pub mod git_blame {
    pub type Request = tracedecay_application::git::GitBlameSurfaceRequest;
    pub type Result = tracedecay_application::git::GitReadResultV1;
}
typed_operation!(
    GitBlame,
    git_blame,
    "operation.application.git_blame",
    OperationTransport::McpTool {
        tool_name: "tracedecay_git_blame"
    },
    "binding.mcp.git_blame.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.git.blame.result",
    1
);

pub mod git_diff {
    pub type Request = tracedecay_application::git::GitDiffSurfaceRequest;
    pub type Result = tracedecay_application::git::GitReadResultV1;
}
typed_operation!(
    GitDiff,
    git_diff,
    "operation.application.git_diff",
    OperationTransport::McpTool {
        tool_name: "tracedecay_git_diff"
    },
    "binding.mcp.git_diff.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.git.diff.result",
    1
);

pub mod git_history {
    pub type Request = tracedecay_application::git::GitHistorySurfaceRequest;
    pub type Result = tracedecay_application::git::GitReadResultV1;
}
typed_operation!(
    GitHistory,
    git_history,
    "operation.application.git_history",
    OperationTransport::McpTool {
        tool_name: "tracedecay_git_history"
    },
    "binding.mcp.git_history.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.git.history.result",
    1
);

pub mod git_hunks {
    pub type Request = tracedecay_application::git::GitHunksSurfaceRequest;
    pub type Result = tracedecay_application::git::GitReadResultV1;
}
typed_operation!(
    GitHunks,
    git_hunks,
    "operation.application.git_hunks",
    OperationTransport::McpTool {
        tool_name: "tracedecay_git_hunks"
    },
    "binding.mcp.git_hunks.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.git.hunks.result",
    1
);

pub mod git_preview {
    pub type Request = tracedecay_application::git::GitPreviewSurfaceRequest;
    pub type Result = tracedecay_domain::GitIndexPreviewV1;
}
typed_operation!(
    GitPreview,
    git_preview,
    "operation.application.git_preview",
    OperationTransport::McpTool {
        tool_name: "tracedecay_git_preview"
    },
    "binding.mcp.git_preview.v1",
    EffectClass::Preview,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.git.preview.result",
    1
);

pub mod git_status {
    pub type Request = tracedecay_application::git::GitStatusSurfaceRequest;
    pub type Result = tracedecay_application::git::GitReadResultV1;
}
typed_operation!(
    GitStatus,
    git_status,
    "operation.application.git_status",
    OperationTransport::McpTool {
        tool_name: "tracedecay_git_status"
    },
    "binding.mcp.git_status.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.git.status.result",
    1
);

pub mod health_delta {
    pub type Request = tracedecay_application::retrieval::HealthDeltaRequest;
    pub type Result = tracedecay_application::retrieval::HealthDeltaResult;
}
typed_operation!(
    HealthDelta,
    health_delta,
    "operation.application.health_delta",
    OperationTransport::McpTool {
        tool_name: "tracedecay_health_delta"
    },
    "binding.mcp.health_delta.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.health-delta.result",
    1
);

pub mod health_read {
    pub type Request = tracedecay_application::retrieval::HealthReadRequest;
    pub type Result = tracedecay_application::retrieval::HealthReadResult;
}
typed_operation!(
    HealthRead,
    health_read,
    "operation.application.health_read",
    OperationTransport::McpTool {
        tool_name: "tracedecay_health_read"
    },
    "binding.mcp.health_read.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.health-read.result",
    1
);

pub mod insert_at {
    pub type Request = tracedecay_application::source_edit::InsertAtSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    InsertAt,
    insert_at,
    "operation.application.insert_at",
    OperationTransport::McpTool {
        tool_name: "tracedecay_insert_at"
    },
    "binding.mcp.insert_at.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.insert-at.result",
    1
);

pub mod insert_at_symbol {
    pub type Request = tracedecay_application::source_edit::InsertAtSymbolSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    InsertAtSymbol,
    insert_at_symbol,
    "operation.application.insert_at_symbol",
    OperationTransport::McpTool {
        tool_name: "tracedecay_insert_at_symbol"
    },
    "binding.mcp.insert_at_symbol.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.insert-at-symbol.result",
    1
);

pub mod application_lcm_describe {
    pub type Request = tracedecay_application::retained_surfaces::LcmDescribeRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::LcmDescribeResultV1;
}
typed_operation!(
    ApplicationLcmDescribe,
    application_lcm_describe,
    "operation.application.lcm_describe",
    OperationTransport::Http {
        route: "/application/retained/lcm_describe"
    },
    "binding.http.lcm_describe.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.lcm-describe.result",
    1
);

pub mod application_lcm_doctor {
    pub type Request = tracedecay_application::retained_surfaces::LcmDoctorRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::LcmDoctorResultV1;
}
typed_operation!(
    ApplicationLcmDoctor,
    application_lcm_doctor,
    "operation.application.lcm_doctor",
    OperationTransport::Http {
        route: "/application/retained/lcm_doctor"
    },
    "binding.http.lcm_doctor.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.lcm-doctor.result",
    1
);

pub mod application_lcm_expand {
    pub type Request = tracedecay_application::retained_surfaces::LcmExpandRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::LcmExpandResultV1;
}
typed_operation!(
    ApplicationLcmExpand,
    application_lcm_expand,
    "operation.application.lcm_expand",
    OperationTransport::Http {
        route: "/application/retained/lcm_expand"
    },
    "binding.http.lcm_expand.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.lcm-expand.result",
    1
);

pub mod application_lcm_expand_query {
    pub type Request = tracedecay_application::retained_surfaces::LcmExpandQueryRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::LcmExpandQueryResultV1;
}
typed_operation!(
    ApplicationLcmExpandQuery,
    application_lcm_expand_query,
    "operation.application.lcm_expand_query",
    OperationTransport::Http {
        route: "/application/retained/lcm_expand_query"
    },
    "binding.http.lcm_expand_query.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.lcm-expand-query.result",
    1
);

pub mod application_lcm_grep {
    pub type Request = tracedecay_application::retained_surfaces::LcmGrepRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::LcmGrepResultV1;
}
typed_operation!(
    ApplicationLcmGrep,
    application_lcm_grep,
    "operation.application.lcm_grep",
    OperationTransport::Http {
        route: "/application/retained/lcm_grep"
    },
    "binding.http.lcm_grep.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.lcm-grep.result",
    1
);

pub mod application_lcm_load_session {
    pub type Request = tracedecay_application::retained_surfaces::LcmLoadSessionRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::LcmLoadSessionResultV1;
}
typed_operation!(
    ApplicationLcmLoadSession,
    application_lcm_load_session,
    "operation.application.lcm_load_session",
    OperationTransport::Http {
        route: "/application/retained/lcm_load_session"
    },
    "binding.http.lcm_load_session.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.lcm-load-session.result",
    1
);

pub mod application_lcm_status {
    pub type Request = tracedecay_application::retained_surfaces::LcmStatusRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::LcmStatusResultV1;
}
typed_operation!(
    ApplicationLcmStatus,
    application_lcm_status,
    "operation.application.lcm_status",
    OperationTransport::Http {
        route: "/application/retained/lcm_status"
    },
    "binding.http.lcm_status.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.lcm-status.result",
    1
);

pub mod application_memory_status {
    pub type Request = tracedecay_application::retained_surfaces::MemoryStatusRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::MemoryStatusResultV1;
}
typed_operation!(
    ApplicationMemoryStatus,
    application_memory_status,
    "operation.application.memory_status",
    OperationTransport::Http {
        route: "/application/retained/memory_status"
    },
    "binding.http.memory_status.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.memory-status.result",
    1
);

pub mod application_message_search {
    pub type Request = tracedecay_application::retained_surfaces::MessageSearchRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::MessageSearchResultV1;
}
typed_operation!(
    ApplicationMessageSearch,
    application_message_search,
    "operation.application.message_search",
    OperationTransport::Http {
        route: "/application/retained/message_search"
    },
    "binding.http.message_search.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.message-search.result",
    1
);

pub mod module_api {
    pub type Request = tracedecay_application::retrieval::ModuleApiPrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::ModuleApiPrimitiveResult;
}
typed_operation!(
    ModuleApi,
    module_api,
    "operation.application.module_api",
    OperationTransport::McpTool {
        tool_name: "tracedecay_module_api"
    },
    "binding.mcp.module_api.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.module-api.result",
    1
);

pub mod move_symbol {
    pub type Request = tracedecay_application::source_edit::MoveSymbolSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    MoveSymbol,
    move_symbol,
    "operation.application.move_symbol",
    OperationTransport::McpTool {
        tool_name: "tracedecay_move_symbol"
    },
    "binding.mcp.move_symbol.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.move-symbol.result",
    1
);

pub mod multi_str_replace {
    pub type Request = tracedecay_application::source_edit::MultiStrReplaceSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    MultiStrReplace,
    multi_str_replace,
    "operation.application.multi_str_replace",
    OperationTransport::McpTool {
        tool_name: "tracedecay_multi_str_replace"
    },
    "binding.mcp.multi_str_replace.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.multi-str-replace.result",
    1
);

pub mod native_integration_status {
    pub type Request = tracedecay_application::git::NativeIntegrationStatusSurfaceRequest;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    NativeIntegrationStatus,
    native_integration_status,
    "operation.application.native_integration_status",
    OperationTransport::McpTool {
        tool_name: "tracedecay_native_integration_status"
    },
    "binding.mcp.native_integration_status.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.native-integration.status.result",
    1
);

pub mod preflight_native_integration {
    pub type Request = tracedecay_application::git::NativeIntegrationPreflightSurfaceRequest;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    PreflightNativeIntegration,
    preflight_native_integration,
    "operation.application.preflight_native_integration",
    OperationTransport::McpTool {
        tool_name: "tracedecay_preflight_native_integration"
    },
    "binding.mcp.preflight_native_integration.v1",
    EffectClass::Preview,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.native-integration.preflight.result",
    1
);

pub mod qualified_name {
    pub type Request = tracedecay_application::retrieval::QualifiedNamePrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::QualifiedNamePrimitiveResult;
}
typed_operation!(
    QualifiedName,
    qualified_name,
    "operation.application.qualified_name",
    OperationTransport::McpTool {
        tool_name: "tracedecay_qualified_name"
    },
    "binding.mcp.qualified_name.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.qualified-name.result",
    1
);

pub mod rename_symbol {
    pub type Request = tracedecay_application::source_edit::RenameSymbolSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    RenameSymbol,
    rename_symbol,
    "operation.application.rename_symbol",
    OperationTransport::McpTool {
        tool_name: "tracedecay_rename_symbol"
    },
    "binding.mcp.rename_symbol.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.rename-symbol.result",
    1
);

pub mod replace_symbol {
    pub type Request = tracedecay_application::source_edit::ReplaceSymbolSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    ReplaceSymbol,
    replace_symbol,
    "operation.application.replace_symbol",
    OperationTransport::McpTool {
        tool_name: "tracedecay_replace_symbol"
    },
    "binding.mcp.replace_symbol.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.replace-symbol.result",
    1
);

pub mod application_semantic_embedding_import_configured_https {
    pub type Request =
        tracedecay_application::configuration::SemanticEmbeddingConfiguredHttpsImportRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticEmbeddingImportConfiguredHttps,
    application_semantic_embedding_import_configured_https,
    "operation.application.semantic_embedding_import_configured_https",
    OperationTransport::Http {
        route: "/application/configuration/semantic_embedding_import_configured_https"
    },
    "binding.http.semantic_embedding_import_configured_https.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight
    ],
    900000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_embedding_import_configured_https.result",
    1
);

pub mod application_semantic_embedding_import_local {
    pub type Request = tracedecay_application::configuration::SemanticEmbeddingLocalImportRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticEmbeddingImportLocal,
    application_semantic_embedding_import_local,
    "operation.application.semantic_embedding_import_local",
    OperationTransport::Http {
        route: "/application/configuration/semantic_embedding_import_local"
    },
    "binding.http.semantic_embedding_import_local.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight
    ],
    900000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_embedding_import_local.result",
    1
);

pub mod application_semantic_model_remove {
    pub type Request = tracedecay_application::configuration::SemanticLifecycleControlRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticModelRemove,
    application_semantic_model_remove,
    "operation.application.semantic_model_remove",
    OperationTransport::Http {
        route: "/application/configuration/semantic_model_remove"
    },
    "binding.http.semantic_model_remove.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_model_remove.result",
    1
);

pub mod application_semantic_model_retry {
    pub type Request = tracedecay_application::configuration::SemanticLifecycleControlRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticModelRetry,
    application_semantic_model_retry,
    "operation.application.semantic_model_retry",
    OperationTransport::Http {
        route: "/application/configuration/semantic_model_retry"
    },
    "binding.http.semantic_model_retry.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_model_retry.result",
    1
);

pub mod application_semantic_model_rollback {
    pub type Request = tracedecay_application::configuration::SemanticLifecycleControlRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticModelRollback,
    application_semantic_model_rollback,
    "operation.application.semantic_model_rollback",
    OperationTransport::Http {
        route: "/application/configuration/semantic_model_rollback"
    },
    "binding.http.semantic_model_rollback.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_model_rollback.result",
    1
);

pub mod application_semantic_reranker_import_configured_https {
    pub type Request =
        tracedecay_application::configuration::SemanticRerankerConfiguredHttpsImportRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticRerankerImportConfiguredHttps,
    application_semantic_reranker_import_configured_https,
    "operation.application.semantic_reranker_import_configured_https",
    OperationTransport::Http {
        route: "/application/configuration/semantic_reranker_import_configured_https"
    },
    "binding.http.semantic_reranker_import_configured_https.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight
    ],
    900000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_reranker_import_configured_https.result",
    1
);

pub mod application_semantic_reranker_import_local {
    pub type Request = tracedecay_application::configuration::SemanticRerankerLocalImportRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticRerankerImportLocal,
    application_semantic_reranker_import_local,
    "operation.application.semantic_reranker_import_local",
    OperationTransport::Http {
        route: "/application/configuration/semantic_reranker_import_local"
    },
    "binding.http.semantic_reranker_import_local.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight
    ],
    900000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_reranker_import_local.result",
    1
);

pub mod application_semantic_reranker_rollback {
    pub type Request = tracedecay_application::configuration::SemanticRerankerRollbackRequestV1;
    pub type Result = tracedecay_application::configuration::SemanticLifecycleOperationResultV1;
}
typed_operation!(
    ApplicationSemanticRerankerRollback,
    application_semantic_reranker_rollback,
    "operation.application.semantic_reranker_rollback",
    OperationTransport::Http {
        route: "/application/configuration/semantic_reranker_rollback"
    },
    "binding.http.semantic_reranker_rollback.v1",
    EffectClass::ConfigurationWrite,
    IdempotencyContract::Required,
    false,
    &[],
    15000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.configuration.semantic_reranker_rollback.result",
    1
);

pub mod session_lookup {
    pub type Request = tracedecay_application::retrieval::SessionLookupRequest;
    pub type Result = tracedecay_application::retrieval::SessionLookupResult;
}
typed_operation!(
    SessionLookup,
    session_lookup,
    "operation.application.session_lookup",
    OperationTransport::McpTool {
        tool_name: "tracedecay_session_lookup"
    },
    "binding.mcp.session_lookup.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.session-lookup.result",
    1
);

pub mod application_session_refresh_begin {
    pub type Request = tracedecay_application::retained_surfaces::SessionRefreshActionRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::SessionRefreshBeginResultV1;
}
typed_operation!(
    ApplicationSessionRefreshBegin,
    application_session_refresh_begin,
    "operation.application.session_refresh_begin",
    OperationTransport::Http {
        route: "/application/retained/session_refresh_begin"
    },
    "binding.http.session_refresh_begin.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::Reconciling,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.retained.session-refresh-begin.result",
    1
);

pub mod application_session_refresh_cancel {
    pub type Request = tracedecay_application::retained_surfaces::SessionRefreshActionRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::SessionRefreshCancelResultV1;
}
typed_operation!(
    ApplicationSessionRefreshCancel,
    application_session_refresh_cancel,
    "operation.application.session_refresh_cancel",
    OperationTransport::Http {
        route: "/application/retained/session_refresh_cancel"
    },
    "binding.http.session_refresh_cancel.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::Reconciling,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.retained.session-refresh-cancel.result",
    1
);

pub mod application_session_refresh_status {
    pub type Request = tracedecay_application::retained_surfaces::SessionRefreshActionRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::SessionRefreshStatusResultV1;
}
typed_operation!(
    ApplicationSessionRefreshStatus,
    application_session_refresh_status,
    "operation.application.session_refresh_status",
    OperationTransport::Http {
        route: "/application/retained/session_refresh_status"
    },
    "binding.http.session_refresh_status.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.session-refresh-status.result",
    1
);

pub mod application_sessions_for {
    pub type Request = tracedecay_application::retained_surfaces::SessionsForRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::SessionsForResultV1;
}
typed_operation!(
    ApplicationSessionsFor,
    application_sessions_for,
    "operation.application.sessions_for",
    OperationTransport::Http {
        route: "/application/retained/sessions_for"
    },
    "binding.http.sessions_for.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.sessions-for.result",
    1
);

pub mod source_body {
    pub type Request = tracedecay_application::retrieval::SourceBodyPrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::SourceBodyPrimitiveResult;
}
typed_operation!(
    SourceBody,
    source_body,
    "operation.application.source_body",
    OperationTransport::McpTool {
        tool_name: "tracedecay_source_body"
    },
    "binding.mcp.source_body.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.source-body.result",
    1
);

pub mod source_edit_reconcile {
    pub type Request = tracedecay_application::source_edit::SourceEditReconcileSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    SourceEditReconcile,
    source_edit_reconcile,
    "operation.application.source_edit_reconcile",
    OperationTransport::McpTool {
        tool_name: "tracedecay_source_edit_reconcile"
    },
    "binding.mcp.source-edit-reconcile.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.reconcile.result",
    1
);

pub mod source_edit_rollback {
    pub type Request = tracedecay_application::source_edit::SourceEditRollbackSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    SourceEditRollback,
    source_edit_rollback,
    "operation.application.source_edit_rollback",
    OperationTransport::McpTool {
        tool_name: "tracedecay_source_edit_rollback"
    },
    "binding.mcp.source-edit-rollback.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.rollback.result",
    1
);

pub mod source_lines {
    pub type Request = tracedecay_application::retrieval::SourceLinesRequest;
    pub type Result = tracedecay_application::retrieval::SourceLinesResult;
}
typed_operation!(
    SourceLines,
    source_lines,
    "operation.application.source_lines",
    OperationTransport::McpTool {
        tool_name: "tracedecay_source_lines"
    },
    "binding.mcp.source_lines.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.source-lines.result",
    1
);

pub mod source_outline {
    pub type Request = tracedecay_application::retrieval::SourceOutlinePrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::SourceOutlinePrimitiveResult;
}
typed_operation!(
    SourceOutline,
    source_outline,
    "operation.application.source_outline",
    OperationTransport::McpTool {
        tool_name: "tracedecay_source_outline"
    },
    "binding.mcp.source_outline.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.source-outline.result",
    1
);

pub mod stack_snapshot {
    pub type Request = tracedecay_application::git::NativeIntegrationStackSnapshotSurfaceRequest;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    StackSnapshot,
    stack_snapshot,
    "operation.application.stack_snapshot",
    OperationTransport::McpTool {
        tool_name: "tracedecay_stack_snapshot"
    },
    "binding.mcp.stack_snapshot.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.native-integration.stack-snapshot.result",
    1
);

pub mod storage_status {
    pub type Request = tracedecay_application::retrieval::StorageStatusPrimitiveRequest;
    pub type Result = tracedecay_application::retrieval::StorageStatusPrimitiveResult;
}
typed_operation!(
    StorageStatus,
    storage_status,
    "operation.application.storage_status",
    OperationTransport::McpTool {
        tool_name: "tracedecay_storage_status"
    },
    "binding.mcp.storage_status.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    10000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.primitive.storage-status.result",
    1
);

pub mod str_replace {
    pub type Request = tracedecay_application::source_edit::StrReplaceSurfaceRequestV1;
    pub type Result = tracedecay_application::source_edit::SourceEditSurfaceResultV1;
}
typed_operation!(
    StrReplace,
    str_replace,
    "operation.application.str_replace",
    OperationTransport::McpTool {
        tool_name: "tracedecay_str_replace"
    },
    "binding.mcp.str_replace.v1",
    EffectClass::SourceEdit,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.source-edit.str-replace.result",
    1
);

pub mod test_results {
    pub type Request = tracedecay_application::feedback::TestResultsSurfaceRequestV1;
    pub type Result = tracedecay_application::feedback::TestResultsResultV1;
}
typed_operation!(
    TestResults,
    test_results,
    "operation.application.test_results",
    OperationTransport::McpTool {
        tool_name: "tracedecay_test_results"
    },
    "binding.mcp.test_results.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    15000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.feedback.test-results.result",
    1
);

pub mod application_workflows {
    pub type Request = tracedecay_application::retained_surfaces::WorkflowsRequestV1;
    pub type Result = tracedecay_application::retained_surfaces::WorkflowsResultV1;
}
typed_operation!(
    ApplicationWorkflows,
    application_workflows,
    "operation.application.workflows",
    OperationTransport::Http {
        route: "/application/retained/workflows"
    },
    "binding.http.workflows.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Unavailable,
        TerminalState::Partial
    ],
    "schema.application.retained.workflows.result",
    1
);

pub mod worktree_cleanup_confirm {
    pub type Request = tracedecay_application::git::WorktreeCleanupConfirmRequestV1;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    WorktreeCleanupConfirm,
    worktree_cleanup_confirm,
    "operation.application.worktree_cleanup_confirm",
    OperationTransport::McpTool {
        tool_name: "tracedecay_worktree_cleanup_confirm"
    },
    "binding.mcp.worktree_cleanup_confirm.v1",
    EffectClass::Preview,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.native-integration.worktree-cleanup-confirm.result",
    1
);

pub mod worktree_cleanup_inspect {
    pub type Request = tracedecay_application::git::WorktreeCleanupInspectRequestV1;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    WorktreeCleanupInspect,
    worktree_cleanup_inspect,
    "operation.application.worktree_cleanup_inspect",
    OperationTransport::McpTool {
        tool_name: "tracedecay_worktree_cleanup_inspect"
    },
    "binding.mcp.worktree_cleanup_inspect.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.native-integration.worktree-cleanup-inspect.result",
    1
);

pub mod worktree_cleanup_reconcile {
    pub type Request = tracedecay_application::git::WorktreeCleanupReconcileRequestV1;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    WorktreeCleanupReconcile,
    worktree_cleanup_reconcile,
    "operation.application.worktree_cleanup_reconcile",
    OperationTransport::McpTool {
        tool_name: "tracedecay_worktree_cleanup_reconcile"
    },
    "binding.mcp.worktree_cleanup_reconcile.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.native-integration.worktree-cleanup-reconcile.result",
    1
);

pub mod worktree_cleanup_remove {
    pub type Request = tracedecay_application::git::WorktreeCleanupRemoveRequestV1;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    WorktreeCleanupRemove,
    worktree_cleanup_remove,
    "operation.application.worktree_cleanup_remove",
    OperationTransport::McpTool {
        tool_name: "tracedecay_worktree_cleanup_remove"
    },
    "binding.mcp.worktree_cleanup_remove.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.application.native-integration.worktree-cleanup-remove.result",
    1
);

pub mod worktree_inventory {
    pub type Request = tracedecay_application::git::WorktreeInventoryRequestV1;
    pub type Result = tracedecay_application::git::NativeIntegrationSurfaceResultV1;
}
typed_operation!(
    WorktreeInventory,
    worktree_inventory,
    "operation.application.worktree_inventory",
    OperationTransport::McpTool {
        tool_name: "tracedecay_worktree_inventory"
    },
    "binding.mcp.worktree_inventory.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.application.native-integration.worktree-inventory.result",
    1
);

pub mod handoff_issue_task_handoff {
    pub type Request = tracedecay_application::handoff::IssueTaskHandoffRequestV1;
    pub type Result = tracedecay_application::handoff::IssueTaskHandoffResultV1;
}
typed_operation!(
    HandoffIssueTaskHandoff,
    handoff_issue_task_handoff,
    "operation.handoff.issue_task_handoff",
    OperationTransport::Http {
        route: "/application/handoff/issue-task"
    },
    "binding.http.handoff.issue_task_handoff",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.handoff.issue_task_handoff.result",
    1
);

pub mod handoff_open_investigation_handoff {
    pub type Request = tracedecay_application::handoff::OpenInvestigationHandoffRequestV1;
    pub type Result = tracedecay_application::handoff::OpenInvestigationHandoffResultV1;
}
typed_operation!(
    HandoffOpenInvestigationHandoff,
    handoff_open_investigation_handoff,
    "operation.handoff.open_investigation_handoff",
    OperationTransport::Http {
        route: "/application/handoff/open-investigation"
    },
    "binding.http.handoff.open_investigation_handoff",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.handoff.open_investigation_handoff.result",
    1
);

pub mod handoff_open_task_handoff {
    pub type Request = tracedecay_application::handoff::OpenTaskHandoffRequestV1;
    pub type Result = tracedecay_application::handoff::OpenTaskHandoffResultV1;
}
typed_operation!(
    HandoffOpenTaskHandoff,
    handoff_open_task_handoff,
    "operation.handoff.open_task_handoff",
    OperationTransport::Http {
        route: "/application/handoff/open-task"
    },
    "binding.http.handoff.open_task_handoff",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.handoff.open_task_handoff.result",
    1
);

pub mod multi_root_execute {
    pub type Request = tracedecay_application::multi_root::MultiRootExecuteRequestV1;
    pub type Result = tracedecay_application::multi_root::MultiRootQueryPageV1<serde_json::Value>;
}
typed_operation!(
    MultiRootExecute,
    multi_root_execute,
    "operation.multi_root.execute",
    OperationTransport::Http {
        route: "/multi-root/execute"
    },
    "binding.http.multi_root.execute.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.tracedecay.multi-root.execute-result.v1",
    1
);

pub mod multi_root_scope_set_compare_and_swap {
    pub type Request = tracedecay_application::multi_root::MultiRootScopeSetCasRequestV1;
    pub type Result = tracedecay_application::multi_root::MultiRootScopeSetCasResultV1;
}
typed_operation!(
    MultiRootScopeSetCompareAndSwap,
    multi_root_scope_set_compare_and_swap,
    "operation.multi_root.scope_set_compare_and_swap",
    OperationTransport::Http {
        route: "/multi-root/scope-set/compare-and-swap"
    },
    "binding.http.multi_root.scope_set_compare_and_swap.v1",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.tracedecay.multi-root.scope-set-compare-and-swap-result.v1",
    1
);

pub mod multi_root_scope_set_read {
    pub type Request = tracedecay_application::multi_root::MultiRootScopeSetReadRequestV1;
    pub type Result = core::option::Option<tracedecay_application::multi_root::AuthorizedScopeSet>;
}
typed_operation!(
    MultiRootScopeSetRead,
    multi_root_scope_set_read,
    "operation.multi_root.scope_set_read",
    OperationTransport::Http {
        route: "/multi-root/scope-set/read"
    },
    "binding.http.multi_root.scope_set_read.v1",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.tracedecay.multi-root.scope-set-read-result.v1",
    1
);

pub mod work_accept_proposal {
    pub type Request = tracedecay_application::AcceptProposalCommand;
    pub type Result = tracedecay_domain::WorkProjection;
}
typed_operation!(
    WorkAcceptProposal,
    work_accept_proposal,
    "operation.work.accept_proposal",
    OperationTransport::Http {
        route: "/application/work/accept-proposal"
    },
    "binding.http.work.accept_proposal",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.accept_proposal.result",
    1
);

pub mod work_accept_task {
    pub type Request = tracedecay_application::AcceptTaskCommand;
    pub type Result = tracedecay_domain::WorkProjection;
}
typed_operation!(
    WorkAcceptTask,
    work_accept_task,
    "operation.work.accept_task",
    OperationTransport::Http {
        route: "/application/work/accept-task"
    },
    "binding.http.work.accept_task",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.accept_task.result",
    1
);

pub mod work_adjudicate_duplicate {
    pub type Request = tracedecay_domain::WorkDuplicateAdjudicationCommandV1;
    pub type Result = tracedecay_application::WorkDuplicateAdjudicationAppendOutcomeV1;
}
typed_operation!(
    WorkAdjudicateDuplicate,
    work_adjudicate_duplicate,
    "operation.work.adjudicate_duplicate",
    OperationTransport::Http {
        route: "/application/work/adjudicate-duplicate"
    },
    "binding.http.work.adjudicate_duplicate",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.adjudicate_duplicate.result",
    1
);

pub mod work_adjudicate_leak {
    pub type Request = tracedecay_application::AdjudicateWorkLeakCommandV1;
    pub type Result = tracedecay_application::WorkLeakAdjudicationOutcomeV1;
}
typed_operation!(
    WorkAdjudicateLeak,
    work_adjudicate_leak,
    "operation.work.adjudicate_leak",
    OperationTransport::Http {
        route: "/application/work/adjudicate-leak"
    },
    "binding.http.work.adjudicate_leak",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.adjudicate_leak.result",
    1
);

pub mod work_admit_execution {
    pub type Request = tracedecay_application::AdmitExecutionCommand;
    pub type Result = tracedecay_domain::WorkProjection;
}
typed_operation!(
    WorkAdmitExecution,
    work_admit_execution,
    "operation.work.admit_execution",
    OperationTransport::Http {
        route: "/application/work/admit-execution"
    },
    "binding.http.work.admit_execution",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.admit_execution.result",
    1
);

pub mod work_admit_placement {
    pub type Request = tracedecay_application::AdmitWorkPlacementCommand;
    pub type Result = tracedecay_domain::WorkPlacementV1;
}
typed_operation!(
    WorkAdmitPlacement,
    work_admit_placement,
    "operation.work.admit_placement",
    OperationTransport::Http {
        route: "/application/work/admit-placement"
    },
    "binding.http.work.admit_placement",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.admit_placement.result",
    1
);

pub mod work_attach_runtime_evidence {
    pub type Request = tracedecay_application::AttachRuntimeEvidenceCommand;
    pub type Result = tracedecay_domain::WorkProjection;
}
typed_operation!(
    WorkAttachRuntimeEvidence,
    work_attach_runtime_evidence,
    "operation.work.attach_runtime_evidence",
    OperationTransport::Http {
        route: "/application/work/attach-runtime-evidence"
    },
    "binding.http.work.attach_runtime_evidence",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.attach_runtime_evidence.result",
    1
);

pub mod work_attempt_status {
    pub type Request = tracedecay_application::WorkAttemptStatusRequestV1;
    pub type Result = tracedecay_domain::WorkAttemptV1;
}
typed_operation!(
    WorkAttemptStatus,
    work_attempt_status,
    "operation.work.attempt_status",
    OperationTransport::Http {
        route: "/application/work/attempt-status"
    },
    "binding.http.work.attempt_status",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.attempt_status.result",
    1
);

pub mod work_cancel_attempt {
    pub type Request = tracedecay_application::CancelWorkAttemptCommand;
    pub type Result = tracedecay_domain::WorkAttemptV1;
}
typed_operation!(
    WorkCancelAttempt,
    work_cancel_attempt,
    "operation.work.cancel_attempt",
    OperationTransport::Http {
        route: "/application/work/cancel-attempt"
    },
    "binding.http.work.cancel_attempt",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.cancel_attempt.result",
    1
);

pub mod work_create {
    pub type Request = tracedecay_application::CreateWorkCommand;
    pub type Result = tracedecay_domain::WorkProjection;
}
typed_operation!(
    WorkCreate,
    work_create,
    "operation.work.create",
    OperationTransport::Http {
        route: "/application/work/create"
    },
    "binding.http.work.create",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.create.result",
    1
);

pub mod work_delta {
    pub type Request = tracedecay_application::WorkProjectionDeltaRequestV1;
    pub type Result = tracedecay_domain::WorkProjectionDeltaV1;
}
typed_operation!(
    WorkDelta,
    work_delta,
    "operation.work.delta",
    OperationTransport::Http {
        route: "/application/work/delta"
    },
    "binding.http.work.delta",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.delta.result",
    1
);

pub mod work_generate_proposal {
    pub type Request = tracedecay_application::GenerateProposalRequest;
    pub type Result = tracedecay_application::GeneratedWorkProposal;
}
typed_operation!(
    WorkGenerateProposal,
    work_generate_proposal,
    "operation.work.generate_proposal",
    OperationTransport::Http {
        route: "/application/work/generate-proposal"
    },
    "binding.http.work.generate_proposal",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.generate_proposal.result",
    1
);

pub mod work_hydrate_artifacts {
    pub type Request = tracedecay_application::WorkArtifactHydrationRequestV1;
    pub type Result = tracedecay_application::WorkArtifactHydrationV1;
}
typed_operation!(
    WorkHydrateArtifacts,
    work_hydrate_artifacts,
    "operation.work.hydrate_artifacts",
    OperationTransport::Http {
        route: "/application/work/hydrate-artifacts"
    },
    "binding.http.work.hydrate_artifacts",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.hydrate_artifacts.result",
    1
);

pub mod work_list_attempts {
    pub type Request = tracedecay_application::WorkAttemptListRequestV1;
    pub type Result = tracedecay_application::WorkAttemptListV1;
}
typed_operation!(
    WorkListAttempts,
    work_list_attempts,
    "operation.work.list_attempts",
    OperationTransport::Http {
        route: "/application/work/list-attempts"
    },
    "binding.http.work.list_attempts",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.list_attempts.result",
    1
);

pub mod work_mint_retry_test_binding {
    pub type Request = tracedecay_application::WorkRetryTestBindingTokenRequestV1;
    pub type Result = tracedecay_application::WorkRetryTestBindingTokenOutcomeV1;
}
typed_operation!(
    WorkMintRetryTestBinding,
    work_mint_retry_test_binding,
    "operation.work.mint_retry_test_binding",
    OperationTransport::Http {
        route: "/application/work/mint-retry-test-binding"
    },
    "binding.http.work.mint_retry_test_binding",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.mint_retry_test_binding.result",
    1
);

pub mod work_mutate_graph {
    pub type Request = tracedecay_application::WorkProductMutationRequestV1;
    pub type Result = tracedecay_application::WorkProductMutationReceiptV1;
}
typed_operation!(
    WorkMutateGraph,
    work_mutate_graph,
    "operation.work.mutate_graph",
    OperationTransport::Http {
        route: "/application/work/mutate-graph"
    },
    "binding.http.work.mutate_graph",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.mutate_graph.result",
    1
);

pub mod work_pause_run {
    pub type Request = tracedecay_application::PauseWorkRunCommand;
    pub type Result = tracedecay_domain::WorkRunControlV1;
}
typed_operation!(
    WorkPauseRun,
    work_pause_run,
    "operation.work.pause_run",
    OperationTransport::Http {
        route: "/application/work/pause-run"
    },
    "binding.http.work.pause_run",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.pause_run.result",
    1
);

pub mod work_placement_preflight {
    pub type Request = tracedecay_application::WorkPlacementPreflightRequestV1;
    pub type Result = tracedecay_domain::WorkPlacementPreflightV1;
}
typed_operation!(
    WorkPlacementPreflight,
    work_placement_preflight,
    "operation.work.placement_preflight",
    OperationTransport::Http {
        route: "/application/work/placement-preflight"
    },
    "binding.http.work.placement_preflight",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.placement_preflight.result",
    1
);

pub mod work_placement_status {
    pub type Request = tracedecay_application::WorkPlacementStatusRequestV1;
    pub type Result = tracedecay_application::WorkPlacementReadingV1;
}
typed_operation!(
    WorkPlacementStatus,
    work_placement_status,
    "operation.work.placement_status",
    OperationTransport::Http {
        route: "/application/work/placement-status"
    },
    "binding.http.work.placement_status",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.placement_status.result",
    1
);

pub mod work_release_placement {
    pub type Request = tracedecay_application::ReleaseWorkPlacementCommand;
    pub type Result = tracedecay_domain::WorkPlacementV1;
}
typed_operation!(
    WorkReleasePlacement,
    work_release_placement,
    "operation.work.release_placement",
    OperationTransport::Http {
        route: "/application/work/release-placement"
    },
    "binding.http.work.release_placement",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.release_placement.result",
    1
);

pub mod work_replan_dependencies {
    pub type Request = tracedecay_application::ReplanDependenciesCommand;
    pub type Result = tracedecay_domain::WorkProjection;
}
typed_operation!(
    WorkReplanDependencies,
    work_replan_dependencies,
    "operation.work.replan_dependencies",
    OperationTransport::Http {
        route: "/application/work/replan-dependencies"
    },
    "binding.http.work.replan_dependencies",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.replan_dependencies.result",
    1
);

pub mod work_resume_attempts {
    pub type Request = tracedecay_application::ResumeWorkAttemptsCommand;
    pub type Result = tracedecay_application::WorkAttemptRecoveryReportV1;
}
typed_operation!(
    WorkResumeAttempts,
    work_resume_attempts,
    "operation.work.resume_attempts",
    OperationTransport::Http {
        route: "/application/work/resume-attempts"
    },
    "binding.http.work.resume_attempts",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.resume_attempts.result",
    1
);

pub mod work_resume_run {
    pub type Request = tracedecay_application::ResumeWorkRunCommand;
    pub type Result = tracedecay_domain::WorkRunControlV1;
}
typed_operation!(
    WorkResumeRun,
    work_resume_run,
    "operation.work.resume_run",
    OperationTransport::Http {
        route: "/application/work/resume-run"
    },
    "binding.http.work.resume_run",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.resume_run.result",
    1
);

pub mod work_retry_attempt {
    pub type Request = tracedecay_application::RetryWorkAttemptCommandV1;
    pub type Result = tracedecay_application::WorkRetryAttemptOutcomeV1;
}
typed_operation!(
    WorkRetryAttempt,
    work_retry_attempt,
    "operation.work.retry_attempt",
    OperationTransport::Http {
        route: "/application/work/retry-attempt"
    },
    "binding.http.work.retry_attempt",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.retry_attempt.result",
    1
);

pub mod work_review_proposal {
    pub type Request = tracedecay_application::ReviewProposalRequestV1;
    pub type Result = tracedecay_domain::WorkProjection;
}
typed_operation!(
    WorkReviewProposal,
    work_review_proposal,
    "operation.work.review_proposal",
    OperationTransport::Http {
        route: "/application/work/review-proposal"
    },
    "binding.http.work.review_proposal",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.review_proposal.result",
    1
);

pub mod work_run_control {
    pub type Request = tracedecay_application::WorkRunControlRequestV1;
    pub type Result = tracedecay_application::WorkRunControlReadingV1;
}
typed_operation!(
    WorkRunControl,
    work_run_control,
    "operation.work.run_control",
    OperationTransport::Http {
        route: "/application/work/run-control"
    },
    "binding.http.work.run_control",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.run_control.result",
    1
);

pub mod work_snapshot {
    pub type Request = tracedecay_application::WorkProjectionSnapshotRequestV1;
    pub type Result = tracedecay_domain::WorkProjectionSnapshotV1;
}
typed_operation!(
    WorkSnapshot,
    work_snapshot,
    "operation.work.snapshot",
    OperationTransport::Http {
        route: "/application/work/snapshot"
    },
    "binding.http.work.snapshot",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.snapshot.result",
    1
);

pub mod work_start_attempt {
    pub type Request = tracedecay_application::StartWorkAttemptCommand;
    pub type Result = tracedecay_domain::WorkAttemptV1;
}
typed_operation!(
    WorkStartAttempt,
    work_start_attempt,
    "operation.work.start_attempt",
    OperationTransport::Http {
        route: "/application/work/start-attempt"
    },
    "binding.http.work.start_attempt",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.start_attempt.result",
    1
);

pub mod work_synthesize {
    pub type Request = tracedecay_application::AdmitWorkSynthesisCommand;
    pub type Result = tracedecay_application::WorkSynthesisAttemptV1;
}
typed_operation!(
    WorkSynthesize,
    work_synthesize,
    "operation.work.synthesize",
    OperationTransport::Http {
        route: "/application/work/synthesize"
    },
    "binding.http.work.synthesize",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeEffect,
        CancellationPoint::EffectInFlight,
        CancellationPoint::AfterCommit
    ],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.work.synthesize.result",
    1
);

pub mod work_topology {
    pub type Request = tracedecay_application::WorkTopologyViewRequestV1;
    pub type Result = tracedecay_application::ExecutionTopologyViewV1;
}
typed_operation!(
    WorkTopology,
    work_topology,
    "operation.work.topology",
    OperationTransport::Http {
        route: "/application/work/topology"
    },
    "binding.http.work.topology",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.topology.result",
    1
);

pub mod work_topology_metrics {
    pub type Request = tracedecay_application::ExecutionTopologyMetricsRequestV1;
    pub type Result = tracedecay_application::ExecutionTopologyMetricsV1;
}
typed_operation!(
    WorkTopologyMetrics,
    work_topology_metrics,
    "operation.work.topology_metrics",
    OperationTransport::Http {
        route: "/application/work/topology-metrics"
    },
    "binding.http.work.topology_metrics",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.topology_metrics.result",
    1
);

pub mod work_views {
    pub type Request = tracedecay_application::WorkGraphReadRequestV1;
    pub type Result = tracedecay_application::WorkGraphReadV1;
}
typed_operation!(
    WorkViews,
    work_views,
    "operation.work.views",
    OperationTransport::Http {
        route: "/application/work/views"
    },
    "binding.http.work.views",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.work.views.result",
    1
);

pub mod workflow_activate_definition {
    pub type Request = tracedecay_application::WorkflowDefinitionActivateRequest;
    pub type Result = tracedecay_application::WorkflowDefinitionDisposition;
}
typed_operation!(
    WorkflowActivateDefinition,
    workflow_activate_definition,
    "operation.workflow.activate_definition",
    OperationTransport::Http {
        route: "/application/workflow/activate-definition"
    },
    "binding.http.workflow.activate_definition",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.activate_definition.result",
    1
);

pub mod workflow_cancel_run {
    pub type Request = tracedecay_application::WorkflowRunCancelRequest;
    pub type Result = tracedecay_domain::WorkflowRunProjection;
}
typed_operation!(
    WorkflowCancelRun,
    workflow_cancel_run,
    "operation.workflow.cancel_run",
    OperationTransport::Http {
        route: "/application/workflow/cancel-run"
    },
    "binding.http.workflow.cancel_run",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.cancel_run.result",
    1
);

pub mod workflow_definition_history {
    extern crate alloc;
    pub type Request = tracedecay_application::WorkflowDefinitionHistoryRequest;
    pub type Result = alloc::vec::Vec<tracedecay_domain::WorkflowDefinition>;
}
typed_operation!(
    WorkflowDefinitionHistory,
    workflow_definition_history,
    "operation.workflow.definition_history",
    OperationTransport::Http {
        route: "/application/workflow/definition-history"
    },
    "binding.http.workflow.definition_history",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.workflow.definition_history.result",
    1
);

pub mod workflow_diff_definition {
    pub type Request = tracedecay_application::WorkflowDefinitionDiffRequest;
    pub type Result = tracedecay_application::WorkflowDefinitionDiff;
}
typed_operation!(
    WorkflowDiffDefinition,
    workflow_diff_definition,
    "operation.workflow.diff_definition",
    OperationTransport::Http {
        route: "/application/workflow/diff-definition"
    },
    "binding.http.workflow.diff_definition",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.workflow.diff_definition.result",
    1
);

pub mod workflow_get_definition {
    pub type Request = tracedecay_application::WorkflowDefinitionGetRequest;
    pub type Result = tracedecay_domain::WorkflowDefinition;
}
typed_operation!(
    WorkflowGetDefinition,
    workflow_get_definition,
    "operation.workflow.get_definition",
    OperationTransport::Http {
        route: "/application/workflow/get-definition"
    },
    "binding.http.workflow.get_definition",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.workflow.get_definition.result",
    1
);

pub mod workflow_get_run {
    pub type Request = tracedecay_application::WorkflowRunGetRequest;
    pub type Result = tracedecay_domain::WorkflowRunProjection;
}
typed_operation!(
    WorkflowGetRun,
    workflow_get_run,
    "operation.workflow.get_run",
    OperationTransport::Http {
        route: "/application/workflow/get-run"
    },
    "binding.http.workflow.get_run",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.workflow.get_run.result",
    1
);

pub mod workflow_handoff_issue {
    pub type Request = tracedecay_application::TaskHandoffIssueRequest;
    pub type Result = tracedecay_application::TaskHandoffGrant;
}
typed_operation!(
    WorkflowHandoffIssue,
    workflow_handoff_issue,
    "operation.workflow.handoff_issue",
    OperationTransport::Http {
        route: "/application/workflow/handoff-issue"
    },
    "binding.http.workflow.handoff_issue",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.handoff_issue.result",
    1
);

pub mod workflow_handoff_redeem {
    pub type Request = tracedecay_application::TaskHandoffRedeemRequest;
    pub type Result = tracedecay_application::TaskHandoffRedeemed;
}
typed_operation!(
    WorkflowHandoffRedeem,
    workflow_handoff_redeem,
    "operation.workflow.handoff_redeem",
    OperationTransport::Http {
        route: "/application/workflow/handoff-redeem"
    },
    "binding.http.workflow.handoff_redeem",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.handoff_redeem.result",
    1
);

pub mod workflow_list_definitions {
    extern crate alloc;
    pub type Request = tracedecay_application::WorkflowDefinitionListRequest;
    pub type Result = alloc::vec::Vec<tracedecay_domain::WorkflowDefinition>;
}
typed_operation!(
    WorkflowListDefinitions,
    workflow_list_definitions,
    "operation.workflow.list_definitions",
    OperationTransport::Http {
        route: "/application/workflow/list-definitions"
    },
    "binding.http.workflow.list_definitions",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.workflow.list_definitions.result",
    1
);

pub mod workflow_pause_run {
    pub type Request = tracedecay_application::WorkflowRunPauseRequest;
    pub type Result = tracedecay_domain::WorkflowRunProjection;
}
typed_operation!(
    WorkflowPauseRun,
    workflow_pause_run,
    "operation.workflow.pause_run",
    OperationTransport::Http {
        route: "/application/workflow/pause-run"
    },
    "binding.http.workflow.pause_run",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.pause_run.result",
    1
);

pub mod workflow_register_definition {
    pub type Request = tracedecay_application::WorkflowDefinitionRegisterRequest;
    pub type Result = tracedecay_domain::WorkflowDefinition;
}
typed_operation!(
    WorkflowRegisterDefinition,
    workflow_register_definition,
    "operation.workflow.register_definition",
    OperationTransport::Http {
        route: "/application/workflow/register-definition"
    },
    "binding.http.workflow.register_definition",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.register_definition.result",
    1
);

pub mod workflow_reject_definition {
    pub type Request = tracedecay_application::WorkflowDefinitionRejectRequest;
    pub type Result = tracedecay_application::WorkflowDefinitionDisposition;
}
typed_operation!(
    WorkflowRejectDefinition,
    workflow_reject_definition,
    "operation.workflow.reject_definition",
    OperationTransport::Http {
        route: "/application/workflow/reject-definition"
    },
    "binding.http.workflow.reject_definition",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.reject_definition.result",
    1
);

pub mod workflow_resume_run {
    pub type Request = tracedecay_application::WorkflowRunResumeRequest;
    pub type Result = tracedecay_domain::WorkflowRunProjection;
}
typed_operation!(
    WorkflowResumeRun,
    workflow_resume_run,
    "operation.workflow.resume_run",
    OperationTransport::Http {
        route: "/application/workflow/resume-run"
    },
    "binding.http.workflow.resume_run",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.resume_run.result",
    1
);

pub mod workflow_retire_definition {
    pub type Request = tracedecay_application::WorkflowDefinitionRetireRequest;
    pub type Result = tracedecay_application::WorkflowDefinitionDisposition;
}
typed_operation!(
    WorkflowRetireDefinition,
    workflow_retire_definition,
    "operation.workflow.retire_definition",
    OperationTransport::Http {
        route: "/application/workflow/retire-definition"
    },
    "binding.http.workflow.retire_definition",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.retire_definition.result",
    1
);

pub mod workflow_start_run {
    pub type Request = tracedecay_application::WorkflowRunStartRequest;
    pub type Result = tracedecay_domain::WorkflowRunProjection;
}
typed_operation!(
    WorkflowStartRun,
    workflow_start_run,
    "operation.workflow.start_run",
    OperationTransport::Http {
        route: "/application/workflow/start-run"
    },
    "binding.http.workflow.start_run",
    EffectClass::Administrative,
    IdempotencyContract::Required,
    false,
    &[],
    30000,
    DeadlineBehavior::ReturnEffectReceipt,
    ReconciliationContract::Required,
    ReceiptContract::DurableEffect,
    &[
        TerminalState::Completed,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::EffectUnknown,
        TerminalState::Partial
    ],
    "schema.workflow.start_run.result",
    1
);

pub mod workflow_validate_definition {
    pub type Request = tracedecay_application::WorkflowDefinitionValidateRequest;
    pub type Result = tracedecay_application::WorkflowDefinitionValidation;
}
typed_operation!(
    WorkflowValidateDefinition,
    workflow_validate_definition,
    "operation.workflow.validate_definition",
    OperationTransport::Http {
        route: "/application/workflow/validate-definition"
    },
    "binding.http.workflow.validate_definition",
    EffectClass::Read,
    IdempotencyContract::NotRequired,
    true,
    &[
        CancellationPoint::BeforeAdmission,
        CancellationPoint::BeforeRead,
        CancellationPoint::DuringRead
    ],
    30000,
    DeadlineBehavior::ReturnOperationReceipt,
    ReconciliationContract::NotRequired,
    ReceiptContract::Operation,
    &[
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial
    ],
    "schema.workflow.validate_definition.result",
    1
);

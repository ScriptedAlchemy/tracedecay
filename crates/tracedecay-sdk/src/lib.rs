//! Native Rust lifecycle client and facade over TraceDecay's public contracts.

#![forbid(unsafe_code)]

pub mod client;
pub mod operations;

/// Canonical HTTP/SSE presentation contracts.
pub use tracedecay_api as api;
/// Canonical transport-neutral use-case contracts, ports, and results.
pub use tracedecay_application as application;
/// Canonical cancellation observations, identity, and process-local signal.
pub use tracedecay_application::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId,
};
/// Canonical pure domain values and validation contracts.
pub use tracedecay_domain as domain;
/// Canonical capability, binding, schema, and operation metadata.
pub use tracedecay_tool_catalog as operation;

/// Canonical Work commands, projections, and executable capability inventory.
pub mod work {
    pub use tracedecay_application::{
        AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand,
        AttachRuntimeEvidenceCommand, CreateWorkCommand, ReplanDependenciesCommand,
        ReviewProposalCommand, WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
        work_executable_binding_registry as executable_binding_registry,
    };
    pub use tracedecay_domain::{WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1};
}

/// Workflow definition, activation, placement, and task-handoff
/// commands, plus their executable capability inventory.
pub mod workflow {
    pub use tracedecay_application::{
        TaskHandoffIssueRequestV1, TaskHandoffRedeemRequestV1, TaskHandoffRedeemedV1,
        WorkflowActivationV1, WorkflowDefinitionActivateRequestV1,
        WorkflowDefinitionRegisterRequestV1, WorkflowPlacementRequestV1,
        workflow_executable_binding_registry as executable_binding_registry,
    };
    pub use tracedecay_domain::{WorkflowDefinitionV1, WorkflowStepV1};
}

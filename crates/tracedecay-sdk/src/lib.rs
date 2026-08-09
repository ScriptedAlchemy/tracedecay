//! Native Rust lifecycle client and facade over TraceDecay's public contracts.

#![forbid(unsafe_code)]

pub mod client;
pub mod operations;
pub mod remote_client;

/// Canonical HTTP/SSE presentation contracts.
pub use tracedecay_api as api;
/// Canonical transport-neutral use-case contracts, ports, and results.
pub use tracedecay_application as application;
/// Canonical transport-neutral remote authority, protocol, and outcome contracts.
pub use tracedecay_application::remote;
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
        AcceptTaskCommand, AdmitWorkExecutionRequestV1, AdmitWorkPlacementCommand,
        AdmitWorkSynthesisCommand, CreateWorkTaskRequestV1, DecideWorkProposalRequestV1,
        PauseWorkRunCommand, ReleaseWorkPlacementCommand, ReplanDependenciesCommand,
        ResumeWorkRunCommand, WorkArtifactHydrationRequestV1,
        WorkArtifactHydrationV1, WorkAttemptArtifactsV1, WorkAttemptEvidenceStateV1,
        WorkPlacementPreflightRequestV1, WorkPlacementReadingV1, WorkPlacementStatusRequestV1,
        WorkProductMutationReceiptV1, WorkProjectionDeltaRequestV1,
        WorkProjectionSnapshotRequestV1, WorkRunControlReadingV1, WorkRunControlRequestV1,
        WorkSynthesisAdmissionV1, WorkSynthesisAttemptV1, WorkSynthesisEvidenceGroupV1,
        WorkSynthesisRefusalV1, WorkSynthesisSourceEnvelopeV1, WorkSynthesisSourceOutcomeV1,
        WorkSynthesisSourceSetV1, work_executable_binding_registry as executable_binding_registry,
    };
    pub use tracedecay_domain::{
        WorkPlacementBlockerV1, WorkPlacementKindV1, WorkPlacementPreflightV1,
        WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1, WorkProjection,
        WorkProjectionDeltaV1, WorkProjectionSnapshotV1, WorkRunControlReasonV1,
        WorkRunControlStateV1, WorkRunControlV1,
    };
}

/// Workflow definition storage and task-handoff commands, plus their
/// executable capability inventory.
pub mod workflow {
    pub use tracedecay_application::{
        TaskHandoffIssueRequest, TaskHandoffRedeemRequest, TaskHandoffRedeemed,
        WorkflowDefinitionRegisterRequest,
        workflow_executable_binding_registry as executable_binding_registry,
    };
    pub use tracedecay_domain::{WorkflowDefinition, WorkflowStep};
}

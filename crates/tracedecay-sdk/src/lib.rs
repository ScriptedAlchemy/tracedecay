//! Native Rust lifecycle client and facade over TraceDecay's public contracts.

#![forbid(unsafe_code)]

pub mod client;
mod observe;
pub mod operations;
pub mod remote_client;
mod request_control;
mod semantic;

/// Canonical cancellation observations, identity, and process-local signal.
pub use tracedecay_contracts::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId,
};

/// Canonical Work commands, projections, and executable capability inventory.
pub mod work {
    pub use tracedecay_contracts::{
        AdmitWorkExecutionRequestV1, AdmitWorkPlacementCommand, AdmitWorkSynthesisCommand,
        CreateWorkTaskRequestV1, DecideWorkProposalRequestV1, PauseWorkRunCommand,
        ReleaseWorkPlacementCommand, ResumeWorkRunCommand, WorkArtifactHydrationRequestV1,
        WorkArtifactHydrationV1, WorkAttemptArtifactsV1, WorkAttemptEvidenceStateV1,
        WorkPlacementPreflightRequestV1, WorkPlacementReadingV1, WorkPlacementStatusRequestV1,
        WorkProductMutationReceiptV1, WorkRunControlReadingV1, WorkRunControlRequestV1,
        WorkSynthesisAdmissionV1, WorkSynthesisAttemptV1, WorkSynthesisEvidenceGroupV1,
        WorkSynthesisRefusalV1, WorkSynthesisSourceEnvelopeV1, WorkSynthesisSourceOutcomeV1,
        WorkSynthesisSourceSetV1, work_executable_binding_registry as executable_binding_registry,
    };
    pub use tracedecay_domain::{
        WorkPlacementBlockerV1, WorkPlacementKindV1, WorkPlacementPreflightV1,
        WorkPlacementStateV1, WorkPlacementTargetV1, WorkPlacementV1, WorkRunControlReasonV1,
        WorkRunControlStateV1, WorkRunControlV1,
    };
}

/// Workflow definition storage and task-handoff commands, plus their
/// executable capability inventory.
pub mod workflow {
    pub use tracedecay_contracts::{
        TaskHandoffIssueRequest, TaskHandoffRedeemRequest, TaskHandoffRedeemed,
        WorkflowDefinitionRegisterRequest,
        workflow_executable_binding_registry as executable_binding_registry,
    };
    pub use tracedecay_domain::{WorkflowDefinition, WorkflowStep};
}

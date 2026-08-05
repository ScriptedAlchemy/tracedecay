//! Canonical Work payloads carried by the daemon invocation protocol.

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, ApplicationOutcome,
    AttachRuntimeEvidenceCommand, CreateWorkCommand, ExpandWorkEvidenceRequestV1,
    GenerateWorkProposalRequestV1, ReplanDependenciesCommand, ReviewProposalRequestV1,
    WorkAttemptAcquireLeaseRequestV1, WorkAttemptCancelRequestV1, WorkAttemptFinishRequestV1,
    WorkAttemptPublishArtifactRequestV1, WorkAttemptPublishProgressRequestV1,
    WorkAttemptRecoverRequestV1, WorkAttemptRenewLeaseRequestV1, WorkAttemptStartRequestV1,
    WorkAttemptTerminalizeRequestV1, WorkEvidenceExpansionV1, WorkProductMutationReceiptV1,
    WorkProductMutationRequestV1, WorkProductProjectionReadV1, WorkProductProjectionsRequestV1,
    WorkProductSnapshotRequestV1, WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
    WorkTaskEvidenceRequestV1, WorkTopologyReadV1,
};
use tracedecay_domain::{
    WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1, WorkProposalV1,
    WorkTaskEvidenceV1,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "attempt_operation",
    content = "request",
    rename_all = "snake_case"
)]
pub(crate) enum WorkAttemptInvocationV1 {
    AcquireLease(Box<WorkAttemptAcquireLeaseRequestV1>),
    RenewLease(WorkAttemptRenewLeaseRequestV1),
    Start(WorkAttemptStartRequestV1),
    PublishProgress(WorkAttemptPublishProgressRequestV1),
    PublishArtifact(WorkAttemptPublishArtifactRequestV1),
    Cancel(WorkAttemptCancelRequestV1),
    Recover(WorkAttemptRecoverRequestV1),
    Finish(WorkAttemptFinishRequestV1),
    Terminalize(WorkAttemptTerminalizeRequestV1),
}

impl WorkAttemptInvocationV1 {
    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::AcquireLease(_) => "attempt_acquire_lease",
            Self::RenewLease(_) => "attempt_renew_lease",
            Self::Start(_) => "attempt_start",
            Self::PublishProgress(_) => "attempt_publish_progress",
            Self::PublishArtifact(_) => "attempt_publish_artifact",
            Self::Cancel(_) => "attempt_cancel",
            Self::Recover(_) => "attempt_recover",
            Self::Finish(_) => "attempt_finish",
            Self::Terminalize(_) => "attempt_terminalize",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub(crate) enum WorkApplicationInvocationV1 {
    Snapshot(WorkProjectionSnapshotRequestV1),
    Delta(WorkProjectionDeltaRequestV1),
    Create(CreateWorkCommand),
    ReplanDependencies(ReplanDependenciesCommand),
    ReviewProposal(ReviewProposalRequestV1),
    AcceptProposal(AcceptProposalCommand),
    AdmitExecution(AdmitExecutionCommand),
    AttachRuntimeEvidence(AttachRuntimeEvidenceCommand),
    AcceptTask(AcceptTaskCommand),
    ProductSnapshot(WorkProductSnapshotRequestV1),
    ProductProjections(WorkProductProjectionsRequestV1),
    TaskEvidence(WorkTaskEvidenceRequestV1),
    ExpandTaskEvidence(ExpandWorkEvidenceRequestV1),
    GenerateWorkProposal(GenerateWorkProposalRequestV1),
    ApplyWorkCommand(WorkProductMutationRequestV1),
}

impl WorkApplicationInvocationV1 {
    pub(crate) const fn is_product_operation(&self) -> bool {
        matches!(
            self,
            Self::ProductSnapshot(_)
                | Self::ProductProjections(_)
                | Self::TaskEvidence(_)
                | Self::ExpandTaskEvidence(_)
                | Self::GenerateWorkProposal(_)
                | Self::ApplyWorkCommand(_)
        )
    }

    pub(crate) const fn operation_key(&self) -> &'static str {
        match self {
            Self::Snapshot(_) => "snapshot",
            Self::Delta(_) => "delta",
            Self::Create(_) => "create",
            Self::ReplanDependencies(_) => "replan_dependencies",
            Self::ReviewProposal(_) => "review_proposal",
            Self::AcceptProposal(_) => "accept_proposal",
            Self::AdmitExecution(_) => "admit_execution",
            Self::AttachRuntimeEvidence(_) => "attach_runtime_evidence",
            Self::AcceptTask(_) => "accept_task",
            Self::ProductSnapshot(_) => "product_snapshot",
            Self::ProductProjections(_) => "product_projections",
            Self::TaskEvidence(_) => "task_evidence",
            Self::ExpandTaskEvidence(_) => "expand_task_evidence",
            Self::GenerateWorkProposal(_) => "generate_work_proposal",
            Self::ApplyWorkCommand(_) => "apply_work_command",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub(crate) enum WorkApplicationOutcomeV1 {
    Snapshot(ApplicationOutcome<WorkProjectionSnapshotV1>),
    Delta(ApplicationOutcome<WorkProjectionDeltaV1>),
    Create(ApplicationOutcome<WorkProjection>),
    ReplanDependencies(ApplicationOutcome<WorkProjection>),
    ReviewProposal(ApplicationOutcome<WorkProjection>),
    AcceptProposal(ApplicationOutcome<WorkProjection>),
    AdmitExecution(ApplicationOutcome<WorkProjection>),
    AttachRuntimeEvidence(ApplicationOutcome<WorkProjection>),
    AcceptTask(ApplicationOutcome<WorkProjection>),
    ProductSnapshot(ApplicationOutcome<WorkTopologyReadV1>),
    ProductProjections(ApplicationOutcome<WorkProductProjectionReadV1>),
    TaskEvidence(ApplicationOutcome<WorkTaskEvidenceV1>),
    ExpandTaskEvidence(ApplicationOutcome<WorkEvidenceExpansionV1>),
    GenerateWorkProposal(ApplicationOutcome<WorkProposalV1>),
    ApplyWorkCommand(ApplicationOutcome<WorkProductMutationReceiptV1>),
}

use serde::{Deserialize, Serialize};
use tracedecay_contracts::{
    ApplicationOutcome, TaskHandoffGrant, TaskHandoffIssueRequest, TaskHandoffRedeemRequest,
    TaskHandoffRedeemed, WorkflowDefinitionActivateRequest, WorkflowDefinitionDiff,
    WorkflowDefinitionDiffRequest, WorkflowDefinitionDisposition, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRejectRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionValidateRequest,
    WorkflowDefinitionValidation,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum WorkflowApplicationInvocation {
    RegisterDefinition(WorkflowDefinitionRegisterRequest),
    ActivateDefinition(WorkflowDefinitionActivateRequest),
    RetireDefinition(WorkflowDefinitionRetireRequest),
    RejectDefinition(WorkflowDefinitionRejectRequest),
    ValidateDefinition(WorkflowDefinitionValidateRequest),
    GetDefinition(WorkflowDefinitionGetRequest),
    ListDefinitions(WorkflowDefinitionListRequest),
    DefinitionHistory(WorkflowDefinitionHistoryRequest),
    DiffDefinition(WorkflowDefinitionDiffRequest),
    HandoffIssue(TaskHandoffIssueRequest),
    HandoffRedeem(TaskHandoffRedeemRequest),
    StartRun(Box<tracedecay_contracts::WorkflowRunStartRequest>),
    PauseRun(tracedecay_contracts::WorkflowRunPauseRequest),
    ResumeRun(tracedecay_contracts::WorkflowRunResumeRequest),
    CancelRun(tracedecay_contracts::WorkflowRunCancelRequest),
    GetRun(tracedecay_contracts::WorkflowRunGetRequest),
}

impl WorkflowApplicationInvocation {
    #[hotpath::skip]
    pub const fn operation_key(&self) -> &'static str {
        match self {
            Self::RegisterDefinition(_) => "register_definition",
            Self::ActivateDefinition(_) => "activate_definition",
            Self::RetireDefinition(_) => "retire_definition",
            Self::RejectDefinition(_) => "reject_definition",
            Self::ValidateDefinition(_) => "validate_definition",
            Self::GetDefinition(_) => "get_definition",
            Self::ListDefinitions(_) => "list_definitions",
            Self::DefinitionHistory(_) => "definition_history",
            Self::DiffDefinition(_) => "diff_definition",
            Self::HandoffIssue(_) => "handoff_issue",
            Self::HandoffRedeem(_) => "handoff_redeem",
            Self::StartRun(_) => "start_run",
            Self::PauseRun(_) => "pause_run",
            Self::ResumeRun(_) => "resume_run",
            Self::CancelRun(_) => "cancel_run",
            Self::GetRun(_) => "get_run",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub enum WorkflowApplicationOutcome {
    RegisterDefinition(ApplicationOutcome<tracedecay_domain::WorkflowDefinition>),
    ActivateDefinition(ApplicationOutcome<WorkflowDefinitionDisposition>),
    RetireDefinition(ApplicationOutcome<WorkflowDefinitionDisposition>),
    RejectDefinition(ApplicationOutcome<WorkflowDefinitionDisposition>),
    ValidateDefinition(ApplicationOutcome<WorkflowDefinitionValidation>),
    GetDefinition(ApplicationOutcome<tracedecay_domain::WorkflowDefinition>),
    ListDefinitions(ApplicationOutcome<Vec<tracedecay_domain::WorkflowDefinition>>),
    DefinitionHistory(ApplicationOutcome<Vec<tracedecay_domain::WorkflowDefinition>>),
    DiffDefinition(ApplicationOutcome<WorkflowDefinitionDiff>),
    HandoffIssue(ApplicationOutcome<TaskHandoffGrant>),
    HandoffRedeem(ApplicationOutcome<TaskHandoffRedeemed>),
    StartRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    PauseRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    ResumeRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    CancelRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
    GetRun(ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>),
}

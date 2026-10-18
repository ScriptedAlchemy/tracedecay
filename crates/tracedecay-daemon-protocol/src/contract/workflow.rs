use tracedecay_contracts::{
    ApplicationOutcome, TaskHandoffGrant, TaskHandoffIssueRequest, TaskHandoffRedeemRequest,
    TaskHandoffRedeemed, WorkflowDefinitionActivateRequest, WorkflowDefinitionDiff,
    WorkflowDefinitionDiffRequest, WorkflowDefinitionDisposition, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionRejectRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionValidateRequest,
    WorkflowDefinitionValidation,
};

application_operation_family! {
    invocation WorkflowApplicationInvocation;
    #[derive(Eq)]
    outcome WorkflowApplicationOutcome;

    RegisterDefinition: "register_definition" =>
        WorkflowDefinitionRegisterRequest,
        ApplicationOutcome<tracedecay_domain::WorkflowDefinition>;
    ActivateDefinition: "activate_definition" =>
        WorkflowDefinitionActivateRequest,
        ApplicationOutcome<WorkflowDefinitionDisposition>;
    RetireDefinition: "retire_definition" =>
        WorkflowDefinitionRetireRequest,
        ApplicationOutcome<WorkflowDefinitionDisposition>;
    RejectDefinition: "reject_definition" =>
        WorkflowDefinitionRejectRequest,
        ApplicationOutcome<WorkflowDefinitionDisposition>;
    ValidateDefinition: "validate_definition" =>
        WorkflowDefinitionValidateRequest,
        ApplicationOutcome<WorkflowDefinitionValidation>;
    GetDefinition: "get_definition" =>
        WorkflowDefinitionGetRequest,
        ApplicationOutcome<tracedecay_domain::WorkflowDefinition>;
    ListDefinitions: "list_definitions" =>
        WorkflowDefinitionListRequest,
        ApplicationOutcome<Vec<tracedecay_domain::WorkflowDefinition>>;
    DefinitionHistory: "definition_history" =>
        WorkflowDefinitionHistoryRequest,
        ApplicationOutcome<Vec<tracedecay_domain::WorkflowDefinition>>;
    DiffDefinition: "diff_definition" =>
        WorkflowDefinitionDiffRequest,
        ApplicationOutcome<WorkflowDefinitionDiff>;
    HandoffIssue: "handoff_issue" =>
        TaskHandoffIssueRequest,
        ApplicationOutcome<TaskHandoffGrant>;
    HandoffRedeem: "handoff_redeem" =>
        TaskHandoffRedeemRequest,
        ApplicationOutcome<TaskHandoffRedeemed>;
    StartRun: "start_run" =>
        Box<tracedecay_contracts::WorkflowRunStartRequest>,
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>;
    PauseRun: "pause_run" =>
        tracedecay_contracts::WorkflowRunPauseRequest,
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>;
    ResumeRun: "resume_run" =>
        tracedecay_contracts::WorkflowRunResumeRequest,
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>;
    CancelRun: "cancel_run" =>
        tracedecay_contracts::WorkflowRunCancelRequest,
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>;
    GetRun: "get_run" =>
        tracedecay_contracts::WorkflowRunGetRequest,
        ApplicationOutcome<tracedecay_domain::WorkflowRunProjection>;
}

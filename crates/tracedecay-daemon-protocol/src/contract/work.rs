use tracedecay_contracts::{
    AcceptWorkProposalRequestV1, AdjudicateWorkLeakCommandV1, AdmitWorkExecutionRequestV1,
    AdmitWorkPlacementCommand, AdmitWorkSynthesisCommand, AdmittedWorkExecutionV1,
    AdvanceWorkJobRequestV1, AdvanceWorkJobResultV1, ApplicationOutcome, CancelWorkAttemptCommand,
    CreateWorkTaskRequestV1, ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1,
    ExecutionTopologyViewV1, GenerateProposalRequest, GeneratedWorkProposal, PauseWorkRunCommand,
    PrepareWorkDuplicateAdjudicationRequestV1, PrepareWorkProductMutationRequestV1,
    ReleaseWorkPlacementCommand, ResumeWorkAttemptsCommand, ResumeWorkRunCommand,
    RetryWorkAttemptCommandV1, ReviewWorkProposalRequestV1, StartWorkAttemptCommand,
    WorkArtifactHydrationRequestV1, WorkArtifactHydrationV1, WorkAttemptListRequestV1,
    WorkAttemptListV1, WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkEvidenceRetrievalV1,
    WorkEvidenceRetrieveRequestV1, WorkExecutionHistoryV1, WorkExperienceRequestV1,
    WorkExperienceV1, WorkGraphReadRequestV1, WorkGraphReadV1, WorkLeakAdjudicationOutcomeV1,
    WorkPlacementPreflightRequestV1, WorkPlacementReadingV1, WorkPlacementStatusRequestV1,
    WorkProductMutationReceiptV1, WorkProductMutationRequestV1, WorkProposalComparisonRequestV1,
    WorkProposalComparisonV1, WorkRunControlReadingV1, WorkRunControlRequestV1,
    WorkSynthesisAttemptV1, WorkTopologyViewRequestV1,
};
use tracedecay_domain::{
    WorkAttemptV1, WorkDuplicateAdjudicationCommandV1, WorkPlacementPreflightV1, WorkPlacementV1,
    WorkRunControlV1,
};

application_operation_family! {
    #[allow(clippy::large_enum_variant)]
    invocation WorkApplicationInvocationV1;
    outcome WorkApplicationOutcomeV1;

    GenerateProposal: "generate_proposal" =>
        GenerateProposalRequest,
        ApplicationOutcome<GeneratedWorkProposal>;
    Create: "create" =>
        CreateWorkTaskRequestV1,
        ApplicationOutcome<WorkProductMutationReceiptV1>;
    ReviewProposal: "review_proposal" =>
        ReviewWorkProposalRequestV1,
        ApplicationOutcome<WorkProductMutationReceiptV1>;
    AcceptProposal: "accept_proposal" =>
        AcceptWorkProposalRequestV1,
        ApplicationOutcome<WorkProductMutationReceiptV1>;
    AdmitExecution: "admit_execution" =>
        AdmitWorkExecutionRequestV1,
        ApplicationOutcome<AdmittedWorkExecutionV1>;
    StartAttempt: "start_attempt" =>
        StartWorkAttemptCommand,
        ApplicationOutcome<WorkAttemptV1>;
    Synthesize: "synthesize" =>
        AdmitWorkSynthesisCommand,
        ApplicationOutcome<WorkSynthesisAttemptV1>;
    AttemptStatus: "attempt_status" =>
        WorkAttemptStatusRequestV1,
        ApplicationOutcome<WorkAttemptV1>;
    CancelAttempt: "cancel_attempt" =>
        CancelWorkAttemptCommand,
        ApplicationOutcome<WorkAttemptV1>;
    ResumeAttempts: "resume_attempts" =>
        ResumeWorkAttemptsCommand,
        ApplicationOutcome<WorkAttemptRecoveryReportV1>;
    RetryAttempt: "retry_attempt" =>
        RetryWorkAttemptCommandV1,
        Box<ApplicationOutcome<tracedecay_contracts::WorkRetryAttemptOutcomeV1>>;
    ListAttempts: "list_attempts" =>
        WorkAttemptListRequestV1,
        ApplicationOutcome<WorkAttemptListV1>;
    ExecutionHistory: "execution_history" =>
        WorkAttemptListRequestV1,
        ApplicationOutcome<WorkExecutionHistoryV1>;
    HydrateArtifacts: "hydrate_artifacts" =>
        WorkArtifactHydrationRequestV1,
        ApplicationOutcome<WorkArtifactHydrationV1>;
    RetrieveEvidence: "retrieve_evidence" =>
        WorkEvidenceRetrieveRequestV1,
        ApplicationOutcome<WorkEvidenceRetrievalV1>;
    Views: "views" =>
        WorkGraphReadRequestV1,
        ApplicationOutcome<WorkGraphReadV1>;
    Experience: "experience" =>
        WorkExperienceRequestV1,
        ApplicationOutcome<WorkExperienceV1>;
    CompareProposal: "compare_proposal" =>
        WorkProposalComparisonRequestV1,
        ApplicationOutcome<WorkProposalComparisonV1>;
    PrepareGraphMutation: "prepare_graph_mutation" =>
        PrepareWorkProductMutationRequestV1,
        ApplicationOutcome<WorkProductMutationRequestV1>;
    MutateGraph: "mutate_graph" =>
        WorkProductMutationRequestV1,
        ApplicationOutcome<WorkProductMutationReceiptV1>;
    Topology: "topology" =>
        WorkTopologyViewRequestV1,
        ApplicationOutcome<ExecutionTopologyViewV1>;
    TopologyMetrics: "topology_metrics" =>
        ExecutionTopologyMetricsRequestV1,
        ApplicationOutcome<ExecutionTopologyMetricsV1>;
    PrepareDuplicateAdjudication: "prepare_duplicate_adjudication" =>
        PrepareWorkDuplicateAdjudicationRequestV1,
        ApplicationOutcome<WorkDuplicateAdjudicationCommandV1>;
    AdjudicateDuplicate: "adjudicate_duplicate" =>
        WorkDuplicateAdjudicationCommandV1,
        ApplicationOutcome<WorkDuplicateAdjudicationAppendOutcomeV1>;
    AdjudicateLeak: "adjudicate_leak" =>
        AdjudicateWorkLeakCommandV1,
        ApplicationOutcome<WorkLeakAdjudicationOutcomeV1>;
    PauseRun: "pause_run" =>
        PauseWorkRunCommand,
        ApplicationOutcome<WorkRunControlV1>;
    ResumeRun: "resume_run" =>
        ResumeWorkRunCommand,
        ApplicationOutcome<WorkRunControlV1>;
    RunControl: "run_control" =>
        WorkRunControlRequestV1,
        ApplicationOutcome<WorkRunControlReadingV1>;
    PlacementPreflight: "placement_preflight" =>
        WorkPlacementPreflightRequestV1,
        ApplicationOutcome<WorkPlacementPreflightV1>;
    AdmitPlacement: "admit_placement" =>
        AdmitWorkPlacementCommand,
        ApplicationOutcome<WorkPlacementV1>;
    PlacementStatus: "placement_status" =>
        WorkPlacementStatusRequestV1,
        ApplicationOutcome<WorkPlacementReadingV1>;
    ReleasePlacement: "release_placement" =>
        ReleaseWorkPlacementCommand,
        ApplicationOutcome<WorkPlacementV1>;
    AdvanceJob: "advance_job" =>
        AdvanceWorkJobRequestV1,
        ApplicationOutcome<AdvanceWorkJobResultV1>;
}

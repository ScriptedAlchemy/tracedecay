//! Rust-owned schema bundle for the dashboard wire boundary.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use tracedecay_api::read_model::multi_root::{MultiRootCapabilityV1, MultiRootQueryReadModelV1};
use tracedecay_application::retained_surfaces::{
    AutomationRunProblemV1, AutomationRunResultV1, FactStoreCurateRequestV1,
};
use tracedecay_application::{
    AdjudicateWorkLeakCommandV1, AdmitWorkExecutionRequestV1, AdmitWorkPlacementCommand,
    AdmitWorkSynthesisCommand, AuthorizedScopeSet, CancelWorkAttemptCommand, CostsReadModelV1,
    CreateWorkTaskRequestV1, DecideWorkProposalRequestV1, ExecutionTopologyMetricsRequestV1,
    ExecutionTopologyMetricsV1, ExecutionTopologyViewV1, GenerateProposalRequest,
    GeneratedWorkProposal, ListTaskHandoffsRequestV1, ListTaskHandoffsResultV1,
    MultiRootExecuteRequestV1, MultiRootScopeSetCasRequestV1, MultiRootScopeSetCasResultV1,
    MultiRootScopeSetReadRequestV1, ObservatoryReadModelV1, PauseWorkRunCommand,
    PrepareWorkDuplicateAdjudicationRequestV1, PrepareWorkProductMutationRequestV1,
    ReleaseWorkPlacementCommand, ResumeWorkAttemptsCommand, ResumeWorkRunCommand,
    RetryWorkAttemptCommandV1, StartWorkAttemptCommand, WorkArtifactHydrationRequestV1,
    WorkArtifactHydrationV1, WorkAttemptListRequestV1, WorkAttemptListV1,
    WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkEvidenceRetrievalV1,
    WorkEvidenceRetrieveRequestV1, WorkExecutionHistoryV1, WorkExperienceRequestV1,
    WorkExperienceV1, WorkGraphReadRequestV1, WorkGraphReadV1, WorkLeakAdjudicationOutcomeV1,
    WorkPlacementPreflightRequestV1, WorkPlacementReadingV1, WorkPlacementStatusRequestV1,
    WorkProductMutationReceiptV1, WorkProductMutationRequestV1, WorkProposalComparisonRequestV1,
    WorkProposalComparisonV1, WorkRetryAttemptOutcomeV1, WorkRunControlReadingV1,
    WorkRunControlRequestV1, WorkSynthesisAttemptV1, WorkTopologyViewRequestV1,
    WorkflowDefinitionActivateRequest, WorkflowDefinitionDisposition, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRejectRequest, WorkflowDefinitionRetireRequest, WorkflowRunGetRequest,
};
use tracedecay_domain::{
    WorkAttemptV1, WorkDuplicateAdjudicationCommandV1, WorkPlacementPreflightV1, WorkPlacementV1,
    WorkRunControlV1, WorkflowDefinition, WorkflowRunProjection,
};

use super::analytics_api::{
    AnalyticsAgentsPayloadV1, AnalyticsDiagnosticsPayloadV1, AnalyticsHintsPayloadV1,
    AnalyticsOverviewPayloadV1, AnalyticsSubagentTreePayloadV1, AnalyticsUnderusedPayloadV1,
    AnalyticsUsageSummaryV1,
};
use super::automation_scheduler_api::AutomationSchedulerStatusV1;
use super::code_index_freshness_api::CodeIndexFreshnessPayloadV1;
use super::delivery_api::DeliveryOverviewV1;
use super::doctor_findings_api::DoctorFindingsPayloadV1;
use super::explorer_api::{ExplorerQueryRunV1, ExplorerReadContextV1, ExplorerSessionSizeV1};
use super::graph_service::{
    GraphNeighborsPayloadV1, GraphNodePayloadV1, GraphOverviewPayloadV1, GraphPathPayloadV1,
    GraphSearchPayloadV1, GraphSubgraphPayloadV1,
};
use super::graph_structure_api::{
    CallChainMeasurementV1, FactMatchesMeasurementV1, NodeSessionsMeasurementV1,
    StrataMeasurementV1, StructureReadV1, TestMapMeasurementV1, registered_route_contracts,
};
use super::lcm_api::{
    LcmOverviewPayloadV1, LcmSearchPayloadV1, LcmSessionPayloadV1, LcmTimelinePayloadV1,
};
use super::loom_api::LoomTemporalPayloadV1;
use super::memory_api::{
    MemoryFactDetailPayloadV1, MemoryOverviewPayloadV1, MemoryStatusPayloadV1,
};
use super::projects::{ProjectContextPayloadV1, ProjectsPayloadV1};
use super::read_model::{DASHBOARD_SCHEMA_REVISION_V1, DashboardEnvelopeV1};
use super::remote_status_api::RemoteOperationalStatusPayloadV1;
use super::savings_api::{SavingsOverviewPayloadV1, SavingsSessionsPayloadV1};
use super::settings_api::{ProjectSettingsPatch, SettingsPayloadV1, UserSettingsPatch};
use super::storage_findings_api::StorageFindingsPayloadV1;
use super::storage_telemetry_api::StorageTelemetryPayloadV1;
use super::work_api::registered_route_contracts as registered_work_route_contracts;
use crate::application::feedback::observations::FeedbackObservationReadModelV1;

#[derive(JsonSchema)]
#[allow(dead_code)]
struct DashboardContractCatalogV1 {
    envelope: DashboardEnvelopeV1<DashboardPayloadMarkerV1>,
    storage_telemetry: StorageTelemetryPayloadV1,
    storage_findings: StorageFindingsPayloadV1,
    doctor_findings: DoctorFindingsPayloadV1,
    remote_operational_status: RemoteOperationalStatusPayloadV1,
    explorer_query_run: ExplorerQueryRunV1,
    explorer_session_size: DashboardEnvelopeV1<Option<ExplorerSessionSizeV1>>,
    explorer_read_context: DashboardEnvelopeV1<Option<ExplorerReadContextV1>>,
    projects: DashboardEnvelopeV1<ProjectsPayloadV1>,
    project_context: DashboardEnvelopeV1<ProjectContextPayloadV1>,
    graph_overview: DashboardEnvelopeV1<Option<GraphOverviewPayloadV1>>,
    graph_search: DashboardEnvelopeV1<Option<GraphSearchPayloadV1>>,
    graph_node: DashboardEnvelopeV1<Option<GraphNodePayloadV1>>,
    graph_neighbors: DashboardEnvelopeV1<Option<GraphNeighborsPayloadV1>>,
    graph_subgraph: DashboardEnvelopeV1<Option<GraphSubgraphPayloadV1>>,
    graph_path: DashboardEnvelopeV1<Option<GraphPathPayloadV1>>,
    memory_overview: DashboardEnvelopeV1<Option<MemoryOverviewPayloadV1>>,
    memory_status: DashboardEnvelopeV1<Option<MemoryStatusPayloadV1>>,
    memory_fact_detail: DashboardEnvelopeV1<Option<MemoryFactDetailPayloadV1>>,
    analytics_overview: DashboardEnvelopeV1<Option<AnalyticsOverviewPayloadV1>>,
    analytics_usage: DashboardEnvelopeV1<Option<AnalyticsUsageSummaryV1>>,
    analytics_agents: DashboardEnvelopeV1<Option<AnalyticsAgentsPayloadV1>>,
    analytics_subagent_tree: DashboardEnvelopeV1<Option<AnalyticsSubagentTreePayloadV1>>,
    /// The handoff-token frontier. Contracted here because the Agents surface
    /// reads it directly off the mounted application route; the two `open_*`
    /// operations stay uncontracted for the dashboard because it never redeems
    /// a bearer and must not grow a client that could.
    handoff_list_task_handoffs_request: ListTaskHandoffsRequestV1,
    handoff_list_task_handoffs: ListTaskHandoffsResultV1,
    analytics_hints: DashboardEnvelopeV1<Option<AnalyticsHintsPayloadV1>>,
    analytics_underused: DashboardEnvelopeV1<Option<AnalyticsUnderusedPayloadV1>>,
    analytics_diagnostics: DashboardEnvelopeV1<Option<AnalyticsDiagnosticsPayloadV1>>,
    savings_overview: DashboardEnvelopeV1<Option<SavingsOverviewPayloadV1>>,
    savings_sessions: SavingsSessionsPayloadV1,
    lcm_session: DashboardEnvelopeV1<Option<LcmSessionPayloadV1>>,
    lcm_timeline: DashboardEnvelopeV1<Option<LcmTimelinePayloadV1>>,
    lcm_overview: DashboardEnvelopeV1<Option<LcmOverviewPayloadV1>>,
    lcm_search: DashboardEnvelopeV1<Option<LcmSearchPayloadV1>>,
    loom_temporal: LoomTemporalPayloadV1,
    delivery_overview: DeliveryOverviewV1,
    feedback_status: DashboardEnvelopeV1<FeedbackObservationReadModelV1>,
    code_index_freshness: CodeIndexFreshnessPayloadV1,
    settings: SettingsPayloadV1,
    settings_project_patch: ProjectSettingsPatch,
    settings_user_patch: UserSettingsPatch,
    observatory: ObservatoryReadModelV1,
    costs: CostsReadModelV1,
    graph_call_chain: StructureReadV1<CallChainMeasurementV1>,
    graph_strata: StructureReadV1<StrataMeasurementV1>,
    graph_fact_matches: StructureReadV1<FactMatchesMeasurementV1>,
    graph_test_map: StructureReadV1<TestMapMeasurementV1>,
    graph_node_sessions: StructureReadV1<NodeSessionsMeasurementV1>,
    work_generate_proposal_request: GenerateProposalRequest,
    work_generated_proposal: GeneratedWorkProposal,
    work_create_task_request: CreateWorkTaskRequestV1,
    work_decide_proposal_request: DecideWorkProposalRequestV1,
    work_admit_execution_request: AdmitWorkExecutionRequestV1,
    work_start_attempt_command: StartWorkAttemptCommand,
    work_admit_synthesis_command: AdmitWorkSynthesisCommand,
    work_synthesis_attempt: WorkSynthesisAttemptV1,
    work_attempt_status_request: WorkAttemptStatusRequestV1,
    work_cancel_attempt_command: CancelWorkAttemptCommand,
    work_resume_attempts_command: ResumeWorkAttemptsCommand,
    work_retry_attempt_command: RetryWorkAttemptCommandV1,
    work_retry_attempt_outcome: WorkRetryAttemptOutcomeV1,
    work_attempt: WorkAttemptV1,
    work_attempt_recovery_report: WorkAttemptRecoveryReportV1,
    work_attempt_list_request: WorkAttemptListRequestV1,
    work_attempt_list: WorkAttemptListV1,
    work_execution_history: WorkExecutionHistoryV1,
    work_evidence_retrieve_request: WorkEvidenceRetrieveRequestV1,
    work_evidence_retrieval: WorkEvidenceRetrievalV1,
    work_graph_read_request: WorkGraphReadRequestV1,
    work_graph_read: WorkGraphReadV1,
    work_experience_request: WorkExperienceRequestV1,
    work_experience: WorkExperienceV1,
    work_proposal_comparison_request: WorkProposalComparisonRequestV1,
    work_proposal_comparison: WorkProposalComparisonV1,
    work_prepare_product_mutation_request: PrepareWorkProductMutationRequestV1,
    work_product_mutation_request: WorkProductMutationRequestV1,
    work_product_mutation_receipt: WorkProductMutationReceiptV1,
    work_artifact_hydration_request: WorkArtifactHydrationRequestV1,
    work_artifact_hydration: WorkArtifactHydrationV1,
    work_pause_run_command: PauseWorkRunCommand,
    work_resume_run_command: ResumeWorkRunCommand,
    work_run_control: WorkRunControlV1,
    work_run_control_request: WorkRunControlRequestV1,
    work_run_control_reading: WorkRunControlReadingV1,
    work_placement_preflight_request: WorkPlacementPreflightRequestV1,
    work_placement_preflight: WorkPlacementPreflightV1,
    work_admit_placement_command: AdmitWorkPlacementCommand,
    work_release_placement_command: ReleaseWorkPlacementCommand,
    work_placement: WorkPlacementV1,
    work_placement_status_request: WorkPlacementStatusRequestV1,
    work_placement_reading: WorkPlacementReadingV1,
    work_topology_view_request: WorkTopologyViewRequestV1,
    work_topology_view: ExecutionTopologyViewV1,
    work_topology_metrics_request: ExecutionTopologyMetricsRequestV1,
    work_topology_metrics: ExecutionTopologyMetricsV1,
    work_prepare_duplicate_adjudication_request: PrepareWorkDuplicateAdjudicationRequestV1,
    work_duplicate_adjudication_command: WorkDuplicateAdjudicationCommandV1,
    work_duplicate_adjudication_result: WorkDuplicateAdjudicationAppendOutcomeV1,
    work_leak_adjudication_command: AdjudicateWorkLeakCommandV1,
    work_leak_adjudication_result: WorkLeakAdjudicationOutcomeV1,
    /// The workflow definition/run slice the Workflows workspace consumes.
    /// Handoffs and run control stay uncontracted: the browser never holds a
    /// bearer or mints fences, command ids, or provider admissions.
    workflow_definition: WorkflowDefinition,
    workflow_definition_list_request: WorkflowDefinitionListRequest,
    workflow_definition_get_request: WorkflowDefinitionGetRequest,
    workflow_definition_history_request: WorkflowDefinitionHistoryRequest,
    workflow_definition_activate_request: WorkflowDefinitionActivateRequest,
    workflow_definition_retire_request: WorkflowDefinitionRetireRequest,
    workflow_definition_reject_request: WorkflowDefinitionRejectRequest,
    workflow_definition_disposition: WorkflowDefinitionDisposition,
    workflow_run_get_request: WorkflowRunGetRequest,
    workflow_run_projection: WorkflowRunProjection,
    multi_root_capability: MultiRootCapabilityV1,
    multi_root_scope_set_read_request: MultiRootScopeSetReadRequestV1,
    multi_root_scope_set: Option<AuthorizedScopeSet>,
    multi_root_scope_set_cas_request: MultiRootScopeSetCasRequestV1,
    multi_root_scope_set_cas_result: MultiRootScopeSetCasResultV1,
    multi_root_execute_request: MultiRootExecuteRequestV1,
    multi_root_query: MultiRootQueryReadModelV1<DashboardPayloadMarkerV1>,
    /// Served identically by `GET /api/automation/scheduler/status` and by the
    /// `pause`/`resume` controls, which re-read rather than acknowledge.
    automation_scheduler_status: AutomationSchedulerStatusV1,
    fact_store_curate_request: FactStoreCurateRequestV1,
    automation_run: AutomationRunResultV1,
    automation_problem: AutomationRunProblemV1,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct DashboardPayloadMarkerV1;

/// Render the complete Rust dashboard wire schema as deterministic JSON.
pub fn render_dashboard_contract_schema() -> Result<String, serde_json::Error> {
    let generator = SchemaSettings::default().for_serialize().into_generator();
    let mut schema =
        serde_json::to_value(generator.into_root_schema_for::<DashboardContractCatalogV1>())?;
    schema["schemaRevision"] = serde_json::json!(DASHBOARD_SCHEMA_REVISION_V1);
    assert_registered_route_responses_are_contracted(&schema);
    assert_registered_work_route_payloads_are_contracted(&schema);
    serde_json::to_string_pretty(&schema).map(|rendered| format!("{rendered}\n"))
}

fn assert_registered_route_responses_are_contracted(schema: &serde_json::Value) {
    let Some(definitions) = schema["$defs"].as_object() else {
        panic!("dashboard contracts must expose schema definitions");
    };
    for route in registered_route_contracts() {
        let response = (route.response_schema_name)();
        assert!(
            definitions.contains_key(response.as_ref()),
            "{} {} response {response} is registered but absent from the dashboard contract catalog",
            route.method,
            route.path,
        );
    }
}

fn assert_registered_work_route_payloads_are_contracted(schema: &serde_json::Value) {
    let Some(definitions) = schema["$defs"].as_object() else {
        panic!("dashboard contracts must expose schema definitions");
    };
    for route in registered_work_route_contracts() {
        for (direction, schema_name) in [
            ("request", (route.request_schema_name)()),
            ("response", (route.response_schema_name)()),
        ] {
            assert!(
                definitions.contains_key(schema_name.as_ref()),
                "{} {} {direction} {schema_name} is registered but absent from the dashboard contract catalog",
                route.method,
                route.path,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render_dashboard_contract_schema;

    #[test]
    #[ignore = "invoked by dashboard contracts:generate/check"]
    fn writes_dashboard_contract_schema() {
        let output = std::env::var_os("TRACEDECAY_DASHBOARD_CONTRACT_SCHEMA_OUT")
            .map(std::path::PathBuf::from)
            .expect("contract codegen must provide an output path");
        let schema =
            render_dashboard_contract_schema().expect("serialize dashboard contract schema");
        std::fs::write(output, schema).expect("write dashboard contract schema");
    }

    #[test]
    fn registered_dashboard_route_responses_are_contracted() {
        render_dashboard_contract_schema().expect("render validated dashboard contracts");
    }

    #[test]
    fn fact_store_curate_contracts_are_registered() {
        let schema: serde_json::Value = serde_json::from_str(
            &render_dashboard_contract_schema().expect("render validated dashboard contracts"),
        )
        .expect("parse dashboard contract schema");
        let definitions = schema["$defs"]
            .as_object()
            .expect("dashboard contracts expose schema definitions");

        let request = definitions
            .get("FactStoreCurateRequestV1")
            .expect("fact-store curate request contract");
        assert_eq!(request["additionalProperties"], false);
        assert_eq!(
            request["properties"]
                .as_object()
                .expect("fact-store curate request properties")
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            ["fact_review_limit", "min_confidence_millionths"]
                .into_iter()
                .collect()
        );
        assert!(!definitions.contains_key("AutomationRunRequestV1"));
        assert!(definitions.contains_key("AutomationRunResultV1"));
        assert!(definitions.contains_key("AutomationRunProblemV1"));
    }

    #[test]
    fn registered_dashboard_work_route_payloads_are_contracted() {
        render_dashboard_contract_schema().expect("render validated dashboard Work contracts");
    }

    #[test]
    fn canonical_work_contracts_are_registered() {
        let schema: serde_json::Value = serde_json::from_str(
            &render_dashboard_contract_schema().expect("render validated dashboard contracts"),
        )
        .expect("parse dashboard contract schema");
        let definitions = schema["$defs"]
            .as_object()
            .expect("dashboard contracts expose schema definitions");

        for (field, contract) in [
            ("work_generate_proposal_request", "GenerateProposalRequest"),
            ("work_generated_proposal", "GeneratedWorkProposal"),
            ("work_create_task_request", "CreateWorkTaskRequestV1"),
            (
                "work_decide_proposal_request",
                "DecideWorkProposalRequestV1",
            ),
            (
                "work_admit_execution_request",
                "AdmitWorkExecutionRequestV1",
            ),
            ("work_start_attempt_command", "StartWorkAttemptCommand"),
            ("work_admit_synthesis_command", "AdmitWorkSynthesisCommand"),
            ("work_synthesis_attempt", "WorkSynthesisAttemptV1"),
            ("work_attempt_status_request", "WorkAttemptStatusRequestV1"),
            ("work_cancel_attempt_command", "CancelWorkAttemptCommand"),
            ("work_resume_attempts_command", "ResumeWorkAttemptsCommand"),
            ("work_retry_attempt_command", "RetryWorkAttemptCommandV1"),
            ("work_retry_attempt_outcome", "WorkRetryAttemptOutcomeV1"),
            ("work_attempt", "WorkAttemptV1"),
            (
                "work_attempt_recovery_report",
                "WorkAttemptRecoveryReportV1",
            ),
            ("work_attempt_list_request", "WorkAttemptListRequestV1"),
            ("work_attempt_list", "WorkAttemptListV1"),
            ("work_execution_history", "WorkExecutionHistoryV1"),
            (
                "work_evidence_retrieve_request",
                "WorkEvidenceRetrieveRequestV1",
            ),
            ("work_evidence_retrieval", "WorkEvidenceRetrievalV1"),
            ("work_graph_read_request", "WorkGraphReadRequestV1"),
            ("work_graph_read", "WorkGraphReadV1"),
            ("work_experience_request", "WorkExperienceRequestV1"),
            ("work_experience", "WorkExperienceV1"),
            (
                "work_proposal_comparison_request",
                "WorkProposalComparisonRequestV1",
            ),
            ("work_proposal_comparison", "WorkProposalComparisonV1"),
            (
                "work_prepare_product_mutation_request",
                "PrepareWorkProductMutationRequestV1",
            ),
            (
                "work_product_mutation_request",
                "WorkProductMutationRequestV1",
            ),
            (
                "work_product_mutation_receipt",
                "WorkProductMutationReceiptV1",
            ),
            (
                "work_artifact_hydration_request",
                "WorkArtifactHydrationRequestV1",
            ),
            ("work_artifact_hydration", "WorkArtifactHydrationV1"),
            ("work_pause_run_command", "PauseWorkRunCommand"),
            ("work_resume_run_command", "ResumeWorkRunCommand"),
            ("work_run_control", "WorkRunControlV1"),
            ("work_run_control_request", "WorkRunControlRequestV1"),
            ("work_run_control_reading", "WorkRunControlReadingV1"),
            (
                "work_placement_preflight_request",
                "WorkPlacementPreflightRequestV1",
            ),
            ("work_placement_preflight", "WorkPlacementPreflightV1"),
            ("work_admit_placement_command", "AdmitWorkPlacementCommand"),
            (
                "work_release_placement_command",
                "ReleaseWorkPlacementCommand",
            ),
            ("work_placement", "WorkPlacementV1"),
            (
                "work_placement_status_request",
                "WorkPlacementStatusRequestV1",
            ),
            ("work_placement_reading", "WorkPlacementReadingV1"),
            ("work_topology_view_request", "WorkTopologyViewRequestV1"),
            ("work_topology_view", "ExecutionTopologyViewV1"),
            (
                "work_topology_metrics_request",
                "ExecutionTopologyMetricsRequestV1",
            ),
            ("work_topology_metrics", "ExecutionTopologyMetricsV1"),
            (
                "work_prepare_duplicate_adjudication_request",
                "PrepareWorkDuplicateAdjudicationRequestV1",
            ),
            (
                "work_duplicate_adjudication_command",
                "WorkDuplicateAdjudicationCommandV1",
            ),
            (
                "work_duplicate_adjudication_result",
                "WorkDuplicateAdjudicationAppendOutcomeV1",
            ),
            (
                "work_leak_adjudication_command",
                "AdjudicateWorkLeakCommandV1",
            ),
            (
                "work_leak_adjudication_result",
                "WorkLeakAdjudicationOutcomeV1",
            ),
        ] {
            assert!(
                definitions.contains_key(contract),
                "canonical Work contract {contract} is absent from the dashboard catalog"
            );
            assert_eq!(
                schema["properties"][field]["$ref"],
                format!("#/$defs/{contract}"),
                "dashboard catalog field {field} must directly register {contract}"
            );
        }
    }

    #[test]
    fn canonical_workflow_contracts_are_registered() {
        let schema: serde_json::Value = serde_json::from_str(
            &render_dashboard_contract_schema().expect("render validated dashboard contracts"),
        )
        .expect("parse dashboard contract schema");
        let definitions = schema["$defs"]
            .as_object()
            .expect("dashboard contracts expose schema definitions");

        for (field, contract) in [
            ("workflow_definition", "WorkflowDefinition"),
            (
                "workflow_definition_list_request",
                "WorkflowDefinitionListRequest",
            ),
            (
                "workflow_definition_get_request",
                "WorkflowDefinitionGetRequest",
            ),
            (
                "workflow_definition_history_request",
                "WorkflowDefinitionHistoryRequest",
            ),
            (
                "workflow_definition_activate_request",
                "WorkflowDefinitionActivateRequest",
            ),
            (
                "workflow_definition_retire_request",
                "WorkflowDefinitionRetireRequest",
            ),
            (
                "workflow_definition_reject_request",
                "WorkflowDefinitionRejectRequest",
            ),
            (
                "workflow_definition_disposition",
                "WorkflowDefinitionDisposition",
            ),
            ("workflow_run_get_request", "WorkflowRunGetRequest"),
            ("workflow_run_projection", "WorkflowRunProjection"),
        ] {
            assert!(
                definitions.contains_key(contract),
                "canonical Workflow contract {contract} is absent from the dashboard catalog"
            );
            assert_eq!(
                schema["properties"][field]["$ref"],
                format!("#/$defs/{contract}"),
                "dashboard catalog field {field} must directly register {contract}"
            );
        }

        for excluded in [
            "TaskHandoffIssueRequest",
            "TaskHandoffRedeemRequest",
            "WorkflowRunStartRequest",
            "WorkflowRunPauseRequest",
            "WorkflowRunResumeRequest",
            "WorkflowRunCancelRequest",
        ] {
            assert!(
                !definitions.contains_key(excluded),
                "{excluded} must not be published to the dashboard contract catalog"
            );
        }
    }

    #[test]
    fn legacy_dashboard_route_families_are_contracted() {
        let schema: serde_json::Value = serde_json::from_str(
            &render_dashboard_contract_schema().expect("render validated dashboard contracts"),
        )
        .expect("parse dashboard contract schema");
        let definitions = schema["$defs"]
            .as_object()
            .expect("dashboard contracts expose schema definitions");

        for response in [
            "ProjectsPayloadV1",
            "ProjectContextPayloadV1",
            "GraphOverviewPayloadV1",
            "GraphSearchPayloadV1",
            "GraphNodePayloadV1",
            "GraphNeighborsPayloadV1",
            "GraphSubgraphPayloadV1",
            "GraphPathPayloadV1",
            "MemoryOverviewPayloadV1",
            "MemoryStatusPayloadV1",
            "MemoryFactDetailPayloadV1",
            "MemoryFactRowV1",
            "MemoryEntityRowV1",
            "AnalyticsOverviewPayloadV1",
            "SavingsOverviewPayloadV1",
            "SavingsSessionsPayloadV1",
            "LcmSessionPayloadV1",
            "LcmTimelinePayloadV1",
            "LcmMessageV1",
            "LcmSummaryNodeV1",
            "LoomTemporalPayloadV1",
            "DeliveryOverviewV1",
            "ExplorerSessionSizeV1",
            "ExplorerReadContextV1",
        ] {
            assert!(
                definitions.contains_key(response),
                "dashboard response {response} is absent from the contract catalog"
            );
        }
    }

    fn schema_references(value: &serde_json::Value, definition: &str) -> bool {
        if value.get("$ref").and_then(serde_json::Value::as_str) == Some(definition) {
            return true;
        }
        match value {
            serde_json::Value::Array(values) => values
                .iter()
                .any(|value| schema_references(value, definition)),
            serde_json::Value::Object(fields) => fields
                .values()
                .any(|value| schema_references(value, definition)),
            _ => false,
        }
    }

    #[test]
    fn memory_overview_schema_uses_closed_canonical_graph_and_payload_states() {
        let schema: serde_json::Value = serde_json::from_str(
            &render_dashboard_contract_schema().expect("render validated dashboard contracts"),
        )
        .expect("parse dashboard contract schema");
        let definitions = schema["$defs"]
            .as_object()
            .expect("dashboard contracts expose schema definitions");

        for (contract, field, definition) in [
            (
                "MemoryFactRowV1",
                "payload_access",
                "#/$defs/PayloadAccessState",
            ),
            (
                "MemoryReadStatusV1",
                "state",
                "#/$defs/DashboardDomainStateV1",
            ),
            (
                "MemoryFactsCoverageV1",
                "graph",
                "#/$defs/FactSearchGraphCoverageV1",
            ),
            (
                "MemoryHolographicPayloadV1",
                "graph",
                "#/$defs/MemoryGraphPayloadV1",
            ),
            (
                "MemoryGraphEdgeV1",
                "kind",
                "#/$defs/ProjectMemoryGraphRelationKindV1",
            ),
            (
                "MemoryGraphPayloadV1",
                "coverage",
                "#/$defs/DashboardCoverageV1",
            ),
        ] {
            let field_schema = &definitions[contract]["properties"][field];
            assert!(
                schema_references(field_schema, definition),
                "{contract}.{field} must reference canonical {definition}, got {field_schema}"
            );
        }

        assert_eq!(
            definitions["PayloadAccessState"]["enum"],
            serde_json::json!([
                "eligible",
                "redacted",
                "quarantined",
                "retention_expired",
                "deleted",
                "unavailable",
                "ambiguous",
            ])
        );
        assert_eq!(
            definitions["ProjectMemoryGraphRelationKindV1"]["enum"],
            serde_json::json!([
                "supports",
                "contradicts",
                "supersedes",
                "derived_from",
                "mentions",
                "active_assertion",
                "evidence_anchor",
            ])
        );
    }

    #[test]
    fn memory_facts_coverage_schema_preserves_admitted_limit_range() {
        let schema: serde_json::Value = serde_json::from_str(
            &render_dashboard_contract_schema().expect("render validated dashboard contracts"),
        )
        .expect("parse dashboard contract schema");
        let limit = &schema["$defs"]["MemoryFactsCoverageV1"]["properties"]["limit"];

        assert_eq!(limit["minimum"], serde_json::json!(1));
        assert_eq!(limit["maximum"], serde_json::json!(100));
    }
}

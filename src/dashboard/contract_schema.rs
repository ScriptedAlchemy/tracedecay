//! Rust-owned schema bundle for the dashboard wire boundary.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;

use super::automation_scheduler_api::AutomationSchedulerStatusV1;
use super::code_index_freshness_api::CodeIndexFreshnessPayloadV1;
use super::doctor_findings_api::DoctorFindingsPayloadV1;
use super::doctor_remediation_api::{
    DoctorRemediationApplyRequestV1, DoctorRemediationPayloadV1, DoctorRemediationPreviewRequestV1,
};
use super::explorer_api::ExplorerQueryRunV1;
use super::graph_structure_api::{
    CallChainMeasurementV1, FactMatchesMeasurementV1, NodeSessionsMeasurementV1,
    StrataMeasurementV1, StructureReadV1, TestMapMeasurementV1, registered_route_contracts,
};
use super::read_model::{DASHBOARD_SCHEMA_REVISION_V1, DashboardEnvelopeV1};
use super::storage_findings_api::StorageFindingsPayloadV1;
use super::storage_telemetry_api::StorageTelemetryPayloadV1;

#[derive(JsonSchema)]
#[allow(dead_code)]
struct DashboardContractCatalogV1 {
    envelope: DashboardEnvelopeV1<DashboardPayloadMarkerV1>,
    storage_telemetry: StorageTelemetryPayloadV1,
    storage_findings: StorageFindingsPayloadV1,
    doctor_findings: DoctorFindingsPayloadV1,
    doctor_remediation_preview_request: DoctorRemediationPreviewRequestV1,
    doctor_remediation_apply_request: DoctorRemediationApplyRequestV1,
    doctor_remediation: DoctorRemediationPayloadV1,
    explorer_query_run: ExplorerQueryRunV1,
    code_index_freshness: CodeIndexFreshnessPayloadV1,
    graph_call_chain: StructureReadV1<CallChainMeasurementV1>,
    graph_strata: StructureReadV1<StrataMeasurementV1>,
    graph_fact_matches: StructureReadV1<FactMatchesMeasurementV1>,
    graph_test_map: StructureReadV1<TestMapMeasurementV1>,
    graph_node_sessions: StructureReadV1<NodeSessionsMeasurementV1>,
    /// Served identically by `GET /api/automation/scheduler/status` and by the
    /// `pause`/`resume` controls, which re-read rather than acknowledge.
    automation_scheduler_status: AutomationSchedulerStatusV1,
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
    serde_json::to_string_pretty(&schema).map(|rendered| format!("{rendered}\n"))
}

fn assert_registered_route_responses_are_contracted(schema: &serde_json::Value) {
    let definitions = schema["$defs"]
        .as_object()
        .expect("dashboard contracts must expose schema definitions");
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

#[cfg(test)]
mod tests {
    use super::render_dashboard_contract_schema;

    #[test]
    fn registered_dashboard_route_responses_are_contracted() {
        render_dashboard_contract_schema().expect("render validated dashboard contracts");
    }
}

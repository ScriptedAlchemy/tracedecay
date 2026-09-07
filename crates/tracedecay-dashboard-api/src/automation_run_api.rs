use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use super::DashboardState;
use super::util::http_detail;
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord, find_run_record,
    read_published_artifact_chain, read_run_artifact_payload,
};

#[derive(Debug, Default, Deserialize)]
pub struct RunListParams {
    limit: Option<i64>,
}

/// The newest automation runs from the ledger, projected to the fields the
/// run-history surface reads. Heavy per-run payloads (proposed/applied ops,
/// validation reports) stay behind the per-run artifact routes.
#[hotpath::measure(label = "dashboard_api.runs.list", future = true)]
pub async fn run_list(
    State(state): State<DashboardState>,
    axum::extract::Query(params): axum::extract::Query<RunListParams>,
) -> (StatusCode, Json<Value>) {
    let limit = super::util::coerce_limit(params.limit, 50, 200) as usize;
    // The locked ledger tail read is this route's only I/O; row projection
    // after it is linear in the (bounded) page.
    match hotpath::future!(
        tracedecay_automation_runtime::automation::run_ledger::load_run_records_page(
            &state.dashboard_root,
            limit,
        ),
        label = "dashboard_api.runs.ledger_read"
    )
    .await
    {
        Ok(page) => {
            let runs: Vec<Value> = page.records.iter().map(run_history_row).collect();
            let count = runs.len();
            let completeness = if page.is_complete() {
                "known"
            } else {
                "partial"
            };
            (
                StatusCode::OK,
                Json(json!({
                    "runs": runs,
                    "count": count,
                    "limit": limit,
                    "has_more": page.has_more,
                    "malformed_row_count": page.malformed_row_count,
                    "completeness": completeness,
                    "error": "",
                })),
            )
        }
        Err(err) => internal_error(&format!("Failed to read automation run ledger: {err}")),
    }
}

/// One ledger record as the run-history row: identity, outcome, review tallies,
/// and which artifacts exist — every field measured from the record itself.
fn run_history_row(record: &AutomationRunLedgerRecord) -> Value {
    json!({
        "run_id": record.run_id,
        "task": record.task,
        "trigger": record.trigger,
        "backend": record.backend,
        "model": record.model,
        "status": record.status,
        "reviewed_count": record.reviewed_count,
        "accepted_count": record.accepted_count,
        "rejected_count": record.rejected_count,
        "skipped_count": record.skipped_count,
        "error": record.error,
        "started_at": record.started_at,
        "completed_at": record.completed_at,
        "artifact_kinds": record
            .artifacts
            .iter()
            .map(|artifact| artifact.kind.clone())
            .collect::<Vec<_>>(),
    })
}

#[hotpath::measure(label = "dashboard_api.runs.artifacts", future = true)]
pub async fn artifact_list(
    State(state): State<DashboardState>,
    AxumPath(run_id): AxumPath<String>,
) -> (StatusCode, Json<Value>) {
    match find_run_record(&state.dashboard_root, &run_id).await {
        Ok(Some(record)) => {
            let count = record.artifacts.len();
            // Integrity verification re-reads the publication chain from disk
            // on every list call; measure it apart from the record lookup.
            let integrity = hotpath::future!(
                read_published_artifact_chain(&state.dashboard_root, &run_id, None),
                label = "dashboard_api.runs.chain_verify"
            )
            .await;
            let (integrity_status, integrity_verified) = match integrity {
                Ok(Some(published)) if published == record.artifacts => ("verified", true),
                Ok(Some(_)) => ("ledger_publication_mismatch", false),
                Ok(None) => ("publication_unavailable", false),
                Err(_) => ("verification_failed", false),
            };
            (
                StatusCode::OK,
                Json(json!({
                    "run_id": run_id,
                    "artifacts": record.artifacts,
                    "artifact_chain": artifact_chain_summary(
                        &record.artifacts,
                        integrity_status,
                        integrity_verified,
                    ),
                    "count": count,
                    "error": "",
                })),
            )
        }
        Ok(None) => not_found(&format!("automation run '{run_id}' not found")),
        Err(err) => internal_error(&format!("Failed to load automation run artifacts: {err}")),
    }
}

#[hotpath::measure(label = "dashboard_api.runs.artifact", future = true)]
pub async fn artifact_payload(
    State(state): State<DashboardState>,
    AxumPath((run_id, kind)): AxumPath<(String, String)>,
) -> (StatusCode, Json<Value>) {
    let record = match find_run_record(&state.dashboard_root, &run_id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return not_found(&format!("automation run '{run_id}' not found"));
        }
        Err(err) => {
            return internal_error(&format!("Failed to load automation run artifact: {err}"));
        }
    };
    let Some(artifact) = find_artifact(&record.artifacts, &kind) else {
        return not_found(&format!(
            "automation run artifact '{kind}' not found for run '{run_id}'"
        ));
    };
    // Heavy per-run payloads (proposed/applied ops, validation reports) are
    // read and parsed here; this span scales with artifact size while the
    // surrounding handler phases stay fixed-price.
    match hotpath::future!(
        read_run_artifact_payload(&state.dashboard_root, &run_id, artifact),
        label = "dashboard_api.runs.artifact_read"
    )
    .await
    {
        Ok(payload) => (
            StatusCode::OK,
            Json(json!({
                "run_id": run_id,
                "artifact": artifact,
                "payload": payload,
                "error": "",
            })),
        ),
        Err(err) => internal_error(&format!("Failed to read automation run artifact: {err}")),
    }
}

fn not_found(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(http_detail(message)))
}

fn internal_error(message: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(http_detail(message)),
    )
}

fn find_artifact<'a>(
    artifacts: &'a [AutomationRunArtifact],
    kind: &str,
) -> Option<&'a AutomationRunArtifact> {
    artifacts.iter().find(|artifact| artifact.kind == kind)
}

fn artifact_chain_summary(
    artifacts: &[AutomationRunArtifact],
    integrity_status: &str,
    integrity_verified: bool,
) -> Value {
    let expected_kinds = expected_artifact_chain_kinds();
    let present_kinds = artifacts
        .iter()
        .map(|artifact| artifact.kind.as_str())
        .collect::<Vec<_>>();
    let complete = expected_kinds
        .iter()
        .all(|expected| present_kinds.iter().any(|present| present == expected));
    json!({
        "expected_kinds": expected_kinds,
        "present_kinds": present_kinds,
        "metadata_complete": complete,
        "complete": complete && integrity_verified,
        "integrity_status": integrity_status,
    })
}

fn expected_artifact_chain_kinds() -> Vec<&'static str> {
    vec![
        AutomationRunArtifactKind::Traces.as_str(),
        AutomationRunArtifactKind::Feedback.as_str(),
        AutomationRunArtifactKind::GeneratedEvals.as_str(),
        AutomationRunArtifactKind::ValidationGate.as_str(),
        AutomationRunArtifactKind::OptimizerDiagnosis.as_str(),
        AutomationRunArtifactKind::CodexHandoff.as_str(),
    ]
}

#[cfg(test)]
mod run_list_tests {
    use super::*;

    #[test]
    fn run_history_row_projects_identity_outcome_and_artifact_kinds() {
        let record: AutomationRunLedgerRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "run_id": "run-1",
            "trigger": "scheduler",
            "task": "memory_curator",
            "backend": "claude",
            "status": "succeeded",
            "reviewed_count": 4,
            "accepted_count": 3,
            "rejected_count": 1,
            "error": "quota exhausted",
            "artifacts": [{
                "schema_version": 1,
                "kind": "traces",
                "path": "runs/run-1/traces.json",
                "sha256": "ab",
                "created_at": "1754000060"
            }],
            "started_at": "1754000000",
            "completed_at": "1754000060"
        }))
        .expect("ledger record fixture parses");

        let row = run_history_row(&record);
        assert_eq!(row["run_id"], json!("run-1"));
        assert_eq!(row["task"], json!("memory_curator"));
        assert_eq!(row["status"], json!("succeeded"));
        assert_eq!(row["accepted_count"], json!(3));
        assert_eq!(row["error"], json!("quota exhausted"));
        assert_eq!(row["artifact_kinds"], json!(["traces"]));
        // The heavy per-run payloads stay behind the artifact routes: a list
        // row must never carry proposed or applied operation bodies.
        assert!(row.get("proposed_ops").is_none());
        assert!(row.get("applied_ops").is_none());
    }
}

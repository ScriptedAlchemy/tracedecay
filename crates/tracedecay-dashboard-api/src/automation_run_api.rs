use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_automation::backend::{AgentTaskFailureClass, AgentTaskKind};

use super::DashboardState;
use super::util::{internal_error, json_error};
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifact, AutomationRunArtifactKind, AutomationRunLedgerRecord,
    AutomationRunStatus, AutomationTrigger, find_run_record, read_published_artifact_chain,
    read_run_artifact_payload,
};

/// One ledger record as the run-history row. `task_key` is the exact per-job
/// identity (`user_job:<id>`); rows written before it existed carry `null` and
/// cannot be joined to a job.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AutomationRunRowV1 {
    run_id: String,
    task: AgentTaskKind,
    task_key: Option<String>,
    trigger: AutomationTrigger,
    backend: String,
    model: Option<String>,
    status: AutomationRunStatus,
    reviewed_count: usize,
    accepted_count: usize,
    rejected_count: usize,
    skipped_count: usize,
    error: Option<String>,
    error_classification: Option<AgentTaskFailureClass>,
    error_retryable: Option<bool>,
    backend_attempt_count: usize,
    started_at: String,
    completed_at: String,
    artifact_kinds: Vec<String>,
}

/// `known` only when the ledger page holds every row and none was malformed.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutomationRunLedgerCompletenessV1 {
    Known,
    Partial,
}

/// `GET /api/automation/runs`, newest first under `limit`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AutomationRunsPayloadV1 {
    runs: Vec<AutomationRunRowV1>,
    count: usize,
    limit: usize,
    has_more: bool,
    malformed_row_count: usize,
    completeness: AutomationRunLedgerCompletenessV1,
}

/// Whether the ledger's artifact list matches the published artifact chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutomationRunArtifactIntegrityV1 {
    Verified,
    LedgerPublicationMismatch,
    PublicationUnavailable,
    VerificationFailed,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AutomationRunArtifactChainV1 {
    expected_kinds: Vec<AutomationRunArtifactKind>,
    present_kinds: Vec<String>,
    metadata_complete: bool,
    /// Every expected kind is present and the chain verified.
    complete: bool,
    integrity_status: AutomationRunArtifactIntegrityV1,
}

/// `GET /api/automation/runs/{id}/artifacts`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AutomationRunArtifactsPayloadV1 {
    run_id: String,
    artifacts: Vec<AutomationRunArtifact>,
    artifact_chain: AutomationRunArtifactChainV1,
    count: usize,
}

/// `GET /api/automation/runs/{id}/artifacts/{kind}`. The artifact kind owns its
/// payload shape, so it is served as opaque JSON.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AutomationRunArtifactPayloadV1 {
    run_id: String,
    artifact: AutomationRunArtifact,
    payload: Value,
}

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
) -> Response {
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
            let runs: Vec<_> = page.records.iter().map(run_history_row).collect();
            Json(AutomationRunsPayloadV1 {
                count: runs.len(),
                runs,
                limit,
                has_more: page.has_more,
                malformed_row_count: page.malformed_row_count,
                completeness: if page.is_complete() {
                    AutomationRunLedgerCompletenessV1::Known
                } else {
                    AutomationRunLedgerCompletenessV1::Partial
                },
            })
            .into_response()
        }
        Err(err) => {
            internal_error(format!("Failed to read automation run ledger: {err}")).into_response()
        }
    }
}

fn run_history_row(record: &AutomationRunLedgerRecord) -> AutomationRunRowV1 {
    AutomationRunRowV1 {
        run_id: record.run_id.clone(),
        task: record.task,
        task_key: record.task_key.clone(),
        trigger: record.trigger,
        backend: record.backend.clone(),
        model: record.model.clone(),
        status: record.status,
        reviewed_count: record.reviewed_count,
        accepted_count: record.accepted_count,
        rejected_count: record.rejected_count,
        skipped_count: record.skipped_count,
        error: record.error.clone(),
        error_classification: record.error_classification,
        error_retryable: record.error_retryable,
        backend_attempt_count: record.backend_attempt_count,
        started_at: record.started_at.clone(),
        completed_at: record.completed_at.clone(),
        artifact_kinds: record
            .artifacts
            .iter()
            .map(|artifact| artifact.kind.clone())
            .collect(),
    }
}

#[hotpath::measure(label = "dashboard_api.runs.artifacts", future = true)]
pub async fn artifact_list(
    State(state): State<DashboardState>,
    AxumPath(run_id): AxumPath<String>,
) -> Response {
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
            let integrity_status = match integrity {
                Ok(Some(published)) if published == record.artifacts => {
                    AutomationRunArtifactIntegrityV1::Verified
                }
                Ok(Some(_)) => AutomationRunArtifactIntegrityV1::LedgerPublicationMismatch,
                Ok(None) => AutomationRunArtifactIntegrityV1::PublicationUnavailable,
                Err(_) => AutomationRunArtifactIntegrityV1::VerificationFailed,
            };
            Json(AutomationRunArtifactsPayloadV1 {
                artifact_chain: artifact_chain_summary(&record.artifacts, integrity_status),
                run_id,
                artifacts: record.artifacts,
                count,
            })
            .into_response()
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            format!("automation run '{run_id}' not found"),
        )
        .into_response(),
        Err(err) => {
            internal_error(format!("Failed to load automation run artifacts: {err}")).into_response()
        }
    }
}

#[hotpath::measure(label = "dashboard_api.runs.artifact", future = true)]
pub async fn artifact_payload(
    State(state): State<DashboardState>,
    AxumPath((run_id, kind)): AxumPath<(String, String)>,
) -> Response {
    let record = match find_run_record(&state.dashboard_root, &run_id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return json_error(
                StatusCode::NOT_FOUND,
                format!("automation run '{run_id}' not found"),
            )
            .into_response();
        }
        Err(err) => {
            return internal_error(format!("Failed to load automation run artifact: {err}"))
                .into_response();
        }
    };
    let Some(artifact) = find_artifact(&record.artifacts, &kind) else {
        return json_error(
            StatusCode::NOT_FOUND,
            format!("automation run artifact '{kind}' not found for run '{run_id}'"),
        )
        .into_response();
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
        Ok(payload) => Json(AutomationRunArtifactPayloadV1 {
            run_id,
            artifact: artifact.clone(),
            payload,
        })
        .into_response(),
        Err(err) => {
            internal_error(format!("Failed to read automation run artifact: {err}")).into_response()
        }
    }
}

fn find_artifact<'a>(
    artifacts: &'a [AutomationRunArtifact],
    kind: &str,
) -> Option<&'a AutomationRunArtifact> {
    artifacts.iter().find(|artifact| artifact.kind == kind)
}

const EXPECTED_ARTIFACT_CHAIN_KINDS: [AutomationRunArtifactKind; 6] = [
    AutomationRunArtifactKind::Traces,
    AutomationRunArtifactKind::Feedback,
    AutomationRunArtifactKind::GeneratedEvals,
    AutomationRunArtifactKind::ValidationGate,
    AutomationRunArtifactKind::OptimizerDiagnosis,
    AutomationRunArtifactKind::CodexHandoff,
];

fn artifact_chain_summary(
    artifacts: &[AutomationRunArtifact],
    integrity_status: AutomationRunArtifactIntegrityV1,
) -> AutomationRunArtifactChainV1 {
    let present_kinds: Vec<String> = artifacts
        .iter()
        .map(|artifact| artifact.kind.clone())
        .collect();
    let metadata_complete = EXPECTED_ARTIFACT_CHAIN_KINDS
        .iter()
        .all(|expected| present_kinds.iter().any(|present| present == expected.as_str()));
    AutomationRunArtifactChainV1 {
        expected_kinds: EXPECTED_ARTIFACT_CHAIN_KINDS.to_vec(),
        present_kinds,
        metadata_complete,
        complete: metadata_complete
            && integrity_status == AutomationRunArtifactIntegrityV1::Verified,
        integrity_status,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(value: Value) -> AutomationRunLedgerRecord {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn run_history_row_carries_job_identity_and_typed_failure_fields() {
        let row = run_history_row(&record(json!({
            "schema_version": 2,
            "run_id": "dashboard_user_job_nightly_1",
            "trigger": "scheduler",
            "task": "user_job",
            "task_key": "user_job:nightly",
            "backend": "codex_app_server",
            "status": "failed",
            "accepted_count": 0,
            "rejected_count": 0,
            "error": "provider lease expired",
            "error_classification": "retryable",
            "error_retryable": true,
            "backend_attempt_count": 2,
            "started_at": "1754000000",
            "completed_at": "1754000031",
        })));

        assert_eq!(row.task_key.as_deref(), Some("user_job:nightly"));
        assert_eq!(
            row.error_classification,
            Some(AgentTaskFailureClass::Retryable)
        );
        assert_eq!(row.error_retryable, Some(true));
        assert_eq!(row.backend_attempt_count, 2);
        assert!(row.artifact_kinds.is_empty());
    }

    #[test]
    fn run_history_row_keeps_absent_identity_and_failure_fields_null() {
        let row = run_history_row(&record(json!({
            "schema_version": 2,
            "run_id": "legacy_run",
            "trigger": "manual_cli",
            "task": "memory_curator",
            "backend": "claude",
            "status": "succeeded",
            "accepted_count": 1,
            "rejected_count": 0,
            "started_at": "1754000000",
            "completed_at": "1754000060",
        })));

        // A pre-`task_key` row must not be joined to any job, and an absent
        // failure classification is an absence rather than a default class.
        assert_eq!(row.task_key, None);
        assert_eq!(row.error_classification, None);
        assert_eq!(row.error_retryable, None);
        assert_eq!(row.backend_attempt_count, 0);
    }
}

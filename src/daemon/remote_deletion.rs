//! Authenticated remote account/project deletion wire contract.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracedecay_domain::ProjectId;

use super::{StoreAdministration, http_application::DaemonHttpApplicationRegistry};
pub(super) use crate::global_db::{RemoteDeletionFailureCode, RemoteDeletionPhase};

const MAX_REMOTE_DELETION_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub(super) struct RemoteDeletionRuntimeOwners {
    pub(super) administration: StoreAdministration,
    pub(super) invocation: super::DaemonInvocationState,
    pub(super) project_open_gates: std::sync::Arc<tokio::sync::Mutex<super::ProjectOpenGates>>,
}

pub(super) enum RemoteDeletionBootMode {
    Ordinary,
    DeletionOnly(RemoteDeletionReceipt),
}

pub(super) async fn resume_remote_account_deletion_for_boot(
    owners: &RemoteDeletionRuntimeOwners,
) -> crate::errors::Result<RemoteDeletionBootMode> {
    let Some(tombstone) = owners
        .administration
        .remote_account_deletion_tombstone()
        .await?
    else {
        return Ok(RemoteDeletionBootMode::Ordinary);
    };
    match owners
        .administration
        .execute_remote_deletion(
            owners,
            RemoteDeletionReceiptTarget::Account,
            None,
            tombstone.tombstone_id,
        )
        .await
    {
        Ok(receipt) => Ok(RemoteDeletionBootMode::DeletionOnly(receipt)),
        Err(error) if error.receipt.tombstone_recorded => {
            Ok(RemoteDeletionBootMode::DeletionOnly(error.receipt))
        }
        Err(error) => Err(error.source),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum RemoteDeletionHttpTarget {
    Account,
    Project { project_id: String },
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct RemoteDeletionHttpRequest {
    #[serde(flatten)]
    pub(super) target: RemoteDeletionHttpTarget,
    pub(super) tombstone_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RemoteDeletionReceiptTarget {
    Account,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RemoteDeletionStatus {
    Deleted,
    Settling,
    Partial,
    Denied,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct RemoteDeletionFailure {
    pub(super) code: RemoteDeletionFailureCode,
    pub(super) phase: RemoteDeletionPhase,
    pub(super) retryable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct RemoteDeletionReceipt {
    pub(super) status: RemoteDeletionStatus,
    pub(super) target: Option<RemoteDeletionReceiptTarget>,
    pub(super) profile_id: Option<String>,
    pub(super) tombstone_id: Option<String>,
    pub(super) project_id: Option<String>,
    pub(super) tombstone_recorded: bool,
    pub(super) removed_project_ids: Vec<String>,
    pub(super) pending_project_ids: Vec<String>,
    pub(super) failure: Option<RemoteDeletionFailure>,
}

impl RemoteDeletionReceipt {
    pub(super) fn pending(
        target: RemoteDeletionReceiptTarget,
        profile_id: Option<String>,
        tombstone_id: String,
        project_id: Option<String>,
    ) -> Self {
        let pending_project_ids = project_id.iter().cloned().collect();
        Self {
            status: RemoteDeletionStatus::Failed,
            target: Some(target),
            profile_id,
            tombstone_id: Some(tombstone_id),
            project_id,
            tombstone_recorded: false,
            removed_project_ids: Vec::new(),
            pending_project_ids,
            failure: None,
        }
    }

    pub(super) fn complete(mut self) -> Self {
        self.status = RemoteDeletionStatus::Deleted;
        self.pending_project_ids.clear();
        self.failure = None;
        self
    }

    fn invalid_request() -> Self {
        Self {
            status: RemoteDeletionStatus::Failed,
            target: None,
            profile_id: None,
            tombstone_id: None,
            project_id: None,
            tombstone_recorded: false,
            removed_project_ids: Vec::new(),
            pending_project_ids: Vec::new(),
            failure: Some(RemoteDeletionFailure {
                code: RemoteDeletionFailureCode::InvalidRequest,
                phase: RemoteDeletionPhase::ValidateRequest,
                retryable: false,
            }),
        }
    }

    fn authority_unavailable(request: RemoteDeletionHttpRequest) -> Self {
        let (target, project_id) = match request.target {
            RemoteDeletionHttpTarget::Account => (RemoteDeletionReceiptTarget::Account, None),
            RemoteDeletionHttpTarget::Project { project_id } => {
                (RemoteDeletionReceiptTarget::Project, Some(project_id))
            }
        };
        Self {
            status: RemoteDeletionStatus::Failed,
            target: Some(target),
            profile_id: None,
            tombstone_id: Some(request.tombstone_id),
            project_id,
            tombstone_recorded: false,
            removed_project_ids: Vec::new(),
            pending_project_ids: Vec::new(),
            failure: Some(RemoteDeletionFailure {
                code: RemoteDeletionFailureCode::AuthorityUnavailable,
                phase: RemoteDeletionPhase::ResolveAuthority,
                retryable: true,
            }),
        }
    }

    fn http_status(&self) -> StatusCode {
        match self.status {
            RemoteDeletionStatus::Deleted => StatusCode::OK,
            RemoteDeletionStatus::Settling | RemoteDeletionStatus::Partial => StatusCode::CONFLICT,
            RemoteDeletionStatus::Denied => StatusCode::NOT_FOUND,
            RemoteDeletionStatus::Failed => match self.failure.as_ref().map(|failure| failure.code)
            {
                Some(RemoteDeletionFailureCode::InvalidRequest) => StatusCode::BAD_REQUEST,
                Some(RemoteDeletionFailureCode::TombstoneConflict) => StatusCode::CONFLICT,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            },
        }
    }
}

pub(super) async fn dispatch_remote_deletion(
    State(registry): State<DaemonHttpApplicationRegistry>,
    request: Request<Body>,
) -> Response {
    let receipt = match parse_remote_deletion_request(request).await {
        Ok(request) => match registry.remote_deletion_runtime_owners() {
            Ok(Some(owners)) => match execute_remote_deletion(&owners, request).await {
                Ok(receipt) => receipt,
                Err(error) => {
                    tracing::warn!(error = %error.source, "remote deletion request failed");
                    error.receipt
                }
            },
            Ok(None) | Err(_) => RemoteDeletionReceipt::authority_unavailable(request),
        },
        Err(receipt) => receipt,
    };
    if receipt.tombstone_recorded
        && let Some(target) = receipt.target
    {
        registry
            .forget_remote_deleted_routes(target, receipt.project_id.as_deref())
            .await;
    }
    (receipt.http_status(), axum::Json(receipt)).into_response()
}

#[derive(Debug)]
pub(super) struct RemoteDeletionExecutionError {
    pub(super) receipt: RemoteDeletionReceipt,
    pub(super) source: crate::errors::TraceDecayError,
}

impl RemoteDeletionExecutionError {
    pub(super) fn new(
        mut receipt: RemoteDeletionReceipt,
        code: RemoteDeletionFailureCode,
        phase: RemoteDeletionPhase,
        retryable: bool,
        source: crate::errors::TraceDecayError,
    ) -> Self {
        receipt.status = if receipt.tombstone_recorded {
            if matches!(
                code,
                RemoteDeletionFailureCode::RuntimeOwnersSettling
                    | RemoteDeletionFailureCode::RuntimeRetirementIncomplete
            ) {
                RemoteDeletionStatus::Settling
            } else {
                RemoteDeletionStatus::Partial
            }
        } else if code == RemoteDeletionFailureCode::TargetNotFound {
            RemoteDeletionStatus::Denied
        } else {
            RemoteDeletionStatus::Failed
        };
        receipt.failure = Some(RemoteDeletionFailure {
            code,
            phase,
            retryable,
        });
        Self { receipt, source }
    }
}

async fn execute_remote_deletion(
    owners: &RemoteDeletionRuntimeOwners,
    request: RemoteDeletionHttpRequest,
) -> Result<RemoteDeletionReceipt, RemoteDeletionExecutionError> {
    let (target, project_id) = match request.target {
        RemoteDeletionHttpTarget::Account => (RemoteDeletionReceiptTarget::Account, None),
        RemoteDeletionHttpTarget::Project { project_id } => {
            ProjectId::new(project_id.clone()).map_err(|error| {
                RemoteDeletionExecutionError::new(
                    RemoteDeletionReceipt::pending(
                        RemoteDeletionReceiptTarget::Project,
                        None,
                        request.tombstone_id.clone(),
                        Some(project_id.clone()),
                    ),
                    RemoteDeletionFailureCode::InvalidRequest,
                    RemoteDeletionPhase::ValidateRequest,
                    false,
                    crate::errors::TraceDecayError::Config {
                        message: format!("remote deletion project identity is invalid: {error}"),
                    },
                )
            })?;
            (RemoteDeletionReceiptTarget::Project, Some(project_id))
        }
    };
    owners
        .administration
        .execute_remote_deletion(owners, target, project_id, request.tombstone_id)
        .await
}

async fn parse_remote_deletion_request(
    request: Request<Body>,
) -> Result<RemoteDeletionHttpRequest, RemoteDeletionReceipt> {
    if !has_json_content_type(request.headers()) {
        return Err(RemoteDeletionReceipt::invalid_request());
    }
    let body = to_bytes(request.into_body(), MAX_REMOTE_DELETION_BODY_BYTES)
        .await
        .map_err(|_| RemoteDeletionReceipt::invalid_request())?;
    serde_json::from_slice(&body).map_err(|_| RemoteDeletionReceipt::invalid_request())
}

fn has_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
}

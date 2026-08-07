//! Authenticated remote account/project deletion wire contract.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::http_application::DaemonHttpApplicationRegistry;

const MAX_REMOTE_DELETION_BODY_BYTES: usize = 16 * 1024;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RemoteDeletionPhase {
    ValidateRequest,
    ResolveAuthority,
    ResolveTarget,
    PersistTombstone,
    CancelRuntimeOwners,
    RemoveShard,
    RemoveRegistryEntry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RemoteDeletionFailureCode {
    InvalidRequest,
    AuthorityUnavailable,
    TargetNotFound,
    TombstoneConflict,
    TombstoneUnavailable,
    RuntimeOwnersSettling,
    ShardCleanupFailed,
    RegistryCleanupFailed,
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
        Ok(request) => match registry.remote_deletion_executor() {
            Ok(Some(executor)) => executor(request).await,
            Ok(None) | Err(_) => RemoteDeletionReceipt::authority_unavailable(request),
        },
        Err(receipt) => receipt,
    };
    (receipt.http_status(), axum::Json(receipt)).into_response()
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

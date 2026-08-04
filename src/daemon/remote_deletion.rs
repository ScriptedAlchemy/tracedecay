//! Authenticated remote deletion endpoint and its typed receipts.
//!
//! The route is mounted on the daemon's existing loopback listener. It never
//! accepts storage paths or a profile selector: the listener's daemon-owned
//! profile authority supplies those exact identities.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracedecay_domain::ProjectId;

use super::{StoreAdministration, http_application::DaemonHttpApplicationRegistry};

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum RemoteDeletionHttpTarget {
    Account,
    Project { project_id: String },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteDeletionHttpRequest {
    #[serde(flatten)]
    pub(super) target: RemoteDeletionHttpTarget,
    pub(super) tombstone_id: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RemoteDeletionReceiptTarget {
    Account,
    Project,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct RemoteDeletionReceipt {
    pub(super) status: &'static str,
    pub(super) target: RemoteDeletionReceiptTarget,
    pub(super) profile_id: String,
    pub(super) tombstone_id: String,
    pub(super) project_id: Option<String>,
    pub(super) removed_project_count: usize,
}

pub(super) async fn dispatch_remote_deletion(
    State(registry): State<DaemonHttpApplicationRegistry>,
    Json(request): Json<RemoteDeletionHttpRequest>,
) -> Response {
    let administration = match registry.remote_deletion_administration() {
        Ok(Some(administration)) => administration,
        Ok(None) => return remote_deletion_problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        Err(error) => {
            tracing::warn!(%error, "remote deletion route administration is unavailable");
            return remote_deletion_problem(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        }
    };
    match execute_remote_deletion(&administration, request).await {
        Ok(receipt) => (StatusCode::OK, Json(receipt)).into_response(),
        Err(error) => {
            tracing::warn!(%error, "remote deletion request failed");
            remote_deletion_problem(StatusCode::CONFLICT, "deletion_failed")
        }
    }
}

async fn execute_remote_deletion(
    administration: &StoreAdministration,
    request: RemoteDeletionHttpRequest,
) -> crate::errors::Result<RemoteDeletionReceipt> {
    let (target, project_id) = match request.target {
        RemoteDeletionHttpTarget::Account => (RemoteDeletionReceiptTarget::Account, None),
        RemoteDeletionHttpTarget::Project { project_id } => {
            ProjectId::new(project_id.clone()).map_err(|error| {
                crate::errors::TraceDecayError::Config {
                    message: format!("remote deletion project identity is invalid: {error}"),
                }
            })?;
            (RemoteDeletionReceiptTarget::Project, Some(project_id))
        }
    };
    administration
        .execute_remote_deletion(target, project_id, request.tombstone_id)
        .await
}

fn remote_deletion_problem(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "status": "failed",
            "code": code,
        })),
    )
        .into_response()
}

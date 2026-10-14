//! Operator revocation of a Remote Brain node's enrollments.
//!
//! The operator names the node they provisioned. Every enrollment that node
//! holds is revoked durably; the node's later capture, transfer, and replay
//! requests are refused with `EnrollmentRevoked` at credential admission.
//! Grants the node never redeemed are spent, so they can no longer enroll it.

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{BrainNodeId, CredentialRevocationReceiptV1, EntityId};
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_rusqlite_runtime::remote::RemoteSqliteStorageErrorV1;

use super::http_application::{
    DaemonHttpApplicationRegistry, live_daemon_unavailable, live_local_http,
};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteNodeRevocationRequestV1 {
    node_id: BrainNodeId,
}

/// One receipt per enrollment the node holds. Revoking an already revoked
/// node returns its original receipts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeRevocationV1 {
    pub revocations: Vec<CredentialRevocationReceiptV1>,
    /// Grants this request spent before the node redeemed them. A repeat
    /// finds none left.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked_grants: Vec<EntityId>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RemoteNodeRevocationErrorV1 {
    NodeNotRegistered,
    Conflict,
    Corruption,
    Unavailable,
}

impl IntoResponse for RemoteNodeRevocationErrorV1 {
    fn into_response(self) -> Response {
        let status = match self {
            Self::NodeNotRegistered => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Corruption => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        (status, Json(serde_json::json!({ "code": self }))).into_response()
    }
}

#[tracing::instrument(
    name = "daemon.http.application.remote_node_revoke",
    level = "trace",
    skip_all
)]
pub(super) async fn revoke_remote_node(
    State(registry): State<DaemonHttpApplicationRegistry>,
    Json(request): Json<RemoteNodeRevocationRequestV1>,
) -> Response {
    let Some(runtime) = registry.remote_runtime() else {
        return RemoteNodeRevocationErrorV1::Unavailable.into_response();
    };
    let storage = match runtime
        .remote_credential_authority()
        .registered_node_storage(&request.node_id)
    {
        Ok(Some(storage)) => storage,
        Ok(None) => return RemoteNodeRevocationErrorV1::NodeNotRegistered.into_response(),
        Err(_) => return RemoteNodeRevocationErrorV1::Unavailable.into_response(),
    };
    let node_id = request.node_id.clone();
    let revoked = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        let revoked_at = tracedecay_contracts::clock::now_micros();
        // Grants first: an enrollment that consumes a grant concurrently is
        // then already committed when the node's enrollments are revoked.
        let revoked_grants = storage.revoke_node_grants(&node_id, revoked_at)?;
        let revocations = storage.revoke_node_enrollments(&node_id, revoked_at)?;
        Ok::<_, RemoteSqliteStorageErrorV1>(RemoteNodeRevocationV1 {
            revocations,
            revoked_grants,
        })
    })
    .await;
    match revoked {
        Ok(Ok(revocation)) => {
            if let Err(error) = runtime
                .remote_credential_authority()
                .refresh_storage(&request.node_id)
            {
                // The spent grants already fail lookup; only their stale
                // routing entries remain until the next refresh.
                tracing::warn!(%error, "remote credential index refresh after revocation failed");
            }
            Json(revocation).into_response()
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "remote node revocation refused");
            match error {
                RemoteSqliteStorageErrorV1::Conflict => RemoteNodeRevocationErrorV1::Conflict,
                RemoteSqliteStorageErrorV1::Corruption
                | RemoteSqliteStorageErrorV1::BindingMismatch => {
                    RemoteNodeRevocationErrorV1::Corruption
                }
                _ => RemoteNodeRevocationErrorV1::Unavailable,
            }
            .into_response()
        }
        Err(_) => RemoteNodeRevocationErrorV1::Unavailable.into_response(),
    }
}

/// Revokes `node_id` through the running daemon's authenticated loopback HTTP
/// application. Never opens a local store.
pub fn live_remote_node_revocation(
    profile: &ProfileRoot,
    node_id: &BrainNodeId,
) -> Result<RemoteNodeRevocationV1> {
    let (origin, auth_token, agent) = live_local_http(profile)?;
    let mut response = agent
        .post(&format!("{origin}/remote-nodes/revoke"))
        .header("Authorization", format!("Bearer {auth_token}"))
        .header("Origin", origin)
        .send_json(RemoteNodeRevocationRequestV1 {
            node_id: node_id.clone(),
        })
        .map_err(|_| live_daemon_unavailable(profile))?;
    let status = response.status().as_u16();
    if status != 200 {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        return Err(TraceDecayError::tool_refused(
            format!("remote revoke {node_id}"),
            serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value["code"].as_str().map(str::to_owned)),
            Some(format!("TraceDecay daemon answered HTTP {status}")),
        ));
    }
    response
        .body_mut()
        .read_json()
        .map_err(|error| TraceDecayError::Config {
            message: format!("TraceDecay daemon revocation was not a typed receipt list: {error}"),
        })
}

//! Operator drain: a joined node hands its pending frames, in capture
//! sequence order, to the owning authority over the authority's trust-rooted
//! channel, and releases each frame only after the authority's receipt.

use std::time::Duration;

use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracedecay_contracts::RequestId;
use tracedecay_contracts::remote::auth::OpaqueRemoteCredential;
use tracedecay_contracts::remote::credential_admission::RemoteCredentialUseV1;
use tracedecay_contracts::remote::discovery::RemoteAuthorityDiscoveryV1;
use tracedecay_contracts::remote::protocol::RemoteProtocolRequestV1;
use tracedecay_contracts::remote::transfer::RemoteFrameTransferReceiptV1;
use tracedecay_domain::errors::Result;
use tracedecay_store_runtime::remote_credentials::presented_spool_keyring;
use tracedecay_store_runtime::{DaemonRemoteCredentialAuthorityV1, RemoteNodeOperatorErrorV1};

use super::http_application::{DaemonHttpApplicationRegistry, remote_node_operator_refusal};
use super::remote_join::{
    RemoteNodeJoinRequestV1, authority_call_failure, enrollment_credential,
    live_remote_node_operation, record_unreachable, refresh_membership, remote_client,
};

/// The authority's receipt for each frame one drain released, in sequence
/// order. An empty list means nothing was pending.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeDrainReceiptV1 {
    pub transferred: Vec<RemoteFrameTransferReceiptV1>,
}

#[tracing::instrument(
    name = "daemon.http.application.remote_node_drain",
    level = "trace",
    skip_all
)]
pub(super) async fn drain_remote_node(
    State(registry): State<DaemonHttpApplicationRegistry>,
    headers: HeaderMap,
    Json(request): Json<RemoteNodeJoinRequestV1>,
) -> Response {
    let Some(runtime) = registry.remote_runtime() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(credential) = enrollment_credential(&headers) else {
        return remote_node_operator_refusal(RemoteNodeOperatorErrorV1::InvalidRequest);
    };
    let discovery = match refresh_membership(&runtime, &request, credential.clone()).await {
        Ok(discovery) => discovery,
        Err(error) => return remote_node_operator_refusal(error),
    };
    let credentials = runtime.remote_credential_authority();
    let drained = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        drain_pending_frames(&credentials, &request, &credential, &discovery)
    })
    .await
    .unwrap_or(Err(RemoteNodeOperatorErrorV1::Unavailable));
    match drained {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => remote_node_operator_refusal(error),
    }
}

/// Transfers every pending frame of the node `discovery` names, lowest
/// sequence first, stopping at the first frame the authority does not
/// acknowledge so the authority never sees a sequence gap.
fn drain_pending_frames(
    credentials: &DaemonRemoteCredentialAuthorityV1,
    request: &RemoteNodeJoinRequestV1,
    credential: &[u8],
    discovery: &RemoteAuthorityDiscoveryV1,
) -> std::result::Result<RemoteNodeDrainReceiptV1, RemoteNodeOperatorErrorV1> {
    let enrollment = &discovery.enrollment.enrollment;
    let presented = OpaqueRemoteCredential::new(credential.to_vec())
        .map_err(|_| RemoteNodeOperatorErrorV1::InvalidRequest)?;
    let registered = credentials
        .storage_for_presented(RemoteCredentialUseV1::CaptureOffline, &presented)
        .map_err(|_| RemoteNodeOperatorErrorV1::Unavailable)?;
    if !registered.storage.is_joined() || registered.node_id != enrollment.node_id {
        return Err(RemoteNodeOperatorErrorV1::InvalidRequest);
    }
    let keyring = presented_spool_keyring(&presented, enrollment.revision)
        .ok_or(RemoteNodeOperatorErrorV1::InvalidRequest)?;
    let storage = registered.storage.with_keyring(keyring);
    let client = remote_client(request, credential)?;
    let persistence = |error| {
        tracing::warn!(%error, "remote drain persistence failed");
        RemoteNodeOperatorErrorV1::Unavailable
    };
    let mut transferred = Vec::new();
    for event_id in storage
        .pending_drain_frames(&enrollment.enrollment_id)
        .map_err(persistence)?
    {
        let sent_at = tracedecay_contracts::clock::now_micros();
        let expires_at = sent_at.0.saturating_add(
            i64::try_from(Duration::from_secs(request.timeout_secs).as_micros())
                .unwrap_or(i64::MAX),
        );
        let frame = storage
            .export_frame_transfer(&event_id, expires_at)
            .map_err(persistence)?;
        let transfer = RemoteProtocolRequestV1::new(
            RequestId::new(format!(
                "request.remote-drain.{}.{}",
                frame.enrollment_revision, frame.sequence.sequence
            ))
            .map_err(|_| RemoteNodeOperatorErrorV1::InvalidRequest)?,
            enrollment.brain_id.clone(),
            enrollment.node_id.clone(),
            enrollment.revision,
            Some(frame.writer.authority.fence.clone()),
            sent_at,
            frame,
        )
        .map_err(|_| RemoteNodeOperatorErrorV1::InvalidRequest)?;
        let response = client.transfer_frame(&transfer).map_err(|error| {
            tracing::warn!(%error, "remote frame transfer failed during drain");
            match authority_call_failure(&error) {
                RemoteNodeOperatorErrorV1::AuthorityUnreachable => {
                    record_unreachable(credentials, &presented, &request.discovery)
                }
                failure => failure,
            }
        })?;
        let receipt = response
            .result
            .ok()
            .and_then(|envelope| envelope.outcome.payload().cloned())
            .ok_or(RemoteNodeOperatorErrorV1::AuthorityUnavailable)?;
        storage
            .acknowledge_drained_frame(&receipt)
            .map_err(persistence)?;
        transferred.push(receipt);
    }
    Ok(RemoteNodeDrainReceiptV1 { transferred })
}

/// Drain contract for the Remote Brain operator CLI: the live daemon, not the
/// CLI, talks to the authority and releases what it acknowledged.
pub fn live_remote_drain(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    request: &RemoteNodeJoinRequestV1,
    enrollment_credential: &[u8],
) -> Result<RemoteNodeDrainReceiptV1> {
    // ponytail: no overall deadline; each authority call is bounded by
    // `timeout_secs`, add a drain budget when spools grow large.
    live_remote_node_operation(profile, "drain", request, enrollment_credential, None)
}

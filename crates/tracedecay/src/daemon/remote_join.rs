//! Operator join: this profile becomes an enrolled node of another profile's
//! Brain, from what the authority itself answers to the node's credential.

use std::time::Duration;

use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tracedecay_api::remote::REMOTE_ENROLLMENT_CREDENTIAL_HEADER;
use tracedecay_contracts::ApplicationOutcome;
use tracedecay_contracts::remote::auth::OpaqueRemoteCredential;
use tracedecay_contracts::remote::discovery::RemoteAuthorityDiscoveryV1;
use tracedecay_contracts::remote::protocol::{CurrentAuthorityRequestV1, RemoteProtocolRequestV1};
use tracedecay_domain::RemoteCredentialFingerprintV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_sdk::remote_client::{EnrolledRemoteClient, RemoteClientError};
use tracedecay_store_runtime::RemoteNodeOperatorErrorV1;

use super::http_application::{DaemonHttpApplicationRegistry, remote_node_operator_refusal};

/// The authority to discover and the enrolled identity to discover it for.
/// The enrollment credential travels only in the
/// `x-tracedecay-enrollment-credential` header.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeJoinRequestV1 {
    /// The authority's `https://.../remote/` endpoint.
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_root_pem: Option<String>,
    pub timeout_secs: u64,
    pub discovery: RemoteProtocolRequestV1<CurrentAuthorityRequestV1>,
}

#[tracing::instrument(
    name = "daemon.http.application.remote_node_join",
    level = "trace",
    skip_all
)]
pub(super) async fn join_remote_node(
    State(registry): State<DaemonHttpApplicationRegistry>,
    headers: HeaderMap,
    Json(request): Json<RemoteNodeJoinRequestV1>,
) -> Response {
    let Some(runtime) = registry.remote_runtime() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(credential) = headers
        .get(REMOTE_ENROLLMENT_CREDENTIAL_HEADER)
        .map(|value| value.as_bytes().to_vec())
    else {
        return remote_node_operator_refusal(RemoteNodeOperatorErrorV1::InvalidRequest);
    };
    let identity = (
        request.discovery.brain_id.clone(),
        request.discovery.caller_node_id.clone(),
    );
    let presented = OpaqueRemoteCredential::new(credential.clone()).ok();
    // The SDK client is blocking; it is built, used, and dropped off the runtime.
    let discovered = tokio::task::spawn_blocking(move || discover_authority(&request, &credential))
        .await
        .unwrap_or(Err(RemoteNodeOperatorErrorV1::Unavailable));
    let discovery = match discovered {
        Ok(discovery) => discovery,
        Err(RemoteNodeOperatorErrorV1::AuthorityUnreachable) => {
            let Some(presented) = presented else {
                return remote_node_operator_refusal(RemoteNodeOperatorErrorV1::InvalidRequest);
            };
            let (brain_id, node_id) = identity;
            let recorded = runtime
                .remote_credential_authority()
                .record_authority_unreachable(
                    &presented,
                    &brain_id,
                    &node_id,
                    tracedecay_contracts::clock::now_micros(),
                );
            return remote_node_operator_refusal(
                recorded
                    .err()
                    .unwrap_or(RemoteNodeOperatorErrorV1::AuthorityUnreachable),
            );
        }
        Err(error) => return remote_node_operator_refusal(error),
    };
    match runtime.join_remote_node(&discovery).await {
        Ok(()) => Json(discovery).into_response(),
        Err(error) => remote_node_operator_refusal(error),
    }
}

/// Asks the authority, over its trust-rooted channel, for the enrollment the
/// presented credential holds, and accepts only an answer bound to exactly
/// that credential and the requested identity.
fn discover_authority(
    request: &RemoteNodeJoinRequestV1,
    credential: &[u8],
) -> std::result::Result<RemoteAuthorityDiscoveryV1, RemoteNodeOperatorErrorV1> {
    let presented = RemoteCredentialFingerprintV1::from_secret(credential)
        .map_err(|_| RemoteNodeOperatorErrorV1::InvalidRequest)?;
    if request.timeout_secs == 0 {
        return Err(RemoteNodeOperatorErrorV1::InvalidRequest);
    }
    let timeout = Duration::from_secs(request.timeout_secs);
    let client = match &request.trust_root_pem {
        Some(pem) => EnrolledRemoteClient::new_with_root_certificate(
            &request.endpoint,
            credential,
            timeout,
            pem.as_bytes(),
        ),
        None => EnrolledRemoteClient::new(&request.endpoint, credential, timeout),
    }
    .map_err(|_| RemoteNodeOperatorErrorV1::InvalidRequest)?;
    let response = client
        .discover_authority(&request.discovery)
        .map_err(|error| {
            tracing::warn!(%error, "remote authority discovery failed during join");
            match error {
                RemoteClientError::Transport(_) => RemoteNodeOperatorErrorV1::AuthorityUnreachable,
                _ => RemoteNodeOperatorErrorV1::AuthorityUnavailable,
            }
        })?;
    let Ok(envelope) = response.result else {
        return Err(RemoteNodeOperatorErrorV1::AuthorityUnavailable);
    };
    let ApplicationOutcome::Result(discovery) = envelope.outcome else {
        return Err(RemoteNodeOperatorErrorV1::AuthorityUnavailable);
    };
    discovery
        .verify_for(&request.discovery, &presented, &response.authority)
        .map_err(|_| RemoteNodeOperatorErrorV1::BrainIdentityConflict)?;
    Ok(discovery)
}

const REMOTE_JOIN_HTTP_TIMEOUT_MARGIN: Duration = Duration::from_secs(10);

/// Join contract for the Remote Brain operator CLI: the live daemon, not the
/// CLI, talks to the authority and records what it observed.
pub fn live_remote_join(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    request: &RemoteNodeJoinRequestV1,
    enrollment_credential: &[u8],
) -> Result<RemoteAuthorityDiscoveryV1> {
    let connection = super::current_profile_daemon_connection(profile)?;
    let Some(endpoint) = connection.http_application_endpoint() else {
        return Err(TraceDecayError::Config {
            message: "TraceDecay daemon HTTP application endpoint is not published. Start or restart the daemon.".to_owned(),
        });
    };
    let credential =
        std::str::from_utf8(enrollment_credential).map_err(|_| TraceDecayError::Config {
            message: "Remote Brain enrollment credential must be printable ASCII".to_owned(),
        })?;
    let origin = format!("http://{endpoint}");
    let agent = tracedecay_application::http_agent::http_agent(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(
                Duration::from_secs(request.timeout_secs) + REMOTE_JOIN_HTTP_TIMEOUT_MARGIN,
            ))
            .build(),
    );
    let mut response = agent
        .post(&format!("{origin}/remote-nodes/join"))
        .header(
            "Authorization",
            format!("Bearer {}", connection.auth_token()),
        )
        .header("Origin", &origin)
        .header(REMOTE_ENROLLMENT_CREDENTIAL_HEADER, credential)
        .send_json(request)
        .map_err(|error| TraceDecayError::Config {
            message: format!("TraceDecay daemon did not answer the Remote Brain join: {error}"),
        })?;
    let status = response.status().as_u16();
    let body: serde_json::Value =
        response
            .body_mut()
            .read_json()
            .map_err(|error| TraceDecayError::Config {
                message: format!("TraceDecay daemon Remote Brain join returned HTTP {status} without a typed body: {error}"),
            })?;
    if status != 200 {
        return Err(TraceDecayError::tool_refused(
            "remote join",
            body["code"].as_str().map(str::to_owned),
            Some(format!(
                "TraceDecay daemon refused the Remote Brain join with HTTP {status}"
            )),
        ));
    }
    serde_json::from_value(body).map_err(|error| TraceDecayError::Config {
        message: format!("TraceDecay daemon Remote Brain join was not a typed discovery: {error}"),
    })
}

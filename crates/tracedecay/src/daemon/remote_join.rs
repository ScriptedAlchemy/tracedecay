//! Operator join: this profile becomes an enrolled node of another profile's
//! Brain, from what the authority itself answers to the node's credential.

use std::time::Duration;

use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracedecay_api::remote::REMOTE_ENROLLMENT_CREDENTIAL_HEADER;
use tracedecay_contracts::ApplicationOutcome;
use tracedecay_contracts::remote::auth::OpaqueRemoteCredential;
use tracedecay_contracts::remote::discovery::RemoteAuthorityDiscoveryV1;
use tracedecay_contracts::remote::protocol::{CurrentAuthorityRequestV1, RemoteProtocolRequestV1};
use tracedecay_domain::RemoteCredentialFingerprintV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_sdk::remote_client::{EnrolledRemoteClient, RemoteClientError};
use tracedecay_store_runtime::{
    DaemonRemoteCredentialAuthorityV1, DaemonSessionRuntimeRegistryV1, RemoteNodeOperatorErrorV1,
};

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
    let Some(credential) = enrollment_credential(&headers) else {
        return remote_node_operator_refusal(RemoteNodeOperatorErrorV1::InvalidRequest);
    };
    match refresh_membership(&runtime, &request, credential).await {
        Ok(discovery) => Json(discovery).into_response(),
        Err(error) => remote_node_operator_refusal(error),
    }
}

pub(super) fn enrollment_credential(headers: &HeaderMap) -> Option<Vec<u8>> {
    headers
        .get(REMOTE_ENROLLMENT_CREDENTIAL_HEADER)
        .map(|value| value.as_bytes().to_vec())
}

/// Discovers the authority for the presented node credential and records what
/// this node observed: the writer the authority answered with, or that the
/// authority could not be reached.
pub(super) async fn refresh_membership(
    runtime: &DaemonSessionRuntimeRegistryV1,
    request: &RemoteNodeJoinRequestV1,
    credential: Vec<u8>,
) -> std::result::Result<RemoteAuthorityDiscoveryV1, RemoteNodeOperatorErrorV1> {
    let presented = OpaqueRemoteCredential::new(credential.clone())
        .map_err(|_| RemoteNodeOperatorErrorV1::InvalidRequest)?;
    let discover = request.clone();
    // The SDK client is blocking; it is built, used, and dropped off the runtime.
    let discovered =
        tokio::task::spawn_blocking(move || discover_authority(&discover, &credential))
            .await
            .unwrap_or(Err(RemoteNodeOperatorErrorV1::Unavailable));
    match discovered {
        Ok(discovery) => {
            runtime.join_remote_node(&discovery).await?;
            Ok(discovery)
        }
        Err(RemoteNodeOperatorErrorV1::AuthorityUnreachable) => Err(record_unreachable(
            &runtime.remote_credential_authority(),
            &presented,
            &request.discovery,
        )),
        Err(error) => Err(error),
    }
}

/// Records that the authority `discovery` names did not answer the node, and
/// returns the refusal to report.
pub(super) fn record_unreachable(
    credentials: &DaemonRemoteCredentialAuthorityV1,
    presented: &OpaqueRemoteCredential,
    discovery: &RemoteProtocolRequestV1<CurrentAuthorityRequestV1>,
) -> RemoteNodeOperatorErrorV1 {
    credentials
        .record_authority_unreachable(
            presented,
            &discovery.brain_id,
            &discovery.caller_node_id,
            tracedecay_contracts::clock::now_micros(),
        )
        .err()
        .unwrap_or(RemoteNodeOperatorErrorV1::AuthorityUnreachable)
}

/// The node's client for the authority endpoint `request` names.
pub(super) fn remote_client(
    request: &RemoteNodeJoinRequestV1,
    credential: &[u8],
) -> std::result::Result<EnrolledRemoteClient, RemoteNodeOperatorErrorV1> {
    if request.timeout_secs == 0 {
        return Err(RemoteNodeOperatorErrorV1::InvalidRequest);
    }
    let timeout = Duration::from_secs(request.timeout_secs);
    match &request.trust_root_pem {
        Some(pem) => EnrolledRemoteClient::new_with_root_certificate(
            &request.endpoint,
            credential,
            timeout,
            pem.as_bytes(),
        ),
        None => EnrolledRemoteClient::new(&request.endpoint, credential, timeout),
    }
    .map_err(|_| RemoteNodeOperatorErrorV1::InvalidRequest)
}

/// A failed call to the authority: no answer at all is unreachable, any
/// answer that is not the expected result is unavailable.
pub(super) fn authority_call_failure(error: &RemoteClientError) -> RemoteNodeOperatorErrorV1 {
    match error {
        RemoteClientError::Transport(_) => RemoteNodeOperatorErrorV1::AuthorityUnreachable,
        _ => RemoteNodeOperatorErrorV1::AuthorityUnavailable,
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
    let response = remote_client(request, credential)?
        .discover_authority(&request.discovery)
        .map_err(|error| {
            tracing::warn!(%error, "remote authority discovery failed");
            authority_call_failure(&error)
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
    live_remote_node_operation(
        profile,
        "join",
        request,
        enrollment_credential,
        Some(Duration::from_secs(request.timeout_secs) + REMOTE_JOIN_HTTP_TIMEOUT_MARGIN),
    )
}

/// Posts one node operation to the live daemon's `/remote-nodes/{operation}`
/// route and decodes its typed answer. `http_timeout` bounds the whole call;
/// `None` leaves each authority call the daemon makes to its own timeout.
pub(super) fn live_remote_node_operation<T: DeserializeOwned>(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    operation: &str,
    request: &RemoteNodeJoinRequestV1,
    enrollment_credential: &[u8],
    http_timeout: Option<Duration>,
) -> Result<T> {
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
            .timeout_global(http_timeout)
            .build(),
    );
    let mut response = agent
        .post(&format!("{origin}/remote-nodes/{operation}"))
        .header(
            "Authorization",
            format!("Bearer {}", connection.auth_token()),
        )
        .header("Origin", &origin)
        .header(REMOTE_ENROLLMENT_CREDENTIAL_HEADER, credential)
        .send_json(request)
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "TraceDecay daemon did not answer the Remote Brain {operation}: {error}"
            ),
        })?;
    let status = response.status().as_u16();
    let body: serde_json::Value =
        response
            .body_mut()
            .read_json()
            .map_err(|error| TraceDecayError::Config {
                message: format!("TraceDecay daemon Remote Brain {operation} returned HTTP {status} without a typed body: {error}"),
            })?;
    if status != 200 {
        return Err(TraceDecayError::tool_refused(
            format!("remote {operation}"),
            body["code"].as_str().map(str::to_owned),
            Some(format!(
                "TraceDecay daemon refused the Remote Brain {operation} with HTTP {status}"
            )),
        ));
    }
    serde_json::from_value(body).map_err(|error| TraceDecayError::Config {
        message: format!(
            "TraceDecay daemon Remote Brain {operation} answer was not typed: {error}"
        ),
    })
}

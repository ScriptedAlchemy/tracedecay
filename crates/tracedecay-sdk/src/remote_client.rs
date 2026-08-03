//! Enrolled HTTPS client for the canonical Remote Brain protocol.
//!
//! This client deliberately has no project-application route construction:
//! remote operations always target the versioned `/enrollment`, `/replay`,
//! `/query`, `/backup`, `/restore`, and `/failover` protocol endpoints.

use std::fmt;
use std::time::Duration;

use reqwest::blocking::Client as HttpClient;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_application::remote::protocol::{
    RemoteAuthorityDiscoveryProtocolRequestV1, RemoteEnrollmentProtocolRequestV1,
    RemoteProtocolBodyV1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
};
use tracedecay_application::remote::query::RemoteQueryRequestV1;
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_application::remote::replay_node::{
    RemoteReplayTransportErrorV1, RemoteReplayTransportPortV1,
};
use tracedecay_application::{ApplicationEnvelope, RequestId};
use tracedecay_domain::CurrentRemoteAuthorityStateV1;

const MAX_CREDENTIAL_BYTES: usize = 4_096;

#[derive(Clone)]
pub struct EnrolledRemoteClient {
    http: HttpClient,
    endpoint: reqwest::Url,
    authorization: HeaderValue,
}

impl fmt::Debug for EnrolledRemoteClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrolledRemoteClient")
            .field("endpoint", &self.endpoint)
            .field("authorization", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub enum RemoteClientError {
    Configuration(String),
    Transport(String),
    Protocol(String),
}

impl fmt::Display for RemoteClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) => {
                write!(
                    formatter,
                    "Remote Brain endpoint configuration is invalid: {message}"
                )
            }
            Self::Transport(message) => {
                write!(formatter, "Remote Brain transport failed: {message}")
            }
            Self::Protocol(message) => {
                write!(
                    formatter,
                    "Remote Brain protocol response was invalid: {message}"
                )
            }
        }
    }
}

impl std::error::Error for RemoteClientError {}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteProtocolWireResponseV1 {
    pub protocol_version: u16,
    pub request_id: RequestId,
    pub authority: CurrentRemoteAuthorityStateV1,
    pub result: serde_json::Value,
}

impl EnrolledRemoteClient {
    pub fn new(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
    ) -> Result<Self, RemoteClientError> {
        let endpoint = reqwest::Url::parse(endpoint.as_ref())
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.username() != ""
            || endpoint.password().is_some()
        {
            return Err(RemoteClientError::Configuration(
                "Remote Brain endpoint must be a credential-free HTTPS URL".to_owned(),
            ));
        }
        let credential = credential.as_ref();
        if credential.is_empty() || credential.len() > MAX_CREDENTIAL_BYTES {
            return Err(RemoteClientError::Configuration(
                "Remote Brain credential length is invalid".to_owned(),
            ));
        }
        let authorization = authorization_header(credential)?;
        let http = HttpClient::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        Ok(Self {
            http,
            endpoint,
            authorization,
        })
    }

    /// Build a client authenticated by an explicit private CA and client identity.
    ///
    /// The caller resolves both PEM values through its secret authority. This
    /// method consumes and clears those input buffers after rustls parses them.
    pub fn new_mutual_tls(
        endpoint: impl AsRef<str>,
        credential: impl AsRef<[u8]>,
        timeout: Duration,
        mut authority_ca_pem: Vec<u8>,
        mut client_identity_pem: Vec<u8>,
    ) -> Result<Self, RemoteClientError> {
        let result = (|| {
            let endpoint = validated_endpoint(endpoint.as_ref())?;
            let authorization = authorization_header(credential.as_ref())?;
            let authority_ca = reqwest::Certificate::from_pem(&authority_ca_pem)
                .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
            let client_identity = reqwest::Identity::from_pem(&client_identity_pem)
                .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
            let http = HttpClient::builder()
                .timeout(timeout)
                .tls_built_in_root_certs(false)
                .add_root_certificate(authority_ca)
                .identity(client_identity)
                .https_only(true)
                .build()
                .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
            Ok(Self {
                http,
                endpoint,
                authorization,
            })
        })();
        authority_ca_pem.fill(0);
        client_identity_pem.fill(0);
        std::hint::black_box(&authority_ca_pem);
        std::hint::black_box(&client_identity_pem);
        result
    }

    pub fn execute<Request>(
        &self,
        route: &str,
        request: &RemoteProtocolRequestV1<Request>,
    ) -> Result<RemoteProtocolWireResponseV1, RemoteClientError>
    where
        Request: RemoteProtocolBodyV1 + Serialize,
    {
        request
            .validate_metadata()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        request
            .body
            .validate_remote_protocol_body()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let url = self
            .endpoint
            .join(route.trim_start_matches('/'))
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({ "request": request }))
            .send()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        response
            .json::<serde_json::Value>()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))
            .and_then(decode_wire_response)
    }

    pub fn discover_authority(
        &self,
        request: &RemoteAuthorityDiscoveryProtocolRequestV1,
    ) -> Result<RemoteProtocolWireResponseV1, RemoteClientError> {
        request
            .validate_metadata()
            .and_then(|()| request.body.validate_remote_protocol_body())
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let url = self
            .endpoint
            .join("discovery")
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .header(CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({ "request": request }))
            .send()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        response
            .json::<serde_json::Value>()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))
            .and_then(decode_wire_response)
    }

    pub fn replay(
        &self,
        request: &RemoteProtocolRequestV1<RemoteReplayRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteReplayOutcomeV1>, RemoteClientError> {
        self.execute("replay", request)
            .and_then(decode_success_response)
    }

    pub fn query(
        &self,
        request: &RemoteProtocolRequestV1<RemoteQueryRequestV1>,
    ) -> Result<RemoteProtocolWireResponseV1, RemoteClientError> {
        self.execute("query", request)
    }

    pub fn execute_enrollment(
        &self,
        request: &RemoteEnrollmentProtocolRequestV1,
        enrollment_credential: impl AsRef<[u8]>,
    ) -> Result<RemoteProtocolWireResponseV1, RemoteClientError> {
        request
            .validate_initial_enrollment_metadata()
            .and_then(|()| request.body.validate_remote_protocol_body())
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?;
        let enrollment_credential = HeaderValue::from_bytes(enrollment_credential.as_ref())
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let url = self
            .endpoint
            .join("enrollment")
            .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.authorization.clone())
            .header("x-tracedecay-enrollment-credential", enrollment_credential)
            .header(CONTENT_TYPE, "application/json")
            .json(&serde_json::json!({ "request": request }))
            .send()
            .map_err(|error| RemoteClientError::Transport(error.to_string()))?;
        response
            .json::<serde_json::Value>()
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))
            .and_then(decode_wire_response)
    }
}

impl RemoteReplayTransportPortV1 for EnrolledRemoteClient {
    fn replay(
        &self,
        request: &RemoteProtocolRequestV1<RemoteReplayRequestV1>,
    ) -> Result<RemoteProtocolResponseV1<RemoteReplayOutcomeV1>, RemoteReplayTransportErrorV1> {
        EnrolledRemoteClient::replay(self, request).map_err(|error| match error {
            RemoteClientError::Configuration(_) | RemoteClientError::Protocol(_) => {
                RemoteReplayTransportErrorV1::InvalidResponse
            }
            RemoteClientError::Transport(_) => RemoteReplayTransportErrorV1::Unavailable,
        })
    }
}

fn validated_endpoint(endpoint: &str) -> Result<reqwest::Url, RemoteClientError> {
    let endpoint = reqwest::Url::parse(endpoint)
        .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.username() != ""
        || endpoint.password().is_some()
    {
        return Err(RemoteClientError::Configuration(
            "Remote Brain endpoint must be a credential-free HTTPS URL".to_owned(),
        ));
    }
    Ok(endpoint)
}

fn authorization_header(credential: &[u8]) -> Result<HeaderValue, RemoteClientError> {
    if credential.is_empty() || credential.len() > MAX_CREDENTIAL_BYTES {
        return Err(RemoteClientError::Configuration(
            "Remote Brain credential length is invalid".to_owned(),
        ));
    }
    let mut value = Vec::with_capacity(b"Bearer ".len() + credential.len());
    value.extend_from_slice(b"Bearer ");
    value.extend_from_slice(credential);
    let header = HeaderValue::from_bytes(&value)
        .map_err(|error| RemoteClientError::Configuration(error.to_string()));
    value.fill(0);
    std::hint::black_box(&value);
    header
}

fn decode_wire_response(
    value: serde_json::Value,
) -> Result<RemoteProtocolWireResponseV1, RemoteClientError> {
    serde_json::from_value(value.get("response").cloned().unwrap_or(value))
        .map_err(|error| RemoteClientError::Protocol(error.to_string()))
}

fn decode_success_response<T>(
    wire: RemoteProtocolWireResponseV1,
) -> Result<RemoteProtocolResponseV1<T>, RemoteClientError>
where
    T: DeserializeOwned,
{
    let result =
        serde_json::from_value::<Result<ApplicationEnvelope<T>, serde_json::Value>>(wire.result)
            .map_err(|error| RemoteClientError::Protocol(error.to_string()))?
            .map_err(|_| {
                RemoteClientError::Protocol(
                    "Remote Brain returned an application problem".to_owned(),
                )
            })?;
    RemoteProtocolResponseV1::new(wire.request_id, wire.authority, Ok(result))
        .map_err(|error| RemoteClientError::Protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrolled_remote_client_requires_https() {
        let error = EnrolledRemoteClient::new(
            "http://remote.example",
            "credential",
            Duration::from_secs(1),
        )
        .expect_err("plaintext endpoint must fail");

        assert!(matches!(error, RemoteClientError::Configuration(_)));
    }

    #[test]
    fn enrolled_remote_client_rejects_url_credentials() {
        let error = EnrolledRemoteClient::new(
            "https://secret@remote.example",
            "credential",
            Duration::from_secs(1),
        )
        .expect_err("URL credentials must fail");

        assert!(matches!(error, RemoteClientError::Configuration(_)));
    }

    #[test]
    fn enrolled_remote_client_debug_redacts_bearer_credential() {
        let client = EnrolledRemoteClient::new(
            "https://remote.example",
            "credential-that-must-stay-secret",
            Duration::from_secs(1),
        )
        .unwrap();
        let debug = format!("{client:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("credential-that-must-stay-secret"));
    }
}

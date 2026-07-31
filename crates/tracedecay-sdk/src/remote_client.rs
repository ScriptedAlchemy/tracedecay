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
use tracedecay_application::RequestId;
use tracedecay_application::remote::protocol::{RemoteProtocolBodyV1, RemoteProtocolRequestV1};
use tracedecay_domain::CurrentRemoteAuthorityStateV1;

const MAX_CREDENTIAL_BYTES: usize = 4_096;

#[derive(Clone, Debug)]
pub struct EnrolledRemoteClient {
    http: HttpClient,
    endpoint: reqwest::Url,
    authorization: HeaderValue,
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
        let authorization =
            HeaderValue::from_bytes([b"Bearer ".as_slice(), credential].concat().as_slice())
                .map_err(|error| RemoteClientError::Configuration(error.to_string()))?;
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
            .validate_remote_protocol_body(request.sent_at)
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
            .and_then(|value| {
                serde_json::from_value(value.get("response").cloned().unwrap_or(value))
                    .map_err(|error| RemoteClientError::Protocol(error.to_string()))
            })
    }
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
}

//! Thin HTTP boundary for the authenticated remote Brain protocol.
//!
//! HTTP carries versioned application payloads and an opaque authorization
//! header. Authority authentication remains the rustls/transport owner's
//! responsibility; this adapter does not accept trust flags, URLs, database
//! locations, or storage bytes.

use std::fmt;
use std::hint::black_box;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::remote::auth::OpaqueRemoteCredential;
use tracedecay_application::remote::protocol::{
    REMOTE_PROTOCOL_VERSION_V1, RemoteProtocolRequestV1, RemoteProtocolResponseV1,
};

const BEARER_PREFIX: &[u8] = b"Bearer ";

/// Parsed HTTP credential header. It cannot be cloned, serialized, or logged.
pub struct RemoteAuthorizationHeader {
    credential: OpaqueRemoteCredential,
}

impl RemoteAuthorizationHeader {
    /// Consume an owned authorization header so the adapter does not retain a
    /// second plaintext copy after admission.
    pub fn from_owned_bytes(mut header: Vec<u8>) -> Result<Self, RemoteHttpBoundaryError> {
        if !header.starts_with(BEARER_PREFIX) {
            zeroize_rejected(&mut header);
            return Err(RemoteHttpBoundaryError::MissingOrInvalidAuthorization);
        }
        header.drain(..BEARER_PREFIX.len());
        let credential = match OpaqueRemoteCredential::new(header.into_boxed_slice()) {
            Ok(credential) => credential,
            Err(_) => return Err(RemoteHttpBoundaryError::MissingOrInvalidAuthorization),
        };
        Ok(Self { credential })
    }

    pub fn into_credential(self) -> OpaqueRemoteCredential {
        self.credential
    }
}

impl fmt::Debug for RemoteAuthorizationHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteAuthorizationHeader([REDACTED])")
    }
}

/// Current authorization plus a newly generated credential for enrollment or
/// rotation. Neither value can enter a serializable request body.
pub struct RemoteCredentialPairHeaders {
    current: RemoteAuthorizationHeader,
    replacement: OpaqueRemoteCredential,
}

impl RemoteCredentialPairHeaders {
    pub fn from_owned_bytes(
        current_authorization: Vec<u8>,
        replacement: Vec<u8>,
    ) -> Result<Self, RemoteHttpBoundaryError> {
        Ok(Self {
            current: RemoteAuthorizationHeader::from_owned_bytes(current_authorization)?,
            replacement: OpaqueRemoteCredential::new(replacement.into_boxed_slice())
                .map_err(|_| RemoteHttpBoundaryError::MissingOrInvalidAuthorization)?,
        })
    }

    pub fn into_credentials(self) -> (OpaqueRemoteCredential, OpaqueRemoteCredential) {
        (self.current.into_credential(), self.replacement)
    }
}

impl fmt::Debug for RemoteCredentialPairHeaders {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteCredentialPairHeaders([REDACTED])")
    }
}

fn zeroize_rejected(bytes: &mut [u8]) {
    bytes.fill(0);
    black_box(bytes);
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RemoteHttpBoundaryError {
    #[error("remote authorization is missing or invalid")]
    MissingOrInvalidAuthorization,
    #[error("remote protocol version is unsupported")]
    UnsupportedProtocolVersion,
    #[error("remote request metadata is invalid")]
    InvalidRequest,
}

/// Wire request body. Secret material is supplied separately through
/// `RemoteAuthorizationHeader`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteHttpRequestV1<T> {
    pub request: RemoteProtocolRequestV1<T>,
}

impl<T> RemoteHttpRequestV1<T> {
    pub fn validate(&self) -> Result<(), RemoteHttpBoundaryError> {
        if self.request.protocol_version != REMOTE_PROTOCOL_VERSION_V1 {
            return Err(RemoteHttpBoundaryError::UnsupportedProtocolVersion);
        }
        self.request
            .validate_metadata()
            .map_err(|_| RemoteHttpBoundaryError::InvalidRequest)
    }

    /// Join the public body and secret header only after both have passed the
    /// HTTP boundary. The resulting admission object has no serialization or
    /// debug implementation.
    pub fn admit(
        self,
        authorization: RemoteAuthorizationHeader,
    ) -> Result<RemoteHttpAdmissionV1<T>, RemoteHttpBoundaryError> {
        self.validate()?;
        Ok(RemoteHttpAdmissionV1 {
            request: self.request,
            credential: authorization.into_credential(),
        })
    }

    pub fn admit_with_replacement(
        self,
        credentials: RemoteCredentialPairHeaders,
    ) -> Result<RemoteHttpCredentialRotationAdmissionV1<T>, RemoteHttpBoundaryError> {
        self.validate()?;
        let (current, replacement) = credentials.into_credentials();
        Ok(RemoteHttpCredentialRotationAdmissionV1 {
            request: self.request,
            current,
            replacement,
        })
    }
}

/// Non-serializable input handed to the application owner.
pub struct RemoteHttpAdmissionV1<T> {
    pub request: RemoteProtocolRequestV1<T>,
    pub credential: OpaqueRemoteCredential,
}

/// Non-serializable enrollment/rotation input with both opaque credentials.
pub struct RemoteHttpCredentialRotationAdmissionV1<T> {
    pub request: RemoteProtocolRequestV1<T>,
    pub current: OpaqueRemoteCredential,
    pub replacement: OpaqueRemoteCredential,
}

/// HTTP response is a transparent presentation of the versioned canonical
/// application response.
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteHttpResponseV1<T> {
    pub response: RemoteProtocolResponseV1<T>,
}

impl<T> From<RemoteProtocolResponseV1<T>> for RemoteHttpResponseV1<T> {
    fn from(response: RemoteProtocolResponseV1<T>) -> Self {
        Self { response }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_header_is_always_redacted() {
        let header = RemoteAuthorizationHeader::from_owned_bytes(
            b"Bearer 0123456789abcdef0123456789abcdef".to_vec(),
        )
        .unwrap();
        assert_eq!(
            format!("{header:?}"),
            "RemoteAuthorizationHeader([REDACTED])"
        );
    }

    #[test]
    fn malformed_authorization_fails_closed() {
        assert_eq!(
            RemoteAuthorizationHeader::from_owned_bytes(
                b"Basic 0123456789abcdef0123456789abcdef".to_vec()
            )
            .unwrap_err(),
            RemoteHttpBoundaryError::MissingOrInvalidAuthorization
        );
        assert_eq!(
            RemoteAuthorizationHeader::from_owned_bytes(b"Bearer short".to_vec()).unwrap_err(),
            RemoteHttpBoundaryError::MissingOrInvalidAuthorization
        );
    }

    #[test]
    fn credential_pair_debug_never_exposes_either_secret() {
        let headers = RemoteCredentialPairHeaders::from_owned_bytes(
            b"Bearer 0123456789abcdef0123456789abcdef".to_vec(),
            b"fedcba9876543210fedcba9876543210".to_vec(),
        )
        .unwrap();
        assert_eq!(
            format!("{headers:?}"),
            "RemoteCredentialPairHeaders([REDACTED])"
        );
    }

    #[test]
    fn public_http_payload_never_contains_the_credential() {
        let request: RemoteHttpRequestV1<()> = serde_json::from_value(serde_json::json!({
            "request": {
                "protocol_version": 1,
                "request_id": "request.remote",
                "brain_id": "brain.remote",
                "caller_node_id": "node.remote",
                "enrollment_revision": 1,
                "expected_authority": null,
                "sent_at": 10,
                "body": null
            }
        }))
        .unwrap();
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("credential"));
        assert!(!json.contains("authorization"));
    }
}

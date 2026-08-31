//! Client-side discovery of the current daemon connection.
//!
//! Resolves the profile's authority record into an endpoint plus credential
//! and keeps that resolution honest while a request is in flight: the
//! [`DaemonLivenessProbe`] handed to the protocol crate re-reads the record so
//! a restarted daemon (rotated epoch and token) surfaces as a typed error
//! instead of silence. Transport — connects, retries, tool calls — stays with
//! the caller; nothing here opens a stream.

use std::net::SocketAddr;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_daemon_protocol::{DaemonEndpoint, DaemonLivenessProbe};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::authority;

/// A discovered daemon endpoint plus its credential and private authority
/// provenance.
#[derive(Clone)]
pub struct DaemonConnection {
    pub endpoint: DaemonEndpoint,
    pub auth_token: Option<String>,
    authority_record: Option<authority::DaemonAuthorityRecord>,
}

impl DaemonConnection {
    /// The loopback HTTP application endpoint published by this connection's
    /// authority, when one is available.
    pub fn http_application_endpoint(&self) -> Option<SocketAddr> {
        self.authority_record
            .as_ref()
            .and_then(|record| record.http_application_endpoint)
    }

    pub fn into_protocol(self) -> tracedecay_daemon_protocol::DaemonConnection {
        let connection =
            tracedecay_daemon_protocol::DaemonConnection::new(self.endpoint, self.auth_token);
        match self.authority_record {
            Some(record) => connection
                .with_daemon_version(record.version.clone())
                .with_liveness(Arc::new(AuthorityLivenessProbe { record })),
            None => connection,
        }
    }

    /// Fails when the authority record that named this endpoint is no longer
    /// current (the daemon restarted or its authority disappeared).
    /// Connections without a discovered record have nothing to check.
    pub fn ensure_authority_current(&self, request_label: &str) -> Result<()> {
        match self.authority_record.as_ref() {
            Some(record) => ensure_record_current(record, request_label),
            None => Ok(()),
        }
    }
}

struct AuthorityLivenessProbe {
    record: authority::DaemonAuthorityRecord,
}

impl DaemonLivenessProbe for AuthorityLivenessProbe {
    fn ensure_live(&self, request_label: &str) -> Result<()> {
        ensure_record_current(&self.record, request_label)
    }
}

fn ensure_record_current(
    expected: &authority::DaemonAuthorityRecord,
    request_label: &str,
) -> Result<()> {
    let current = authority::current_record(&expected.profile_root)?;
    let Some(current) = current else {
        return Err(TraceDecayError::Config {
            message: format!(
                "daemon authority disappeared while request '{request_label}' was awaiting a response; the request was already sent and was not retried"
            ),
        });
    };
    if current.epoch != expected.epoch || current.process_run_id != expected.process_run_id {
        return Err(TraceDecayError::Config {
            message: format!(
                "daemon restarted while request '{request_label}' was awaiting a response (expected epoch {}, current epoch {}); the request was already sent and was not retried",
                expected.epoch, current.epoch
            ),
        });
    }
    Ok(())
}

/// Authenticated invocation client for this process's current daemon authority.
pub fn invocation_client_for_current(
    handshake: tracedecay_daemon_protocol::DaemonHandshake,
) -> Result<tracedecay_daemon_protocol::DaemonInvocationClient> {
    Ok(tracedecay_daemon_protocol::DaemonInvocationClient::new(
        current_daemon_connection()?.into_protocol(),
        handshake,
    ))
}

pub fn current_daemon_connection() -> Result<DaemonConnection> {
    let profile_root = tracedecay_runtime_core::config::user_data_dir().ok_or_else(|| {
        TraceDecayError::Config {
            message: "could not determine TraceDecay user data directory".to_string(),
        }
    })?;
    let record =
        authority::current_record(&profile_root)?.ok_or_else(|| TraceDecayError::Config {
            message:
                "TraceDecay daemon authority record is not available. Start or restart the daemon."
                    .to_string(),
        })?;
    Ok(DaemonConnection {
        endpoint: record.endpoint.clone(),
        auth_token: Some(record.auth_token.clone()),
        authority_record: Some(record),
    })
}

#[cfg(unix)]
pub fn connection_for_socket_path(socket_path: &Path) -> DaemonConnection {
    if let Ok(connection) = current_daemon_connection()
        && let DaemonEndpoint::Unix(authority_path) = &connection.endpoint
        && authority::canonical_identity_path(authority_path).ok()
            == authority::canonical_identity_path(socket_path).ok()
    {
        return connection;
    }
    if let Some(profile_root) = socket_path.parent()
        && let Ok(Some(record)) = authority::current_record(profile_root)
        && let DaemonEndpoint::Unix(authority_path) = &record.endpoint
        && authority::canonical_identity_path(authority_path).ok()
            == authority::canonical_identity_path(socket_path).ok()
    {
        return DaemonConnection {
            endpoint: record.endpoint.clone(),
            auth_token: Some(record.auth_token.clone()),
            authority_record: Some(record),
        };
    }
    // Explicit paths are retained for test harnesses and legacy one-shot
    // callers without a discoverable authority record. Default production
    // routing always uses the authority record.
    DaemonConnection {
        endpoint: DaemonEndpoint::Unix(socket_path.to_path_buf()),
        auth_token: None,
        authority_record: None,
    }
}

// Windows discovers the current daemon through a fallible endpoint lookup;
// Unix keeps the same cross-platform contract even though its path is infallible.
#[allow(clippy::unnecessary_wraps)]
pub fn client_connection(socket_path: &Path) -> Result<DaemonConnection> {
    #[cfg(unix)]
    {
        Ok(connection_for_socket_path(socket_path))
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        current_daemon_connection()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_exposes_only_the_published_http_application_endpoint() {
        let http_application_endpoint = "127.0.0.1:43124".parse().unwrap();
        let endpoint = DaemonEndpoint::loopback("127.0.0.1:43123".parse().unwrap()).unwrap();
        let connection = DaemonConnection {
            endpoint: endpoint.clone(),
            auth_token: Some("11".repeat(32)),
            authority_record: Some(authority::DaemonAuthorityRecord {
                pid: 42,
                process_run_id: "run-42".to_owned(),
                started_at_unix_secs: 1_700_000_000,
                epoch: 7,
                version: "test".to_owned(),
                endpoint,
                http_application_endpoint: Some(http_application_endpoint),
                remote_brain_tls_endpoint: None,
                auth_token: "11".repeat(32),
                profile_root: PathBuf::from("/tmp/tracedecay-test-profile"),
                brain_id: None,
                profile_id: None,
            }),
        };

        assert_eq!(
            connection.http_application_endpoint(),
            Some(http_application_endpoint)
        );
    }
}

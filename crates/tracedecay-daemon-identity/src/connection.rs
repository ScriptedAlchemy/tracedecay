//! Client-side discovery of the current daemon connection.
//!
//! Resolves the profile's authority record into an endpoint plus credential
//! and keeps that resolution honest while a request is in flight: the
//! [`DaemonLivenessProbe`] handed to the protocol crate re-reads the record so
//! a restarted daemon (rotated epoch and token) surfaces as a typed error
//! instead of silence. Transport stays with the caller, including connects,
//! retries, and tool calls; nothing here opens a stream.

use std::net::SocketAddr;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_daemon_protocol::{DaemonEndpoint, DaemonLivenessProbe};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::authority;

/// Typed reason code for a daemon endpoint no readable authority record names.
///
/// Retryable only while the record is absent: a starting daemon writes its
/// record before it binds, so absence resolves itself. A record naming a
/// different endpoint does not.
pub const DAEMON_AUTHORITY_UNAVAILABLE: &str = "daemon_authority_unavailable";

/// A daemon endpoint plus its credential, both read from the authority record
/// that names it. Distinct from the protocol crate's transport
/// [`tracedecay_daemon_protocol::DaemonConnection`]; convert with
/// [`Self::into_protocol`].
#[derive(Clone, Debug)]
pub struct ResolvedDaemonConnection {
    record: authority::DaemonAuthorityRecord,
}

impl ResolvedDaemonConnection {
    pub fn endpoint(&self) -> &DaemonEndpoint {
        &self.record.endpoint
    }

    pub fn auth_token(&self) -> &str {
        &self.record.auth_token
    }

    /// The loopback HTTP application endpoint published by this connection's
    /// authority, when one is available.
    pub fn http_application_endpoint(&self) -> Option<SocketAddr> {
        self.record.http_application_endpoint
    }

    pub fn into_protocol(self) -> tracedecay_daemon_protocol::DaemonConnection {
        tracedecay_daemon_protocol::DaemonConnection::new(
            self.record.endpoint.clone(),
            self.record.auth_token.clone(),
        )
        .with_daemon_version(self.record.version.clone())
        .with_liveness(Arc::new(AuthorityLivenessProbe {
            record: self.record,
        }))
    }

    /// Fails when the authority record that named this endpoint is no longer
    /// current (the daemon restarted or its authority disappeared).
    pub fn ensure_authority_current(&self, request_label: &str) -> Result<()> {
        ensure_record_current(&self.record, request_label)
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

#[cfg(unix)]
fn unix_endpoint_matches_socket(endpoint: &DaemonEndpoint, socket_path: &Path) -> bool {
    let DaemonEndpoint::Unix(authority_path) = endpoint else {
        return false;
    };
    matches!(
        (
            authority::canonical_identity_path(authority_path),
            authority::canonical_identity_path(socket_path),
        ),
        (Ok(recorded), Ok(requested)) if recorded == requested
    )
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

pub fn current_daemon_connection() -> Result<ResolvedDaemonConnection> {
    let profile_root = tracedecay_runtime_core::config::user_data_dir().ok_or_else(|| {
        TraceDecayError::Config {
            message: "could not determine TraceDecay user data directory".to_string(),
        }
    })?;
    match authority::current_record(&profile_root)? {
        Some(record) => Ok(ResolvedDaemonConnection { record }),
        None => Err(TraceDecayError::project_route(
            DAEMON_AUTHORITY_UNAVAILABLE,
            true,
            format!(
                "no TraceDecay daemon authority record at '{}'. Start or restart the daemon.",
                authority::record_path(&profile_root)?.display()
            ),
        )),
    }
}

/// The connection for the daemon serving `socket_path`, read from the record
/// that names it: the user profile's record, else the record beside the socket
/// (a daemon whose profile root holds its socket).
#[cfg(unix)]
fn connection_for_socket_path(socket_path: &Path) -> Result<ResolvedDaemonConnection> {
    let user_profile = tracedecay_runtime_core::config::user_data_dir();
    connection_for_socket_in(user_profile.as_deref(), socket_path)
}

#[cfg(unix)]
fn connection_for_socket_in(
    user_profile: Option<&Path>,
    socket_path: &Path,
) -> Result<ResolvedDaemonConnection> {
    let mut checked: Vec<PathBuf> = Vec::new();
    let mut named_elsewhere = Vec::new();
    for profile_root in user_profile.into_iter().chain(socket_path.parent()) {
        let record_path = authority::record_path(profile_root)?;
        if checked.contains(&record_path) {
            continue;
        }
        checked.push(record_path);
        if let Some(record) = authority::current_record(profile_root)? {
            if unix_endpoint_matches_socket(&record.endpoint, socket_path) {
                return Ok(ResolvedDaemonConnection { record });
            }
            named_elsewhere.push(record.endpoint.to_string());
        }
    }
    let records = checked
        .iter()
        .map(|path| format!("'{}'", path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let socket = socket_path.display();
    Err(if named_elsewhere.is_empty() {
        TraceDecayError::project_route(
            DAEMON_AUTHORITY_UNAVAILABLE,
            true,
            format!(
                "no TraceDecay daemon authority record names socket '{socket}' (checked {records}). Start or restart the daemon."
            ),
        )
    } else {
        TraceDecayError::project_route(
            DAEMON_AUTHORITY_UNAVAILABLE,
            false,
            format!(
                "TraceDecay daemon authority records {records} name {} instead of socket '{socket}'. Point {} at the running daemon or restart it.",
                named_elsewhere.join(", "),
                tracedecay_daemon_protocol::SOCKET_ENV,
            ),
        )
    })
}

pub fn client_connection(socket_path: &Path) -> Result<ResolvedDaemonConnection> {
    #[cfg(unix)]
    {
        connection_for_socket_path(socket_path)
    }
    #[cfg(not(unix))]
    {
        let _ = socket_path;
        current_daemon_connection()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn route(error: &TraceDecayError) -> Option<(&str, bool)> {
        error
            .project_route_context()
            .map(|(code, retryable, _)| (code, retryable))
    }

    #[test]
    fn socket_without_an_authority_record_is_a_typed_retryable_absence() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");

        let error = connection_for_socket_in(None, &socket)
            .expect_err("a socket no record names has no credential");

        assert_eq!(route(&error), Some((DAEMON_AUTHORITY_UNAVAILABLE, true)));
        let record = authority::record_path(temp.path()).unwrap();
        assert!(
            error.to_string().contains(&record.display().to_string()),
            "{error}"
        );
    }

    #[test]
    fn socket_resolves_only_from_a_readable_record_that_names_it() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        let authority = authority::DaemonAuthority::acquire(
            temp.path(),
            &DaemonEndpoint::Unix(socket.clone()),
            "test",
        )
        .unwrap();

        let connection = connection_for_socket_in(None, &socket).unwrap();
        assert_eq!(connection.auth_token(), authority.auth_token());

        let other = temp.path().join("other.sock");
        let error = connection_for_socket_in(None, &other)
            .expect_err("a record naming another socket is not this daemon's credential");
        assert_eq!(route(&error), Some((DAEMON_AUTHORITY_UNAVAILABLE, false)));

        let record = authority::record_path(temp.path()).unwrap();
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = connection_for_socket_in(None, &socket)
            .expect_err("an unreadable record must not be skipped");
        assert_eq!(route(&error), None);
        assert!(error.to_string().contains("not private"), "{error}");
    }
}

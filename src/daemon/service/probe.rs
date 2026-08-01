use std::io::{BufRead, BufReader, Read, Write as IoWrite};
#[cfg(not(unix))]
use std::net::TcpStream as StdTcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;

use crate::errors::{Result, TraceDecayError};

#[cfg(unix)]
use super::default_socket_path;

trait ProbeStream: Read + IoWrite {
    fn set_probe_read_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()>;
}

#[cfg(unix)]
impl ProbeStream for StdUnixStream {
    fn set_probe_read_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }
}

#[cfg(not(unix))]
impl ProbeStream for StdTcpStream {
    fn set_probe_read_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }
}

/// Whether a daemon is accepting connections at the default socket path.
///
/// Installers use this to warn when a daemon-scheduled feature is enabled but
/// no daemon service is running to execute it.
#[cfg(unix)]
pub fn daemon_reachable() -> bool {
    default_socket_path().is_ok_and(|path| StdUnixStream::connect(path).is_ok())
}

#[cfg(not(unix))]
pub fn daemon_reachable() -> bool {
    super::default_socket_path()
        .is_ok_and(|path| matches!(daemon_socket_state(&path), DaemonSocketState::Connectable))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonSocketState {
    Missing,
    Connectable,
    Stale,
    #[cfg(unix)]
    PresentNotAccessible,
    PresentUnreachable,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DaemonProtocolState {
    NotRequired,
    Ready,
    Unresponsive(String),
    IdentityMismatch {
        name: Option<String>,
        version: Option<String>,
    },
}

impl std::fmt::Display for DaemonProtocolState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRequired => f.write_str("not required"),
            Self::Ready => f.write_str("ready"),
            Self::Unresponsive(error) => write!(f, "unresponsive ({error})"),
            Self::IdentityMismatch { name, version } => write!(
                f,
                "identity mismatch (name={}, version={}, expected name=tracedecay, version={})",
                name.as_deref().unwrap_or("missing"),
                version.as_deref().unwrap_or("missing"),
                crate::version::build_version()
            ),
        }
    }
}

#[cfg(unix)]
pub(super) fn daemon_protocol_state(socket_path: &Path) -> DaemonProtocolState {
    daemon_protocol_state_with_timeout(socket_path, std::time::Duration::from_secs(10))
}

#[cfg(not(unix))]
pub(super) fn daemon_protocol_state(transport_hint: &Path) -> DaemonProtocolState {
    daemon_protocol_state_with_timeout(transport_hint, std::time::Duration::from_secs(10))
}

pub(super) fn daemon_protocol_state_with_timeout(
    transport_hint: &Path,
    timeout: std::time::Duration,
) -> DaemonProtocolState {
    match query_daemon_identity(transport_hint, timeout) {
        Ok((name, version))
            if name.as_deref() == Some("tracedecay")
                && version.as_deref() == Some(crate::version::build_version()) =>
        {
            DaemonProtocolState::Ready
        }
        Ok((name, version)) => DaemonProtocolState::IdentityMismatch { name, version },
        Err(error) => DaemonProtocolState::Unresponsive(error.to_string()),
    }
}

#[cfg(unix)]
fn query_daemon_identity(
    socket_path: &Path,
    probe_timeout: std::time::Duration,
) -> Result<(Option<String>, Option<String>)> {
    // The first initialize after a restart can sit behind startup recovery
    // on the daemon side; a sub-second read deadline misclassifies a busy,
    // healthy daemon as unresponsive.
    let connection = super::super::client_connection(socket_path)?;
    let stream = StdUnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(probe_timeout))?;
    stream.set_write_timeout(Some(probe_timeout))?;
    query_daemon_identity_stream(stream, connection.auth_token.as_deref(), probe_timeout)
}

#[cfg(not(unix))]
fn query_daemon_identity(
    socket_path: &Path,
    probe_timeout: std::time::Duration,
) -> Result<(Option<String>, Option<String>)> {
    let (address, auth_token) =
        current_loopback_authority(socket_path)?.ok_or_else(missing_loopback_authority)?;
    let stream = StdTcpStream::connect_timeout(&address, probe_timeout)?;
    stream.set_read_timeout(Some(probe_timeout))?;
    stream.set_write_timeout(Some(probe_timeout))?;
    query_daemon_identity_stream(stream, Some(&auth_token), probe_timeout)
}

fn query_daemon_identity_stream(
    mut stream: impl ProbeStream,
    auth_token: Option<&str>,
    probe_timeout: std::time::Duration,
) -> Result<(Option<String>, Option<String>)> {
    const REQUEST_ID: i64 = 1;
    let handshake = super::super::DaemonHandshake::for_current_client(None, None, false, false)?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": REQUEST_ID,
        "method": "initialize"
    });
    let mut preamble = String::new();
    if let Some(auth_token) = auth_token {
        preamble.push_str(&super::super::transport::DaemonAuthPreface::new(auth_token).to_line()?);
        preamble.push('\n');
    }
    preamble.push_str(&handshake.to_line()?);
    preamble.push('\n');
    preamble.push_str(&request.to_string());
    preamble.push('\n');
    IoWrite::write_all(&mut stream, preamble.as_bytes())?;
    IoWrite::flush(&mut stream)?;

    let deadline = std::time::Instant::now() + probe_timeout;
    let mut reader = BufReader::new(stream);
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(TraceDecayError::Config {
                message: "daemon readiness probe exceeded its absolute deadline".to_string(),
            });
        }
        reader
            .get_ref()
            .set_probe_read_timeout(deadline.saturating_duration_since(now))?;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(TraceDecayError::Config {
                message: "daemon closed the readiness probe before returning initialize"
                    .to_string(),
            });
        }
        let response: serde_json::Value = serde_json::from_str(line.trim())?;
        if response.get("id") != Some(&serde_json::json!(REQUEST_ID)) {
            continue;
        }
        if let Some(error) = response.get("error") {
            return Err(TraceDecayError::Config {
                message: format!("daemon rejected the readiness probe: {error}"),
            });
        }
        let name = response
            .pointer("/result/serverInfo/name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let version = response
            .pointer("/result/serverInfo/version")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        return Ok((name, version));
    }
}

#[cfg(not(unix))]
pub(super) enum DaemonShutdownRequest {
    Acknowledged,
    SentWithoutAcknowledgement(String),
}

#[cfg(not(unix))]
pub(super) fn request_daemon_shutdown(transport_hint: &Path) -> Result<DaemonShutdownRequest> {
    const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let (address, auth_token) =
        current_loopback_authority(transport_hint)?.ok_or_else(missing_loopback_authority)?;
    let stream = StdTcpStream::connect_timeout(&address, SHUTDOWN_TIMEOUT)?;
    stream.set_read_timeout(Some(SHUTDOWN_TIMEOUT))?;
    stream.set_write_timeout(Some(SHUTDOWN_TIMEOUT))?;
    request_daemon_shutdown_stream(stream, &auth_token, SHUTDOWN_TIMEOUT)
}

#[cfg(not(unix))]
fn request_daemon_shutdown_stream(
    mut stream: impl ProbeStream,
    auth_token: &str,
    shutdown_timeout: std::time::Duration,
) -> Result<DaemonShutdownRequest> {
    const REQUEST_ID: i64 = 2;
    let handshake = super::super::DaemonHandshake::for_current_client(None, None, false, false)?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": REQUEST_ID,
        "method": super::super::DAEMON_SHUTDOWN_METHOD,
    });
    let preface = super::super::transport::DaemonAuthPreface::new(auth_token).to_line()?;
    let handshake = handshake.to_line()?;
    let request = request.to_string();
    let preamble = format!("{preface}\n{handshake}\n{request}\n");

    let deadline = std::time::Instant::now() + shutdown_timeout;
    let acknowledgement = (|| -> Result<()> {
        IoWrite::write_all(&mut stream, preamble.as_bytes())?;
        IoWrite::flush(&mut stream)?;
        let mut reader = BufReader::new(stream);
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(TraceDecayError::Config {
                    message: "daemon shutdown request exceeded its absolute deadline".to_string(),
                });
            }
            reader
                .get_ref()
                .set_probe_read_timeout(deadline.saturating_duration_since(now))?;
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err(TraceDecayError::Config {
                    message: "daemon closed the shutdown request before acknowledging it"
                        .to_string(),
                });
            }
            let response: serde_json::Value = serde_json::from_str(line.trim())?;
            if response.get("id") != Some(&serde_json::json!(REQUEST_ID)) {
                continue;
            }
            if shutdown_response_accepted(line.trim(), REQUEST_ID) {
                return Ok(());
            }
            return Err(TraceDecayError::Config {
                message: format!("daemon rejected the authenticated shutdown request: {response}"),
            });
        }
    })();
    Ok(match acknowledgement {
        Ok(()) => DaemonShutdownRequest::Acknowledged,
        Err(error) => DaemonShutdownRequest::SentWithoutAcknowledgement(error.to_string()),
    })
}

#[cfg(any(not(unix), test))]
pub(super) fn shutdown_response_accepted(line: &str, request_id: i64) -> bool {
    let Ok(response) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    response.get("id") == Some(&serde_json::json!(request_id))
        && response.pointer("/result/accepted") == Some(&serde_json::Value::Bool(true))
        && response.get("error").is_none()
}

impl DaemonSocketState {
    pub(super) fn is_proven_quiesced(self) -> bool {
        matches!(self, Self::Missing | Self::Stale)
    }
}

impl std::fmt::Display for DaemonSocketState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Missing => "missing",
            Self::Connectable => "connectable",
            Self::Stale => "stale",
            #[cfg(unix)]
            Self::PresentNotAccessible => "present but not accessible",
            Self::PresentUnreachable => "present but unreachable",
        };
        f.write_str(text)
    }
}

#[cfg(unix)]
pub(super) fn daemon_socket_state(socket_path: &Path) -> DaemonSocketState {
    if !socket_path.exists() {
        return DaemonSocketState::Missing;
    }
    match StdUnixStream::connect(socket_path) {
        Ok(_) => DaemonSocketState::Connectable,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => DaemonSocketState::Stale,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            DaemonSocketState::PresentNotAccessible
        }
        Err(_) => DaemonSocketState::PresentUnreachable,
    }
}

#[cfg(not(unix))]
pub(super) fn daemon_socket_state(transport_hint: &Path) -> DaemonSocketState {
    daemon_socket_state_with_timeout(transport_hint, std::time::Duration::from_millis(250))
}

#[cfg(not(unix))]
pub(super) fn daemon_socket_state_with_timeout(
    transport_hint: &Path,
    timeout: std::time::Duration,
) -> DaemonSocketState {
    let address = match current_loopback_authority(transport_hint) {
        Ok(Some((address, _))) => address,
        Ok(None) => return DaemonSocketState::Missing,
        Err(_) => return DaemonSocketState::PresentUnreachable,
    };
    match StdTcpStream::connect_timeout(&address, timeout) {
        Ok(_) => DaemonSocketState::Connectable,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            DaemonSocketState::Stale
        }
        Err(_) => DaemonSocketState::PresentUnreachable,
    }
}

#[cfg(unix)]
pub(super) fn daemon_transport_display(socket_path: &Path) -> String {
    socket_path.display().to_string()
}

#[cfg(not(unix))]
pub(super) fn daemon_transport_display(transport_hint: &Path) -> String {
    current_loopback_authority(transport_hint).map_or_else(
        |_| "authority record unavailable".to_string(),
        |authority| {
            authority.map_or_else(
                || "authority record unavailable".to_string(),
                |(address, _)| format!("tcp://{address}"),
            )
        },
    )
}

#[cfg(not(unix))]
fn current_loopback_authority(
    transport_hint: &Path,
) -> Result<Option<(std::net::SocketAddr, String)>> {
    let profile_root = transport_hint
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(crate::config::user_data_dir)
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine TraceDecay user data directory".to_string(),
        })?;
    let profile_root = super::super::authority::canonical_identity_path(&profile_root)?;
    let Some(record) = super::super::authority::current_record(&profile_root)? else {
        return Ok(None);
    };
    if record.profile_root != profile_root {
        return Err(TraceDecayError::Config {
            message: "TraceDecay daemon authority record names a different profile".to_string(),
        });
    }
    let super::super::transport::DaemonEndpoint::Loopback(address) = record.endpoint;
    if !address.ip().is_loopback() {
        return Err(TraceDecayError::Config {
            message: format!("daemon authority endpoint '{address}' is not loopback"),
        });
    }
    Ok(Some((address, record.auth_token)))
}

#[cfg(not(unix))]
fn missing_loopback_authority() -> TraceDecayError {
    TraceDecayError::Config {
        message: "TraceDecay daemon authority record is not available".to_string(),
    }
}

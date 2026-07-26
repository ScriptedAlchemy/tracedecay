#[cfg(unix)]
use std::io::{BufRead, BufReader, Write as IoWrite};
#[cfg(not(unix))]
use std::net::TcpStream as StdTcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;

#[cfg(unix)]
use crate::errors::{Result, TraceDecayError};

#[cfg(unix)]
use super::default_socket_path;

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
    let Some(profile_root) = crate::config::user_data_dir() else {
        return false;
    };
    let Ok(profile_root) = super::super::authority::canonical_identity_path(&profile_root) else {
        return false;
    };
    let Ok(Some(record)) = super::super::authority::current_record(&profile_root) else {
        return false;
    };
    if record.profile_root != profile_root {
        return false;
    }
    let super::super::transport::DaemonEndpoint::Loopback(address) = record.endpoint;
    address.ip().is_loopback()
        && StdTcpStream::connect_timeout(&address, std::time::Duration::from_millis(250)).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonSocketState {
    Missing,
    Connectable,
    #[cfg(unix)]
    Stale,
    #[cfg(unix)]
    PresentNotAccessible,
    #[cfg(unix)]
    PresentUnreachable,
    #[cfg(not(unix))]
    Present,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum DaemonProtocolState {
    NotRequired,
    #[cfg_attr(not(unix), allow(dead_code))]
    Ready,
    Unresponsive(String),
    #[cfg_attr(not(unix), allow(dead_code))]
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
    match query_daemon_identity(socket_path) {
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

#[cfg(not(unix))]
pub(super) fn daemon_protocol_state(_socket_path: &Path) -> DaemonProtocolState {
    DaemonProtocolState::Unresponsive("daemon protocol is unavailable on this platform".to_string())
}

#[cfg(unix)]
fn query_daemon_identity(socket_path: &Path) -> Result<(Option<String>, Option<String>)> {
    // The first initialize after a restart can sit behind startup recovery
    // on the daemon side; a sub-second read deadline misclassifies a busy,
    // healthy daemon as unresponsive.
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
    const REQUEST_ID: i64 = 1;

    let connection = super::super::client_connection(socket_path)?;
    let mut stream = StdUnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT))?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT))?;
    let handshake = super::super::DaemonHandshake::for_current_client(None, None, false, false)?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": REQUEST_ID,
        "method": "initialize"
    });
    let mut preamble = String::new();
    if let Some(auth_token) = connection.auth_token.as_deref() {
        preamble.push_str(&super::super::transport::DaemonAuthPreface::new(auth_token).to_line()?);
        preamble.push('\n');
    }
    preamble.push_str(&handshake.to_line()?);
    preamble.push('\n');
    preamble.push_str(&request.to_string());
    preamble.push('\n');
    IoWrite::write_all(&mut stream, preamble.as_bytes())?;
    IoWrite::flush(&mut stream)?;

    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let mut reader = BufReader::new(stream);
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Err(TraceDecayError::Config {
                message: "daemon readiness probe exceeded its absolute deadline".to_string(),
            });
        }
        reader
            .get_mut()
            .set_read_timeout(Some(deadline.saturating_duration_since(now)))?;
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

impl DaemonSocketState {
    pub(super) fn is_proven_quiesced(self) -> bool {
        #[cfg(unix)]
        {
            matches!(self, Self::Missing | Self::Stale)
        }
        #[cfg(not(unix))]
        {
            matches!(self, Self::Missing)
        }
    }
}

impl std::fmt::Display for DaemonSocketState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Missing => "missing",
            Self::Connectable => "connectable",
            #[cfg(unix)]
            Self::Stale => "stale",
            #[cfg(unix)]
            Self::PresentNotAccessible => "present but not accessible",
            #[cfg(unix)]
            Self::PresentUnreachable => "present but unreachable",
            #[cfg(not(unix))]
            Self::Present => "present",
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
pub(super) fn daemon_socket_state(socket_path: &Path) -> DaemonSocketState {
    if socket_path.exists() {
        DaemonSocketState::Present
    } else {
        DaemonSocketState::Missing
    }
}

use std::io::{BufRead, BufReader, Read, Write as IoWrite};
#[cfg(not(unix))]
use std::net::TcpStream as StdTcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;

#[cfg(not(unix))]
use tracedecay_daemon_identity::authority;
#[cfg(unix)]
use tracedecay_daemon_identity::client_connection;
use tracedecay_domain::errors::{Result, TraceDecayError};

#[cfg(unix)]
use super::default_socket_path;

trait ProbeStream: Read + IoWrite {
    fn set_probe_read_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()>;
    fn set_probe_write_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()>;
}

#[cfg(unix)]
impl ProbeStream for StdUnixStream {
    fn set_probe_read_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }

    fn set_probe_write_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.set_write_timeout(Some(timeout))
    }
}

#[cfg(not(unix))]
impl ProbeStream for StdTcpStream {
    fn set_probe_read_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }

    fn set_probe_write_timeout(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.set_write_timeout(Some(timeout))
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
        expected_version: String,
    },
}

impl std::fmt::Display for DaemonProtocolState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRequired => f.write_str("not required"),
            Self::Ready => f.write_str("ready"),
            Self::Unresponsive(error) => write!(f, "unresponsive ({error})"),
            Self::IdentityMismatch {
                name,
                version,
                expected_version,
            } => write!(
                f,
                "identity mismatch (name={}, version={}, expected name=tracedecay, version={})",
                name.as_deref().unwrap_or("missing"),
                version.as_deref().unwrap_or("missing"),
                expected_version
            ),
        }
    }
}

#[hotpath::measure(label = "daemon.service.probe.protocol_state")]
// Only the Windows task path and the update-restore tests probe with an
// explicit timeout since readiness unified on one authenticated connection.
#[cfg(any(test, windows))]
pub(super) fn daemon_protocol_state_with_timeout(
    transport_hint: &Path,
    expected_version: &str,
    timeout: std::time::Duration,
) -> DaemonProtocolState {
    daemon_readiness_probe(transport_hint, expected_version, timeout).1
}

fn classify_daemon_protocol_identity(
    identity: Result<(Option<String>, Option<String>)>,
    expected_version: &str,
) -> DaemonProtocolState {
    match identity {
        Ok((name, version))
            if name.as_deref() == Some("tracedecay")
                && version.as_deref() == Some(expected_version) =>
        {
            DaemonProtocolState::Ready
        }
        Ok((name, version)) => DaemonProtocolState::IdentityMismatch {
            name,
            version,
            expected_version: expected_version.to_owned(),
        },
        Err(error) => DaemonProtocolState::Unresponsive(error.to_string()),
    }
}

#[cfg(unix)]
#[hotpath::measure(label = "daemon.service.probe.readiness")]
pub(super) fn daemon_readiness_probe(
    socket_path: &Path,
    expected_version: &str,
    timeout: std::time::Duration,
) -> (DaemonSocketState, DaemonProtocolState) {
    if !socket_path.exists() {
        return (
            DaemonSocketState::Missing,
            DaemonProtocolState::Unresponsive(format!(
                "TraceDecay daemon socket '{}' does not exist",
                socket_path.display()
            )),
        );
    }
    let connection = match client_connection(socket_path) {
        Ok(connection) => connection,
        Err(error) => {
            // The client identity could not be resolved, so nothing has been
            // connected yet: report the socket's observed state instead of
            // asserting it is connectable.
            return (
                daemon_socket_state(socket_path),
                DaemonProtocolState::Unresponsive(error.to_string()),
            );
        }
    };
    let stream = match StdUnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(error) => {
            let socket_state = match error.kind() {
                std::io::ErrorKind::ConnectionRefused => DaemonSocketState::Stale,
                std::io::ErrorKind::PermissionDenied => DaemonSocketState::PresentNotAccessible,
                _ => DaemonSocketState::PresentUnreachable,
            };
            return (
                socket_state,
                DaemonProtocolState::Unresponsive(error.to_string()),
            );
        }
    };
    let deadline = std::time::Instant::now() + timeout;
    let identity = query_daemon_identity_stream(
        stream,
        connection.auth_token.as_deref(),
        expected_version,
        deadline,
    );
    (
        DaemonSocketState::Connectable,
        classify_daemon_protocol_identity(identity, expected_version),
    )
}

#[cfg(not(unix))]
#[hotpath::measure(label = "daemon.service.probe.readiness")]
pub(super) fn daemon_readiness_probe(
    transport_hint: &Path,
    expected_version: &str,
    timeout: std::time::Duration,
) -> (DaemonSocketState, DaemonProtocolState) {
    let (address, auth_token, _) = match current_loopback_authority(transport_hint) {
        Ok(Some(authority)) => authority,
        Ok(None) => {
            return (
                DaemonSocketState::Missing,
                DaemonProtocolState::Unresponsive(
                    "TraceDecay daemon authority record is not available".to_owned(),
                ),
            );
        }
        Err(error) => {
            return (
                DaemonSocketState::PresentUnreachable,
                DaemonProtocolState::Unresponsive(error.to_string()),
            );
        }
    };
    let deadline = std::time::Instant::now() + timeout;
    let remaining = match remaining_probe_time(deadline, "daemon readiness probe") {
        Ok(remaining) => remaining,
        Err(error) => {
            return (
                DaemonSocketState::PresentUnreachable,
                DaemonProtocolState::Unresponsive(error.to_string()),
            );
        }
    };
    let stream = match StdTcpStream::connect_timeout(&address, remaining) {
        Ok(stream) => stream,
        Err(error) => {
            let socket_state = if error.kind() == std::io::ErrorKind::ConnectionRefused {
                DaemonSocketState::Stale
            } else {
                DaemonSocketState::PresentUnreachable
            };
            return (
                socket_state,
                DaemonProtocolState::Unresponsive(error.to_string()),
            );
        }
    };
    let identity =
        query_daemon_identity_stream(stream, Some(&auth_token), expected_version, deadline);
    (
        DaemonSocketState::Connectable,
        classify_daemon_protocol_identity(identity, expected_version),
    )
}

fn query_daemon_identity_stream(
    mut stream: impl ProbeStream,
    auth_token: Option<&str>,
    client_version: &str,
    deadline: std::time::Instant,
) -> Result<(Option<String>, Option<String>)> {
    const REQUEST_ID: i64 = 1;
    let handshake = crate::handshake_for_current_client(client_version, None, None, false, false)?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": REQUEST_ID,
        "method": "initialize"
    });
    let mut preamble = String::new();
    if let Some(auth_token) = auth_token {
        preamble
            .push_str(&tracedecay_daemon_protocol::DaemonAuthPreface::new(auth_token).to_line()?);
        preamble.push('\n');
    }
    preamble.push_str(&handshake.to_line()?);
    preamble.push('\n');
    preamble.push_str(&request.to_string());
    preamble.push('\n');
    arm_probe_timeout(deadline, "daemon readiness probe", |timeout| {
        stream.set_probe_write_timeout(timeout)
    })?;
    IoWrite::write_all(&mut stream, preamble.as_bytes())?;
    IoWrite::flush(&mut stream)?;

    let mut reader = BufReader::new(stream);
    loop {
        arm_probe_timeout(deadline, "daemon readiness probe", |timeout| {
            reader.get_ref().set_probe_read_timeout(timeout)
        })?;
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
pub(super) fn request_daemon_shutdown(
    transport_hint: &Path,
    client_version: &str,
) -> Result<DaemonShutdownRequest> {
    const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    let deadline = std::time::Instant::now() + SHUTDOWN_TIMEOUT;
    let (address, auth_token, _) =
        current_loopback_authority(transport_hint)?.ok_or_else(missing_loopback_authority)?;
    let remaining = remaining_probe_time(deadline, "daemon shutdown request")?;
    let stream = StdTcpStream::connect_timeout(&address, remaining)?;
    request_daemon_shutdown_stream(stream, &auth_token, client_version, deadline)
}

#[cfg(not(unix))]
fn request_daemon_shutdown_stream(
    mut stream: impl ProbeStream,
    auth_token: &str,
    client_version: &str,
    deadline: std::time::Instant,
) -> Result<DaemonShutdownRequest> {
    const REQUEST_ID: i64 = 2;
    let handshake = crate::handshake_for_current_client(client_version, None, None, false, false)?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": REQUEST_ID,
        "method": tracedecay_daemon_protocol::DAEMON_SHUTDOWN_METHOD,
    });
    let preface = tracedecay_daemon_protocol::DaemonAuthPreface::new(auth_token).to_line()?;
    let handshake = handshake.to_line()?;
    let request = request.to_string();
    let preamble = format!("{preface}\n{handshake}\n{request}\n");

    arm_probe_timeout(deadline, "daemon shutdown request", |timeout| {
        stream.set_probe_write_timeout(timeout)
    })?;
    let acknowledgement = (|| -> Result<()> {
        IoWrite::write_all(&mut stream, preamble.as_bytes())?;
        IoWrite::flush(&mut stream)?;
        let mut reader = BufReader::new(stream);
        loop {
            arm_probe_timeout(deadline, "daemon shutdown request", |timeout| {
                reader.get_ref().set_probe_read_timeout(timeout)
            })?;
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

fn deadline_exceeded(operation: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{operation} exceeded its absolute deadline"),
    }
}

fn remaining_probe_time(
    deadline: std::time::Instant,
    operation: &str,
) -> Result<std::time::Duration> {
    deadline
        .checked_duration_since(std::time::Instant::now())
        // `SO_RCVTIMEO`/`SO_SNDTIMEO` use `timeval`. Zero is EINVAL on every
        // platform; macOS also rejects some sub-microsecond remainders after
        // `tv_usec` truncation. Those are a spent budget, not an I/O failure.
        .filter(|remaining| remaining.as_micros() >= 1)
        .ok_or_else(|| deadline_exceeded(operation))
}

fn arm_probe_timeout(
    deadline: std::time::Instant,
    operation: &str,
    set_timeout: impl FnOnce(std::time::Duration) -> std::io::Result<()>,
) -> Result<()> {
    let remaining = remaining_probe_time(deadline, operation)?;
    match set_timeout(remaining) {
        Ok(()) => Ok(()),
        // macOS: `setsockopt(SO_RCVTIMEO)` after the peer answered and closed
        // the Unix socket returns EINVAL. The reply is still in the receive
        // buffer and must be classified.
        Err(error) if error.raw_os_error() == Some(22) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            Err(deadline_exceeded(operation))
        }
        Err(error) => Err(error.into()),
    }
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
        Ok(Some((address, _, _))) => address,
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
                |(address, _, _)| format!("tcp://{address}"),
            )
        },
    )
}

#[cfg(not(unix))]
fn current_loopback_authority(
    transport_hint: &Path,
) -> Result<Option<(std::net::SocketAddr, String, String)>> {
    let profile_root = transport_hint
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(tracedecay_runtime_core::config::user_data_dir)
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine TraceDecay user data directory".to_string(),
        })?;
    let profile_root = authority::canonical_identity_path(&profile_root)?;
    let Some(record) = authority::current_record(&profile_root)? else {
        return Ok(None);
    };
    if record.profile_root != profile_root {
        return Err(TraceDecayError::Config {
            message: "TraceDecay daemon authority record names a different profile".to_string(),
        });
    }
    let tracedecay_daemon_protocol::DaemonEndpoint::Loopback(address) = record.endpoint;
    if !address.ip().is_loopback() {
        return Err(TraceDecayError::Config {
            message: format!("daemon authority endpoint '{address}' is not loopback"),
        });
    }
    Ok(Some((address, record.auth_token, record.version)))
}

#[cfg(not(unix))]
fn missing_loopback_authority() -> TraceDecayError {
    TraceDecayError::Config {
        message: "TraceDecay daemon authority record is not available".to_string(),
    }
}

#[cfg(test)]
mod timeout_classification_tests {
    use std::io::{self, Cursor, Read, Write};
    use std::time::{Duration, Instant};

    use tracedecay_runtime_core::config::PinnedUserDataDir;

    use super::{
        ProbeStream, arm_probe_timeout, query_daemon_identity_stream, remaining_probe_time,
    };

    struct AnsweredThenReadTimeoutEinval {
        reply: Cursor<Vec<u8>>,
    }

    impl Read for AnsweredThenReadTimeoutEinval {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reply.read(buf)
        }
    }

    impl Write for AnsweredThenReadTimeoutEinval {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl ProbeStream for AnsweredThenReadTimeoutEinval {
        fn set_probe_read_timeout(&self, _timeout: Duration) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(22))
        }

        fn set_probe_write_timeout(&self, _timeout: Duration) -> io::Result<()> {
            Ok(())
        }
    }

    struct UnusedStream;

    impl Read for UnusedStream {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            panic!("spent probe budget must not touch the stream");
        }
    }

    impl Write for UnusedStream {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            panic!("spent probe budget must not touch the stream");
        }

        fn flush(&mut self) -> io::Result<()> {
            panic!("spent probe budget must not touch the stream");
        }
    }

    impl ProbeStream for UnusedStream {
        fn set_probe_read_timeout(&self, _timeout: Duration) -> io::Result<()> {
            panic!("spent probe budget must not arm a socket timeout");
        }

        fn set_probe_write_timeout(&self, _timeout: Duration) -> io::Result<()> {
            panic!("spent probe budget must not arm a socket timeout");
        }
    }

    #[test]
    fn readiness_probe_classifies_denial_after_read_timeout_einval() {
        let _profile = PinnedUserDataDir::new();
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32001, "message": "authentication denied"}
        });
        let stream = AnsweredThenReadTimeoutEinval {
            reply: Cursor::new(format!("{response}\n").into_bytes()),
        };
        let error = query_daemon_identity_stream(
            stream,
            Some("token"),
            "0.1.0-test+service-probe",
            Instant::now() + Duration::from_secs(1),
        )
        .expect_err("authentication denial must stay unresponsive");
        assert!(
            error.to_string().contains("authentication denied"),
            "{error}"
        );
    }

    #[test]
    fn spent_readiness_probe_budget_is_typed_deadline() {
        let _profile = PinnedUserDataDir::new();
        let error = query_daemon_identity_stream(
            UnusedStream,
            Some("token"),
            "0.1.0-test+service-probe",
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .expect("clock can express a spent probe deadline"),
        )
        .expect_err("spent budget must fail closed");
        assert!(
            error
                .to_string()
                .contains("daemon readiness probe exceeded its absolute deadline"),
            "{error}"
        );
    }

    #[test]
    fn remaining_probe_time_rejects_spent_budget() {
        let error = remaining_probe_time(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .expect("clock can express a spent probe deadline"),
            "daemon readiness probe",
        )
        .expect_err("spent budget must be a typed deadline");
        assert!(
            error
                .to_string()
                .contains("daemon readiness probe exceeded its absolute deadline"),
            "{error}"
        );
    }

    #[test]
    fn arm_probe_timeout_maps_zero_socket_timeout_to_deadline() {
        let error = arm_probe_timeout(
            Instant::now() + Duration::from_secs(1),
            "daemon readiness probe",
            |_| {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot set a 0 duration timeout",
                ))
            },
        )
        .expect_err("zero socket timeout is a spent budget");
        assert!(
            error
                .to_string()
                .contains("daemon readiness probe exceeded its absolute deadline"),
            "{error}"
        );
    }
}

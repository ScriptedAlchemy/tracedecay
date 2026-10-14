use std::fmt;
use std::net::{IpAddr, SocketAddr};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll, Waker};

use asupersync::io::{AsyncRead, AsyncWrite, ReadBuf};
use serde::{Deserialize, Serialize};

use tracedecay_domain::errors::{Result, TraceDecayError};

pub const AUTH_PREFACE_PROTOCOL: &str = "tracedecay-daemon-v1";

/// Env var naming the daemon socket path override.
pub const SOCKET_ENV: &str = "TRACEDECAY_DAEMON_SOCKET";

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "address", rename_all = "snake_case")]
pub enum DaemonEndpoint {
    #[cfg(unix)]
    Unix(PathBuf),
    Loopback(SocketAddr),
}

impl DaemonEndpoint {
    pub fn loopback(address: SocketAddr) -> Result<Self> {
        if !address.ip().is_loopback() {
            return Err(config_error(format!(
                "daemon TCP endpoint must be loopback, got {address}"
            )));
        }
        Ok(Self::Loopback(address))
    }
}

impl FromStr for DaemonEndpoint {
    type Err = TraceDecayError;

    fn from_str(value: &str) -> Result<Self> {
        if let Some(address) = value
            .strip_prefix("tcp://")
            .or_else(|| value.strip_prefix("loopback://"))
        {
            let address = address
                .parse::<SocketAddr>()
                .map_err(|error| config_error(format!("invalid daemon endpoint: {error}")))?;
            return Self::loopback(address);
        }
        #[cfg(unix)]
        {
            let path = value.strip_prefix("unix://").unwrap_or(value);
            if path.is_empty() {
                return Err(config_error("daemon Unix endpoint path is empty"));
            }
            Ok(Self::Unix(PathBuf::from(path)))
        }
        #[cfg(not(unix))]
        Err(config_error(
            "daemon endpoint must use tcp://127.0.0.1:PORT on this platform",
        ))
    }
}

impl fmt::Display for DaemonEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => write!(f, "unix://{}", path.display()),
            Self::Loopback(address) => write!(f, "tcp://{address}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonAuthPreface {
    protocol: String,
    auth_token: String,
}

impl DaemonAuthPreface {
    pub fn new(auth_token: impl Into<String>) -> Self {
        Self {
            protocol: AUTH_PREFACE_PROTOCOL.to_string(),
            auth_token: auth_token.into(),
        }
    }

    pub fn to_line(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_line(line: &str) -> Result<Self> {
        let preface: Self = serde_json::from_str(line)?;
        if preface.protocol != AUTH_PREFACE_PROTOCOL {
            return Err(config_error("unsupported daemon transport protocol"));
        }
        Ok(preface)
    }

    pub fn authenticate(&self, expected_token: &str) -> bool {
        let supplied = self.auth_token.as_bytes();
        let expected = expected_token.as_bytes();
        if supplied.len() != expected.len() {
            return false;
        }
        supplied
            .iter()
            .zip(expected)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }
}

/// The client end of a daemon connection.
#[derive(Debug)]
pub enum DaemonClientStream {
    #[cfg(unix)]
    Unix(asupersync::net::unix::UnixStream),
    Tcp(asupersync::net::TcpStream),
}

pub enum DaemonClientReadHalf {
    #[cfg(unix)]
    Unix(asupersync::net::unix::OwnedReadHalf),
    Tcp(asupersync::net::tcp::split::OwnedReadHalf),
}

pub enum DaemonClientWriteHalf {
    #[cfg(unix)]
    Unix(asupersync::net::unix::OwnedWriteHalf),
    Tcp(asupersync::net::tcp::split::OwnedWriteHalf),
}

impl DaemonClientStream {
    pub async fn connect(endpoint: &DaemonEndpoint) -> Result<Self> {
        match endpoint {
            #[cfg(unix)]
            DaemonEndpoint::Unix(path) => Ok(Self::Unix(connect_unix(path)?)),
            DaemonEndpoint::Loopback(address) => {
                if !address.ip().is_loopback() {
                    return Err(config_error("refusing non-loopback daemon endpoint"));
                }
                // Boxed: the connect state machine would otherwise inflate
                // every caller's future past the large-future lint.
                Ok(Self::Tcp(
                    Box::pin(asupersync::net::TcpStream::connect(*address)).await?,
                ))
            }
        }
    }

    /// Dropping the write half shuts down the write direction, so the daemon
    /// reads EOF while the read half stays open.
    pub fn into_split(self) -> (DaemonClientReadHalf, DaemonClientWriteHalf) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let (reader, writer) = stream.into_split();
                (
                    DaemonClientReadHalf::Unix(reader),
                    DaemonClientWriteHalf::Unix(writer),
                )
            }
            Self::Tcp(stream) => {
                let (reader, writer) = stream.into_split();
                (
                    DaemonClientReadHalf::Tcp(reader),
                    DaemonClientWriteHalf::Tcp(writer),
                )
            }
        }
    }
}

/// A Unix stream socket connects synchronously or not at all, so one
/// non-blocking `connect(2)` settles it. A full listen backlog fails with
/// `EAGAIN` (`WouldBlock`), the saturated-daemon signal. Asupersync 0.5.0's
/// own `UnixStream::connect` treats `EAGAIN` as an in-progress connect and,
/// polled without its native I/O driver, re-wakes itself forever.
///
/// Upstream fixed this in asupersync commit 40a12bea0 (2026-10-03), which
/// first ships in 0.6.0. When this workspace moves to asupersync 0.6.0, delete
/// this workaround and `full_listen_backlog_fails_fast_as_saturated`, and
/// connect with `UnixStream::connect`.
#[cfg(unix)]
fn connect_unix(path: &Path) -> std::io::Result<asupersync::net::unix::UnixStream> {
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    socket.connect(&socket2::SockAddr::unix(path)?)?;
    asupersync::net::unix::UnixStream::from_std(socket.into())
}

impl AsyncRead for DaemonClientReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(reader) => Pin::new(reader).poll_read(context, buffer),
            Self::Tcp(reader) => Pin::new(reader).poll_read(context, buffer),
        }
    }
}

impl AsyncWrite for DaemonClientWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(writer) => Pin::new(writer).poll_write(context, buffer),
            Self::Tcp(writer) => Pin::new(writer).poll_write(context, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(writer) => Pin::new(writer).poll_flush(context),
            Self::Tcp(writer) => Pin::new(writer).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(writer) => Pin::new(writer).poll_shutdown(context),
            Self::Tcp(writer) => Pin::new(writer).poll_shutdown(context),
        }
    }
}

/// The daemon's end of an accepted connection.
#[derive(Debug)]
pub enum BrokerStream {
    #[cfg(unix)]
    Unix(asupersync::net::unix::UnixStream),
    Tcp(asupersync::net::TcpStream),
}

pub enum BrokerReadHalf {
    #[cfg(unix)]
    Unix(asupersync::net::unix::OwnedReadHalf),
    Tcp(asupersync::net::tcp::split::OwnedReadHalf),
}

pub enum BrokerWriteHalf {
    #[cfg(unix)]
    Unix(asupersync::net::unix::OwnedWriteHalf),
    Tcp(asupersync::net::tcp::split::OwnedWriteHalf),
}

impl BrokerStream {
    pub fn into_owned_split(self) -> (BrokerReadHalf, BrokerWriteHalf) {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => {
                let (reader, writer) = stream.into_split();
                (BrokerReadHalf::Unix(reader), BrokerWriteHalf::Unix(writer))
            }
            Self::Tcp(stream) => {
                let (reader, writer) = stream.into_split();
                (BrokerReadHalf::Tcp(reader), BrokerWriteHalf::Tcp(writer))
            }
        }
    }
}

impl AsyncRead for BrokerReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(reader) => Pin::new(reader).poll_read(context, buffer),
            Self::Tcp(reader) => Pin::new(reader).poll_read(context, buffer),
        }
    }
}

impl AsyncWrite for BrokerWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(writer) => Pin::new(writer).poll_write(context, buffer),
            Self::Tcp(writer) => Pin::new(writer).poll_write(context, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(writer) => Pin::new(writer).poll_flush(context),
            Self::Tcp(writer) => Pin::new(writer).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(writer) => Pin::new(writer).poll_shutdown(context),
            Self::Tcp(writer) => Pin::new(writer).poll_shutdown(context),
        }
    }
}

impl BrokerWriteHalf {
    /// True once the peer has closed both directions, not just its request
    /// half. A zero-length write sends nothing, yet fails after a full close:
    /// `EPIPE` on a Unix socket, the peer's reset on TCP. It never waits, so
    /// a caller may probe while holding the writer's lock, on any executor.
    pub fn peer_fully_closed(&mut self) -> bool {
        let mut context = Context::from_waker(Waker::noop());
        matches!(
            Pin::new(self).poll_write(&mut context, &[]),
            Poll::Ready(Err(error)) if !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            )
        )
    }
}

/// Test peers drive the daemon end with Tokio I/O.
#[cfg(any(test, feature = "test-helpers"))]
impl BrokerStream {
    /// The daemon end of a Unix socket a test built with Tokio.
    #[cfg(unix)]
    pub fn from_tokio_unix(stream: tokio::net::UnixStream) -> std::io::Result<Self> {
        Ok(Self::Unix(asupersync::net::unix::UnixStream::from_std(
            stream.into_std()?,
        )?))
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl tokio::io::AsyncRead for BrokerStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut native = ReadBuf::new(buffer.initialize_unfilled());
        let polled = match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_read(context, &mut native),
            Self::Tcp(stream) => Pin::new(stream).poll_read(context, &mut native),
        };
        let filled = native.filled().len();
        if let Poll::Ready(Ok(())) = polled {
            buffer.advance(filled);
        }
        polled
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl tokio::io::AsyncWrite for BrokerStream {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_write(context, buffer),
            Self::Tcp(stream) => Pin::new(stream).poll_write(context, buffer),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_flush(context),
            Self::Tcp(stream) => Pin::new(stream).poll_flush(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_shutdown(context),
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(context),
        }
    }
}

pub enum BrokerListener {
    #[cfg(unix)]
    Unix(asupersync::net::unix::UnixListener),
    Tcp(asupersync::net::TcpListener),
}

/// Owner-only mode the daemon's Unix socket must carry for its whole lifetime.
#[cfg(unix)]
const DAEMON_SOCKET_MODE: u32 = 0o600;

/// Longest socket path `bind(2)`/`connect(2)` accept on this platform.
///
/// `sockaddr_un` reserves 104 bytes for the NUL-terminated path on macOS and
/// the BSDs and 108 on Linux; a longer path fails the syscall itself, so it
/// must be refused (or re-derived) before it reaches the kernel.
#[cfg(unix)]
pub const MAX_UNIX_SOCKET_PATH_BYTES: usize =
    if cfg!(any(target_os = "linux", target_os = "android")) {
        107
    } else {
        103
    };

#[cfg(unix)]
pub fn unix_socket_path_within_limit(path: &Path) -> bool {
    path.as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES
}

/// Refuse to publish a socket through a symlink or a directory that is not
/// owned privately by the current user.
#[cfg(unix)]
pub fn ensure_private_socket_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    tracedecay_private_fs::validate_private_directory(parent).map_err(|error| {
        config_error(format!(
            "refusing to publish daemon socket '{}' outside a private directory owned by the current user '{}': {error}",
            path.display(),
            parent.display(),
        ))
    })
}

/// Binds the daemon's Unix socket and narrows it to its owner before the
/// listener is handed back.
///
/// `bind(2)` creates the socket inode with `0o777 & !umask`, so the socket is
/// briefly group- and world-connectable. Callers used to close that gap after
/// recording the endpoint in the authority file, which left the wide-open
/// socket live and discoverable across a durable write. Narrowing here means no
/// caller can publish or accept on a socket that is not already owner-only, and
/// a socket that cannot be narrowed is torn down instead of served.
#[cfg(unix)]
fn bind_owner_only_unix_listener(path: &Path) -> Result<asupersync::net::unix::UnixListener> {
    // Binding the real path (rather than staging elsewhere and renaming) keeps
    // `EADDRINUSE` as the kernel-level guarantee that only one daemon owns the
    // endpoint.
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    if let Err(e) =
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(DAEMON_SOCKET_MODE))
    {
        drop(listener);
        let _ = std::fs::remove_file(path);
        return Err(config_error(format!(
            "failed to restrict daemon socket '{}' to its owner: {e}",
            path.display()
        )));
    }
    Ok(asupersync::net::unix::UnixListener::from_std(listener)?)
}

impl BrokerListener {
    pub async fn bind(endpoint: &DaemonEndpoint) -> Result<(Self, DaemonEndpoint)> {
        match endpoint {
            #[cfg(unix)]
            DaemonEndpoint::Unix(path) => {
                // Refuse an over-long path with a typed remedy instead of the
                // kernel's opaque "invalid argument": SUN_LEN overflow must
                // never surface as an unexplained daemon startup failure.
                if !unix_socket_path_within_limit(path) {
                    return Err(config_error(format!(
                        "daemon socket path '{}' exceeds this platform's Unix socket path limit ({MAX_UNIX_SOCKET_PATH_BYTES} bytes); set {} to a shorter path",
                        path.display(),
                        crate::transport::SOCKET_ENV,
                    )));
                }
                ensure_private_socket_parent(path)?;
                let listener = bind_owner_only_unix_listener(path)?;
                Ok((Self::Unix(listener), endpoint.clone()))
            }
            DaemonEndpoint::Loopback(address) => {
                if !address.ip().is_loopback() {
                    return Err(config_error("refusing non-loopback daemon listener"));
                }
                let listener = asupersync::net::TcpListener::bind(*address).await?;
                let endpoint = DaemonEndpoint::loopback(listener.local_addr()?)?;
                Ok((Self::Tcp(listener), endpoint))
            }
        }
    }

    pub async fn accept(&self) -> Result<BrokerStream> {
        match self {
            #[cfg(unix)]
            Self::Unix(listener) => Ok(BrokerStream::Unix(listener.accept().await?.0)),
            Self::Tcp(listener) => Ok(BrokerStream::Tcp(listener.accept().await?.0)),
        }
    }
}

/// Contract-level default daemon endpoint constructor.
///
/// Dependents (the composition root and their tests) use this as the shared
/// loopback bind address (`127.0.0.1:0`) rather than reconstructing the same
/// [`DaemonEndpoint`] locally.
pub fn default_loopback_endpoint() -> DaemonEndpoint {
    DaemonEndpoint::Loopback(SocketAddr::new(
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[test]
    fn loopback_endpoint_round_trips_and_rejects_remote_addresses() {
        let endpoint = "tcp://127.0.0.1:43123".parse::<DaemonEndpoint>().unwrap();
        assert_eq!(endpoint.to_string(), "tcp://127.0.0.1:43123");
        assert!("tcp://192.0.2.1:43123".parse::<DaemonEndpoint>().is_err());
    }

    #[test]
    fn auth_preface_validates_protocol_and_token() {
        let preface = DaemonAuthPreface::new("0123456789abcdef");
        let decoded = DaemonAuthPreface::from_line(&preface.to_line().unwrap()).unwrap();
        assert!(decoded.authenticate("0123456789abcdef"));
        assert!(!decoded.authenticate("0123456789abcdee"));
        assert!(!decoded.authenticate("short"));
    }

    fn native_runtime() -> asupersync::runtime::Runtime {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "the broker socket must serve with no Tokio runtime entered"
        );
        asupersync::runtime::RuntimeBuilder::current_thread()
            .build()
            .expect("native runtime")
    }

    /// A one-shot client half-closes its request side and keeps reading, so
    /// only the later full close may end its session.
    #[cfg(unix)]
    #[test]
    fn unix_broker_accepts_and_tells_a_full_close_from_a_half_close_natively() {
        use std::os::unix::fs::PermissionsExt;

        let runtime = native_runtime();
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("daemon.sock");
        let (listener, _endpoint) = runtime
            .block_on(BrokerListener::bind(&DaemonEndpoint::Unix(path.clone())))
            .unwrap();

        let client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let (_reader, mut writer) = runtime
            .block_on(listener.accept())
            .unwrap()
            .into_owned_split();
        assert!(!writer.peer_fully_closed(), "an open peer is not closed");

        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(
            !writer.peer_fully_closed(),
            "a request-half close is not a full close"
        );

        drop(client);
        assert!(writer.peer_fully_closed(), "the full close must be seen");
    }

    /// TCP reports a vanished peer only through its reset, which the probe
    /// must surface as a full close.
    #[test]
    fn loopback_broker_accepts_and_sees_a_reset_peer_as_fully_closed_natively() {
        use asupersync::io::AsyncReadExt;

        let runtime = native_runtime();
        let (listener, endpoint) = runtime
            .block_on(BrokerListener::bind(&default_loopback_endpoint()))
            .unwrap();
        let DaemonEndpoint::Loopback(address) = endpoint else {
            panic!("loopback bind returned {endpoint}");
        };

        let client = std::net::TcpStream::connect(address).unwrap();
        let (mut reader, mut writer) = runtime
            .block_on(listener.accept())
            .unwrap()
            .into_owned_split();
        assert!(!writer.peer_fully_closed(), "an open peer is not closed");

        socket2::SockRef::from(&client)
            .set_linger(Some(std::time::Duration::ZERO))
            .unwrap();
        drop(client);
        let mut byte = [0_u8; 1];
        let reset = runtime.block_on(reader.read(&mut byte)).unwrap_err();
        assert_eq!(reset.kind(), std::io::ErrorKind::ConnectionReset);
        assert!(writer.peer_fully_closed(), "the reset must read as closed");
    }

    /// The daemon socket must never be observable with wider-than-owner
    /// permissions, so `bind` narrows it before any caller can publish the
    /// endpoint or accept on it.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_is_owner_only_the_moment_bind_returns() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("daemon.sock");
        let (_listener, _endpoint) = BrokerListener::bind(&DaemonEndpoint::Unix(path.clone()))
            .await
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, DAEMON_SOCKET_MODE, "daemon socket must be owner-only");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_rejects_non_private_parent_before_binding() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.path().join("daemon.sock");

        let Err(error) = BrokerListener::bind(&DaemonEndpoint::Unix(path.clone())).await else {
            panic!("public socket parent must be rejected");
        };

        assert!(matches!(&error, TraceDecayError::Config { .. }), "{error}");
        assert!(error.to_string().contains("private directory"), "{error}");
        assert!(!path.exists(), "socket must not be bound before rejection");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_rejects_symlinked_private_parent_before_binding() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked_parent = dir.path().join("linked-parent");
        symlink(&target, &linked_parent).unwrap();
        let path = linked_parent.join("daemon.sock");

        let Err(error) = BrokerListener::bind(&DaemonEndpoint::Unix(path.clone())).await else {
            panic!("symlinked socket parent must be rejected");
        };

        assert!(matches!(&error, TraceDecayError::Config { .. }), "{error}");
        assert!(error.to_string().contains("private directory"), "{error}");
        assert!(
            !target.join("daemon.sock").exists(),
            "rejection must not bind through the symlink"
        );
    }

    /// A `SUN_LEN` overflow must be a typed refusal naming its remedy, not the
    /// kernel's opaque bind failure that reads as an unexplained daemon crash.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_refuses_over_long_socket_path_with_a_typed_remedy() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir
            .path()
            .join("s".repeat(MAX_UNIX_SOCKET_PATH_BYTES))
            .join("daemon.sock");

        let Err(error) = BrokerListener::bind(&DaemonEndpoint::Unix(path.clone())).await else {
            panic!("over-long socket path must be refused before bind");
        };

        assert!(matches!(&error, TraceDecayError::Config { .. }), "{error}");
        let message = error.to_string();
        assert!(message.contains("Unix socket path limit"), "{message}");
        assert!(
            message.contains(crate::transport::SOCKET_ENV),
            "the refusal must name the override remedy: {message}"
        );
    }

    #[tokio::test]
    async fn loopback_listener_authenticates_twelve_concurrent_clients() {
        const CLIENTS: usize = 12;
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";

        let (listener, endpoint) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            for _ in 0..CLIENTS {
                let stream = listener.accept().await.unwrap();
                clients.spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    let preface = DaemonAuthPreface::from_line(line.trim()).unwrap();
                    assert!(preface.authenticate(TOKEN));
                });
            }
            while let Some(client) = clients.join_next().await {
                client.unwrap();
            }
        });

        let mut clients = tokio::task::JoinSet::new();
        for _ in 0..CLIENTS {
            let endpoint = endpoint.clone();
            clients.spawn(async move {
                use asupersync::io::AsyncWriteExt;

                let (_reader, mut writer) = DaemonClientStream::connect(&endpoint)
                    .await
                    .unwrap()
                    .into_split();
                let line = DaemonAuthPreface::new(TOKEN).to_line().unwrap();
                writer.write_all(line.as_bytes()).await.unwrap();
                writer.write_all(b"\n").await.unwrap();
                writer.shutdown().await.unwrap();
            });
        }
        while let Some(client) = clients.join_next().await {
            client.unwrap();
        }
        server.await.unwrap();
    }
}

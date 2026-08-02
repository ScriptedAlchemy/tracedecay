use std::fmt;
use std::future::Future;
#[cfg(any(not(unix), test))]
use std::net::IpAddr;
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::errors::{Result, TraceDecayError};

pub const AUTH_PREFACE_PROTOCOL: &str = "tracedecay-daemon-v1";

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

    #[cfg(test)]
    pub fn parse(value: &str) -> Result<Self> {
        value.parse()
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

#[derive(Debug)]
pub enum BrokerStream {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

/// Owned halves keep the concrete Tokio socket available for readiness
/// probes. The generic `tokio::io::split` halves intentionally hide it behind
/// a mutex and cannot report the distinction between RDHUP (request-half
/// close) and HUP (full peer close).
pub(super) enum BrokerReadHalf {
    #[cfg(unix)]
    Unix(tokio::net::unix::OwnedReadHalf),
    Tcp(tokio::net::tcp::OwnedReadHalf),
}

pub(super) enum BrokerWriteHalf {
    #[cfg(unix)]
    Unix(tokio::net::unix::OwnedWriteHalf),
    Tcp(tokio::net::tcp::OwnedWriteHalf),
}

impl BrokerStream {
    pub async fn connect(endpoint: &DaemonEndpoint) -> Result<Self> {
        match endpoint {
            #[cfg(unix)]
            DaemonEndpoint::Unix(path) => {
                Ok(Self::Unix(tokio::net::UnixStream::connect(path).await?))
            }
            DaemonEndpoint::Loopback(address) => {
                if !address.ip().is_loopback() {
                    return Err(config_error("refusing non-loopback daemon endpoint"));
                }
                Ok(Self::Tcp(tokio::net::TcpStream::connect(address).await?))
            }
        }
    }

    pub fn into_split(self) -> (tokio::io::ReadHalf<Self>, tokio::io::WriteHalf<Self>) {
        tokio::io::split(self)
    }

    pub(super) fn into_owned_split(self) -> (BrokerReadHalf, BrokerWriteHalf) {
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
    /// Poll the native writable-readiness future once without waiting while a
    /// caller holds the shared writer mutex. A pending readiness registration
    /// is retried by the caller on its next bounded polling interval.
    pub(super) async fn peer_write_readiness_now(
        &self,
    ) -> Option<std::io::Result<tokio::io::Ready>> {
        let mut readiness = Box::pin(self.peer_write_readiness());
        std::future::poll_fn(|context| match readiness.as_mut().poll(context) {
            std::task::Poll::Ready(result) => std::task::Poll::Ready(Some(result)),
            std::task::Poll::Pending => std::task::Poll::Ready(None),
        })
        .await
    }

    pub(super) async fn peer_write_readiness(&self) -> std::io::Result<tokio::io::Ready> {
        match self {
            #[cfg(unix)]
            Self::Unix(writer) => writer.ready(tokio::io::Interest::WRITABLE).await,
            Self::Tcp(writer) => writer.ready(tokio::io::Interest::WRITABLE).await,
        }
    }

    pub(super) fn consume_write_readiness(&self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(writer) => writer.try_write(&[]).map(|_| ()),
            Self::Tcp(writer) => writer.try_write(&[]).map(|_| ()),
        }
    }
}

impl AsyncRead for BrokerStream {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            Self::Unix(stream) => Pin::new(stream).poll_read(context, buffer),
            Self::Tcp(stream) => Pin::new(stream).poll_read(context, buffer),
        }
    }
}

impl AsyncWrite for BrokerStream {
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
    Unix(tokio::net::UnixListener),
    Tcp(tokio::net::TcpListener),
}

/// Owner-only mode the daemon's Unix socket must carry for its whole lifetime.
#[cfg(unix)]
const DAEMON_SOCKET_MODE: u32 = 0o600;

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
fn bind_owner_only_unix_listener(path: &std::path::Path) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    // Binding the real path (rather than staging elsewhere and renaming) keeps
    // `EADDRINUSE` as the kernel-level guarantee that only one daemon owns the
    // endpoint.
    let listener = tokio::net::UnixListener::bind(path)?;
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
    Ok(listener)
}

impl BrokerListener {
    pub async fn bind(endpoint: &DaemonEndpoint) -> Result<(Self, DaemonEndpoint)> {
        match endpoint {
            #[cfg(unix)]
            DaemonEndpoint::Unix(path) => {
                let listener = bind_owner_only_unix_listener(path)?;
                Ok((Self::Unix(listener), endpoint.clone()))
            }
            DaemonEndpoint::Loopback(address) => {
                if !address.ip().is_loopback() {
                    return Err(config_error("refusing non-loopback daemon listener"));
                }
                let listener = tokio::net::TcpListener::bind(address).await?;
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

#[cfg(any(not(unix), test))]
pub fn default_loopback_endpoint() -> DaemonEndpoint {
    DaemonEndpoint::Loopback(SocketAddr::new(
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn loopback_endpoint_round_trips_and_rejects_remote_addresses() {
        let endpoint = DaemonEndpoint::parse("tcp://127.0.0.1:43123").unwrap();
        assert_eq!(endpoint.to_string(), "tcp://127.0.0.1:43123");
        assert!(DaemonEndpoint::parse("tcp://192.0.2.1:43123").is_err());
    }

    #[test]
    fn auth_preface_validates_protocol_and_token() {
        let preface = DaemonAuthPreface::new("0123456789abcdef");
        let decoded = DaemonAuthPreface::from_line(&preface.to_line().unwrap()).unwrap();
        assert!(decoded.authenticate("0123456789abcdef"));
        assert!(!decoded.authenticate("0123456789abcdee"));
        assert!(!decoded.authenticate("short"));
    }

    /// The daemon socket must never be observable with wider-than-owner
    /// permissions, so `bind` narrows it before any caller can publish the
    /// endpoint or accept on it.
    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_is_owner_only_the_moment_bind_returns() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("daemon.sock");
        let (_listener, _endpoint) = BrokerListener::bind(&DaemonEndpoint::Unix(path.clone()))
            .await
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, DAEMON_SOCKET_MODE, "daemon socket must be owner-only");
    }

    #[tokio::test]
    async fn loopback_listener_connects_and_accepts() {
        let (listener, endpoint) = BrokerListener::bind(&default_loopback_endpoint())
            .await
            .unwrap();
        let client = BrokerStream::connect(&endpoint);
        let server = listener.accept();
        let (client, server) = tokio::join!(client, server);
        assert!(client.is_ok());
        assert!(server.is_ok());
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
                let mut stream = BrokerStream::connect(&endpoint).await.unwrap();
                let line = DaemonAuthPreface::new(TOKEN).to_line().unwrap();
                stream.write_all(line.as_bytes()).await.unwrap();
                stream.write_all(b"\n").await.unwrap();
                stream.shutdown().await.unwrap();
            });
        }
        while let Some(client) = clients.join_next().await {
            client.unwrap();
        }
        server.await.unwrap();
    }
}

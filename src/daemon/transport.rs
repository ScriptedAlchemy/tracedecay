use std::fmt;
#[cfg(any(not(unix), test))]
use std::net::IpAddr;
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
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

#[cfg(unix)]
fn ensure_private_socket_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let metadata = std::fs::metadata(parent).map_err(|error| {
        config_error(format!(
            "failed to inspect daemon socket directory '{}': {error}",
            parent.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(config_error(format!(
            "daemon socket parent '{}' is not a directory",
            parent.display()
        )));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(config_error(format!(
            "refusing to publish daemon socket '{}' outside a private directory: '{}' has mode {mode:04o}; restrict it to 0700",
            path.display(),
            parent.display(),
        )));
    }
    Ok(())
}

impl BrokerListener {
    pub async fn bind(endpoint: &DaemonEndpoint) -> Result<(Self, DaemonEndpoint)> {
        match endpoint {
            #[cfg(unix)]
            DaemonEndpoint::Unix(path) => {
                ensure_private_socket_parent(path)?;
                let listener = tokio::net::UnixListener::bind(path)?;
                if let Err(error) =
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                {
                    drop(listener);
                    let cleanup = match std::fs::remove_file(path) {
                        Ok(()) => String::new(),
                        Err(cleanup_error) => {
                            format!("; cleanup also failed: {cleanup_error}")
                        }
                    };
                    return Err(config_error(format!(
                        "failed to restrict permissions on daemon socket '{}': {error}{cleanup}",
                        path.display(),
                    )));
                }
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_is_owner_only_when_bind_returns() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("daemon.sock");
        let endpoint = DaemonEndpoint::Unix(path.clone());

        let (_listener, _) = BrokerListener::bind(&endpoint).await.unwrap();

        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_listener_rejects_public_parent_before_publishing_socket() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = directory.path().join("daemon.sock");
        let endpoint = DaemonEndpoint::Unix(path.clone());

        let Err(error) = BrokerListener::bind(&endpoint).await else {
            panic!("public socket parent must be rejected");
        };

        assert!(error.to_string().contains("private directory"), "{error}");
        assert!(!path.exists());
    }

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

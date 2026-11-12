//! Loopback listener binding and the HTTP serve loop shared by daemon startup,
//! the hosted dashboard, and test transports.
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::state::config_error;
use tracedecay_domain::errors::Result;

/// Shared bind logic for the MCP `tracedecay_dashboard` tool and the test
/// harness (so port 0 allocation and URL formatting are consistent).
pub async fn bind_dashboard(host: &str, port: u16) -> Result<(TcpListener, SocketAddr)> {
    let host = validate_dashboard_host(host)?;
    let listener = TcpListener::bind((host.to_owned(), port))
        .await
        .map_err(|e| config_error(format!("failed to bind {host}:{port}: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| config_error(format!("failed to read local address: {e}")))?;
    Ok((listener, addr))
}

pub fn validate_dashboard_host(host: &str) -> Result<&str> {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "::1") {
        return Ok(host);
    }

    Err(config_error(format!(
        "dashboard host is loopback-only; use 127.0.0.1, localhost, or ::1 (got {host:?})"
    )))
}

/// Serves `router` on `listener` until `shutdown` resolves, then closes the
/// listener and lets every open connection finish its in-flight request.
///
/// The drain has no deadline because callers own one. Accept errors
/// never end the loop. Dropping the future aborts every connection.
pub async fn serve_loopback(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()>,
) {
    let graceful = CancellationToken::new();
    let admission = Arc::new(tokio::sync::Semaphore::new(128));
    let mut connections = Connections::default();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            Some(()) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, address)) => {
                    let Ok(permit) = Arc::clone(&admission).try_acquire_owned() else {
                        drop(stream);
                        tracing::warn!("loopback HTTP connection capacity reached");
                        continue;
                    };
                    let router = router.clone();
                    let graceful = graceful.clone();
                    connections.spawn(async move {
                        let _permit = permit;
                        if let Err(error) = serve_http_connection(stream, router, true, graceful).await {
                            tracing::trace!(%error, %address, "loopback HTTP connection stopped");
                        }
                    });
                }
                Err(error) => wait_after_accept_error(&error).await,
            },
        }
    }
    drop(listener);
    graceful.cancel();
    while connections.join_next().await.is_some() {}
}

/// The listener retains every connection until shutdown drains it. Dropping
/// the set aborts unfinished tasks through Tokio's structured task owner.
#[derive(Default)]
struct Connections(JoinSet<()>);

impl Connections {
    fn spawn(&mut self, connection: impl Future<Output = ()> + Send + 'static) {
        self.0.spawn(connection);
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    async fn join_next(&mut self) -> Option<()> {
        if let Err(error) = self.0.join_next().await?
            && !error.is_cancelled()
        {
            tracing::warn!(%error, "loopback HTTP connection task failed");
        }
        Some(())
    }
}

/// A peer reset affects one connection. Any other error, such as `EMFILE`, is
/// process-wide, so back off before accepting again, as `axum::serve` does.
async fn wait_after_accept_error(error: &io::Error) {
    if matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    ) {
        return;
    }
    tracing::error!(%error, "loopback HTTP accept failed");
    tokio::time::sleep(Duration::from_secs(1)).await;
}

/// Serves one HTTP/1.1 connection with `router` and starts a graceful
/// shutdown once `graceful` is cancelled. Header reads, including idle
/// keep-alive connections, use the native timer without limiting live SSE bodies.
pub async fn serve_http_connection<T>(
    io: T,
    router: Router,
    keep_alive: bool,
    graceful: CancellationToken,
) -> hyper::Result<()>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service =
        router.map_request(|request: hyper::Request<hyper::body::Incoming>| request.map(Body::new));
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.keep_alive(keep_alive);
    builder.timer(TokioTimer::new());
    builder.header_read_timeout(Duration::from_secs(15));
    let connection = builder.serve_connection(TokioIo::new(io), TowerToHyperService::new(service));
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => result,
        () = graceful.cancelled() => {
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    }
}

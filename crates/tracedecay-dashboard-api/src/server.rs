//! Loopback listener binding and the HTTP serve loop shared by daemon startup,
//! the hosted dashboard, and test transports.
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use asupersync::io::{AsyncRead, AsyncWrite, ReadBuf};
use asupersync::net::TcpListener;
use asupersync::time::{Sleep, wall_now};
use axum::Router;
use axum::body::Body;
use hyper_util::service::TowerToHyperService;
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
/// Matches `axum::serve(..).with_graceful_shutdown(..)` on an Asupersync
/// socket. The drain has no deadline because callers own one. Accept errors
/// never end the loop. Dropping the future aborts every connection.
/// Connections run on `runtime`, because daemon routes served here still
/// reach Tokio.
pub async fn serve_loopback(
    runtime: tokio::runtime::Handle,
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()>,
) {
    let graceful = CancellationToken::new();
    let mut connections = JoinSet::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => break,
            Some(joined) = connections.join_next(), if !connections.is_empty() => {
                observe_connection_join(joined);
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, address)) => {
                    let router = router.clone();
                    let graceful = graceful.clone();
                    connections.spawn_on(async move {
                        if let Err(error) = serve_http_connection(stream, router, true, graceful).await {
                            tracing::trace!(%error, %address, "loopback HTTP connection stopped");
                        }
                    }, &runtime);
                }
                Err(error) => wait_after_accept_error(&error).await,
            },
        }
    }
    drop(listener);
    graceful.cancel();
    while let Some(joined) = connections.join_next().await {
        observe_connection_join(joined);
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
    Sleep::after(wall_now(), Duration::from_secs(1)).await;
}

fn observe_connection_join(joined: std::result::Result<(), tokio::task::JoinError>) {
    if let Err(error) = joined
        && !error.is_cancelled()
    {
        tracing::warn!(%error, "loopback HTTP connection task failed");
    }
}

/// Serves one HTTP/1.1 connection with `router` and starts a graceful
/// shutdown once `graceful` is cancelled. hyper runs without an executor or a
/// timer, so the IO is the only runtime seam.
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
    let connection = builder.serve_connection(HyperIo(io), TowerToHyperService::new(service));
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => result,
        () = graceful.cancelled() => {
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    }
}

/// Adapts an Asupersync byte stream to hyper's IO traits.
struct HyperIo<T>(T);

impl<T: AsyncRead + Unpin> hyper::rt::Read for HyperIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        mut cursor: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let filled = {
            let mut buffer = ReadBuf::new(cursor.initialize_unfilled());
            match Pin::new(&mut self.0).poll_read(context, &mut buffer) {
                Poll::Ready(Ok(())) => buffer.filled().len(),
                other => return other,
            }
        };
        // SAFETY: `initialize_unfilled` initialized the whole unfilled region
        // and the read filled its first `filled` bytes.
        unsafe { cursor.advance(filled) };
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWrite + Unpin> hyper::rt::Write for HyperIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(context)
    }
}

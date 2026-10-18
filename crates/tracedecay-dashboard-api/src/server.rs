//! Loopback listener binding and the HTTP serve loop shared by daemon startup,
//! the hosted dashboard, and test transports.
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use asupersync::io::{AsyncRead, AsyncWrite, ReadBuf};
use asupersync::net::TcpListener;
use asupersync::time::{Sleep, wall_now};
use axum::Router;
use axum::body::Body;
use hyper_util::service::TowerToHyperService;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracedecay_runtime_core::native_runtime::{NativeTaskOutcome, NativeTaskSet};

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
pub async fn serve_loopback(
    executor: ConnectionExecutor,
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()>,
) {
    let graceful = CancellationToken::new();
    let admission = Arc::new(tokio::sync::Semaphore::new(128));
    let mut connections = Connections::new(executor);
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

/// Where [`serve_loopback`] runs each connection.
pub enum ConnectionExecutor {
    Native,
    /// For daemon routes that still reach `project_server_lifecycle` inside
    /// their connection task.
    Tokio(tokio::runtime::Handle),
}

enum Connections {
    Native(NativeTaskSet<()>),
    Tokio(tokio::runtime::Handle, JoinSet<()>),
}

impl Connections {
    fn new(executor: ConnectionExecutor) -> Self {
        match executor {
            ConnectionExecutor::Native => Self::Native(NativeTaskSet::default()),
            ConnectionExecutor::Tokio(runtime) => Self::Tokio(runtime, JoinSet::new()),
        }
    }

    /// A refused connection's IO drops with it, closing the socket.
    fn spawn(&mut self, connection: impl Future<Output = ()> + Send + 'static) {
        match self {
            Self::Native(set) => {
                if let Err(error) = set.spawn(connection) {
                    tracing::warn!(%error, "loopback HTTP connection not admitted");
                }
            }
            Self::Tokio(runtime, set) => {
                set.spawn_on(connection, runtime);
            }
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Native(set) => set.is_empty(),
            Self::Tokio(_, set) => set.is_empty(),
        }
    }

    /// Joins the next connection to end, logging a failed one; `None` once
    /// none remain.
    async fn join_next(&mut self) -> Option<()> {
        match self {
            Self::Native(set) => {
                if let NativeTaskOutcome::Panicked(payload) = set.join_next().await? {
                    tracing::warn!(?payload, "loopback HTTP connection task failed");
                }
            }
            Self::Tokio(_, set) => {
                if let Err(error) = set.join_next().await?
                    && !error.is_cancelled()
                {
                    tracing::warn!(%error, "loopback HTTP connection task failed");
                }
            }
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
    Sleep::after(wall_now(), Duration::from_secs(1)).await;
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
    builder.timer(HttpTimer);
    builder.header_read_timeout(Duration::from_secs(15));
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

#[derive(Clone)]
struct HttpTimer;

struct HttpSleep(Mutex<Pin<Box<Sleep>>>);

impl Future for HttpSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut sleep = match self.0.lock() {
            Ok(sleep) => sleep,
            Err(poisoned) => poisoned.into_inner(),
        };
        sleep.as_mut().poll(cx)
    }
}

impl hyper::rt::Sleep for HttpSleep {}

impl hyper::rt::Timer for HttpTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn hyper::rt::Sleep>> {
        Box::pin(HttpSleep(Mutex::new(Box::pin(Sleep::after(
            wall_now(),
            duration,
        )))))
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn hyper::rt::Sleep>> {
        self.sleep(deadline.saturating_duration_since(Instant::now()))
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

//! Bounded Remote Brain HTTPS transport for an injected canonical application router.
//! Owns TLS identity admission, HTTP framing, connection budgets, and draining;
//! credential and project authorities remain with the daemon composition.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use asupersync::io::{AsyncRead, AsyncWrite, ReadBuf};
use asupersync::net::{TcpListener, TcpStream};
use asupersync::time::{Sleep, wall_now};
use asupersync::tls::{TlsAcceptor, TlsStream};
use asupersync::types::Time;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, header::CONNECTION};
use axum::middleware::Next;
use axum::response::Response;
use axum::{Router, middleware};
use rustls::client::{verify_server_cert_signed_by_trust_anchor, verify_server_name};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use tracedecay_daemon_control::RemoteBrainTlsConfig;
use tracedecay_dashboard_api::serve_http_connection;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::native_runtime;

const MAX_REMOTE_BRAIN_TLS_CONNECTIONS: usize = 128;
const MAX_REMOTE_BRAIN_TLS_HEADER_BYTES: usize = 64 * 1024;

/// Per-connection deadlines of the Remote Brain TLS listener. Production uses
/// [`RemoteBrainTlsDeadlines::PRODUCTION`]; tests inject shorter ones through
/// `RemoteBrainTlsListener::with_deadlines` and age them in real time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteBrainTlsDeadlines {
    /// Longest silence while the handshake or request is still being read.
    pub read_idle: Duration,
    /// Absolute bound on reading one request. It starts when the listener
    /// first reads from the peer and restarts when the handshake completes.
    pub request_read: Duration,
    /// Longest stall of a started response write.
    pub write_idle: Duration,
    /// Absolute bound on writing one response, from its first byte.
    pub response: Duration,
    /// Bound on closing the connection, close notification included.
    pub close: Duration,
    /// Bound on draining connections after shutdown is requested.
    pub shutdown_drain: Duration,
}

impl RemoteBrainTlsDeadlines {
    pub const PRODUCTION: Self = Self {
        read_idle: Duration::from_secs(5),
        request_read: Duration::from_mins(1),
        write_idle: Duration::from_secs(5),
        response: Duration::from_secs(30),
        close: Duration::from_secs(5),
        shutdown_drain: Duration::from_secs(5),
    };
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteBrainTlsEgressSnapshot {
    pub active: usize,
    pub backpressured: usize,
    pub idle_expirations: usize,
    pub idle_deadline_contract_violations: usize,
    pub response_expirations: usize,
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteBrainTlsIngressSnapshot {
    pub headers_complete: usize,
    pub body_bytes_observed: usize,
    pub read_idle_expirations: usize,
    pub request_read_expirations: usize,
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Default)]
pub struct RemoteBrainTlsEgressObserver {
    active: AtomicUsize,
    backpressured: AtomicUsize,
    idle_expirations: AtomicUsize,
    idle_deadline_contract_violations: AtomicUsize,
    response_expirations: AtomicUsize,
    headers_complete: AtomicUsize,
    body_bytes_observed: AtomicUsize,
    read_idle_expirations: AtomicUsize,
    request_read_expirations: AtomicUsize,
}

#[cfg(any(test, feature = "test-helpers"))]
impl RemoteBrainTlsEgressObserver {
    pub fn snapshot(&self) -> RemoteBrainTlsEgressSnapshot {
        RemoteBrainTlsEgressSnapshot {
            active: self.active.load(Ordering::SeqCst),
            backpressured: self.backpressured.load(Ordering::SeqCst),
            idle_expirations: self.idle_expirations.load(Ordering::SeqCst),
            idle_deadline_contract_violations: self
                .idle_deadline_contract_violations
                .load(Ordering::SeqCst),
            response_expirations: self.response_expirations.load(Ordering::SeqCst),
        }
    }

    pub fn ingress_snapshot(&self) -> RemoteBrainTlsIngressSnapshot {
        RemoteBrainTlsIngressSnapshot {
            headers_complete: self.headers_complete.load(Ordering::SeqCst),
            body_bytes_observed: self.body_bytes_observed.load(Ordering::SeqCst),
            read_idle_expirations: self.read_idle_expirations.load(Ordering::SeqCst),
            request_read_expirations: self.request_read_expirations.load(Ordering::SeqCst),
        }
    }
}

async fn force_remote_connection_close(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

pub struct RemoteBrainTlsListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    admission: Arc<Semaphore>,
    deadlines: RemoteBrainTlsDeadlines,
    #[cfg(any(test, feature = "test-helpers"))]
    egress: Arc<RemoteBrainTlsEgressObserver>,
}

impl RemoteBrainTlsListener {
    #[cfg(any(test, feature = "test-helpers"))]
    #[must_use]
    pub fn with_deadlines(mut self, deadlines: RemoteBrainTlsDeadlines) -> Self {
        self.deadlines = deadlines;
        self
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn admission(&self) -> Arc<Semaphore> {
        Arc::clone(&self.admission)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn egress_observer(&self) -> Arc<RemoteBrainTlsEgressObserver> {
        Arc::clone(&self.egress)
    }

    pub async fn serve(
        self,
        router: Router,
        mut shutdown_requested: oneshot::Receiver<()>,
    ) -> Result<()> {
        let router = router.layer(middleware::from_fn(force_remote_connection_close));
        let graceful = CancellationToken::new();
        let mut connections = RemoteBrainTlsConnections::default();
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_requested => break,
                accepted = self.accept() => {
                    if let Some((io, address)) = accepted {
                        connections.reap_finished();
                        let router = router.clone();
                        let graceful = graceful.clone();
                        let connection = native_runtime::NativeTask::spawn(tracing::Instrument::instrument(async move {
                                if let Err(error) = serve_http_connection(io, router, false, graceful).await {
                                    tracing::debug!(%error, %address, "Remote Brain TLS connection stopped");
                                }
                            }, tracing::trace_span!("daemon.http.application.remote_tls_connection")));
                        match connection {
                            Ok(connection) => connections.0.push(connection),
                            // The refused connection's IO drops here, closing it.
                            Err(error) => tracing::warn!(%error, %address, "Remote Brain TLS connection could not start"),
                        }
                    }
                }
            }
        }

        graceful.cancel();
        if native_runtime::timeout(self.deadlines.shutdown_drain, connections.join_all())
            .await
            .is_err()
        {
            connections.abort_all();
            connections.join_all().await;
        }
        Ok(())
    }

    #[tracing::instrument(name = "daemon.http.application.tls_bind", level = "trace", skip_all)]
    pub async fn bind(config: &RemoteBrainTlsConfig) -> Result<Self> {
        let certificates = CertificateDer::pem_file_iter(config.certificate_chain())
            .map_err(|error| tls_configuration_error("open Remote Brain TLS certificate", error))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                tls_configuration_error("decode Remote Brain TLS certificate", error)
            })?;
        if certificates.len() < 2 {
            return Err(TraceDecayError::Config {
                message: "Remote Brain TLS certificate chain requires a leaf followed by an explicit trust anchor".to_owned(),
            });
        }
        let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());
        validate_remote_brain_tls_identity(
            &certificates,
            config.listen(),
            UnixTime::now(),
            &crypto_provider,
        )?;
        let private_key_file = tracedecay_private_fs::open_private_file(config.private_key())
            .map_err(|error| {
                tls_configuration_error("open and validate Remote Brain TLS private key", error)
            })?;
        let private_key = PrivateKeyDer::from_pem_reader(private_key_file).map_err(|error| {
            tls_configuration_error("decode Remote Brain TLS private key", error)
        })?;
        let mut server = rustls::ServerConfig::builder_with_provider(crypto_provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| {
                tls_configuration_error("select Remote Brain TLS protocol versions", error)
            })?
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .map_err(|error| tls_configuration_error("bind Remote Brain TLS identity", error))?;
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        let listener = TcpListener::bind(config.listen())
            .await
            .map_err(|error| tls_configuration_error("bind Remote Brain TLS listener", error))?;
        Ok(Self {
            listener,
            acceptor: TlsAcceptor::new(server),
            admission: Arc::new(Semaphore::new(MAX_REMOTE_BRAIN_TLS_CONNECTIONS)),
            deadlines: RemoteBrainTlsDeadlines::PRODUCTION,
            #[cfg(any(test, feature = "test-helpers"))]
            egress: Arc::new(RemoteBrainTlsEgressObserver::default()),
        })
    }

    pub fn bound_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    #[tracing::instrument(name = "daemon.http.application.tls_accept", level = "trace", skip_all)]
    async fn accept(&self) -> Option<(RemoteBrainTlsIo, SocketAddr)> {
        let (stream, address) = match self.listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "Remote Brain TLS listener accept failed");
                Sleep::after(wall_now(), Duration::from_millis(50)).await;
                return None;
            }
        };
        let permit = match Arc::clone(&self.admission).try_acquire_owned() {
            Ok(permit) => permit,
            Err(error) => {
                tracing::warn!(%error, %address, "Remote Brain TLS connection admission saturated");
                return None;
            }
        };
        let acceptor = self.acceptor.clone();
        let handshake_read = Arc::new(AtomicBool::new(false));
        let stream = RemoteBrainTcpStream {
            stream,
            read: Arc::clone(&handshake_read),
        };
        let handshake =
            Box::pin(async move { acceptor.accept(stream).await.map_err(io::Error::other) });
        Some((
            RemoteBrainTlsIo::new(
                handshake,
                handshake_read,
                permit,
                self.deadlines,
                #[cfg(any(test, feature = "test-helpers"))]
                Arc::clone(&self.egress),
            ),
            address,
        ))
    }
}

#[tracing::instrument(
    name = "daemon.http.application.tls_validate_identity",
    level = "trace",
    skip_all
)]
fn validate_remote_brain_tls_identity(
    certificates: &[CertificateDer<'_>],
    listen: SocketAddr,
    now: UnixTime,
    crypto_provider: &rustls::crypto::CryptoProvider,
) -> Result<()> {
    let leaf = certificates
        .first()
        .ok_or_else(|| TraceDecayError::Config {
            message: "Remote Brain TLS certificate chain is empty".to_owned(),
        })?;
    if certificates.len() < 2 {
        return Err(TraceDecayError::Config {
            message: "Remote Brain TLS certificate chain requires a leaf followed by an explicit trust anchor".to_owned(),
        });
    }
    let mut roots = rustls::RootCertStore::empty();
    let trust_anchor = &certificates[certificates.len() - 1];
    if leaf.as_ref() == trust_anchor.as_ref() {
        return Err(TraceDecayError::Config {
            message:
                "Remote Brain TLS leaf and explicit trust anchor must be distinct certificates"
                    .to_owned(),
        });
    }
    roots.add(trust_anchor.clone()).map_err(|error| {
        tls_configuration_error("load Remote Brain TLS chain trust anchor", error)
    })?;
    let intermediates = &certificates[1..certificates.len() - 1];
    let parsed_leaf = ParsedCertificate::try_from(leaf).map_err(|error| {
        tls_configuration_error("parse Remote Brain TLS leaf certificate", error)
    })?;
    let server_name = ServerName::IpAddress(listen.ip().into());
    verify_server_cert_signed_by_trust_anchor(
        &parsed_leaf,
        &roots,
        intermediates,
        now,
        crypto_provider.signature_verification_algorithms.all,
    )
    .map_err(|error| {
        tls_configuration_error("validate Remote Brain TLS certificate chain", error)
    })?;
    verify_server_name(&parsed_leaf, &server_name).map_err(|error| {
        tls_configuration_error("validate Remote Brain TLS listen address identity", error)
    })?;
    Ok(())
}

#[cfg(any(test, feature = "test-helpers"))]
pub fn validate_remote_brain_tls_identity_at(
    certificates: &[CertificateDer<'_>],
    listen: SocketAddr,
    now: UnixTime,
) -> Result<()> {
    validate_remote_brain_tls_identity(
        certificates,
        listen,
        now,
        &rustls::crypto::ring::default_provider(),
    )
}

/// The listener's open connections. Dropping the set aborts them.
#[derive(Default)]
struct RemoteBrainTlsConnections(Vec<native_runtime::NativeTask<()>>);

impl RemoteBrainTlsConnections {
    fn reap_finished(&mut self) {
        self.0.retain(|connection| match connection.try_finished() {
            Some(outcome) => {
                observe_remote_brain_connection_end(outcome);
                false
            }
            None => true,
        });
    }

    /// Keeps every connection in the set until all have ended, so a cancelled
    /// join leaves them for [`Self::abort_all`].
    async fn join_all(&mut self) {
        for connection in &self.0 {
            observe_remote_brain_connection_end(connection.wait().await);
        }
        self.0.clear();
    }

    fn abort_all(&self) {
        for connection in &self.0 {
            connection.abort();
        }
    }
}

impl Drop for RemoteBrainTlsConnections {
    fn drop(&mut self) {
        self.abort_all();
    }
}

fn observe_remote_brain_connection_end(outcome: native_runtime::NativeTaskOutcome<()>) {
    if let native_runtime::NativeTaskOutcome::Panicked(panic) = outcome {
        tracing::warn!(%panic, "Remote Brain TLS connection task failed");
    }
}

/// A resettable connection deadline that reports expiry on every poll.
///
/// The connection task runs on the native runtime and polls an Asupersync
/// `Sleep` directly. It is a documented exception to
/// `native_runtime::sleep_until`: the read and write deadlines reset on every
/// transferred chunk, and a native task per reset would cost tens of
/// microseconds each. `Sleep` panics when polled after it completes,
/// so expiry is latched here until the next reset.
struct IoDeadline {
    sleep: Sleep,
    expired: bool,
}

impl IoDeadline {
    fn after(duration: Duration) -> Self {
        Self {
            sleep: Sleep::after(wall_now(), duration),
            expired: false,
        }
    }

    fn reset_after(&mut self, now: Time, duration: Duration) {
        self.sleep.reset_after(now, duration);
        self.expired = false;
    }

    fn deadline(&self) -> Time {
        self.sleep.deadline()
    }

    fn poll_expired(&mut self, context: &mut Context<'_>) -> bool {
        if !self.expired {
            self.expired = Pin::new(&mut self.sleep).poll(context).is_ready();
        }
        self.expired
    }
}

/// The accepted TCP stream. It flags every read that returns bytes, so a
/// handshake flight the listener reads late restarts the read-idle deadline,
/// which measures the peer's silence rather than the listener's own latency.
struct RemoteBrainTcpStream {
    stream: TcpStream,
    read: Arc<AtomicBool>,
}

impl AsyncRead for RemoteBrainTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.stream).poll_read(context, buffer);
        if buffer.filled().len() > before {
            self.read.store(true, Ordering::Relaxed);
        }
        result
    }
}

impl AsyncWrite for RemoteBrainTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(context, buffer)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write_vectored(context, buffers)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

type RemoteBrainTlsHandshake =
    Pin<Box<dyn Future<Output = io::Result<TlsStream<RemoteBrainTcpStream>>> + Send>>;

enum RemoteBrainTlsTransport {
    Handshaking(RemoteBrainTlsHandshake),
    Streaming(Box<TlsStream<RemoteBrainTcpStream>>),
    Failed,
}

struct RemoteBrainTlsIo {
    transport: RemoteBrainTlsTransport,
    handshake_read: Arc<AtomicBool>,
    peer_heard: bool,
    deadlines: RemoteBrainTlsDeadlines,
    read_idle_deadline: IoDeadline,
    request_read_deadline: IoDeadline,
    write_idle_deadline: IoDeadline,
    response_deadline: IoDeadline,
    close_deadline: Option<IoDeadline>,
    egress_started: bool,
    #[cfg(any(test, feature = "test-helpers"))]
    egress_write_pending: bool,
    #[cfg(any(test, feature = "test-helpers"))]
    egress_flush_pending: bool,
    #[cfg(any(test, feature = "test-helpers"))]
    egress_idle_expired: bool,
    #[cfg(any(test, feature = "test-helpers"))]
    egress_response_expired: bool,
    #[cfg(any(test, feature = "test-helpers"))]
    egress_last_positive_write: Option<Time>,
    #[cfg(any(test, feature = "test-helpers"))]
    egress_write_idle_deadline_at: Option<Time>,
    #[cfg(any(test, feature = "test-helpers"))]
    egress: Arc<RemoteBrainTlsEgressObserver>,
    header_bytes: Vec<u8>,
    header_terminator_prefix: usize,
    http2_preface_prefix: Option<usize>,
    headers_complete: bool,
    request_body_remaining: u64,
    request_read_complete: bool,
    _permit: OwnedSemaphorePermit,
}

impl RemoteBrainTlsIo {
    fn new(
        handshake: RemoteBrainTlsHandshake,
        handshake_read: Arc<AtomicBool>,
        permit: OwnedSemaphorePermit,
        deadlines: RemoteBrainTlsDeadlines,
        #[cfg(any(test, feature = "test-helpers"))] egress: Arc<RemoteBrainTlsEgressObserver>,
    ) -> Self {
        Self {
            transport: RemoteBrainTlsTransport::Handshaking(handshake),
            handshake_read,
            peer_heard: false,
            deadlines,
            read_idle_deadline: IoDeadline::after(deadlines.read_idle),
            request_read_deadline: IoDeadline::after(deadlines.request_read),
            write_idle_deadline: IoDeadline::after(deadlines.write_idle),
            response_deadline: IoDeadline::after(deadlines.response),
            close_deadline: None,
            egress_started: false,
            #[cfg(any(test, feature = "test-helpers"))]
            egress_write_pending: false,
            #[cfg(any(test, feature = "test-helpers"))]
            egress_flush_pending: false,
            #[cfg(any(test, feature = "test-helpers"))]
            egress_idle_expired: false,
            #[cfg(any(test, feature = "test-helpers"))]
            egress_response_expired: false,
            #[cfg(any(test, feature = "test-helpers"))]
            egress_last_positive_write: None,
            #[cfg(any(test, feature = "test-helpers"))]
            egress_write_idle_deadline_at: None,
            #[cfg(any(test, feature = "test-helpers"))]
            egress,
            header_bytes: Vec::new(),
            header_terminator_prefix: 0,
            http2_preface_prefix: Some(0),
            headers_complete: false,
            request_body_remaining: 0,
            request_read_complete: false,
            _permit: permit,
        }
    }

    /// Checks both request read deadlines. Called only when a read would wait
    /// on the peer, so bytes the peer sent in time are read first however late
    /// the listener polls. When both deadlines have passed, the earlier one
    /// names the timeout.
    fn poll_request_read_deadlines(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.request_read_complete {
            return Ok(());
        }
        let idle_expired = self.read_idle_deadline.poll_expired(context);
        let request_expired = self.request_read_deadline.poll_expired(context);
        let idle_first = idle_expired
            && !(request_expired
                && self.request_read_deadline.deadline() < self.read_idle_deadline.deadline());
        if !idle_first && !request_expired {
            return Ok(());
        }
        // Counted before the transport drops the socket, so a peer that sees
        // the close also sees the count.
        #[cfg(any(test, feature = "test-helpers"))]
        if idle_first {
            self.egress
                .read_idle_expirations
                .fetch_add(1, Ordering::SeqCst);
        } else {
            self.egress
                .request_read_expirations
                .fetch_add(1, Ordering::SeqCst);
        }
        self.transport = RemoteBrainTlsTransport::Failed;
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            if idle_first {
                "Remote Brain TLS request read timed out"
            } else {
                "Remote Brain TLS request admission timed out"
            },
        ))
    }

    fn poll_write_deadline(
        &mut self,
        context: &mut Context<'_>,
        start_egress: bool,
    ) -> io::Result<()> {
        if !self.egress_started && start_egress {
            let now = wall_now();
            self.reset_write_idle_deadline(now);
            self.response_deadline
                .reset_after(now, self.deadlines.response);
            self.egress_started = true;
            #[cfg(any(test, feature = "test-helpers"))]
            self.egress.active.fetch_add(1, Ordering::SeqCst);
        }
        if !self.egress_started {
            return Ok(());
        }
        let write_idle_expired = self.write_idle_deadline.poll_expired(context);
        let response_expired = self.response_deadline.poll_expired(context);
        if !write_idle_expired && !response_expired {
            return Ok(());
        }
        // A listener polled after both deadlines passed counts the earlier.
        #[cfg(any(test, feature = "test-helpers"))]
        let write_idle_expired = write_idle_expired
            && !(response_expired
                && self.response_deadline.deadline() < self.write_idle_deadline.deadline());
        #[cfg(any(test, feature = "test-helpers"))]
        if write_idle_expired && !self.egress_idle_expired {
            self.egress_idle_expired = true;
            self.egress.idle_expirations.fetch_add(1, Ordering::SeqCst);
            let violates_idle_deadline = self
                .egress_last_positive_write
                .zip(self.egress_write_idle_deadline_at)
                .is_none_or(|(last_write, deadline_at)| {
                    Duration::from_nanos(deadline_at.duration_since(last_write))
                        != self.deadlines.write_idle
                });
            if violates_idle_deadline {
                self.egress
                    .idle_deadline_contract_violations
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
        #[cfg(any(test, feature = "test-helpers"))]
        if !write_idle_expired && !self.egress_response_expired {
            self.egress_response_expired = true;
            self.egress
                .response_expirations
                .fetch_add(1, Ordering::SeqCst);
        }
        #[cfg(any(test, feature = "test-helpers"))]
        self.clear_egress_backpressure();
        self.transport = RemoteBrainTlsTransport::Failed;
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Remote Brain TLS response write timed out",
        ))
    }

    fn reset_write_idle_deadline(&mut self, now: Time) {
        self.write_idle_deadline
            .reset_after(now, self.deadlines.write_idle);
        #[cfg(any(test, feature = "test-helpers"))]
        {
            self.egress_last_positive_write = Some(now);
            self.egress_write_idle_deadline_at = Some(self.write_idle_deadline.sleep.deadline());
        }
    }

    fn poll_close_deadline(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let close = self.deadlines.close;
        let deadline = self
            .close_deadline
            .get_or_insert_with(|| IoDeadline::after(close));
        if !deadline.poll_expired(context) {
            Poll::Pending
        } else {
            self.transport = RemoteBrainTlsTransport::Failed;
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Remote Brain TLS close notification timed out",
            )))
        }
    }

    fn poll_handshake(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let RemoteBrainTlsTransport::Handshaking(handshake) = &mut self.transport else {
            return Poll::Ready(Ok(()));
        };
        match handshake.as_mut().poll(context) {
            Poll::Pending => {
                if self.handshake_read.swap(false, Ordering::Relaxed) {
                    let now = wall_now();
                    self.read_idle_deadline
                        .reset_after(now, self.deadlines.read_idle);
                    if !self.peer_heard {
                        self.peer_heard = true;
                        self.request_read_deadline
                            .reset_after(now, self.deadlines.request_read);
                    }
                }
                Poll::Pending
            }
            Poll::Ready(Ok(stream)) => {
                // A write-side poll can finish the handshake after the reader
                // parked on it. Its final read may already hold the request,
                // so no readiness edge follows, and the reset deadlines below
                // arm only when polled: wake the reader to re-poll the stream.
                context.waker().wake_by_ref();
                self.transport = RemoteBrainTlsTransport::Streaming(Box::new(stream));
                let now = wall_now();
                self.read_idle_deadline
                    .reset_after(now, self.deadlines.read_idle);
                self.request_read_deadline
                    .reset_after(now, self.deadlines.request_read);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.transport = RemoteBrainTlsTransport::Failed;
                Poll::Ready(Err(error))
            }
        }
    }

    #[tracing::instrument(
        name = "daemon.http.application.observe_http_request",
        level = "trace",
        skip_all
    )]
    fn observe_http_request(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.request_read_complete {
            return Ok(());
        }
        const TERMINATOR: &[u8] = b"\r\n\r\n";
        const HTTP2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
        let mut body_offset = 0;
        if !self.headers_complete {
            for (offset, byte) in bytes.iter().enumerate() {
                self.header_bytes.push(*byte);
                if self.header_bytes.len() > MAX_REMOTE_BRAIN_TLS_HEADER_BYTES {
                    return self.fail_request(
                        io::ErrorKind::InvalidData,
                        "Remote Brain TLS request headers exceed the admission bound",
                    );
                }
                if let Some(prefix) = self.http2_preface_prefix {
                    if *byte == HTTP2_PREFACE[prefix] {
                        let prefix = prefix + 1;
                        if prefix == HTTP2_PREFACE.len() {
                            return self.fail_request(
                                io::ErrorKind::InvalidData,
                                "Remote Brain TLS listener accepts HTTP/1.1 only",
                            );
                        }
                        self.http2_preface_prefix = Some(prefix);
                    } else {
                        self.http2_preface_prefix = None;
                    }
                }
                if *byte == TERMINATOR[self.header_terminator_prefix] {
                    self.header_terminator_prefix += 1;
                    if self.header_terminator_prefix == TERMINATOR.len() {
                        if self.http2_preface_prefix.is_some() {
                            self.header_terminator_prefix = 0;
                            continue;
                        }
                        self.headers_complete = true;
                        #[cfg(any(test, feature = "test-helpers"))]
                        self.egress.headers_complete.fetch_add(1, Ordering::SeqCst);
                        body_offset = offset + 1;
                        break;
                    }
                } else {
                    self.header_terminator_prefix = usize::from(*byte == TERMINATOR[0]);
                }
            }
            if !self.headers_complete {
                return Ok(());
            }
            self.request_body_remaining = match declared_http11_body_length(&self.header_bytes) {
                Ok(length) => length,
                Err(error) => {
                    self.transport = RemoteBrainTlsTransport::Failed;
                    return Err(error);
                }
            };
            self.header_bytes = Vec::new();
        }

        let available_body =
            u64::try_from(bytes.len().saturating_sub(body_offset)).unwrap_or(u64::MAX);
        if available_body > self.request_body_remaining {
            return self.fail_request(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS connection carried bytes after its declared request body",
            );
        }
        self.request_body_remaining -= available_body;
        #[cfg(any(test, feature = "test-helpers"))]
        self.egress.body_bytes_observed.fetch_add(
            usize::try_from(available_body).unwrap_or(usize::MAX),
            Ordering::SeqCst,
        );
        self.request_read_complete = self.request_body_remaining == 0;
        Ok(())
    }

    fn fail_request(&mut self, kind: io::ErrorKind, message: &'static str) -> io::Result<()> {
        self.transport = RemoteBrainTlsTransport::Failed;
        Err(io::Error::new(kind, message))
    }

    fn failed_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "Remote Brain TLS handshake failed",
        )
    }
}

impl AsyncRead for RemoteBrainTlsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // The handshake goes first: one that completes on this poll restarts
        // the request deadline, so it must not fail on the expired one.
        match self.poll_handshake(context) {
            Poll::Pending => {
                return match self.poll_request_read_deadlines(context) {
                    Ok(()) => Poll::Pending,
                    Err(error) => Poll::Ready(Err(error)),
                };
            }
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let before = buffer.filled().len();
        let result = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => {
                Pin::new(stream).poll_read(context, buffer)
            }
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        if matches!(result, Poll::Ready(Ok(()))) {
            if buffer.filled().len() > before {
                let read_idle = self.deadlines.read_idle;
                self.read_idle_deadline.reset_after(wall_now(), read_idle);
            }
            if let Err(error) = self.observe_http_request(&buffer.filled()[before..]) {
                return Poll::Ready(Err(error));
            }
        }
        if result.is_pending()
            && let Err(error) = self.poll_request_read_deadlines(context)
        {
            return Poll::Ready(Err(error));
        }
        result
    }
}

impl AsyncWrite for RemoteBrainTlsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if let Err(error) = self.poll_write_deadline(context, !buffer.is_empty()) {
            return Poll::Ready(Err(error));
        }
        match self.poll_handshake(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let result = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => {
                Pin::new(stream).poll_write(context, buffer)
            }
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        if matches!(result, Poll::Ready(Ok(written)) if written > 0) {
            self.reset_write_idle_deadline(wall_now());
        }
        #[cfg(any(test, feature = "test-helpers"))]
        self.observe_egress_write_pending(result.is_pending());
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Err(error) = self.poll_write_deadline(context, false) {
            return Poll::Ready(Err(error));
        }
        match self.poll_handshake(context) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let result = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => Pin::new(stream).poll_flush(context),
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        #[cfg(any(test, feature = "test-helpers"))]
        {
            let flush_pending = self.egress_started && result.is_pending();
            self.observe_egress_flush_pending(flush_pending);
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.poll_handshake(context) {
            Poll::Pending => return self.poll_close_deadline(context),
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => {}
        }
        let shutdown = match &mut self.transport {
            RemoteBrainTlsTransport::Streaming(stream) => Pin::new(stream).poll_shutdown(context),
            RemoteBrainTlsTransport::Failed => Poll::Ready(Err(Self::failed_error())),
            RemoteBrainTlsTransport::Handshaking(_) => Poll::Pending,
        };
        if !shutdown.is_pending() {
            return shutdown;
        }
        self.poll_close_deadline(context)
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl RemoteBrainTlsIo {
    fn observe_egress_write_pending(&mut self, pending: bool) {
        let was_backpressured = self.egress_write_pending || self.egress_flush_pending;
        self.egress_write_pending = pending;
        self.update_egress_backpressure(was_backpressured);
    }

    fn observe_egress_flush_pending(&mut self, pending: bool) {
        let was_backpressured = self.egress_write_pending || self.egress_flush_pending;
        self.egress_flush_pending = pending;
        self.update_egress_backpressure(was_backpressured);
    }

    fn update_egress_backpressure(&self, was_backpressured: bool) {
        let backpressured = self.egress_write_pending || self.egress_flush_pending;
        if backpressured == was_backpressured {
            return;
        }
        if backpressured {
            self.egress.backpressured.fetch_add(1, Ordering::SeqCst);
        } else {
            self.egress.backpressured.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn clear_egress_backpressure(&mut self) {
        let was_backpressured = self.egress_write_pending || self.egress_flush_pending;
        self.egress_write_pending = false;
        self.egress_flush_pending = false;
        self.update_egress_backpressure(was_backpressured);
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl Drop for RemoteBrainTlsIo {
    fn drop(&mut self) {
        self.clear_egress_backpressure();
        if self.egress_started {
            self.egress.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[tracing::instrument(
    name = "daemon.http.application.parse_body_length",
    level = "trace",
    skip_all
)]
fn declared_http11_body_length(header_bytes: &[u8]) -> io::Result<u64> {
    let headers = std::str::from_utf8(header_bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Remote Brain TLS request headers are invalid",
        )
    })?;
    if !headers.ends_with("\r\n\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Remote Brain TLS request headers are incomplete",
        ));
    }
    let mut lines = headers.split("\r\n");
    if lines.next().is_none_or(str::is_empty) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Remote Brain TLS request line is missing",
        ));
    }
    let mut content_length = None;
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS request header is invalid",
            )
        })?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS requests require an explicit content length",
            ));
        }
        if !name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        if content_length.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS request repeats content length",
            ));
        }
        let value = value.trim().parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Remote Brain TLS request content length is invalid",
            )
        })?;
        content_length = Some(value);
    }
    Ok(content_length.unwrap_or(0))
}

fn tls_configuration_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("failed to {operation}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::{Condvar, Mutex};
    use std::task::{Wake, Waker};

    use super::*;

    const CERTIFICATE_CHAIN: &[u8] =
        include_bytes!("../../../tests/fixtures/remote_tls/localhost.crt.pem");
    const PRIVATE_KEY: &[u8] =
        include_bytes!("../../../tests/fixtures/remote_tls/localhost.key.pem");
    const TRUST_ROOT: &[u8] =
        include_bytes!("../../../tests/fixtures/remote_tls/localhost-root.crt.pem");
    const REQUEST: &[u8] = b"GET /remote/authority HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";

    #[derive(Default)]
    struct CountingWaker {
        wakes: Mutex<usize>,
        woken: Condvar,
    }

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            *self.wakes.lock().unwrap() += 1;
            self.woken.notify_all();
        }
    }

    impl CountingWaker {
        fn count(&self) -> usize {
            *self.wakes.lock().unwrap()
        }

        fn wait_past(&self, seen: usize) -> usize {
            let wakes = self.wakes.lock().unwrap();
            *self
                .woken
                .wait_while(wakes, |wakes| *wakes <= seen)
                .unwrap()
        }
    }

    fn tls_listener(directory: &Path) -> RemoteBrainTlsListener {
        let certificate_chain = directory.join("chain.pem");
        let private_key = directory.join("key.pem");
        std::fs::write(&certificate_chain, CERTIFICATE_CHAIN).unwrap();
        std::fs::write(&private_key, PRIVATE_KEY).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&private_key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = RemoteBrainTlsConfig::from_optional_parts(
            Some(SocketAddr::from(([127, 0, 0, 1], 0))),
            Some(certificate_chain),
            Some(private_key),
        )
        .unwrap()
        .unwrap();
        native_runtime::block_on(RemoteBrainTlsListener::bind(&config))
            .unwrap()
            .unwrap()
    }

    /// Completes the handshake, sends `REQUEST` in the same flight as the
    /// client Finished, then holds the connection open until released, so the
    /// server sees no readiness edge after that flight. Reports on `sent` once
    /// the `ClientHello` and once the final flight are on the wire. Answers
    /// the server's flight only once `answer` fires, so the listener's first
    /// read cannot finish the handshake however its thread is scheduled.
    fn hold_open_client(
        address: SocketAddr,
        sent: mpsc::Sender<()>,
        answer: mpsc::Receiver<()>,
        release: mpsc::Receiver<()>,
    ) {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(TRUST_ROOT).unwrap())
            .unwrap();
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let mut tls = rustls::ClientConnection::new(
            Arc::new(config),
            ServerName::IpAddress(address.ip().into()),
        )
        .unwrap();
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        tls.writer().write_all(REQUEST).unwrap();
        while tls.wants_write() {
            tls.write_tls(&mut socket).unwrap();
        }
        let _ = sent.send(());
        let _ = answer.recv();
        while tls.is_handshaking() || tls.wants_write() {
            tls.complete_io(&mut socket).unwrap();
        }
        let _ = sent.send(());
        let _ = release.recv();
    }

    #[test]
    fn a_handshake_finished_by_a_flush_wakes_the_parked_reader() {
        let directory = tempfile::tempdir().unwrap();
        let listener = tls_listener(directory.path());
        let address = listener.bound_addr().unwrap();
        let (sent, _) = mpsc::channel();
        let (answer, await_answer) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let client =
            std::thread::spawn(move || hold_open_client(address, sent, await_answer, held));
        let (mut io, _) = native_runtime::block_on(listener.accept())
            .unwrap()
            .expect("accepted connection");

        let waker = Arc::new(CountingWaker::default());
        let task_waker = Waker::from(Arc::clone(&waker));
        let mut context = Context::from_waker(&task_waker);
        let mut storage = [0_u8; 4096];
        let mut buffer = ReadBuf::new(&mut storage);
        assert!(
            Pin::new(&mut io)
                .poll_read(&mut context, &mut buffer)
                .is_pending(),
            "the reader parks on the handshake"
        );
        answer.send(()).unwrap();
        let mut seen = 0;
        while matches!(io.transport, RemoteBrainTlsTransport::Handshaking(_)) {
            seen = waker.wait_past(seen);
            if let Poll::Ready(result) = Pin::new(&mut io).poll_flush(&mut context) {
                result.unwrap();
            }
        }
        assert!(
            waker.count() > seen,
            "the flush that finished the handshake wakes the reader parked on it"
        );
        assert!(matches!(
            Pin::new(&mut io).poll_read(&mut context, &mut buffer),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(buffer.filled(), REQUEST);

        drop(release);
        client.join().unwrap();
    }

    /// The listener polls each handshake flight only after its connection has
    /// waited past both read deadlines. The client sent every flight at once,
    /// so the listener's lateness must not count as the client's silence.
    #[test]
    fn a_late_listener_poll_does_not_time_out_a_prompt_handshake() {
        let deadlines = RemoteBrainTlsDeadlines {
            read_idle: Duration::from_millis(200),
            request_read: Duration::from_millis(400),
            ..RemoteBrainTlsDeadlines::PRODUCTION
        };
        let directory = tempfile::tempdir().unwrap();
        let listener = tls_listener(directory.path()).with_deadlines(deadlines);
        let address = listener.bound_addr().unwrap();
        let (sent, client_sent) = mpsc::channel();
        let (answer, await_answer) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let client =
            std::thread::spawn(move || hold_open_client(address, sent, await_answer, held));
        let (mut io, _) = native_runtime::block_on(listener.accept())
            .unwrap()
            .expect("accepted connection");
        let accepted = std::time::Instant::now();
        let poll_late = |past: Duration| {
            std::thread::sleep(past.saturating_sub(accepted.elapsed()));
        };

        let waker = Arc::new(CountingWaker::default());
        let task_waker = Waker::from(Arc::clone(&waker));
        let mut context = Context::from_waker(&task_waker);
        let mut storage = [0_u8; 4096];
        let mut buffer = ReadBuf::new(&mut storage);
        client_sent.recv().expect("client sends its ClientHello");
        poll_late(deadlines.request_read * 3 / 2);
        let first_read = Pin::new(&mut io).poll_read(&mut context, &mut buffer);
        assert!(
            first_read.is_pending(),
            "reading the waiting ClientHello restarts both read deadlines: {first_read:?}"
        );
        answer.send(()).unwrap();
        client_sent.recv().expect("client sends its final flight");
        poll_late(deadlines.request_read * 3);
        let read = Pin::new(&mut io).poll_read(&mut context, &mut buffer);
        assert!(
            matches!(read, Poll::Ready(Ok(()))),
            "a handshake the listener can finish restarts the request deadline: {read:?}"
        );
        assert_eq!(buffer.filled(), REQUEST);

        drop(release);
        client.join().unwrap();
    }

    /// The client sends its headers with the handshake and its body as soon as
    /// the listener has read them, so the whole request is waiting well before
    /// the absolute request deadline. A listener that polls only once that
    /// deadline has passed must read the waiting body rather than time the
    /// client out.
    #[test]
    fn a_late_listener_poll_reads_a_body_that_arrived_in_time() {
        const HEADERS: &[u8] =
            b"POST /remote/authority HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n";
        const BODY: &[u8] = b"{}";
        let deadlines = RemoteBrainTlsDeadlines {
            read_idle: Duration::from_millis(200),
            request_read: Duration::from_millis(400),
            ..RemoteBrainTlsDeadlines::PRODUCTION
        };
        let directory = tempfile::tempdir().unwrap();
        let listener = tls_listener(directory.path()).with_deadlines(deadlines);
        let address = listener.bound_addr().unwrap();
        let (sent, body_sent) = mpsc::channel();
        let (headers_read, send_body) = mpsc::channel::<()>();
        let client = std::thread::spawn(move || {
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(CertificateDer::from_pem_slice(TRUST_ROOT).unwrap())
                .unwrap();
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
            let mut tls = rustls::ClientConnection::new(
                Arc::new(config),
                ServerName::IpAddress(address.ip().into()),
            )
            .unwrap();
            let mut socket = std::net::TcpStream::connect(address).unwrap();
            tls.writer().write_all(HEADERS).unwrap();
            while tls.is_handshaking() || tls.wants_write() {
                tls.complete_io(&mut socket).unwrap();
            }
            send_body.recv().expect("the listener reads the headers");
            tls.writer().write_all(BODY).unwrap();
            while tls.wants_write() {
                tls.write_tls(&mut socket).unwrap();
            }
            let _ = sent.send(());
            let _ = send_body.recv();
        });
        let (mut io, _) = native_runtime::block_on(listener.accept())
            .unwrap()
            .expect("accepted connection");

        let waker = Arc::new(CountingWaker::default());
        let task_waker = Waker::from(Arc::clone(&waker));
        let mut context = Context::from_waker(&task_waker);
        let mut storage = [0_u8; 4096];
        let mut buffer = ReadBuf::new(&mut storage);
        let mut seen = 0;
        while Pin::new(&mut io)
            .poll_read(&mut context, &mut buffer)
            .is_pending()
        {
            seen = waker.wait_past(seen);
        }
        assert_eq!(buffer.filled(), HEADERS);
        let headers_read_at = std::time::Instant::now();
        headers_read.send(()).unwrap();

        body_sent.recv().expect("client sends its body");
        std::thread::sleep(
            (deadlines.request_read * 3 / 2).saturating_sub(headers_read_at.elapsed()),
        );
        let mut storage = [0_u8; 4096];
        let mut buffer = ReadBuf::new(&mut storage);
        let read = Pin::new(&mut io).poll_read(&mut context, &mut buffer);
        assert!(
            matches!(read, Poll::Ready(Ok(()))),
            "a body waiting before the request deadline is read on a late poll: {read:?}"
        );
        assert_eq!(buffer.filled(), BODY);

        drop(headers_read);
        client.join().unwrap();
    }
}

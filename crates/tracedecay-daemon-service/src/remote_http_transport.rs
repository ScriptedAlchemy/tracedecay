//! Bounded Remote Brain HTTPS transport for an injected canonical application router.
//! Owns TLS identity admission, HTTP framing, connection budgets, and draining;
//! credential and project authorities remain with the daemon composition.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(any(test, feature = "test-helpers"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, header::CONNECTION};
use axum::middleware::Next;
use axum::response::Response;
use axum::{Router, middleware};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rustls::client::{verify_server_cert_signed_by_trust_anchor, verify_server_name};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracedecay_daemon_control::RemoteBrainTlsConfig;
use tracedecay_domain::errors::{Result, TraceDecayError};

const MAX_REMOTE_BRAIN_TLS_CONNECTIONS: usize = 128;
const MAX_REMOTE_BRAIN_TLS_HEADER_BYTES: usize = 64 * 1024;
const REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE: Duration = Duration::from_secs(5);
const REMOTE_BRAIN_TLS_REQUEST_READ_DEADLINE: Duration = Duration::from_mins(1);
const REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE: Duration = Duration::from_secs(5);
const REMOTE_BRAIN_TLS_RESPONSE_DEADLINE: Duration = Duration::from_secs(30);
const REMOTE_BRAIN_TLS_CLOSE_DEADLINE: Duration = Duration::from_secs(5);
const REMOTE_BRAIN_TLS_SHUTDOWN_DRAIN_DEADLINE: Duration = Duration::from_secs(5);

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
    listener: tokio::net::TcpListener,
    server: Arc<rustls::ServerConfig>,
    admission: Arc<Semaphore>,
    #[cfg(any(test, feature = "test-helpers"))]
    egress: Arc<RemoteBrainTlsEgressObserver>,
}

impl RemoteBrainTlsListener {
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
        let router = crate::application_surface::with_hotpath_server_layer(
            router.layer(middleware::from_fn(force_remote_connection_close)),
        );
        let graceful = CancellationToken::new();
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_requested => break,
                joined = connections.join_next(), if !connections.is_empty() => {
                    if let Some(joined) = joined {
                        observe_remote_brain_connection_join(joined);
                    }
                }
                accepted = self.accept() => {
                    if let Some((io, address)) = accepted {
                        let router = router.clone();
                        let graceful = graceful.clone();
                        connections.spawn(hotpath::future!(
                            async move {
                                serve_remote_brain_tls_connection(io, router, graceful, address).await;
                            },
                            label = "daemon.http.application.remote_tls_connection"
                        ));
                    }
                }
            }
        }

        graceful.cancel();
        let drain = async {
            while let Some(joined) = connections.join_next().await {
                observe_remote_brain_connection_join(joined);
            }
        };
        if tokio::time::timeout(REMOTE_BRAIN_TLS_SHUTDOWN_DRAIN_DEADLINE, drain)
            .await
            .is_err()
        {
            connections.abort_all();
            while let Some(joined) = connections.join_next().await {
                observe_remote_brain_connection_join(joined);
            }
        }
        Ok(())
    }

    #[hotpath::measure(label = "daemon.http.application.tls_bind", future = true)]
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
        let listener = tokio::net::TcpListener::bind(config.listen())
            .await
            .map_err(|error| tls_configuration_error("bind Remote Brain TLS listener", error))?;
        Ok(Self {
            listener,
            server: Arc::new(server),
            admission: Arc::new(Semaphore::new(MAX_REMOTE_BRAIN_TLS_CONNECTIONS)),
            #[cfg(any(test, feature = "test-helpers"))]
            egress: Arc::new(RemoteBrainTlsEgressObserver::default()),
        })
    }

    pub fn bound_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    #[hotpath::measure(label = "daemon.http.application.tls_accept", future = true)]
    async fn accept(&self) -> Option<(RemoteBrainTlsIo, SocketAddr)> {
        let (stream, address) = match self.listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "Remote Brain TLS listener accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
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
        let handshake = tokio_rustls::TlsAcceptor::from(Arc::clone(&self.server)).accept(stream);
        Some((
            RemoteBrainTlsIo::new(
                handshake,
                permit,
                #[cfg(any(test, feature = "test-helpers"))]
                Arc::clone(&self.egress),
            ),
            address,
        ))
    }
}

#[hotpath::measure(label = "daemon.http.application.tls_validate_identity")]
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

async fn serve_remote_brain_tls_connection(
    io: RemoteBrainTlsIo,
    router: Router,
    graceful: CancellationToken,
    address: SocketAddr,
) {
    let service =
        router.map_request(|request: hyper::Request<hyper::body::Incoming>| request.map(Body::new));
    let service = TowerToHyperService::new(service);
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.keep_alive(false);
    let connection = builder.serve_connection(TokioIo::new(io), service);
    tokio::pin!(connection);
    let result = tokio::select! {
        result = &mut connection => result,
        () = graceful.cancelled() => {
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    };
    if let Err(error) = result {
        tracing::debug!(%error, %address, "Remote Brain TLS connection stopped");
    }
}

fn observe_remote_brain_connection_join(joined: std::result::Result<(), tokio::task::JoinError>) {
    if let Err(error) = joined
        && !error.is_cancelled()
    {
        tracing::warn!(%error, "Remote Brain TLS connection task failed");
    }
}

enum RemoteBrainTlsTransport {
    Handshaking(Pin<Box<tokio_rustls::Accept<tokio::net::TcpStream>>>),
    Streaming(Box<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>),
    Failed,
}

struct RemoteBrainTlsIo {
    transport: RemoteBrainTlsTransport,
    read_idle_deadline: Pin<Box<tokio::time::Sleep>>,
    request_read_deadline: Pin<Box<tokio::time::Sleep>>,
    write_idle_deadline: Pin<Box<tokio::time::Sleep>>,
    response_deadline: Pin<Box<tokio::time::Sleep>>,
    close_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
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
    egress_last_positive_write: Option<tokio::time::Instant>,
    #[cfg(any(test, feature = "test-helpers"))]
    egress_write_idle_deadline_at: Option<tokio::time::Instant>,
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
        handshake: tokio_rustls::Accept<tokio::net::TcpStream>,
        permit: OwnedSemaphorePermit,
        #[cfg(any(test, feature = "test-helpers"))] egress: Arc<RemoteBrainTlsEgressObserver>,
    ) -> Self {
        Self {
            transport: RemoteBrainTlsTransport::Handshaking(Box::pin(handshake)),
            read_idle_deadline: Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE)),
            request_read_deadline: Box::pin(tokio::time::sleep(
                REMOTE_BRAIN_TLS_REQUEST_READ_DEADLINE,
            )),
            write_idle_deadline: Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE)),
            response_deadline: Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_RESPONSE_DEADLINE)),
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

    fn poll_request_absolute_deadline(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.request_read_complete {
            return Ok(());
        }
        if self.request_read_deadline.as_mut().poll(context).is_ready() {
            self.transport = RemoteBrainTlsTransport::Failed;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Remote Brain TLS request admission timed out",
            ));
        }
        Ok(())
    }

    fn poll_read_idle_deadline(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.request_read_complete {
            return Ok(());
        }
        if self.read_idle_deadline.as_mut().poll(context).is_pending() {
            return Ok(());
        }
        self.transport = RemoteBrainTlsTransport::Failed;
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Remote Brain TLS request read timed out",
        ))
    }

    fn poll_write_deadline(
        &mut self,
        context: &mut Context<'_>,
        start_egress: bool,
    ) -> io::Result<()> {
        if !self.egress_started && start_egress {
            let now = tokio::time::Instant::now();
            self.write_idle_deadline
                .as_mut()
                .reset(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
            self.response_deadline
                .as_mut()
                .reset(now + REMOTE_BRAIN_TLS_RESPONSE_DEADLINE);
            self.egress_started = true;
            #[cfg(any(test, feature = "test-helpers"))]
            {
                self.egress_last_positive_write = Some(now);
                self.egress_write_idle_deadline_at =
                    Some(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
                self.egress.active.fetch_add(1, Ordering::SeqCst);
            }
        }
        if !self.egress_started {
            return Ok(());
        }
        let write_idle_expired = self.write_idle_deadline.as_mut().poll(context).is_ready();
        let response_expired = self.response_deadline.as_mut().poll(context).is_ready();
        if !write_idle_expired && !response_expired {
            return Ok(());
        }
        self.transport = RemoteBrainTlsTransport::Failed;
        #[cfg(any(test, feature = "test-helpers"))]
        if write_idle_expired && !self.egress_idle_expired {
            self.egress_idle_expired = true;
            self.egress.idle_expirations.fetch_add(1, Ordering::SeqCst);
            let violates_idle_deadline = self
                .egress_last_positive_write
                .zip(self.egress_write_idle_deadline_at)
                .is_none_or(|(last_write, deadline_at)| {
                    deadline_at.saturating_duration_since(last_write)
                        != std::time::Duration::from_secs(5)
                });
            if violates_idle_deadline {
                self.egress
                    .idle_deadline_contract_violations
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
        #[cfg(any(test, feature = "test-helpers"))]
        if response_expired && !self.egress_response_expired {
            self.egress_response_expired = true;
            self.egress
                .response_expirations
                .fetch_add(1, Ordering::SeqCst);
        }
        #[cfg(any(test, feature = "test-helpers"))]
        self.clear_egress_backpressure();
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Remote Brain TLS response write timed out",
        ))
    }

    fn reset_write_idle_deadline(&mut self) {
        let now = tokio::time::Instant::now();
        self.write_idle_deadline
            .as_mut()
            .reset(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
        #[cfg(any(test, feature = "test-helpers"))]
        {
            self.egress_last_positive_write = Some(now);
            self.egress_write_idle_deadline_at = Some(now + REMOTE_BRAIN_TLS_WRITE_IDLE_DEADLINE);
        }
    }

    fn poll_close_deadline(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let deadline = self
            .close_deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(REMOTE_BRAIN_TLS_CLOSE_DEADLINE)));
        if deadline.as_mut().poll(context).is_pending() {
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
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(stream)) => {
                self.transport = RemoteBrainTlsTransport::Streaming(Box::new(stream));
                let now = tokio::time::Instant::now();
                self.read_idle_deadline
                    .as_mut()
                    .reset(now + REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE);
                self.request_read_deadline
                    .as_mut()
                    .reset(now + REMOTE_BRAIN_TLS_REQUEST_READ_DEADLINE);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.transport = RemoteBrainTlsTransport::Failed;
                Poll::Ready(Err(error))
            }
        }
    }

    #[hotpath::measure(label = "daemon.http.application.observe_http_request")]
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
        if let Err(error) = self.poll_request_absolute_deadline(context) {
            return Poll::Ready(Err(error));
        }
        match self.poll_handshake(context) {
            Poll::Pending => {
                return match self.poll_read_idle_deadline(context) {
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
                self.read_idle_deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + REMOTE_BRAIN_TLS_READ_IDLE_DEADLINE);
            }
            if let Err(error) = self.observe_http_request(&buffer.filled()[before..]) {
                return Poll::Ready(Err(error));
            }
        }
        if result.is_pending()
            && let Err(error) = self.poll_read_idle_deadline(context)
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
            self.reset_write_idle_deadline();
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

#[hotpath::measure(label = "daemon.http.application.parse_body_length")]
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

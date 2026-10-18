use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use rustls::pki_types::pem::PemObject;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracedecay_contracts::REMOTE_ENROLLMENT_CREDENTIAL_HEADER;
use tracedecay_contracts::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_contracts::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, PolicyDecisionRef,
    ResolvedScope,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, EnrollmentGrantV1, EntityId, ManifestDigest,
    ProjectId, RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1,
    RepositoryId, RepositoryStateSnapshotId, UserProfileId, UtcMicros, WorktreeId,
    canonical_sha256,
};

use super::{AUTH_TOKEN, current_micros};
use crate::daemon::http_application::{
    DaemonHttpApplicationRegistry, DaemonHttpApplicationService,
};
use tracedecay_daemon_service::DaemonInvocationService;
use tracedecay_daemon_service::remote_http_transport::{
    RemoteBrainTlsDeadlines, validate_remote_brain_tls_identity_at,
};

const REMOTE_TLS_CERTIFICATE: &[u8] =
    include_bytes!("../../../../../tests/fixtures/remote_tls/localhost.crt.pem");
const REMOTE_TLS_LEAF_CERTIFICATE: &[u8] =
    include_bytes!("../../../../../tests/fixtures/remote_tls/localhost-leaf.crt.pem");
const REMOTE_TLS_PRIVATE_KEY: &[u8] =
    include_bytes!("../../../../../tests/fixtures/remote_tls/localhost.key.pem");
const REMOTE_TLS_CA_TRUE_CERTIFICATE: &[u8] =
    include_bytes!("../../../../../tests/fixtures/remote_tls/ca-true.crt.pem");
const REMOTE_TLS_CLIENT_AUTH_CERTIFICATE: &[u8] =
    include_bytes!("../../../../../tests/fixtures/remote_tls/client-auth-only.crt.pem");
const REMOTE_TLS_WRONG_IP_CERTIFICATE: &[u8] =
    include_bytes!("../../../../../tests/fixtures/remote_tls/wrong-ip.crt.pem");
const REMOTE_TLS_ALTERNATE_ROOT_CERTIFICATE: &[u8] =
    include_bytes!("../../../../../tests/fixtures/remote_tls/alternate-root.crt.pem");

fn remote_tls_fixture(
    temporary: &tempfile::TempDir,
) -> (tracedecay_daemon_control::RemoteBrainTlsConfig, PathBuf) {
    let certificate = temporary.path().join("remote.crt.pem");
    let private_key = temporary.path().join("remote.key.pem");
    std::fs::write(&certificate, REMOTE_TLS_CERTIFICATE).expect("write TLS certificate fixture");
    std::fs::write(&private_key, REMOTE_TLS_PRIVATE_KEY).expect("write TLS key fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&private_key, std::fs::Permissions::from_mode(0o600))
            .expect("restrict TLS key fixture");
    }
    // Windows analogue of the mode above. Startup refuses a TLS key whose
    // DACL is not the protected single-ACE current-user one, and a file just
    // created under a temporary directory inherits that directory's ACEs.
    #[cfg(windows)]
    drop(
        tracedecay_private_fs::windows::make_private_file(&private_key)
            .expect("restrict TLS key fixture"),
    );
    let config = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:0".parse().expect("ephemeral TLS listener")),
        Some(certificate.clone()),
        Some(private_key),
    )
    .expect("complete TLS configuration")
    .expect("enabled TLS configuration");
    (config, certificate)
}

fn unprovisioned_remote_registry(identity: &str) -> DaemonHttpApplicationRegistry {
    let credentials = Arc::new(
        tracedecay_store_runtime::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new(format!("brain.{identity}")).expect("brain identity"),
            UserProfileId::new(format!("profile.{identity}")).expect("profile identity"),
        ),
    );
    let transaction =
        Arc::new(tracedecay_store_runtime::DaemonRemoteReplayTransactionAuthorityV1::new());
    let router = tracedecay_daemon_service::build_daemon_remote_protocol_router(
        Arc::clone(&credentials),
        transaction,
        DaemonInvocationService::default(),
    )
    .expect("remote protocol router");
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, None)
        .expect("install remote protocol router");
    registry
}

fn large_response_remote_registry(
    identity: &str,
) -> (DaemonHttpApplicationRegistry, Arc<tokio::sync::Barrier>) {
    let credentials = Arc::new(
        tracedecay_store_runtime::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new(format!("brain.{identity}")).expect("remote brain identity"),
            UserProfileId::new(format!("profile.{identity}")).expect("remote profile identity"),
        ),
    );
    let barrier = Arc::new(tokio::sync::Barrier::new(129));
    let handler_barrier = Arc::clone(&barrier);
    let router = Router::new().route(
        "/egress",
        get(move || {
            let handler_barrier = Arc::clone(&handler_barrier);
            async move {
                handler_barrier.wait().await;
                const CHUNK: &[u8; 64 * 1024] = &[0; 64 * 1024];
                let chunks = futures_util::stream::repeat_with(|| {
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(CHUNK))
                });
                Body::from_stream(chunks)
            }
        }),
    );
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, None)
        .expect("install large-response remote router");
    (registry, barrier)
}

/// The `/slow` handler answers only after the test notifies the returned
/// release, so the test decides how long the handler outlives the request.
fn delayed_response_remote_registry(
    identity: &str,
) -> (
    DaemonHttpApplicationRegistry,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let credentials = Arc::new(
        tracedecay_store_runtime::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new(format!("brain.{identity}")).expect("remote brain identity"),
            UserProfileId::new(format!("profile.{identity}")).expect("remote profile identity"),
        ),
    );
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let handler_started = Arc::clone(&started);
    let handler_release = Arc::clone(&release);
    let router = Router::new().route(
        "/slow",
        get(move || {
            let handler_started = Arc::clone(&handler_started);
            let handler_release = Arc::clone(&handler_release);
            async move {
                handler_started.notify_one();
                handler_release.notified().await;
                "complete"
            }
        }),
    );
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, None)
        .expect("install delayed-response remote router");
    (registry, started, release)
}

fn stalled_response_remote_registry(
    identity: &str,
) -> (DaemonHttpApplicationRegistry, Arc<tokio::sync::Notify>) {
    let credentials = Arc::new(
        tracedecay_store_runtime::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new(format!("brain.{identity}")).expect("remote brain identity"),
            UserProfileId::new(format!("profile.{identity}")).expect("remote profile identity"),
        ),
    );
    let started = Arc::new(tokio::sync::Notify::new());
    let handler_started = Arc::clone(&started);
    let router = Router::new().route(
        "/stalled",
        get(move || {
            let handler_started = Arc::clone(&handler_started);
            async move {
                handler_started.notify_one();
                std::future::pending::<&'static str>().await
            }
        }),
    );
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, None)
        .expect("install stalled-response remote router");
    (registry, started)
}

fn live_remote_grant(brain_id: BrainId, node_id: BrainNodeId, secret: &[u8]) -> EnrollmentGrantV1 {
    let now = current_micros();
    EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.remote-tls-authority").expect("grant identity"),
        brain_id,
        node_id,
        fingerprint: RemoteCredentialFingerprintV1::from_secret(secret)
            .expect("credential fingerprint"),
        revision: 1,
        issued_at: UtcMicros(now.0.saturating_sub(60_000_000)),
        expires_at: UtcMicros(now.0.saturating_add(60_000_000)),
        revoked_at: None,
        capabilities: [RemoteCapabilityV1::Query].into_iter().collect(),
        scope: RemoteRepositoryScopeV1 {
            project_id: ProjectId::new("project.remote-tls").expect("project identity"),
            repository_id: RepositoryId::new("repository.remote-tls").expect("repository identity"),
            worktree_id: WorktreeId::new("worktree.remote-tls").expect("worktree identity"),
            reference: Some(RefId::new("refs/heads/remote-tls").expect("reference identity")),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-tls")
                .expect("snapshot identity"),
        },
    }
}

fn live_remote_admission(grant: &EnrollmentGrantV1) -> RemoteEnrollmentAdmissionEvidenceV1 {
    let now = current_micros();
    let scope = ResolvedScope::new(
        grant.scope.project_id.clone(),
        grant.scope.repository_id.clone(),
        grant.scope.worktree_id.clone(),
        grant.scope.reference.clone(),
    )
    .expect("resolved remote scope");
    let grant_digest = canonical_sha256(grant).expect("grant digest");
    RemoteEnrollmentAdmissionEvidenceV1::new(
        grant,
        scope.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str())
                .expect("capability grant identity"),
            grant_revision: grant.revision,
            grant_digest: grant_digest.clone(),
            authorized_scope_digest: scope.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote-tls",
                1,
                grant_digest,
                ComponentVersion::new("policy.remote-tls.v1").expect("policy component"),
            )
            .expect("policy decision"),
            revalidated_at: now,
        },
        ActorId::new("actor.remote-tls").expect("actor identity"),
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("configuration digest"),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("catalog digest"),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).expect("privacy digest"),
        Deadline::new(UtcMicros(now.0.saturating_add(60_000_000))).expect("deadline"),
    )
    .expect("remote enrollment admission")
}

/// Sends `request` with the handshake and reads the response until the
/// listener closes the connection.
///
/// The listener's deadlines start no earlier than the connect, so a client
/// whose handshake returned within the read-idle deadline met both of them,
/// and closing its connection without a response is a listener defect. Only
/// a slower handshake lets the listener close first, and the attempt is then
/// retried on a fresh connection.
async fn remote_tls_exchange(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
    request: &[u8],
    read_idle: Duration,
) -> String {
    for _ in 0..3 {
        let connect_started = std::time::Instant::now();
        let mut stream = remote_tls_connect_sending(endpoint, certificate, request).await;
        let handshake = connect_started.elapsed();
        let mut response = Vec::new();
        let closed = stream.read_to_end(&mut response).await;
        if !response.is_empty() {
            closed.expect("read TLS response");
            return String::from_utf8(response).expect("HTTP response text");
        }
        assert!(
            handshake >= read_idle,
            "the listener closed a connection whose handshake took {handshake:?} without a response: {closed:?}"
        );
    }
    panic!("the test thread stalled past the read-idle deadline in every handshake")
}

async fn remote_tls_request(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
    path: &str,
    authorization: Option<&[u8]>,
    enrollment_credential: Option<&[u8]>,
    body: &[u8],
) -> String {
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    )
    .into_bytes();
    if let Some(authorization) = authorization {
        request.extend_from_slice(b"Authorization: Bearer ");
        request.extend_from_slice(authorization);
        request.extend_from_slice(b"\r\n");
    }
    if let Some(enrollment_credential) = enrollment_credential {
        request.extend_from_slice(REMOTE_ENROLLMENT_CREDENTIAL_HEADER.as_bytes());
        request.extend_from_slice(b": ");
        request.extend_from_slice(enrollment_credential);
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(body);
    remote_tls_exchange(
        endpoint,
        certificate,
        &request,
        RemoteBrainTlsDeadlines::PRODUCTION.read_idle,
    )
    .await
}

/// Connects with `request` queued before the handshake, so it leaves in the
/// client's final handshake flight and no client scheduling delay can let the
/// listener's read-idle deadline expire before the request arrives. Nagle is
/// off, so each client write is on the wire when it returns and the client's
/// own timings bound the listener's.
async fn remote_tls_connect_sending(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
    request: &[u8],
) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
    let certificates = rustls::pki_types::CertificateDer::pem_file_iter(certificate)
        .expect("open TLS chain fixture")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("decode TLS chain fixture");
    assert_eq!(
        certificates.len(),
        2,
        "TLS chain fixture must be leaf then explicit trust anchor"
    );
    let trust_anchor = certificates
        .last()
        .expect("TLS chain fixture contains an explicit trust anchor")
        .clone();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(trust_anchor)
        .expect("install TLS trust anchor fixture");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let stream = tokio::net::TcpStream::connect(endpoint)
        .await
        .expect("connect Remote Brain TLS listener");
    stream
        .set_nodelay(true)
        .expect("disable Nagle on the Remote Brain TLS client");
    let server_name = rustls::pki_types::ServerName::try_from("localhost")
        .expect("TLS server name")
        .to_owned();
    let stream = connector
        .connect_with(server_name, stream, |connection| {
            std::io::Write::write_all(&mut connection.writer(), request)
                .expect("queue the request behind the TLS handshake");
        })
        .await
        .expect("authenticate Remote Brain TLS authority");
    assert_eq!(
        stream.get_ref().1.alpn_protocol(),
        Some(b"http/1.1".as_slice()),
        "Remote Brain TLS must negotiate only HTTP/1.1"
    );
    stream
}

/// Sends `request` with the handshake and waits for its handler to start.
///
/// The listener's deadlines start no earlier than the connect, so a client
/// whose handshake returned within the read-idle deadline met both of them,
/// and closing its connection is a listener defect. Only a slower handshake
/// lets the listener close first, and the attempt is then retried on a fresh
/// connection.
async fn remote_tls_start_handler(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
    request: &[u8],
    started: &tokio::sync::Notify,
    read_idle: Duration,
) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
    for _ in 0..3 {
        let connect_started = std::time::Instant::now();
        let mut stream = remote_tls_connect_sending(endpoint, certificate, request).await;
        let handshake = connect_started.elapsed();
        let mut probe = [0_u8; 1];
        tokio::select! {
            biased;
            () = started.notified() => return stream,
            closed = stream.read(&mut probe) => assert!(
                handshake >= read_idle,
                "the listener closed a connection whose handshake took {handshake:?}: {closed:?}"
            ),
        }
    }
    panic!("the test thread stalled past the read-idle deadline in every handshake")
}

/// How the listener ended a request body the client trickled to it.
enum Trickle {
    /// The listener answered the complete request.
    Answered(Vec<u8>),
    /// The listener closed the connection at the absolute request deadline.
    ClosedAtRequestDeadline,
    /// The client went quiet for the read-idle deadline between two writes,
    /// so the listener may rightly have closed the connection.
    ClientStalled,
}

/// Sends `headers` with the handshake, then `body` one byte per `step`, and
/// reads until the listener closes the connection.
///
/// The listener's deadlines start no earlier than the connect, and it reads
/// the bytes waiting for it before it applies a deadline, so the client's own
/// clock bounds the listener's. A close is a listener defect unless the
/// client went quiet for the read-idle deadline, or the request deadline
/// closed it after the listener read every byte sent before that deadline
/// could start.
async fn remote_tls_trickle_body(
    service: &DaemonHttpApplicationService,
    endpoint: std::net::SocketAddr,
    certificate: &Path,
    headers: &[u8],
    body: &[u8],
    deadlines: RemoteBrainTlsDeadlines,
    step: Duration,
) -> Trickle {
    let ingress = || {
        service
            .remote_tls_ingress_snapshot()
            .expect("TLS ingress observer")
    };
    let initial = ingress();
    let connect_started = std::time::Instant::now();
    let mut stream = remote_tls_connect_sending(endpoint, certificate, headers).await;
    let mut stalled = connect_started.elapsed() >= deadlines.read_idle;
    let mut previous_write = connect_started;
    let mut sent_in_time = 0;
    for byte in body {
        tokio::time::sleep(step).await;
        let write_started = std::time::Instant::now();
        let mut written = stream.write_all(std::slice::from_ref(byte)).await;
        if written.is_ok() {
            written = stream.flush().await;
        }
        if written.is_err() {
            break;
        }
        stalled |= previous_write.elapsed() >= deadlines.read_idle;
        previous_write = write_started;
        if connect_started.elapsed() < deadlines.request_read {
            sent_in_time += 1;
        }
    }
    let mut response = Vec::new();
    let _closed = stream.read_to_end(&mut response).await;
    if !response.is_empty() {
        return Trickle::Answered(response);
    }
    let closed = ingress();
    if closed.request_read_expirations > initial.request_read_expirations {
        let read = closed.body_bytes_observed - initial.body_bytes_observed;
        assert!(
            read >= sent_in_time,
            "the listener applied the request deadline after reading {read} of the {sent_in_time} body bytes sent before it could start"
        );
        return Trickle::ClosedAtRequestDeadline;
    }
    assert!(
        stalled,
        "the listener closed a connection whose client kept every deadline: {closed:?}"
    );
    Trickle::ClientStalled
}

async fn remote_tls_h2_only_handshake_is_rejected(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
) {
    let certificates = rustls::pki_types::CertificateDer::pem_file_iter(certificate)
        .expect("open TLS chain fixture")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("decode TLS chain fixture");
    assert_eq!(
        certificates.len(),
        2,
        "TLS chain fixture must be leaf then explicit trust anchor"
    );
    let trust_anchor = certificates
        .last()
        .expect("TLS chain fixture contains an explicit trust anchor")
        .clone();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(trust_anchor)
        .expect("install TLS trust anchor fixture");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"h2".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let stream = tokio::net::TcpStream::connect(endpoint)
        .await
        .expect("connect HTTP/2-only TLS probe");
    let server_name = rustls::pki_types::ServerName::try_from("localhost")
        .expect("TLS server name")
        .to_owned();
    assert!(
        connector.connect(server_name, stream).await.is_err(),
        "the HTTP/1.1-only listener must reject an HTTP/2-only ALPN offer"
    );
}

#[test]
fn remote_tls_configuration_rejects_partial_and_wildcard_admission() {
    let path = PathBuf::from("remote.pem");
    let refusal = |listen: &str, key: Option<PathBuf>| {
        match tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
            Some(listen.parse().unwrap()),
            Some(path.clone()),
            key,
        ) {
            Err(TraceDecayError::Config { message }) => message,
            Err(other) => panic!("unexpected refusal kind: {other:?}"),
            Ok(_) => panic!("{listen} must be refused"),
        }
    };
    assert_eq!(
        refusal("127.0.0.1:7443", None),
        "Remote Brain TLS listener requires --remote-listen, --remote-tls-cert, and --remote-tls-key together"
    );
    assert_eq!(
        refusal("0.0.0.0:7443", Some(path.clone())),
        "Remote Brain TLS listener requires an explicit interface address; wildcard addresses are refused"
    );
    let admitted = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:7443".parse().unwrap()),
        Some(path.clone()),
        Some(path.clone()),
    )
    .expect("explicit interface with both paths")
    .expect("configured listener");
    assert_eq!(admitted.listen(), "127.0.0.1:7443".parse().unwrap());
    assert_eq!(admitted.private_key(), path.as_path());
}

#[tokio::test]
async fn remote_tls_startup_rejects_invalid_identity_and_occupied_address() {
    let temporary = tempfile::tempdir().expect("remote TLS startup fixture");
    let invalid_certificate = temporary.path().join("invalid.crt.pem");
    let invalid_key = temporary.path().join("invalid.key.pem");
    std::fs::write(&invalid_certificate, b"not a certificate").expect("write invalid certificate");
    std::fs::write(&invalid_key, b"not a private key").expect("write invalid private key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&invalid_key, std::fs::Permissions::from_mode(0o600))
            .expect("restrict invalid key fixture");
    }
    let invalid = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:0".parse().unwrap()),
        Some(invalid_certificate),
        Some(invalid_key),
    )
    .unwrap()
    .unwrap();
    let invalid_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-invalid"),
        AUTH_TOKEN,
        Some(&invalid),
    )
    .await
    {
        Ok(_) => panic!("invalid TLS identity must stop startup"),
        Err(error) => error,
    };
    assert!(invalid_error.to_string().contains("TLS certificate"));

    let (valid, valid_certificate) = remote_tls_fixture(&temporary);
    let invalid_key = temporary.path().join("invalid.key.pem");
    let invalid_key_config = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:0".parse().unwrap()),
        Some(valid_certificate),
        Some(invalid_key),
    )
    .unwrap()
    .unwrap();
    let invalid_key_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-invalid-key"),
        AUTH_TOKEN,
        Some(&invalid_key_config),
    )
    .await
    {
        Ok(_) => panic!("invalid TLS private key must stop startup"),
        Err(error) => error,
    };
    assert!(invalid_key_error.to_string().contains("TLS private key"));

    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("occupied address");
    let occupied_config = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some(occupied.local_addr().expect("occupied address")),
        Some(valid.certificate_chain().to_path_buf()),
        Some(valid.private_key().to_path_buf()),
    )
    .unwrap()
    .unwrap();
    let bind_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-occupied"),
        AUTH_TOKEN,
        Some(&occupied_config),
    )
    .await
    {
        Ok(_) => panic!("occupied Remote Brain address must stop startup"),
        Err(error) => error,
    };
    assert!(
        bind_error
            .to_string()
            .contains("bind Remote Brain TLS listener")
    );
}

#[tokio::test]
async fn remote_tls_startup_rejects_unusable_leaf_and_chain_constraints() {
    let temporary = tempfile::tempdir().expect("remote TLS constraint fixture");
    let leaf_without_anchor = temporary.path().join("leaf-without-anchor.crt.pem");
    let leaf_without_anchor_key = temporary.path().join("leaf-without-anchor.key.pem");
    std::fs::write(&leaf_without_anchor, REMOTE_TLS_LEAF_CERTIFICATE)
        .expect("write leaf-only TLS certificate");
    std::fs::write(&leaf_without_anchor_key, REMOTE_TLS_PRIVATE_KEY)
        .expect("write leaf-only TLS private key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &leaf_without_anchor_key,
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("restrict leaf-only TLS private key");
    }
    let leaf_without_anchor_config =
        tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
            Some("127.0.0.1:0".parse().unwrap()),
            Some(leaf_without_anchor),
            Some(leaf_without_anchor_key),
        )
        .unwrap()
        .unwrap();
    let leaf_without_anchor_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-leaf-without-anchor"),
        AUTH_TOKEN,
        Some(&leaf_without_anchor_config),
    )
    .await
    {
        Ok(_) => panic!("a leaf without an explicit trust anchor must stop startup"),
        Err(error) => error,
    };
    assert!(
        leaf_without_anchor_error
            .to_string()
            .contains("requires a leaf followed by an explicit trust anchor"),
        "leaf-only identity returned the wrong startup error: {leaf_without_anchor_error}"
    );

    for (name, certificate, expected_error) in [
        (
            "ca-true",
            REMOTE_TLS_CA_TRUE_CERTIFICATE,
            "validate Remote Brain TLS certificate chain",
        ),
        (
            "client-auth-only",
            REMOTE_TLS_CLIENT_AUTH_CERTIFICATE,
            "validate Remote Brain TLS certificate chain",
        ),
        (
            "wrong-ip",
            REMOTE_TLS_WRONG_IP_CERTIFICATE,
            "validate Remote Brain TLS listen address identity",
        ),
    ] {
        let certificate_path = temporary.path().join(format!("{name}.crt.pem"));
        let private_key_path = temporary.path().join(format!("{name}.key.pem"));
        std::fs::write(&certificate_path, certificate).expect("write invalid TLS certificate");
        std::fs::write(&private_key_path, REMOTE_TLS_PRIVATE_KEY)
            .expect("write matching TLS private key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&private_key_path, std::fs::Permissions::from_mode(0o600))
                .expect("restrict TLS private key fixture");
        }
        let config = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
            Some("127.0.0.1:0".parse().unwrap()),
            Some(certificate_path),
            Some(private_key_path),
        )
        .unwrap()
        .unwrap();
        let error = match DaemonHttpApplicationService::bind_with_remote_tls(
            unprovisioned_remote_registry(&format!("remote-tls-{name}")),
            AUTH_TOKEN,
            Some(&config),
        )
        .await
        {
            Ok(_) => panic!("{name} TLS identity must stop startup"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains(expected_error),
            "{name} returned the wrong startup error: {error}"
        );
    }

    let unrelated_chain = temporary.path().join("unrelated-chain.crt.pem");
    let unrelated_key = temporary.path().join("unrelated-chain.key.pem");
    let mut chain = REMOTE_TLS_LEAF_CERTIFICATE.to_vec();
    chain.extend_from_slice(REMOTE_TLS_ALTERNATE_ROOT_CERTIFICATE);
    std::fs::write(&unrelated_chain, chain).expect("write unrelated TLS chain");
    std::fs::write(&unrelated_key, REMOTE_TLS_PRIVATE_KEY).expect("write TLS private key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&unrelated_key, std::fs::Permissions::from_mode(0o600))
            .expect("restrict TLS private key fixture");
    }
    let unrelated = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:0".parse().unwrap()),
        Some(unrelated_chain),
        Some(unrelated_key),
    )
    .unwrap()
    .unwrap();
    let unrelated_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-unrelated-chain"),
        AUTH_TOKEN,
        Some(&unrelated),
    )
    .await
    {
        Ok(_) => panic!("an unrelated terminal trust anchor must stop startup"),
        Err(error) => error,
    };
    assert!(
        unrelated_error
            .to_string()
            .contains("validate Remote Brain TLS certificate chain"),
        "unrelated trust anchor returned the wrong startup error: {unrelated_error}"
    );

    let valid_chain = rustls::pki_types::CertificateDer::pem_slice_iter(REMOTE_TLS_CERTIFICATE)
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("decode valid TLS chain");
    assert_eq!(valid_chain.len(), 2, "fixture must be leaf then root");
    let duplicated_leaf_chain = vec![valid_chain[0].clone(), valid_chain[0].clone()];
    let duplicated_leaf_error = validate_remote_brain_tls_identity_at(
        &duplicated_leaf_chain,
        "127.0.0.1:0".parse().unwrap(),
        rustls::pki_types::UnixTime::now(),
    )
    .expect_err("an end-entity leaf cannot also be its explicit trust anchor");
    assert!(
        duplicated_leaf_error
            .to_string()
            .contains("leaf and explicit trust anchor must be distinct certificates"),
        "duplicated leaf returned the wrong startup error: {duplicated_leaf_error}"
    );
    let not_yet_valid_at =
        rustls::pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_secs(0));
    let not_yet_valid_error = validate_remote_brain_tls_identity_at(
        &valid_chain,
        "127.0.0.1:0".parse().unwrap(),
        not_yet_valid_at,
    )
    .expect_err("a not-yet-valid TLS leaf must fail the startup validator");
    assert!(
        not_yet_valid_error
            .to_string()
            .contains("validate Remote Brain TLS certificate chain"),
        "not-yet-valid leaf returned the wrong startup error: {not_yet_valid_error}"
    );
    let expired_at =
        rustls::pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_hours(1_139_568));
    let expired_error = validate_remote_brain_tls_identity_at(
        &valid_chain,
        "127.0.0.1:0".parse().unwrap(),
        expired_at,
    )
    .expect_err("an expired TLS leaf must fail the startup validator");
    assert!(
        expired_error
            .to_string()
            .contains("validate Remote Brain TLS certificate chain"),
        "expired leaf returned the wrong startup error: {expired_error}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn remote_tls_startup_rejects_non_private_or_non_regular_key_handles() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temporary = tempfile::tempdir().expect("remote TLS private-key fixture");
    let certificate = temporary.path().join("remote.crt.pem");
    let private_key = temporary.path().join("remote.key.pem");
    std::fs::write(&certificate, REMOTE_TLS_CERTIFICATE).expect("write TLS certificate fixture");
    std::fs::write(&private_key, REMOTE_TLS_PRIVATE_KEY).expect("write TLS key fixture");
    std::fs::set_permissions(&private_key, std::fs::Permissions::from_mode(0o644))
        .expect("make TLS key fixture non-private");
    let exposed = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:0".parse().unwrap()),
        Some(certificate.clone()),
        Some(private_key.clone()),
    )
    .unwrap()
    .unwrap();
    let exposed_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-exposed-key"),
        AUTH_TOKEN,
        Some(&exposed),
    )
    .await
    {
        Ok(_) => panic!("non-private TLS key must stop startup"),
        Err(error) => error,
    };
    assert!(exposed_error.to_string().contains("TLS private key"));

    std::fs::set_permissions(&private_key, std::fs::Permissions::from_mode(0o600))
        .expect("restore private TLS key fixture");
    let key_link = temporary.path().join("remote-link.key.pem");
    symlink(&private_key, &key_link).expect("symlink TLS key fixture");
    let linked = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:0".parse().unwrap()),
        Some(certificate.clone()),
        Some(key_link),
    )
    .unwrap()
    .unwrap();
    let linked_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-linked-key"),
        AUTH_TOKEN,
        Some(&linked),
    )
    .await
    {
        Ok(_) => panic!("symlink TLS key must stop startup"),
        Err(error) => error,
    };
    assert!(linked_error.to_string().contains("TLS private key"));

    let directory = temporary.path().join("remote-key-directory");
    std::fs::create_dir(&directory).expect("TLS key directory fixture");
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o600))
        .expect("restrict TLS key directory fixture");
    let directory_key = tracedecay_daemon_control::RemoteBrainTlsConfig::from_optional_parts(
        Some("127.0.0.1:0".parse().unwrap()),
        Some(certificate),
        Some(directory),
    )
    .unwrap()
    .unwrap();
    let directory_error = match DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-directory-key"),
        AUTH_TOKEN,
        Some(&directory_key),
    )
    .await
    {
        Ok(_) => panic!("directory TLS key must stop startup"),
        Err(error) => error,
    };
    assert!(directory_error.to_string().contains("TLS private key"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_tls_listener_serves_only_remote_routes_and_isolates_credential_authorities() {
    let temporary = tempfile::tempdir().expect("remote TLS fixture");
    let profile_root = temporary.path().join("profile");
    #[cfg(unix)]
    let endpoint =
        tracedecay_daemon_protocol::DaemonEndpoint::Unix(profile_root.join("remote-tls.sock"));
    #[cfg(not(unix))]
    let endpoint = tracedecay_daemon_protocol::default_loopback_endpoint();
    let daemon_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &endpoint,
        "test",
    )
    .expect("daemon authority");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote TLS authority isolation",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let runtime = Arc::new(
        tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("session runtime registry"),
    );
    let credential = *b"0123456789abcdef0123456789abcdef";
    let node_id = BrainNodeId::new("node.remote-tls").expect("node identity");
    let grant = live_remote_grant(identity.brain_id().clone(), node_id, &credential);
    runtime
        .provision_remote_node(grant.clone(), live_remote_admission(&grant))
        .await
        .expect("provision first TLS authority enrollment");

    let first_credentials = runtime.remote_credential_authority();
    let first_router = tracedecay_daemon_service::build_daemon_remote_protocol_router(
        Arc::clone(&first_credentials),
        runtime.remote_replay_transaction(),
        DaemonInvocationService::default(),
    )
    .expect("first remote protocol router");
    let first_registry = DaemonHttpApplicationRegistry::default();
    first_registry
        .install_remote(first_router, first_credentials, Some(Arc::clone(&runtime)))
        .expect("install first remote protocol router");
    let (first_tls, certificate) = remote_tls_fixture(&temporary);
    let first = DaemonHttpApplicationService::bind_with_remote_tls(
        first_registry,
        AUTH_TOKEN,
        Some(&first_tls),
    )
    .await
    .expect("bind first Remote Brain TLS service");

    let second_registry = unprovisioned_remote_registry("remote-tls-other");
    let (second_tls, _) = remote_tls_fixture(&temporary);
    let second = DaemonHttpApplicationService::bind_with_remote_tls(
        second_registry,
        AUTH_TOKEN,
        Some(&second_tls),
    )
    .await
    .expect("bind second Remote Brain TLS service");

    let first_endpoint = first.remote_tls_endpoint().expect("first TLS endpoint");
    let mut plaintext = tokio::net::TcpStream::connect(first_endpoint)
        .await
        .expect("connect plaintext probe");
    plaintext
        .write_all(b"GET /remote/query HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write plaintext probe");
    let mut plaintext_response = Vec::new();
    let _closed = plaintext.read_to_end(&mut plaintext_response).await;
    assert!(
        !plaintext_response.starts_with(b"HTTP/1.1"),
        "the external listener must never serve plaintext HTTP"
    );

    let admitted = remote_tls_request(
        first_endpoint,
        &certificate,
        "/remote/enrollment",
        Some(&credential),
        Some(b"abcdef0123456789abcdef0123456789"),
        b"{",
    )
    .await;
    assert!(admitted.starts_with("HTTP/1.1 400"), "{admitted}");

    let isolated = remote_tls_request(
        second.remote_tls_endpoint().expect("second TLS endpoint"),
        &certificate,
        "/remote/enrollment",
        Some(&credential),
        Some(b"abcdef0123456789abcdef0123456789"),
        b"{",
    )
    .await;
    assert!(isolated.starts_with("HTTP/1.1 404"), "{isolated}");

    let local_route = remote_tls_request(
        first_endpoint,
        &certificate,
        "/projects/project.remote-tls/application/tests/results",
        Some(AUTH_TOKEN.as_bytes()),
        None,
        b"{}",
    )
    .await;
    assert!(local_route.starts_with("HTTP/1.1 404"), "{local_route}");

    first.shutdown().await.expect("shutdown first TLS service");
    second
        .shutdown()
        .await
        .expect("shutdown second TLS service");
}

#[tokio::test]
async fn remote_tls_listener_bounds_connections_and_expires_incomplete_headers() {
    let temporary = tempfile::tempdir().expect("remote TLS admission fixture");
    let profile_root = temporary.path().join("admission-profile");
    #[cfg(unix)]
    let daemon_endpoint =
        tracedecay_daemon_protocol::DaemonEndpoint::Unix(profile_root.join("remote-tls.sock"));
    #[cfg(not(unix))]
    let daemon_endpoint = tracedecay_daemon_protocol::default_loopback_endpoint();
    let daemon_authority = tracedecay_daemon_identity::authority::DaemonAuthority::acquire(
        &profile_root,
        &daemon_endpoint,
        "test",
    )
    .expect("daemon authority");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote TLS admission timeout",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let runtime = Arc::new(
        tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity.clone())
            .await
            .expect("session runtime registry"),
    );
    let credential = *b"fedcba9876543210fedcba9876543210";
    let grant = live_remote_grant(
        identity.brain_id().clone(),
        BrainNodeId::new("node.remote-tls-admission").expect("node identity"),
        &credential,
    );
    runtime
        .provision_remote_node(grant.clone(), live_remote_admission(&grant))
        .await
        .expect("provision TLS admission credential");
    let credentials = runtime.remote_credential_authority();
    let router = tracedecay_daemon_service::build_daemon_remote_protocol_router(
        Arc::clone(&credentials),
        runtime.remote_replay_transaction(),
        DaemonInvocationService::default(),
    )
    .expect("remote protocol router");
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, Some(Arc::clone(&runtime)))
        .expect("install provisioned remote protocol router");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    // Scaled from production (5 s idle, 60 s absolute, 30 s response): a
    // progressing body outlives the response deadline, and a slowloris that
    // never idles still reaches the absolute deadline.
    let deadlines = RemoteBrainTlsDeadlines {
        read_idle: Duration::from_secs(1),
        request_read: Duration::from_secs(6),
        response: Duration::from_secs(2),
        ..RemoteBrainTlsDeadlines::PRODUCTION
    };
    let progress_step = deadlines.read_idle * 3 / 10;
    let service = DaemonHttpApplicationService::bind_with_remote_tls_deadlines(
        registry, AUTH_TOKEN, &tls, deadlines,
    )
    .await
    .expect("bind Remote Brain TLS admission service");
    let endpoint = service.remote_tls_endpoint().expect("TLS endpoint");
    // The listener counts which read deadline closed each connection, so the
    // test tells the idle deadline from the absolute one without timing a
    // listener that a loaded host may poll seconds late.
    let read_expirations = || {
        let ingress = service
            .remote_tls_ingress_snapshot()
            .expect("TLS ingress observer");
        (
            ingress.read_idle_expirations,
            ingress.request_read_expirations,
        )
    };

    let (idle, absolute) = read_expirations();
    let mut idle_connections = Vec::with_capacity(128);
    for _ in 0..128 {
        idle_connections.push(
            tokio::net::TcpStream::connect(endpoint)
                .await
                .expect("open bounded idle connection"),
        );
    }
    for connection in &mut idle_connections {
        let _closed = connection.read(&mut [0_u8; 1]).await;
    }
    assert_eq!(
        read_expirations(),
        (idle + 128, absolute),
        "idle handshakes must expire at the read-idle deadline"
    );
    drop(idle_connections);
    while service.remote_tls_available_admissions() != Some(128) {
        tokio::task::yield_now().await;
    }

    // Each request below leaves with the TLS handshake. Written after it, a
    // test thread stalled past the read-idle deadline would find the
    // connection closed as idle before its request arrived.
    let (idle, absolute) = read_expirations();
    let mut partial_headers = remote_tls_connect_sending(
        endpoint,
        &certificate,
        b"POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\n",
    )
    .await;
    let mut response = Vec::new();
    let _closed = partial_headers.read_to_end(&mut response).await;
    assert_eq!(response.len(), 0);
    assert_eq!(
        read_expirations(),
        (idle + 1, absolute),
        "the incomplete-header connection must be torn down at the read-idle deadline"
    );

    let (idle, absolute) = read_expirations();
    let partial_body_request = format!(
        "POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nx-tracedecay-enrollment-credential: {}\r\nContent-Type: application/json\r\nContent-Length: 32\r\n\r\n{{",
        String::from_utf8_lossy(&credential),
        "0123456789abcdef0123456789abcdef",
    )
    .into_bytes();
    let mut partial_body =
        remote_tls_connect_sending(endpoint, &certificate, &partial_body_request).await;
    let mut partial_body_response = Vec::new();
    let _closed = partial_body.read_to_end(&mut partial_body_response).await;
    assert_eq!(partial_body_response.len(), 0);
    assert_eq!(
        read_expirations(),
        (idle + 1, absolute),
        "the incomplete-body connection must be torn down at the read-idle deadline"
    );

    let progressing_body_headers = format!(
        "POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nx-tracedecay-enrollment-credential: {}\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n",
        String::from_utf8_lossy(&credential),
        "0123456789abcdef0123456789abcdef",
    )
    .into_bytes();
    let mut stalled_attempts = 0;
    let progressing_response = loop {
        let progressing_started = std::time::Instant::now();
        if let Trickle::Answered(response) = remote_tls_trickle_body(
            &service,
            endpoint,
            &certificate,
            &progressing_body_headers,
            b"         {}",
            deadlines,
            progress_step,
        )
        .await
        {
            assert!(
                progressing_started.elapsed() > deadlines.response,
                "the progressing body must outlive the response deadline"
            );
            break response;
        }
        stalled_attempts += 1;
        assert!(
            stalled_attempts < 3,
            "the test thread stalled past a listener deadline in every progressing-body attempt"
        );
    };
    assert!(
        progressing_response.starts_with(b"HTTP/1.1 400"),
        "progressing body must reach typed canonical protocol admission past the idle and response deadlines"
    );
    assert!(
        progressing_response
            .windows(b"\"kind\":\"problem\"".len())
            .any(|window| window == b"\"kind\":\"problem\""),
        "progressing body failure must retain the canonical typed problem wrapper"
    );
    // The full 32-byte body takes longer than the absolute deadline to
    // trickle in, one byte per progress step, so the idle deadline never
    // fires. Without the absolute deadline the body completes and the
    // listener answers it.
    let slowloris_headers = format!(
        "POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nx-tracedecay-enrollment-credential: {}\r\nContent-Type: application/json\r\nContent-Length: 32\r\n\r\n",
        String::from_utf8_lossy(&credential),
        "0123456789abcdef0123456789abcdef",
    )
    .into_bytes();
    let mut stalled_attempts = 0;
    loop {
        match remote_tls_trickle_body(
            &service,
            endpoint,
            &certificate,
            &slowloris_headers,
            &[b' '; 32],
            deadlines,
            progress_step,
        )
        .await
        {
            Trickle::ClosedAtRequestDeadline => break,
            Trickle::ClientStalled => {
                stalled_attempts += 1;
                assert!(
                    stalled_attempts < 3,
                    "the test thread stalled past the read-idle deadline in every slowloris attempt"
                );
            }
            Trickle::Answered(_) => {
                panic!("the absolute deadline must close the slowloris before its body completes")
            }
        }
    }
    while service.remote_tls_available_admissions() != Some(128) {
        tokio::task::yield_now().await;
    }

    // The response to a keep-alive-shaped request must still close the
    // connection, so reading to its end returns.
    let force_closed = remote_tls_exchange(
        endpoint,
        &certificate,
        b"GET /unknown HTTP/1.1\r\nHost: localhost\r\n\r\n",
        deadlines.read_idle,
    )
    .await;
    assert!(force_closed.starts_with("HTTP/1.1 404"), "{force_closed}");
    assert!(
        force_closed
            .to_ascii_lowercase()
            .contains("connection: close"),
        "{force_closed}"
    );

    remote_tls_h2_only_handshake_is_rejected(endpoint, &certificate).await;

    let mut http2 =
        remote_tls_connect_sending(endpoint, &certificate, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            .await;
    let mut http2_response = Vec::new();
    let _closed = http2.read_to_end(&mut http2_response).await;
    assert!(
        http2_response.is_empty(),
        "HTTP/2 prior knowledge must be torn down without a response"
    );
    while service.remote_tls_available_admissions() != Some(128) {
        tokio::task::yield_now().await;
    }

    service
        .shutdown()
        .await
        .expect("shutdown TLS admission service");
}

#[tokio::test]
async fn remote_tls_listener_does_not_apply_ingress_timeout_to_a_slow_handler() {
    let temporary = tempfile::tempdir().expect("remote TLS slow-handler fixture");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    let (registry, handler_started, handler_release) =
        delayed_response_remote_registry("remote-tls-slow-handler");
    let deadlines = RemoteBrainTlsDeadlines {
        read_idle: Duration::from_millis(200),
        request_read: Duration::from_millis(400),
        ..RemoteBrainTlsDeadlines::PRODUCTION
    };
    let service = DaemonHttpApplicationService::bind_with_remote_tls_deadlines(
        registry, AUTH_TOKEN, &tls, deadlines,
    )
    .await
    .expect("bind Remote Brain TLS slow-handler service");
    let mut peer = remote_tls_start_handler(
        service.remote_tls_endpoint().expect("TLS endpoint"),
        &certificate,
        b"GET /remote/slow HTTP/1.1\r\nHost: localhost\r\n\r\n",
        &handler_started,
        deadlines.read_idle,
    )
    .await;
    // The handler outlives both ingress deadlines several times over.
    tokio::time::sleep(deadlines.request_read * 3).await;
    handler_release.notify_one();
    let mut response = Vec::new();
    peer.read_to_end(&mut response)
        .await
        .expect("read slow-handler response");
    assert!(response.starts_with(b"HTTP/1.1 200"), "{response:?}");
    assert!(
        response
            .windows(b"complete".len())
            .any(|body| body == b"complete"),
        "the handler response must survive beyond the ingress idle deadline"
    );
    while service.remote_tls_available_admissions() != Some(128) {
        tokio::task::yield_now().await;
    }

    service
        .shutdown()
        .await
        .expect("shutdown TLS slow-handler service");
}

#[tokio::test]
async fn remote_tls_shutdown_bounds_a_fully_read_request_with_a_stalled_handler() {
    let temporary = tempfile::tempdir().expect("remote TLS stalled-handler fixture");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    let (registry, handler_started) =
        stalled_response_remote_registry("remote-tls-stalled-handler");
    let deadlines = RemoteBrainTlsDeadlines {
        shutdown_drain: Duration::from_secs(1),
        ..RemoteBrainTlsDeadlines::PRODUCTION
    };
    let service = DaemonHttpApplicationService::bind_with_remote_tls_deadlines(
        registry, AUTH_TOKEN, &tls, deadlines,
    )
    .await
    .expect("bind Remote Brain TLS stalled-handler service");
    let admission = service
        .remote_tls_admission()
        .expect("TLS admission authority");
    let mut peer = remote_tls_start_handler(
        service.remote_tls_endpoint().expect("TLS endpoint"),
        &certificate,
        b"GET /remote/stalled HTTP/1.1\r\nHost: localhost\r\n\r\n",
        &handler_started,
        deadlines.read_idle,
    )
    .await;
    assert_eq!(admission.available_permits(), 127);
    let shutdown_started = std::time::Instant::now();
    tokio::time::timeout(deadlines.shutdown_drain * 10, service.shutdown())
        .await
        .expect("stalled-handler shutdown must finish after its bounded drain window")
        .expect("shutdown stalled-handler TLS service");
    assert!(
        shutdown_started.elapsed() >= deadlines.shutdown_drain,
        "graceful shutdown should first offer active work its whole drain window"
    );

    let mut response = Vec::new();
    match &peer.read_to_end(&mut response).await {
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ) => {}
        Err(error) => panic!("forced TLS shutdown returned an unexpected error: {error}"),
    }
    assert!(
        response.is_empty(),
        "aborting a handler without a response must not fabricate HTTP success"
    );
    assert_eq!(
        admission.available_permits(),
        128,
        "bounded shutdown must join every connection and release its admission permit"
    );
}

#[tokio::test]
async fn remote_tls_shutdown_wins_while_saturated_admission_rejects_a_ready_backlog() {
    let temporary = tempfile::tempdir().expect("remote TLS saturated-shutdown fixture");
    let (tls, _certificate) = remote_tls_fixture(&temporary);
    // The admitted handshakes outlive the test, so only shutdown ends them.
    let deadlines = RemoteBrainTlsDeadlines {
        read_idle: Duration::from_mins(1),
        shutdown_drain: Duration::from_secs(1),
        ..RemoteBrainTlsDeadlines::PRODUCTION
    };
    let service = DaemonHttpApplicationService::bind_with_remote_tls_deadlines(
        unprovisioned_remote_registry("remote-tls-saturated-shutdown"),
        AUTH_TOKEN,
        &tls,
        deadlines,
    )
    .await
    .expect("bind Remote Brain TLS saturated-shutdown service");
    let endpoint = service.remote_tls_endpoint().expect("TLS endpoint");
    let admission = service
        .remote_tls_admission()
        .expect("TLS admission authority");

    let mut admitted = Vec::with_capacity(128);
    for _ in 0..128 {
        admitted.push(
            tokio::net::TcpStream::connect(endpoint)
                .await
                .expect("fill TLS admission with an idle connection"),
        );
    }
    while admission.available_permits() != 0 {
        tokio::task::yield_now().await;
    }

    let (rejected, mut rejected_connections) = tokio::sync::mpsc::unbounded_channel();
    let mut pressure = Vec::new();
    for _ in 0..16 {
        let rejected = rejected.clone();
        pressure.push(tokio::spawn(async move {
            while let Ok(mut stream) = tokio::net::TcpStream::connect(endpoint).await {
                let mut byte = [0_u8; 1];
                match stream.read(&mut byte).await {
                    Ok(0) | Err(_) => {
                        let _ = rejected.send(());
                    }
                    Ok(_) => panic!("a saturated plaintext probe received unexpected bytes"),
                }
            }
        }));
    }
    drop(rejected);
    for _ in 0..16 {
        rejected_connections
            .recv()
            .await
            .expect("saturation pressure task stopped before rejection");
    }

    // Ready accepts that starved shutdown would hold it forever, far past
    // this bound.
    tokio::time::timeout(deadlines.shutdown_drain * 10, service.shutdown())
        .await
        .expect("ready saturated accepts must not starve shutdown past its bounded drain window")
        .expect("shutdown saturated-shutdown service");
    for task in pressure {
        task.abort();
        let _ = task.await;
    }
    drop(admitted);
    assert_eq!(
        admission.available_permits(),
        128,
        "saturated shutdown must release every admitted connection"
    );
}

#[tokio::test]
async fn remote_tls_listener_expires_saturated_non_reading_responses() {
    let temporary = tempfile::tempdir().expect("remote TLS egress fixture");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    let (registry, handler_barrier) = large_response_remote_registry("remote-tls-egress");
    let service =
        DaemonHttpApplicationService::bind_with_remote_tls(registry, AUTH_TOKEN, Some(&tls))
            .await
            .expect("bind Remote Brain TLS egress service");
    let endpoint = service.remote_tls_endpoint().expect("TLS endpoint");

    // One peer at a time sends its request with its handshake, so no
    // connection waits out its read-idle deadline behind the other 127
    // handshakes on a loaded host.
    let mut non_reading_peers = Vec::with_capacity(128);
    for _ in 0..128 {
        non_reading_peers.push(
            remote_tls_connect_sending(
                endpoint,
                &certificate,
                b"GET /remote/egress HTTP/1.1\r\nHost: localhost\r\n\r\n",
            )
            .await,
        );
    }
    // Every connection holds its admission once its request is read, and none
    // can expire before egress starts at the handler barrier.
    while service
        .remote_tls_ingress_snapshot()
        .is_some_and(|ingress| ingress.headers_complete != 128)
    {
        tokio::task::yield_now().await;
    }
    assert_eq!(service.remote_tls_available_admissions(), Some(0));
    handler_barrier.wait().await;
    // A response stalled at backpressure expires once it has been idle for
    // its write-idle deadline. On a loaded host the first stalled responses
    // can expire before the last one fills its socket, so a response counts
    // once it is backpressured now or has already expired from its stall.
    loop {
        let snapshot = service
            .remote_tls_egress_snapshot()
            .expect("TLS egress observer");
        assert_eq!(snapshot.response_expirations, 0, "{snapshot:?}");
        assert_eq!(
            snapshot.idle_deadline_contract_violations, 0,
            "{snapshot:?}"
        );
        if snapshot.backpressured + snapshot.idle_expirations == 128 {
            break;
        }
        tokio::task::yield_now().await;
    }

    while service
        .remote_tls_egress_snapshot()
        .is_some_and(|snapshot| snapshot.idle_expirations != 128)
    {
        tokio::task::yield_now().await;
    }
    let expired = service
        .remote_tls_egress_snapshot()
        .expect("TLS egress observer after idle expiry");
    assert_eq!(expired.idle_expirations, 128);
    assert_eq!(expired.idle_deadline_contract_violations, 0);
    assert_eq!(expired.response_expirations, 0);
    while service.remote_tls_available_admissions() != Some(128)
        || service
            .remote_tls_egress_snapshot()
            .is_some_and(|snapshot| snapshot.active != 0 || snapshot.backpressured != 0)
    {
        tokio::task::yield_now().await;
    }
    let settled = service
        .remote_tls_egress_snapshot()
        .expect("TLS egress observer after permit recovery");
    assert_eq!(settled.active, 0);
    assert_eq!(settled.backpressured, 0);
    assert_eq!(settled.idle_expirations, 128);
    assert_eq!(settled.idle_deadline_contract_violations, 0);
    assert_eq!(settled.response_expirations, 0);
    drop(non_reading_peers);

    service
        .shutdown()
        .await
        .expect("shutdown TLS egress service");
}

/// The listener serves each connection on the native runtime, so a remote
/// handler runs with no Tokio runtime entered.
#[tokio::test]
async fn remote_tls_connections_are_served_off_tokio() {
    let temporary = tempfile::tempdir().expect("remote TLS runtime fixture");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    let credentials = Arc::new(
        tracedecay_store_runtime::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new("brain.remote-tls-runtime").expect("remote brain identity"),
            UserProfileId::new("profile.remote-tls-runtime").expect("remote profile identity"),
        ),
    );
    let router = Router::new().route(
        "/runtime",
        axum::routing::post(|| async {
            if tokio::runtime::Handle::try_current().is_err() {
                "native"
            } else {
                "tokio"
            }
        }),
    );
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, None)
        .expect("install runtime-probe remote router");
    let hazards = crate::test_support::native_runtime_hazards();
    let service =
        DaemonHttpApplicationService::bind_with_remote_tls(registry, AUTH_TOKEN, Some(&tls))
            .await
            .expect("bind Remote Brain TLS runtime-probe service");

    let response = remote_tls_request(
        service.remote_tls_endpoint().expect("TLS endpoint"),
        &certificate,
        "/remote/runtime",
        None,
        None,
        b"",
    )
    .await;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.ends_with("native"),
        "the remote handler must run off Tokio: {response}"
    );
    service
        .shutdown()
        .await
        .expect("shutdown TLS runtime-probe service");
    assert_eq!(crate::test_support::native_runtime_hazards(), hazards);
}

struct HandlerDropReceipt(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for HandlerDropReceipt {
    fn drop(&mut self) {
        if let Some(receipt) = self.0.take() {
            let _ = receipt.send(());
        }
    }
}

/// Dropping the service aborts its listener, and the listener's connections
/// with it, without waiting out the shutdown drain. The drain is long, so a
/// listener that waited it out stays far outside the bound however slowly the
/// host schedules it.
#[tokio::test]
async fn dropping_the_remote_tls_service_aborts_an_open_connection() {
    let temporary = tempfile::tempdir().expect("remote TLS dropped-service fixture");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    let credentials = Arc::new(
        tracedecay_store_runtime::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new("brain.remote-tls-dropped").expect("remote brain identity"),
            UserProfileId::new("profile.remote-tls-dropped").expect("remote profile identity"),
        ),
    );
    let (dropped, handler_dropped) = tokio::sync::oneshot::channel();
    let dropped = Arc::new(std::sync::Mutex::new(Some(dropped)));
    let started = Arc::new(tokio::sync::Notify::new());
    let handler_started = Arc::clone(&started);
    let router = Router::new().route(
        "/held",
        get(move || {
            let receipt = HandlerDropReceipt(dropped.lock().expect("drop receipt").take());
            let handler_started = Arc::clone(&handler_started);
            async move {
                let _receipt = receipt;
                handler_started.notify_one();
                std::future::pending::<&'static str>().await
            }
        }),
    );
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, None)
        .expect("install held-response remote router");
    let deadlines = RemoteBrainTlsDeadlines {
        shutdown_drain: Duration::from_mins(1),
        ..RemoteBrainTlsDeadlines::PRODUCTION
    };
    let service = DaemonHttpApplicationService::bind_with_remote_tls_deadlines(
        registry, AUTH_TOKEN, &tls, deadlines,
    )
    .await
    .expect("bind Remote Brain TLS dropped-service service");
    let _peer = remote_tls_start_handler(
        service.remote_tls_endpoint().expect("TLS endpoint"),
        &certificate,
        b"GET /remote/held HTTP/1.1\r\nHost: localhost\r\n\r\n",
        &started,
        deadlines.read_idle,
    )
    .await;

    drop(service);

    tokio::time::timeout(deadlines.shutdown_drain / 2, handler_dropped)
        .await
        .expect("dropping the service must abort its open connection")
        .expect("handler drop receipt");
}

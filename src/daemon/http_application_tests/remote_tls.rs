use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use rustls::pki_types::pem::PemObject;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracedecay_api::remote::REMOTE_ENROLLMENT_CREDENTIAL_HEADER;
use tracedecay_application::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_application::{
    AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass, PolicyDecisionRef,
    ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, EnrollmentGrantV1, EntityId, ManifestDigest,
    ProjectId, RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1,
    RepositoryId, RepositoryStateSnapshotId, UserProfileId, UtcMicros, WorktreeId,
    canonical_sha256,
};

use super::{AUTH_TOKEN, current_micros};
use crate::daemon::http_application::{
    DaemonHttpApplicationRegistry, DaemonHttpApplicationService,
    validate_remote_brain_tls_identity_at,
};

const REMOTE_TLS_CERTIFICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/remote_tls/localhost.crt.pem");
const REMOTE_TLS_LEAF_CERTIFICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/remote_tls/localhost-leaf.crt.pem");
const REMOTE_TLS_PRIVATE_KEY: &[u8] =
    include_bytes!("../../../tests/fixtures/remote_tls/localhost.key.pem");
const REMOTE_TLS_CA_TRUE_CERTIFICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/remote_tls/ca-true.crt.pem");
const REMOTE_TLS_CLIENT_AUTH_CERTIFICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/remote_tls/client-auth-only.crt.pem");
const REMOTE_TLS_WRONG_IP_CERTIFICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/remote_tls/wrong-ip.crt.pem");
const REMOTE_TLS_ALTERNATE_ROOT_CERTIFICATE: &[u8] =
    include_bytes!("../../../tests/fixtures/remote_tls/alternate-root.crt.pem");

fn remote_tls_fixture(
    temporary: &tempfile::TempDir,
) -> (crate::daemon::RemoteBrainTlsConfig, PathBuf) {
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
    let config = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
        crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new(format!("brain.{identity}")).expect("brain identity"),
            UserProfileId::new(format!("profile.{identity}")).expect("profile identity"),
        ),
    );
    let transaction = Arc::new(
        crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1::new(
            tokio::runtime::Handle::current(),
        )
        .expect("remote replay transaction authority"),
    );
    let router = crate::daemon::remote_protocol::build_daemon_remote_protocol_router(
        Arc::clone(&credentials),
        transaction,
        crate::daemon::service::invocation::DaemonInvocationService::default(),
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
        crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1::new(
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

fn delayed_response_remote_registry(
    identity: &str,
) -> (DaemonHttpApplicationRegistry, Arc<tokio::sync::Notify>) {
    let credentials = Arc::new(
        crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new(format!("brain.{identity}")).expect("remote brain identity"),
            UserProfileId::new(format!("profile.{identity}")).expect("remote profile identity"),
        ),
    );
    let started = Arc::new(tokio::sync::Notify::new());
    let handler_started = Arc::clone(&started);
    let router = Router::new().route(
        "/slow",
        get(move || {
            let handler_started = Arc::clone(&handler_started);
            async move {
                handler_started.notify_one();
                tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                "complete"
            }
        }),
    );
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, None)
        .expect("install delayed-response remote router");
    (registry, started)
}

fn stalled_response_remote_registry(
    identity: &str,
) -> (DaemonHttpApplicationRegistry, Arc<tokio::sync::Notify>) {
    let credentials = Arc::new(
        crate::daemon::remote_protocol::DaemonRemoteCredentialAuthorityV1::new(
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

async fn remote_tls_request(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
    path: &str,
    authorization: Option<&[u8]>,
    enrollment_credential: Option<&[u8]>,
    body: &[u8],
) -> String {
    let mut stream = remote_tls_connect(endpoint, certificate).await;
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
    stream.write_all(&request).await.expect("write TLS request");
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stream.read_to_end(&mut response),
    )
    .await
    .expect("TLS response must close within the bounded response window")
    .expect("read TLS response");
    String::from_utf8(response).expect("HTTP response text")
}

async fn remote_tls_request_without_connection_close(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
) -> String {
    let mut stream = remote_tls_connect(endpoint, certificate).await;
    stream
        .write_all(b"GET /unknown HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write keep-alive-shaped TLS request");
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        stream.read_to_end(&mut response),
    )
    .await
    .expect("external response must close the TLS connection")
    .expect("read force-closed TLS response");
    String::from_utf8(response).expect("HTTP response text")
}

async fn remote_tls_connect(
    endpoint: std::net::SocketAddr,
    certificate: &Path,
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
    let mut client = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let stream = tokio::net::TcpStream::connect(endpoint)
        .await
        .expect("connect Remote Brain TLS listener");
    let server_name = rustls::pki_types::ServerName::try_from("localhost")
        .expect("TLS server name")
        .to_owned();
    let stream = connector
        .connect(server_name, stream)
        .await
        .expect("authenticate Remote Brain TLS authority");
    assert_eq!(
        stream.get_ref().1.alpn_protocol(),
        Some(b"http/1.1".as_slice()),
        "Remote Brain TLS must negotiate only HTTP/1.1"
    );
    stream
}

async fn remote_tls_write_and_flush_with_wall_timeout(
    stream: &mut tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    bytes: &[u8],
    context: &str,
) {
    let completed = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let watchdog_completed = Arc::clone(&completed);
    let mut watchdog = tokio::task::spawn_blocking(move || {
        let (lock, condition) = &*watchdog_completed;
        let completed = lock.lock().expect("lock real-wall write watchdog");
        let (_completed, wait) = condition
            .wait_timeout_while(completed, std::time::Duration::from_secs(1), |completed| {
                !*completed
            })
            .expect("wait for paused TLS write completion");
        wait.timed_out()
    });
    let operation = async {
        stream.write_all(bytes).await?;
        stream.flush().await
    };
    tokio::pin!(operation);
    let outcome = tokio::select! {
        outcome = &mut operation => outcome,
        timed_out = &mut watchdog => {
            assert!(
                timed_out.expect("join real-wall write watchdog"),
                "real-wall write watchdog stopped before {context} completed"
            );
            panic!("{context} exceeded the one-second real-wall bound");
        }
    };
    let (lock, condition) = &*completed;
    *lock.lock().expect("lock completed TLS write watchdog") = true;
    condition.notify_one();
    assert!(
        !watchdog.await.expect("join real-wall write watchdog"),
        "{context} completed after the one-second real-wall bound"
    );
    outcome.unwrap_or_else(|error| panic!("{context}: {error}"));
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
    let mut client = rustls::ClientConfig::builder()
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
    assert!(
        crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
            Some("127.0.0.1:7443".parse().unwrap()),
            Some(path.clone()),
            None,
        )
        .is_err()
    );
    assert!(
        crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
            Some("0.0.0.0:7443".parse().unwrap()),
            Some(path.clone()),
            Some(path),
        )
        .is_err()
    );
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
    let invalid = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
    let invalid_key_config = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
    let occupied_config = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
    let leaf_without_anchor_config = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
        let config = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
    let unrelated = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
    let expired_at = rustls::pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_secs(
        4_102_444_800,
    ));
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
    let exposed = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
    let linked = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
    let directory_key = crate::daemon::RemoteBrainTlsConfig::from_optional_parts(
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
        crate::daemon::transport::DaemonEndpoint::Unix(profile_root.join("remote-tls.sock"));
    #[cfg(not(unix))]
    let endpoint = crate::daemon::transport::default_loopback_endpoint();
    let daemon_authority =
        crate::daemon::authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
            .expect("daemon authority");
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote TLS authority isolation",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let runtime = Arc::new(
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity.clone(),
        )
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
    let first_router = crate::daemon::remote_protocol::build_daemon_remote_protocol_router(
        Arc::clone(&first_credentials),
        runtime.remote_replay_transaction(),
        crate::daemon::service::invocation::DaemonInvocationService::default(),
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
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        plaintext.read_to_end(&mut plaintext_response),
    )
    .await;
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
        crate::daemon::transport::DaemonEndpoint::Unix(profile_root.join("remote-tls.sock"));
    #[cfg(not(unix))]
    let daemon_endpoint = crate::daemon::transport::default_loopback_endpoint();
    let daemon_authority =
        crate::daemon::authority::DaemonAuthority::acquire(&profile_root, &daemon_endpoint, "test")
            .expect("daemon authority");
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        daemon_authority.record().epoch,
        "remote TLS admission timeout",
    )
    .expect("daemon database scope");
    let identity = daemon_authority.profile_identity().clone();
    let runtime = Arc::new(
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity.clone(),
        )
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
    let router = crate::daemon::remote_protocol::build_daemon_remote_protocol_router(
        Arc::clone(&credentials),
        runtime.remote_replay_transaction(),
        crate::daemon::service::invocation::DaemonInvocationService::default(),
    )
    .expect("remote protocol router");
    let registry = DaemonHttpApplicationRegistry::default();
    registry
        .install_remote(router, credentials, Some(Arc::clone(&runtime)))
        .expect("install provisioned remote protocol router");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    let service =
        DaemonHttpApplicationService::bind_with_remote_tls(registry, AUTH_TOKEN, Some(&tls))
            .await
            .expect("bind Remote Brain TLS admission service");
    let endpoint = service.remote_tls_endpoint().expect("TLS endpoint");

    let mut idle_connections = Vec::with_capacity(128);
    for _ in 0..128 {
        idle_connections.push(
            tokio::net::TcpStream::connect(endpoint)
                .await
                .expect("open bounded idle connection"),
        );
    }
    for _ in 0..128 {
        if service.remote_tls_available_admissions() == Some(0) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(service.remote_tls_available_admissions(), Some(0));

    tokio::time::pause();
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    for _ in 0..128 {
        if service.remote_tls_available_admissions() == Some(128) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(service.remote_tls_available_admissions(), Some(128));
    drop(idle_connections);
    tokio::time::resume();

    let mut partial_headers = remote_tls_connect(endpoint, &certificate).await;
    partial_headers
        .write_all(b"POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .expect("write incomplete HTTP headers");
    partial_headers
        .flush()
        .await
        .expect("flush incomplete HTTP headers");
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let mut response = Vec::new();
    let _closed = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        partial_headers.read_to_end(&mut response),
    )
    .await
    .expect("the incomplete-header connection must be torn down");
    assert!(response.is_empty());

    let mut partial_body = remote_tls_connect(endpoint, &certificate).await;
    let partial_body_headers = format!(
        "POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nx-tracedecay-enrollment-credential: {}\r\nContent-Type: application/json\r\nContent-Length: 32\r\n\r\n",
        String::from_utf8_lossy(&credential),
        "0123456789abcdef0123456789abcdef",
    )
    .into_bytes();
    partial_body
        .write_all(&partial_body_headers)
        .await
        .expect("write complete HTTP headers");
    partial_body
        .write_all(b"{")
        .await
        .expect("write separate incomplete HTTP body chunk");
    partial_body
        .flush()
        .await
        .expect("flush separate incomplete HTTP body chunk");
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let mut partial_body_response = Vec::new();
    let _closed = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        partial_body.read_to_end(&mut partial_body_response),
    )
    .await
    .expect("the incomplete-body connection must be torn down");
    assert!(partial_body_response.is_empty());

    let mut progressing_body = remote_tls_connect(endpoint, &certificate).await;
    let progressing_body_headers = format!(
        "POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nx-tracedecay-enrollment-credential: {}\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n",
        String::from_utf8_lossy(&credential),
        "0123456789abcdef0123456789abcdef",
    )
    .into_bytes();
    let initial_ingress = service
        .remote_tls_ingress_snapshot()
        .expect("TLS ingress observer");
    progressing_body
        .write_all(&progressing_body_headers)
        .await
        .expect("write progressing HTTP body headers");
    progressing_body
        .flush()
        .await
        .expect("flush progressing HTTP body headers");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let observed = service
                .remote_tls_ingress_snapshot()
                .expect("TLS ingress observer")
                .headers_complete;
            assert!(
                observed <= initial_ingress.headers_complete + 1,
                "TLS ingress observed unexpected request headers"
            );
            if observed == initial_ingress.headers_complete + 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("progressing headers must reach the real TLS reader");
    let initial_body_bytes = service
        .remote_tls_ingress_snapshot()
        .expect("TLS ingress observer")
        .body_bytes_observed;
    tokio::time::pause();
    for (offset, byte) in b"         {}".iter().enumerate() {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        tokio::time::resume();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            progressing_body
                .write_all(std::slice::from_ref(byte))
                .await
                .unwrap_or_else(|error| {
                    panic!("write progressing HTTP body byte {offset}: {error}")
                });
            progressing_body.flush().await.unwrap_or_else(|error| {
                panic!("flush progressing HTTP body byte {offset}: {error}")
            });
            loop {
                let observed = service
                    .remote_tls_ingress_snapshot()
                    .expect("TLS ingress observer")
                    .body_bytes_observed;
                let expected = initial_body_bytes + offset + 1;
                assert!(
                    observed <= expected,
                    "TLS ingress consumed bytes out of order"
                );
                if observed == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("progressing body byte {offset} must reach TLS ingress"));
        tokio::time::pause();
    }
    tokio::time::resume();
    let mut progressing_response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        progressing_body.read_to_end(&mut progressing_response),
    )
    .await
    .expect("progressing response must close within the bounded response window")
    .expect("read progressing HTTP body response");
    assert!(
        progressing_response.starts_with(b"HTTP/1.1 400"),
        "progressing body must reach typed canonical protocol admission after more than 30 seconds"
    );
    assert!(
        progressing_response
            .windows(b"\"kind\":\"problem\"".len())
            .any(|window| window == b"\"kind\":\"problem\""),
        "progressing body failure must retain the canonical typed problem wrapper"
    );
    let (mut absolute_slowloris, initial_absolute_body_bytes) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        async {
            let initial_absolute_ingress = service
                .remote_tls_ingress_snapshot()
                .expect("TLS ingress observer");
            let mut stream = remote_tls_connect(endpoint, &certificate).await;
            let headers = format!(
                "POST /remote/enrollment HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nx-tracedecay-enrollment-credential: {}\r\nContent-Type: application/json\r\nContent-Length: 32\r\n\r\n",
                String::from_utf8_lossy(&credential),
                "0123456789abcdef0123456789abcdef",
            )
            .into_bytes();
            stream
                .write_all(&headers)
                .await
                .expect("write absolute slowloris HTTP body headers");
            stream
                .flush()
                .await
                .expect("flush absolute slowloris HTTP body headers");
            loop {
                let observed = service
                    .remote_tls_ingress_snapshot()
                    .expect("TLS ingress observer")
                    .headers_complete;
                assert!(
                    observed <= initial_absolute_ingress.headers_complete + 1,
                    "TLS ingress observed unexpected absolute slowloris request headers"
                );
                if observed == initial_absolute_ingress.headers_complete + 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let body_bytes = service
                .remote_tls_ingress_snapshot()
                .expect("TLS ingress observer")
                .body_bytes_observed;
            (stream, body_bytes)
        }
    )
    .await
    .expect("absolute slowloris setup must reach the real TLS reader within one second");
    let (inhibitor_ready_tx, inhibitor_ready_rx) = std::sync::mpsc::sync_channel(0);
    let (inhibitor_release_tx, inhibitor_release_rx) = std::sync::mpsc::sync_channel(0);
    let auto_advance_inhibitor = tokio::task::spawn_blocking(move || {
        inhibitor_ready_tx
            .send(())
            .expect("announce absolute slowloris auto-advance inhibitor");
        inhibitor_release_rx
            .recv()
            .expect("release absolute slowloris auto-advance inhibitor");
    });
    inhibitor_ready_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("absolute slowloris auto-advance inhibitor must start");
    tokio::time::pause();
    let mut slowloris_bytes = b"              {".iter();
    for (offset, byte) in slowloris_bytes.by_ref().take(14).enumerate() {
        tokio::time::advance(std::time::Duration::from_secs(4)).await;
        remote_tls_write_and_flush_with_wall_timeout(
            &mut absolute_slowloris,
            std::slice::from_ref(byte),
            &format!("write absolute slowloris body byte {offset} before deadline"),
        )
        .await;
        let observation_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let observed = service
                .remote_tls_ingress_snapshot()
                .expect("TLS ingress observer")
                .body_bytes_observed;
            let expected = initial_absolute_body_bytes + offset + 1;
            assert!(
                observed <= expected,
                "TLS ingress consumed absolute slowloris body bytes out of order"
            );
            if observed == expected {
                break;
            }
            assert!(
                std::time::Instant::now() < observation_deadline,
                "absolute slowloris body byte {offset} must reach TLS ingress"
            );
            tokio::task::yield_now().await;
        }
    }
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    let final_offset = 14;
    remote_tls_write_and_flush_with_wall_timeout(
        &mut absolute_slowloris,
        std::slice::from_ref(
            slowloris_bytes
                .next()
                .expect("slowloris fixture extends past the absolute deadline probe"),
        ),
        "write absolute slowloris body byte at 58 seconds",
    )
    .await;
    let observation_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let observed = service
            .remote_tls_ingress_snapshot()
            .expect("TLS ingress observer")
            .body_bytes_observed;
        let expected = initial_absolute_body_bytes + final_offset + 1;
        assert!(
            observed <= expected,
            "TLS ingress consumed absolute slowloris body bytes out of order"
        );
        if observed == expected {
            break;
        }
        assert!(
            std::time::Instant::now() < observation_deadline,
            "absolute slowloris body byte {final_offset} must reach TLS ingress at 58 seconds"
        );
        tokio::task::yield_now().await;
    }
    tokio::time::advance(std::time::Duration::from_secs(3)).await;
    let absolute_slowloris_reader = tokio::task::spawn(async move {
        let mut response = Vec::new();
        let closed = absolute_slowloris.read_to_end(&mut response).await;
        (closed, response)
    });
    let close_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while !absolute_slowloris_reader.is_finished() {
        assert!(
            std::time::Instant::now() < close_deadline,
            "a progressing slowloris must hit the absolute admission deadline"
        );
        tokio::task::yield_now().await;
    }
    let (_closed, absolute_slowloris_response) = absolute_slowloris_reader
        .await
        .expect("join absolute slowloris response reader");
    assert!(absolute_slowloris_response.is_empty());
    let reset_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while service.remote_tls_available_admissions() != Some(128) {
        assert!(
            std::time::Instant::now() < reset_deadline,
            "absolute slowloris admission must be released before the reset idle deadline"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(service.remote_tls_available_admissions(), Some(128));

    inhibitor_release_tx
        .send(())
        .expect("release absolute slowloris auto-advance inhibitor");
    auto_advance_inhibitor
        .await
        .expect("join absolute slowloris auto-advance inhibitor");
    tokio::time::resume();
    let force_closed = remote_tls_request_without_connection_close(endpoint, &certificate).await;
    assert!(force_closed.starts_with("HTTP/1.1 404"), "{force_closed}");
    assert!(
        force_closed
            .to_ascii_lowercase()
            .contains("connection: close"),
        "{force_closed}"
    );

    remote_tls_h2_only_handshake_is_rejected(endpoint, &certificate).await;

    let mut http2 = remote_tls_connect(endpoint, &certificate).await;
    http2
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .await
        .expect("write HTTP/2 prior-knowledge preface");
    let mut http2_response = Vec::new();
    let _closed = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        http2.read_to_end(&mut http2_response),
    )
    .await
    .expect("HTTP/2 connection must be torn down");
    assert!(http2_response.is_empty());
    for _ in 0..128 {
        if service.remote_tls_available_admissions() == Some(128) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(service.remote_tls_available_admissions(), Some(128));

    service
        .shutdown()
        .await
        .expect("shutdown TLS admission service");
}

#[tokio::test]
async fn remote_tls_listener_does_not_apply_ingress_timeout_to_a_slow_handler() {
    let temporary = tempfile::tempdir().expect("remote TLS slow-handler fixture");
    let (tls, certificate) = remote_tls_fixture(&temporary);
    let (registry, handler_started) = delayed_response_remote_registry("remote-tls-slow-handler");
    let service =
        DaemonHttpApplicationService::bind_with_remote_tls(registry, AUTH_TOKEN, Some(&tls))
            .await
            .expect("bind Remote Brain TLS slow-handler service");
    let endpoint = service.remote_tls_endpoint().expect("TLS endpoint");
    let mut peer = remote_tls_connect(endpoint, &certificate).await;

    peer.write_all(b"GET /remote/slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write complete slow-handler request");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        handler_started.notified(),
    )
    .await
    .expect("slow handler must start before the test clock is paused");
    tokio::time::pause();
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    tokio::task::yield_now().await;
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        peer.read_to_end(&mut response),
    )
    .await
    .expect("slow-handler response must close within the bounded response window")
    .expect("read slow-handler response");
    assert!(response.starts_with(b"HTTP/1.1 200"), "{response:?}");
    assert!(
        response
            .windows(b"complete".len())
            .any(|body| body == b"complete"),
        "the handler response must survive beyond the ingress idle deadline"
    );
    assert_eq!(service.remote_tls_available_admissions(), Some(128));
    tokio::time::resume();

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
    let service =
        DaemonHttpApplicationService::bind_with_remote_tls(registry, AUTH_TOKEN, Some(&tls))
            .await
            .expect("bind Remote Brain TLS stalled-handler service");
    let endpoint = service.remote_tls_endpoint().expect("TLS endpoint");
    let admission = service
        .remote_tls_admission()
        .expect("TLS admission authority");
    let mut peer = remote_tls_connect(endpoint, &certificate).await;

    peer.write_all(b"GET /remote/stalled HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write complete stalled-handler request");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        handler_started.notified(),
    )
    .await
    .expect("stalled handler must start before the test clock is paused");
    assert_eq!(admission.available_permits(), 127);
    tokio::time::pause();
    let shutdown = tokio::spawn(service.shutdown());
    tokio::task::yield_now().await;
    assert!(
        !shutdown.is_finished(),
        "graceful shutdown should first offer active work its drain window"
    );
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    assert!(
        shutdown.is_finished(),
        "stalled-handler shutdown must finish within its bounded drain window"
    );
    shutdown
        .await
        .expect("join bounded stalled-handler shutdown")
        .expect("shutdown stalled-handler TLS service");
    tokio::time::resume();

    let mut response = Vec::new();
    let closure = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        peer.read_to_end(&mut response),
    )
    .await
    .expect("stalled-handler connection must close after bounded shutdown");
    match &closure {
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
    let service = DaemonHttpApplicationService::bind_with_remote_tls(
        unprovisioned_remote_registry("remote-tls-saturated-shutdown"),
        AUTH_TOKEN,
        Some(&tls),
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
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while admission.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fill every TLS admission permit");
    assert_eq!(admission.available_permits(), 0);

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
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        for _ in 0..16 {
            rejected_connections
                .recv()
                .await
                .expect("saturation pressure task stopped before rejection");
        }
    })
    .await
    .expect("listener must reject a replenished saturated backlog");

    tokio::time::pause();
    let shutdown = tokio::spawn(service.shutdown());
    for _ in 0..12 {
        if shutdown.is_finished() {
            break;
        }
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    assert!(
        shutdown.is_finished(),
        "saturated shutdown must finish within its bounded drain window"
    );
    tokio::time::resume();
    tokio::time::timeout(std::time::Duration::from_secs(1), shutdown)
        .await
        .expect("ready saturated accepts must not starve shutdown")
        .expect("join saturated-shutdown service")
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

    let mut peer_tasks = Vec::with_capacity(128);
    for _ in 0..128 {
        let certificate = certificate.clone();
        peer_tasks.push(tokio::spawn(async move {
            let mut peer = remote_tls_connect(endpoint, &certificate).await;
            peer.write_all(b"GET /remote/egress HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .expect("request large TLS response");
            peer.flush().await.expect("flush large TLS request");
            peer
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), handler_barrier.wait())
        .await
        .expect("every large-response handler must reach the egress barrier");
    let mut non_reading_peers = Vec::with_capacity(128);
    for task in peer_tasks {
        non_reading_peers.push(task.await.expect("join non-reading TLS peer"));
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let snapshot = service
                .remote_tls_egress_snapshot()
                .expect("TLS egress observer");
            if snapshot.active == 128
                && snapshot.backpressured == 128
                && snapshot.idle_expirations == 0
                && snapshot.idle_deadline_contract_violations == 0
                && snapshot.response_expirations == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("every large response must reach real TLS backpressure");
    assert_eq!(service.remote_tls_available_admissions(), Some(0));

    tokio::time::pause();
    for _ in 0..4 {
        if service
            .remote_tls_egress_snapshot()
            .is_some_and(|snapshot| snapshot.idle_expirations == 128)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    }
    let expired = service
        .remote_tls_egress_snapshot()
        .expect("TLS egress observer after idle expiry");
    assert_eq!(expired.idle_expirations, 128);
    assert_eq!(expired.idle_deadline_contract_violations, 0);
    assert_eq!(expired.response_expirations, 0);
    tokio::time::resume();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while service.remote_tls_available_admissions() != Some(128)
            || service
                .remote_tls_egress_snapshot()
                .is_some_and(|snapshot| snapshot.active != 0 || snapshot.backpressured != 0)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "egress expiry did not settle: snapshot={:?}, permits={:?}",
            service.remote_tls_egress_snapshot(),
            service.remote_tls_available_admissions(),
        )
    });
    assert_eq!(service.remote_tls_available_admissions(), Some(128));
    let settled = service
        .remote_tls_egress_snapshot()
        .expect("TLS egress observer after permit recovery");
    assert_eq!(settled.active, 0);
    assert_eq!(settled.backpressured, 0);
    assert_eq!(settled.idle_expirations, 128);
    assert_eq!(settled.idle_deadline_contract_violations, 0);
    assert_eq!(settled.response_expirations, 0);
    drop(non_reading_peers);

    tokio::time::timeout(std::time::Duration::from_secs(1), service.shutdown())
        .await
        .expect("egress saturation must not delay shutdown")
        .expect("shutdown TLS egress service");
}

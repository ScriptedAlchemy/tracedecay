use std::io::BufReader;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls;

use super::remote_https::{
    RemoteBrainHttpsConfigV1, RemoteBrainHttpsEnablementV1, RemoteBrainHttpsError,
    RemoteBrainHttpsService, RemoteBrainHttpsStateV1,
};

#[test]
fn remote_brain_https_is_unconfigured_by_default() {
    let config = RemoteBrainHttpsConfigV1::default();

    assert_eq!(config.version, 1);
    assert_eq!(config.state(), RemoteBrainHttpsStateV1::Unconfigured);
}

#[test]
fn enabled_remote_brain_listener_rejects_plaintext_advertised_endpoint() {
    let config = RemoteBrainHttpsConfigV1 {
        enablement: RemoteBrainHttpsEnablementV1::Enabled,
        bind_address: Some("127.0.0.1:0".to_owned()),
        advertised_endpoint: Some("http://remote.example".to_owned()),
        certificate_chain_path: Some("test-cert.pem".into()),
        private_key_path: Some("test-key.pem".into()),
        ..RemoteBrainHttpsConfigV1::default()
    };

    assert!(matches!(
        config.validate_enabled(),
        Err(RemoteBrainHttpsError::InvalidAdvertisedEndpoint)
    ));
}

#[test]
fn enabled_remote_brain_listener_requires_client_authority_roots() {
    let config = RemoteBrainHttpsConfigV1 {
        enablement: RemoteBrainHttpsEnablementV1::Enabled,
        bind_address: Some("127.0.0.1:0".to_owned()),
        advertised_endpoint: Some("https://remote.example".to_owned()),
        certificate_chain_path: Some("test-cert.pem".into()),
        private_key_path: Some("test-key.pem".into()),
        ..RemoteBrainHttpsConfigV1::default()
    };

    assert!(matches!(
        config.validate_enabled(),
        Err(RemoteBrainHttpsError::MissingField("client_ca_bundle_path"))
    ));
}

#[test]
fn enabled_remote_brain_listener_retains_client_authority_roots() {
    let config = RemoteBrainHttpsConfigV1 {
        enablement: RemoteBrainHttpsEnablementV1::Enabled,
        bind_address: Some("127.0.0.1:0".to_owned()),
        advertised_endpoint: Some("https://remote.example".to_owned()),
        certificate_chain_path: Some("test-cert.pem".into()),
        private_key_path: Some("test-key.pem".into()),
        client_ca_bundle_path: Some("test-client-ca.pem".into()),
        ..RemoteBrainHttpsConfigV1::default()
    };

    let binding = config.validate_enabled().unwrap();
    assert_eq!(
        binding.client_ca_bundle_path,
        std::path::PathBuf::from("test-client-ca.pem")
    );
}

#[test]
fn missing_remote_brain_config_is_truthfully_unconfigured() {
    let config = RemoteBrainHttpsConfigV1::load_optional(None).unwrap();
    assert_eq!(config.state(), RemoteBrainHttpsStateV1::Unconfigured);
}

#[test]
fn configured_remote_brain_listener_loads_exact_tls_paths() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("remote-brain-https.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "enablement": "enabled",
            "bind_address": "127.0.0.1:0",
            "advertised_endpoint": "https://remote.example",
            "certificate_chain_path": "server.pem",
            "private_key_path": "server-key.pem",
            "client_ca_bundle_path": "client-ca.pem"
        })
        .to_string(),
    )
    .unwrap();

    let config = RemoteBrainHttpsConfigV1::load_optional(Some(&path)).unwrap();
    let binding = config.validate_enabled().unwrap();
    assert_eq!(
        binding.client_ca_bundle_path,
        root.path().join("client-ca.pem")
    );
    assert_eq!(
        binding.certificate_chain_path,
        root.path().join("server.pem")
    );
    assert_eq!(binding.private_key_path, root.path().join("server-key.pem"));
}

#[test]
fn disabled_remote_brain_config_rejects_ignored_tls_fields() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("remote-brain-https.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "enablement": "disabled",
            "bind_address": "127.0.0.1:0"
        })
        .to_string(),
    )
    .unwrap();

    assert!(matches!(
        RemoteBrainHttpsConfigV1::load_optional(Some(&path)),
        Err(RemoteBrainHttpsError::InvalidConfiguration)
    ));
}

#[test]
fn remote_brain_config_read_is_bounded_after_open() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("remote-brain-https.json");
    std::fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();

    assert!(matches!(
        RemoteBrainHttpsConfigV1::load_optional(Some(&path)),
        Err(RemoteBrainHttpsError::InvalidConfiguration)
    ));
}

#[test]
fn absolute_remote_brain_tls_paths_are_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("remote-brain-https.json");
    let server = root.path().join("absolute-server.pem");
    let key = root.path().join("absolute-server-key.pem");
    let client_ca = root.path().join("absolute-client-ca.pem");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "enablement": "enabled",
            "bind_address": "127.0.0.1:0",
            "advertised_endpoint": "https://remote.example",
            "certificate_chain_path": server.clone(),
            "private_key_path": key.clone(),
            "client_ca_bundle_path": client_ca.clone()
        })
        .to_string(),
    )
    .unwrap();

    let binding = RemoteBrainHttpsConfigV1::load_optional(Some(&path))
        .unwrap()
        .validate_enabled()
        .unwrap();
    assert_eq!(binding.certificate_chain_path, server);
    assert_eq!(binding.private_key_path, key);
    assert_eq!(binding.client_ca_bundle_path, client_ca);
}

#[tokio::test]
async fn listener_rejects_anonymous_tls_and_accepts_trusted_client() {
    let fixtures = remote_tls_fixtures();
    let config = RemoteBrainHttpsConfigV1 {
        enablement: RemoteBrainHttpsEnablementV1::Enabled,
        bind_address: Some("127.0.0.1:0".to_owned()),
        advertised_endpoint: Some("https://localhost".to_owned()),
        certificate_chain_path: Some(fixtures.join("server.pem")),
        private_key_path: Some(fixtures.join("server-key.pem")),
        client_ca_bundle_path: Some(fixtures.join("ca.pem")),
        ..RemoteBrainHttpsConfigV1::default()
    };
    let service = RemoteBrainHttpsService::bind_test_router(
        &config,
        Router::new().route("/probe", get(|| async { "trusted" })),
    )
    .await
    .unwrap();
    let endpoint = service.endpoint();
    let server_name = rustls::pki_types::ServerName::try_from("localhost")
        .unwrap()
        .to_owned();

    let anonymous = TlsConnector::from(Arc::new(client_tls_config(&fixtures, false)));
    let anonymous_stream = TcpStream::connect(endpoint).await.unwrap();
    let mut anonymous_stream = anonymous
        .connect(server_name.clone(), anonymous_stream)
        .await
        .unwrap();
    let _ = anonymous_stream
        .write_all(b"GET /probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await;
    let mut anonymous_response = Vec::new();
    let anonymous_read = anonymous_stream.read_to_end(&mut anonymous_response).await;
    assert!(anonymous_read.is_err() || !anonymous_response.starts_with(b"HTTP/1.1 200 OK\r\n"));

    let trusted = TlsConnector::from(Arc::new(client_tls_config(&fixtures, true)));
    let trusted_stream = TcpStream::connect(endpoint).await.unwrap();
    let mut trusted_stream = trusted.connect(server_name, trusted_stream).await.unwrap();
    trusted_stream
        .write_all(b"GET /probe HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    trusted_stream.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("trusted"));

    service.shutdown().await.unwrap();
}

fn client_tls_config(fixtures: &std::path::Path, with_identity: bool) -> rustls::ClientConfig {
    let ca_file = std::fs::File::open(fixtures.join("ca.pem")).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_pemfile::certs(&mut BufReader::new(ca_file)) {
        roots.add(certificate.unwrap()).unwrap();
    }
    let builder = rustls::ClientConfig::builder().with_root_certificates(roots);
    if !with_identity {
        return builder.with_no_client_auth();
    }
    let certificate_file = std::fs::File::open(fixtures.join("client.pem")).unwrap();
    let certificates = rustls_pemfile::certs(&mut BufReader::new(certificate_file))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let private_key_file = std::fs::File::open(fixtures.join("client-key.pem")).unwrap();
    let private_key = rustls_pemfile::private_key(&mut BufReader::new(private_key_file))
        .unwrap()
        .unwrap();
    builder
        .with_client_auth_cert(certificates, private_key)
        .unwrap()
}

fn remote_tls_fixtures() -> std::path::PathBuf {
    option_env!("TRACEDECAY_REMOTE_TLS_FIXTURE_DIR").map_or_else(
        || std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/remote-tls"),
        std::path::PathBuf::from,
    )
}

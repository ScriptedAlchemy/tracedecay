use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rustls::pki_types::PrivatePkcs8KeyDer;
use tracedecay_contracts::RequestId;
use tracedecay_contracts::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
use tracedecay_domain::{
    BrainId, BrainNodeId, EntityId, ProjectId, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
};
use tracedecay_sdk::remote_client::{EnrolledRemoteClient, RemoteClientError};

use super::client::request;
use super::tls_fixture::tls_identity;

const GRANT: &[u8; 32] = b"0123456789abcdef0123456789abcdef";
const CREDENTIAL: &[u8; 32] = b"fedcba9876543210fedcba9876543210";

fn enrollment() -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
    let brain_id = BrainId::new("brain.redirect").unwrap();
    let node_id = BrainNodeId::new("node.redirect").unwrap();
    RemoteProtocolRequestV1::new_initial_enrollment(
        RequestId::new("request.redirect").unwrap(),
        brain_id.clone(),
        node_id.clone(),
        UtcMicros(1_000_000),
        EnrollmentRequestV1 {
            grant_id: EntityId::new("grant.redirect").unwrap(),
            grant_revision: 1,
            enrollment_id: EntityId::new("enrollment.redirect").unwrap(),
            brain_id,
            node_id,
            expires_at: UtcMicros(600_000_000),
            capabilities: [RemoteCapabilityV1::Query].into_iter().collect(),
            scope: RemoteRepositoryScopeV1 {
                project_id: ProjectId::new("project.redirect").unwrap(),
                repository_id: RepositoryId::new("repository.redirect").unwrap(),
                worktree_id: WorktreeId::new("worktree.redirect").unwrap(),
                reference: None,
                snapshot_id: RepositoryStateSnapshotId::new("snapshot.redirect").unwrap(),
            },
        },
    )
    .unwrap()
}

enum RedirectDestination {
    OtherListener,
    SameOrigin,
    OutsideLoopback,
}

fn assert_redirect_refused(status: u16, tls: bool, target: RedirectDestination) {
    let source = TcpListener::bind("127.0.0.1:0").unwrap();
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    let source_address = source.local_addr().unwrap();
    let location = match target {
        RedirectDestination::SameOrigin => "/remote/relocated".to_owned(),
        RedirectDestination::OtherListener => {
            format!(
                "http://{}/remote/relocated",
                destination.local_addr().unwrap()
            )
        }
        RedirectDestination::OutsideLoopback => "http://192.0.2.1/remote/relocated".to_owned(),
    };
    let identity = tls.then(tls_identity);
    let trust_root = identity.map(|identity| &identity.root_pem);
    let server = thread::spawn(move || {
        let (mut socket, _) = source.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let response = format!(
            "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let received = if let Some(identity) = identity {
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![identity.certificate.der().clone()],
                PrivatePkcs8KeyDer::from(identity.key.serialize_der()).into(),
            )
            .unwrap();
            let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let received = request(&mut stream);
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
            received
        } else {
            let received = request(&mut socket);
            socket.write_all(response.as_bytes()).unwrap();
            received
        };
        (source, received)
    });
    let client = if let Some(root) = trust_root {
        EnrolledRemoteClient::new_with_root_certificate(
            format!("https://{source_address}/remote/"),
            GRANT,
            Duration::from_secs(2),
            root,
        )
    } else {
        EnrolledRemoteClient::new_local_daemon(
            format!("http://{source_address}/remote/"),
            GRANT,
            Duration::from_secs(2),
        )
    }
    .unwrap();
    let outcome = client.enroll(&enrollment(), CREDENTIAL);
    let (source, received) = server.join().unwrap();
    assert!(
        received.contains("x-tracedecay-enrollment-credential: fedcba9876543210fedcba9876543210")
    );
    assert!(received.contains("\"enrollment_id\":\"enrollment.redirect\""));
    for listener in [source, destination] {
        listener.set_nonblocking(true).unwrap();
        match listener.accept() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok((mut socket, _)) => {
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                panic!(
                    "redirect disclosed a follow-up request: {}",
                    request(&mut socket)
                );
            }
            Err(error) => panic!("redirect observer failed: {error}"),
        }
    }
    assert!(
        matches!(outcome, Err(RemoteClientError::Protocol(message)) if message.contains("redirect"))
    );
}

#[test]
fn loopback_redirects_never_forward_enrollment_to_another_listener() {
    for status in [307, 308] {
        assert_redirect_refused(status, false, RedirectDestination::OtherListener);
    }
}

#[test]
fn https_redirects_never_downgrade_enrollment_to_plaintext() {
    for status in [307, 308] {
        assert_redirect_refused(status, true, RedirectDestination::OtherListener);
    }
}

#[test]
fn same_origin_redirects_are_protocol_refusals() {
    for status in [307, 308] {
        assert_redirect_refused(status, false, RedirectDestination::SameOrigin);
    }
}

#[test]
fn loopback_redirects_cannot_admit_a_non_loopback_http_endpoint() {
    for status in [307, 308] {
        assert_redirect_refused(status, false, RedirectDestination::OutsideLoopback);
    }
}

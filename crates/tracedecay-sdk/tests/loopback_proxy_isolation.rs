//! The loopback-daemon remote client must never route through a proxy: a
//! proxied plaintext request would carry the Bearer enrollment credential off
//! the machine. reqwest captures environment proxies once per process, so
//! this journey owns its own test binary.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::RequestId;
use tracedecay_application::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
use tracedecay_domain::{
    BrainId, BrainNodeId, EntityId, ProjectId, RefId, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
};
use tracedecay_sdk::remote_client::EnrolledRemoteClient;

/// Counts accepted connections; a hit here is the credential leaving the
/// direct loopback path.
fn spawn_proxy_recorder() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy recorder");
    let address = listener.local_addr().expect("proxy recorder address");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        while let Ok((_stream, _)) = listener.accept() {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    (address, connections)
}

/// Accepts direct connections and answers a bodyless 503 so the client
/// settles with a typed error instead of hanging on its read timeout.
fn spawn_direct_recorder() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind direct recorder");
    let address = listener.local_addr().expect("direct recorder address");
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&connections);
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    (address, connections)
}

fn enrollment_request() -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after the unix epoch");
    let sent_at = UtcMicros(i64::try_from(now.as_micros()).expect("current time fits in i64"));
    let brain_id = BrainId::new("brain.loopback-proxy").expect("brain id");
    let node_id = BrainNodeId::new("node.loopback-proxy").expect("node id");
    RemoteProtocolRequestV1::new_initial_enrollment(
        RequestId::new("request.loopback-proxy-isolation").expect("request id"),
        brain_id.clone(),
        node_id.clone(),
        sent_at,
        EnrollmentRequestV1 {
            grant_id: EntityId::new("grant.loopback-proxy").expect("grant id"),
            grant_revision: 1,
            enrollment_id: EntityId::new("enrollment.loopback-proxy").expect("enrollment id"),
            brain_id,
            node_id,
            expires_at: UtcMicros(sent_at.0.saturating_add(600_000_000)),
            capabilities: BTreeSet::from([RemoteCapabilityV1::CaptureOffline]),
            scope: RemoteRepositoryScopeV1 {
                project_id: ProjectId::new("project.loopback-proxy").expect("project id"),
                repository_id: RepositoryId::new("repository.loopback-proxy")
                    .expect("repository id"),
                worktree_id: WorktreeId::new("worktree.loopback-proxy").expect("worktree id"),
                reference: Some(RefId::new("refs/heads/main").expect("reference")),
                snapshot_id: RepositoryStateSnapshotId::new("snapshot.loopback-proxy")
                    .expect("snapshot id"),
            },
        },
    )
    .expect("canonical initial enrollment request")
}

#[test]
fn loopback_daemon_requests_bypass_configured_proxies() {
    let (proxy_address, proxy_connections) = spawn_proxy_recorder();
    let (daemon_address, daemon_connections) = spawn_direct_recorder();

    // SAFETY: this binary owns the process and the recorder threads never
    // read the environment, so the global mutation cannot race another test.
    unsafe {
        std::env::set_var("HTTP_PROXY", format!("http://{proxy_address}"));
        std::env::set_var("http_proxy", format!("http://{proxy_address}"));
        std::env::set_var("ALL_PROXY", format!("http://{proxy_address}"));
        std::env::set_var("all_proxy", format!("http://{proxy_address}"));
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let client = EnrolledRemoteClient::new_local_daemon(
        format!("http://{daemon_address}/remote/"),
        "0123456789abcdef0123456789abcdef",
        Duration::from_secs(5),
    )
    .expect("loopback daemon target");
    let outcome = client.enroll(&enrollment_request(), "fedcba9876543210fedcba9876543210");

    assert!(
        outcome.is_err(),
        "the bodyless 503 recorder must settle as a typed client error"
    );
    assert_eq!(
        daemon_connections.load(Ordering::SeqCst),
        1,
        "the request must reach the loopback daemon directly"
    );
    assert_eq!(
        proxy_connections.load(Ordering::SeqCst),
        0,
        "the enrollment credential must never traverse a configured proxy"
    );
}

//! The plaintext loopback Remote Brain target must never traverse a system
//! proxy: `HTTP_PROXY`/`ALL_PROXY` would carry the Bearer enrollment
//! credential off the machine unencrypted, defeating the loopback-only
//! plaintext admission. This test lives in its own integration binary because
//! it mutates the process-wide proxy environment.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use tracedecay_application::RequestId;
use tracedecay_application::remote::protocol::{EnrollmentRequestV1, RemoteProtocolRequestV1};
use tracedecay_domain::{
    BrainId, BrainNodeId, EntityId, ProjectId, RefId, RemoteCapabilityV1, RemoteRepositoryScopeV1,
    RepositoryId, RepositoryStateSnapshotId, UtcMicros, WorktreeId,
};
use tracedecay_sdk::remote_client::{EnrolledRemoteClient, RemoteClientError};

#[test]
fn loopback_http_request_never_routes_through_a_system_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
    let proxy_address = proxy.local_addr().expect("proxy address");
    let target = TcpListener::bind("127.0.0.1:0").expect("bind target listener");
    let target_port = target.local_addr().expect("target address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = target.accept().expect("direct loopback connection");
        let mut head = [0u8; 2048];
        let _ = stream.read(&mut head).expect("read request head");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\n\
content-type: application/json\r\n\
content-length: 2\r\n\
connection: close\r\n\r\n{}",
            )
            .expect("write canned response");
    });

    // SAFETY: this integration binary holds only this test, so no other
    // thread reads or writes the process environment concurrently.
    unsafe {
        std::env::set_var("HTTP_PROXY", format!("http://{proxy_address}"));
        std::env::set_var("ALL_PROXY", format!("http://{proxy_address}"));
    }
    let client = EnrolledRemoteClient::new_local_daemon(
        format!("http://127.0.0.1:{target_port}/remote/"),
        "0123456789abcdef0123456789abcdef",
        Duration::from_secs(5),
    );
    // SAFETY: as above.
    unsafe {
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("ALL_PROXY");
    }
    let client = client.expect("loopback client must build under a proxy environment");

    let outcome = client.enroll(&enrollment_request(), *b"fedcba9876543210fedcba9876543210");
    // A protocol error proves the request reached the direct loopback target
    // and got the canned garbage back; a proxied request would have died in
    // transport against the never-accepting proxy listener instead.
    match outcome {
        Err(RemoteClientError::Protocol(_)) => {}
        Err(error) => panic!("expected a protocol error from the direct target, got {error:?}"),
        Ok(_) => panic!("the canned non-canonical response must fail as protocol"),
    }
    server.join().expect("target server thread");

    proxy
        .set_nonblocking(true)
        .expect("nonblocking proxy accept");
    match proxy.accept() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok(_) => panic!("the loopback request must never reach the system proxy"),
        Err(error) => panic!("proxy accept failed: {error}"),
    }
}

fn enrollment_request() -> RemoteProtocolRequestV1<EnrollmentRequestV1> {
    let brain_id = BrainId::new("brain.remote-proxy-test").unwrap();
    let node_id = BrainNodeId::new("node.remote-proxy-test").unwrap();
    RemoteProtocolRequestV1::new_initial_enrollment(
        RequestId::new("request.remote-proxy-test").unwrap(),
        brain_id.clone(),
        node_id.clone(),
        UtcMicros(1_000_000),
        EnrollmentRequestV1 {
            grant_id: EntityId::new("grant.remote-proxy-test").unwrap(),
            grant_revision: 1,
            enrollment_id: EntityId::new("enrollment.remote-proxy-test").unwrap(),
            brain_id,
            node_id,
            expires_at: UtcMicros(600_000_000),
            capabilities: [RemoteCapabilityV1::Query].into_iter().collect(),
            scope: RemoteRepositoryScopeV1 {
                project_id: ProjectId::new("project.remote-proxy-test").unwrap(),
                repository_id: RepositoryId::new("repository.remote-proxy-test").unwrap(),
                worktree_id: WorktreeId::new("worktree.remote-proxy-test").unwrap(),
                reference: Some(RefId::new("refs/heads/remote-proxy-test").unwrap()),
                snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-proxy-test").unwrap(),
            },
        },
    )
    .unwrap()
}

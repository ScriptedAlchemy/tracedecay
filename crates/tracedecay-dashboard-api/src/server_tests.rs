use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tokio::sync::{Notify, oneshot};

use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};

use crate::server::{bind_dashboard, serve_loopback};

fn get_over_std_socket(addr: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect loopback server");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

/// Closing the listener must preserve the response already admitted on a
/// connection until it finishes.
#[test]
fn loopback_server_drains_in_flight_request_after_shutdown() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("Tokio runtime");
    runtime.block_on(async {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let router = Router::new().route(
            "/slow",
            get({
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                move || async move {
                    entered.notify_one();
                    release.notified().await;
                    "drained"
                }
            }),
        );
        let (listener, addr) = bind_dashboard("127.0.0.1", 0)
            .await
            .expect("bind loopback listener");
        let (stop, stopped) = oneshot::channel::<()>();
        let server = tokio::spawn(serve_loopback(listener, router, async {
            let _ = stopped.await;
        }));
        let client = tokio::task::spawn_blocking(move || get_over_std_socket(addr, "/slow"));
        entered.notified().await;

        stop.send(()).expect("serve loop is running");
        tokio::task::spawn_blocking(move || {
            while TcpStream::connect(addr).is_ok() {
                std::thread::yield_now();
            }
        })
        .await
        .expect("listener closes once shutdown is requested");
        assert!(
            !server.is_finished(),
            "the serve loop must wait for the in-flight request"
        );

        release.notify_one();
        let response = client.await.expect("client thread");
        assert!(
            response.starts_with("HTTP/1.1 200 OK") && response.ends_with("drained"),
            "the in-flight request completes after shutdown: {response:?}"
        );
        server.await.expect("serve loop returns after the drain");
    });
}

/// Cancelling the listener must abort the connection tasks it owns, including
/// a handler that cannot finish gracefully.
#[tokio::test]
async fn cancelling_the_loopback_server_drops_its_open_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct HandlerDrop(Arc<Notify>);

    impl Drop for HandlerDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let router = Router::new().route(
        "/held",
        get({
            let entered = Arc::clone(&entered);
            let dropped = Arc::clone(&dropped);
            move || {
                let entered = Arc::clone(&entered);
                let receipt = HandlerDrop(Arc::clone(&dropped));
                async move {
                    let _receipt = receipt;
                    entered.notify_one();
                    std::future::pending::<&'static str>().await
                }
            }
        }),
    );
    let (listener, address) = bind_dashboard("127.0.0.1", 0).await.unwrap();
    let server = tokio::spawn(serve_loopback(listener, router, std::future::pending()));
    let mut peer = tokio::net::TcpStream::connect(address).await.unwrap();
    peer.write_all(b"GET /held HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let bound = std::time::Duration::from_secs(10);
    tokio::time::timeout(bound, entered.notified())
        .await
        .expect("the connection must admit the held handler");

    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    tokio::time::timeout(bound, dropped.notified())
        .await
        .expect("listener cancellation must drop its active handler");
    // The joined task has dropped the listener it exclusively owned. Its
    // ephemeral address may already belong to another parallel test, so a new
    // connection cannot identify that listener. Observe the original accepted
    // connection below instead.
    let mut byte = [0];
    let received = tokio::time::timeout(bound, peer.read(&mut byte))
        .await
        .expect("listener cancellation must close its accepted socket");
    assert!(
        matches!(received, Ok(0))
            || matches!(received, Err(ref error) if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            )),
        "an aborted handler must close without producing a response: {received:?}"
    );
}

/// The installed process runtime serves the dashboard from a synchronous host
/// without requiring the caller to create a second runtime.
#[test]
fn loopback_server_serves_connections_on_the_installed_runtime() {
    let router = Router::new().route(
        "/ping",
        get(|| async {
            assert!(tokio::runtime::Handle::try_current().is_ok());
            "pong"
        }),
    );
    let (listener, addr) = native_runtime::block_on(bind_dashboard("127.0.0.1", 0))
        .expect("native root")
        .expect("bind loopback listener");
    let (stop, stopped) = oneshot::channel::<()>();
    let server = NativeTask::spawn(serve_loopback(listener, router, async {
        let _ = stopped.await;
    }))
    .expect("native serve task");

    let response = get_over_std_socket(addr, "/ping");
    let _ = stop.send(());
    let served = native_runtime::block_on(native_runtime::timeout(
        std::time::Duration::from_secs(10),
        server.wait(),
    ))
    .expect("native root");

    assert!(
        response.starts_with("HTTP/1.1 200 OK") && response.ends_with("pong"),
        "the native-driven loop must serve: {response:?}"
    );
    assert_eq!(served, Ok(NativeTaskOutcome::Completed(())));
}

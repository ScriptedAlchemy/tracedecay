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

/// The loop runs on a Tokio runtime with neither the IO nor the time driver,
/// so any Tokio socket or timer on the serving path panics.
#[test]
fn loopback_server_drains_in_flight_request_after_shutdown_without_tokio_drivers() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .expect("Tokio runtime without drivers");
    let handle = runtime.handle().clone();
    runtime.block_on(async {
        let runtime = handle;
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
        let server = tokio::spawn(serve_loopback(runtime.clone(), listener, router, async {
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

/// A native caller hands the serve loop its Tokio handle; the loop needs no
/// ambient Tokio runtime to serve.
#[test]
fn loopback_server_serves_from_a_native_task_with_an_explicit_handle() {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .expect("Tokio runtime");
    let router = Router::new().route("/ping", get(|| async { "pong" }));
    let (listener, addr) = native_runtime::block_on(bind_dashboard("127.0.0.1", 0))
        .expect("native root")
        .expect("bind loopback listener");
    let (stop, stopped) = oneshot::channel::<()>();
    let server = NativeTask::spawn(serve_loopback(
        tokio.handle().clone(),
        listener,
        router,
        async {
            let _ = stopped.await;
        },
    ))
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

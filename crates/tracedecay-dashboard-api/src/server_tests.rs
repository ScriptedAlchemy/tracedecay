use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tokio::sync::{Notify, oneshot};

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

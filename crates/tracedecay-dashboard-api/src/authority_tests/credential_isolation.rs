//! A browser authenticated to one dashboard must not hand a replayable
//! credential to any other service on the same loopback host.
//!
//! Browsers scope cookies by host, not port, so any `Set-Cookie` a dashboard
//! sends is replayed to every other loopback port the browser visits. The
//! fixture therefore models the two browser stores that matter: a host-scoped
//! cookie jar, and origin-scoped page storage (origin includes the port) that
//! the page script fills from the launch URL fragment. A real second listener
//! records the raw bytes it receives.

use std::collections::HashMap;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use axum::routing::{get, post};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::{DashboardAccessToken, with_dashboard_http_admission};

const PAUSE: &str = "/api/automation/scheduler/pause";

/// One dashboard as a browser reaches it: the admission-wrapped router plus
/// the port its `Host` header must name.
struct Dashboard {
    port: u16,
    token: DashboardAccessToken,
    app: Router,
}

impl Dashboard {
    fn start(port: u16) -> Self {
        async fn ok() -> StatusCode {
            StatusCode::NO_CONTENT
        }
        let token = DashboardAccessToken::mint().expect("dashboard access token");
        let app = with_dashboard_http_admission(
            Router::new().route("/", get(ok)).route(PAUSE, post(ok)),
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            token.clone(),
        );
        Self { port, token, app }
    }

    fn launch_path(&self) -> String {
        let url = self.token.launch_url(([127, 0, 0, 1], self.port).into());
        url.strip_prefix(&format!("http://127.0.0.1:{}", self.port))
            .expect("launch URL names the bound listener")
            .to_owned()
    }

    async fn send(&self, method: Method, uri: &str, authorization: Option<&str>) -> Response<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, format!("127.0.0.1:{}", self.port));
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        self.app
            .clone()
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("admission response")
    }
}

/// The browser state that survives between requests to loopback services.
#[derive(Default)]
struct Browser {
    /// `name=value` pairs keyed by host only, as RFC 6265 scopes them.
    cookie_jar: Vec<String>,
    /// Page storage keyed by origin (`host:port`); only the page script of
    /// that origin reads it back.
    page_storage: HashMap<u16, String>,
}

impl Browser {
    fn store_cookies(&mut self, response: &Response<Body>) {
        for set_cookie in response.headers().get_all(header::SET_COOKIE) {
            let pair = set_cookie.to_str().expect("cookie text");
            self.cookie_jar
                .push(pair.split(';').next().unwrap().to_owned());
        }
    }

    /// Opens the launch URL, follows the redirect, and lets the page script
    /// adopt the fragment, exactly as the dashboard bundle does.
    async fn open(&mut self, dashboard: &Dashboard) {
        let launch = dashboard
            .send(Method::GET, &dashboard.launch_path(), None)
            .await;
        assert_eq!(launch.status(), StatusCode::SEE_OTHER);
        self.store_cookies(&launch);
        let location = launch.headers()[header::LOCATION]
            .to_str()
            .expect("location")
            .to_owned();
        let (path, fragment) = location.split_once('#').expect("token rides the fragment");
        let token = fragment
            .strip_prefix("token=")
            .expect("fragment carries the launch token");
        // A fragment is never part of the request the browser sends.
        let shell = dashboard.send(Method::GET, path, None).await;
        assert_eq!(shell.status(), StatusCode::NO_CONTENT);
        self.store_cookies(&shell);
        self.page_storage.insert(dashboard.port, token.to_owned());
    }

    /// An API write from a dashboard's own page script.
    async fn page_write(&self, dashboard: &Dashboard) -> StatusCode {
        use base64::Engine as _;
        let token = self
            .page_storage
            .get(&dashboard.port)
            .expect("the page adopted a token");
        let basic = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("tracedecay:{token}"))
        );
        dashboard
            .send(Method::POST, PAUSE, Some(&basic))
            .await
            .status()
    }

    /// Visits another loopback service: the browser attaches every cookie its
    /// jar holds for the host, and nothing from any dashboard's page storage.
    async fn visit_raw_listener(&self) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let service = listener.local_addr().unwrap().port();
        let capture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut received = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !received.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.expect("read");
                assert_ne!(read, 0, "the browser closed before finishing its request");
                received.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n")
                .await
                .expect("respond");
            String::from_utf8(received).expect("request text")
        });
        let mut request = format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{service}\r\n");
        if !self.cookie_jar.is_empty() {
            request.push_str(&format!("Cookie: {}\r\n", self.cookie_jar.join("; ")));
        }
        request.push_str("\r\n");
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", service))
            .await
            .expect("connect");
        stream.write_all(request.as_bytes()).await.expect("send");
        capture.await.expect("capture task")
    }
}

#[tokio::test]
async fn an_unrelated_loopback_listener_receives_no_replayable_credential() {
    let dashboard = Dashboard::start(43_201);
    let mut browser = Browser::default();
    browser.open(&dashboard).await;
    assert_eq!(
        browser.page_write(&dashboard).await,
        StatusCode::NO_CONTENT,
        "the dashboard's own page stays authenticated"
    );

    let captured = browser.visit_raw_listener().await;
    let secret = dashboard
        .launch_path()
        .strip_prefix("/?token=")
        .expect("launch token")
        .to_owned();
    assert!(
        !captured.contains(&secret),
        "the other listener saw the dashboard token:\n{captured}"
    );
    let lowered = captured.to_ascii_lowercase();
    assert!(
        !lowered.contains("cookie:") && !lowered.contains("authorization:"),
        "the other listener received credential-bearing headers:\n{captured}"
    );
}

#[tokio::test]
async fn concurrent_dashboards_keep_separate_credentials() {
    let first = Dashboard::start(43_202);
    let second = Dashboard::start(43_203);
    let mut browser = Browser::default();
    browser.open(&first).await;
    browser.open(&second).await;
    assert_eq!(browser.page_write(&first).await, StatusCode::NO_CONTENT);
    assert_eq!(browser.page_write(&second).await, StatusCode::NO_CONTENT);

    let first_token = browser.page_storage[&first.port].clone();
    let basic = {
        use base64::Engine as _;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("tracedecay:{first_token}"))
        )
    };
    let crossed = second.send(Method::POST, PAUSE, Some(&basic)).await;
    assert_eq!(
        crossed.status(),
        StatusCode::UNAUTHORIZED,
        "one dashboard's credential must not open another"
    );
}

/// The credential is not replayable as a cookie either: a client that
/// presents the old cookie shape gets nothing.
#[tokio::test]
async fn a_cookie_is_not_a_credential() {
    let dashboard = Dashboard::start(43_204);
    let token = dashboard
        .launch_path()
        .strip_prefix("/?token=")
        .expect("launch token")
        .to_owned();
    let request = Request::builder()
        .method(Method::POST)
        .uri(PAUSE)
        .header(header::HOST, "127.0.0.1:43204")
        .header(
            header::COOKIE,
            format!("tracedecay_dashboard_43204={token}"),
        )
        .body(Body::empty())
        .unwrap();
    let response = dashboard.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

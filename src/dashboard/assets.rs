//! Embedded static assets for the dashboard.
//!
//! The dist files are produced by `dashboard/build.mjs` (run `npm install &&
//! npm run build` in `dashboard/`) and embedded at compile time, so the
//! installed binary serves the UI with no filesystem dependency.

use axum::extract::Path;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>tokensave dashboard</title>
    <link rel="stylesheet" href="/shell/shell.css" />
  </head>
  <body>
    <div id="root"></div>
    <script src="/shell/shell.js"></script>
  </body>
</html>
"#;

const SHELL_JS: &[u8] = include_bytes!("../../dashboard/shell/dist/shell.js");
const SHELL_CSS: &[u8] = include_bytes!("../../dashboard/shell/dist/shell.css");
// Plugin bundles are embedded as &str (they are UTF-8 esbuild output) so the
// Hermes installer (src/agents/hermes_dashboard.rs) can reuse the exact same
// embedded data when writing the wrapper plugin's dist files to disk.
pub(crate) const HOLOGRAPHIC_JS: &str = include_str!("../../dashboard/holographic/dist/index.js");
pub(crate) const HOLOGRAPHIC_CSS: &str = include_str!("../../dashboard/holographic/dist/style.css");
pub(crate) const LCM_JS: &str = include_str!("../../dashboard/lcm/dist/index.js");
pub(crate) const LCM_CSS: &str = include_str!("../../dashboard/lcm/dist/style.css");
pub(crate) const GRAPH_JS: &str = include_str!("../../dashboard/graph/dist/index.js");
pub(crate) const GRAPH_CSS: &str = include_str!("../../dashboard/graph/dist/style.css");
pub(crate) const SAVINGS_JS: &str = include_str!("../../dashboard/savings/dist/index.js");
pub(crate) const SAVINGS_CSS: &str = include_str!("../../dashboard/savings/dist/style.css");
const ASSET_STAMP: &str = env!("TOKENSAVE_DASHBOARD_ASSET_STAMP");

/// `ETag` value for every embedded asset: the compile-time bundle stamp,
/// quoted per RFC 9110.
fn asset_etag() -> String {
    format!("\"{ASSET_STAMP}\"")
}

/// True when the request's `If-None-Match` matches the embedded-asset
/// `ETag`, so the (up to ~600 KB) bundle body can be skipped with a 304.
fn if_none_match_hits(headers: &HeaderMap) -> bool {
    let etag = asset_etag();
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value == "*"
                || value
                    .split(',')
                    .any(|tag| tag.trim().trim_start_matches("W/") == etag)
        })
}

fn asset_headers(response: &mut Response, content_type: &'static str) {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    // `no-cache` (not `no-store`): the browser revalidates every load, and
    // the build-time stamp answers with a body-less 304 unless the binary —
    // and therefore the embedded bundle — actually changed.
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    if let Ok(etag) = header::HeaderValue::from_str(&asset_etag()) {
        headers.insert(header::ETAG, etag);
    }
    headers.insert(
        header::HeaderName::from_static("x-tokensave-asset-stamp"),
        header::HeaderValue::from_static(ASSET_STAMP),
    );
}

fn static_response(
    headers: &HeaderMap,
    body: &'static [u8],
    content_type: &'static str,
) -> Response {
    let mut response = if if_none_match_hits(headers) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        body.into_response()
    };
    asset_headers(&mut response, content_type);
    response
}

pub(crate) async fn index_html() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub(crate) async fn shell_asset(headers: HeaderMap, Path(file): Path<String>) -> Response {
    match file.as_str() {
        "shell.js" => static_response(&headers, SHELL_JS, "application/javascript"),
        "shell.css" => static_response(&headers, SHELL_CSS, "text/css"),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) async fn plugin_asset(
    headers: HeaderMap,
    Path((plugin, file)): Path<(String, String)>,
) -> Response {
    let serve =
        |body: &'static str, content_type| static_response(&headers, body.as_bytes(), content_type);
    match (plugin.as_str(), file.as_str()) {
        ("holographic", "index.js") => serve(HOLOGRAPHIC_JS, "application/javascript"),
        ("holographic", "style.css") => serve(HOLOGRAPHIC_CSS, "text/css"),
        ("hermes-lcm", "index.js") => serve(LCM_JS, "application/javascript"),
        ("hermes-lcm", "style.css") => serve(LCM_CSS, "text/css"),
        ("graph", "index.js") => serve(GRAPH_JS, "application/javascript"),
        ("graph", "style.css") => serve(GRAPH_CSS, "text/css"),
        ("savings", "index.js") => serve(SAVINGS_JS, "application/javascript"),
        ("savings", "style.css") => serve(SAVINGS_CSS, "text/css"),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

//! Embedded static assets for the dashboard.
//!
//! The dist files are produced by `dashboard/build.mjs` (run `npm install &&
//! npm run build` in `dashboard/`) and embedded at compile time, so the
//! installed binary serves the UI with no filesystem dependency.

use axum::extract::Path;
use axum::http::{header, StatusCode};
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
const HOLOGRAPHIC_JS: &[u8] = include_bytes!("../../dashboard/holographic/dist/index.js");
const HOLOGRAPHIC_CSS: &[u8] = include_bytes!("../../dashboard/holographic/dist/style.css");
const LCM_JS: &[u8] = include_bytes!("../../dashboard/lcm/dist/index.js");
const LCM_CSS: &[u8] = include_bytes!("../../dashboard/lcm/dist/style.css");
const GRAPH_JS: &[u8] = include_bytes!("../../dashboard/graph/dist/index.js");
const GRAPH_CSS: &[u8] = include_bytes!("../../dashboard/graph/dist/style.css");
const ASSET_STAMP: &str = env!("TOKENSAVE_DASHBOARD_ASSET_STAMP");

fn static_response(body: &'static [u8], content_type: &'static str) -> Response {
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response();
    response.headers_mut().insert(
        header::HeaderName::from_static("x-tokensave-asset-stamp"),
        header::HeaderValue::from_static(ASSET_STAMP),
    );
    response
}

pub(crate) async fn index_html() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub(crate) async fn shell_asset(Path(file): Path<String>) -> Response {
    match file.as_str() {
        "shell.js" => static_response(SHELL_JS, "application/javascript"),
        "shell.css" => static_response(SHELL_CSS, "text/css"),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) async fn plugin_asset(Path((plugin, file)): Path<(String, String)>) -> Response {
    match (plugin.as_str(), file.as_str()) {
        ("holographic", "index.js") => static_response(HOLOGRAPHIC_JS, "application/javascript"),
        ("holographic", "style.css") => static_response(HOLOGRAPHIC_CSS, "text/css"),
        ("hermes-lcm", "index.js") => static_response(LCM_JS, "application/javascript"),
        ("hermes-lcm", "style.css") => static_response(LCM_CSS, "text/css"),
        ("graph", "index.js") => static_response(GRAPH_JS, "application/javascript"),
        ("graph", "style.css") => static_response(GRAPH_CSS, "text/css"),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

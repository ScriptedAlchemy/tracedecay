//! Shared HTTP representation delivery and embedded dashboard asset policies.
//!
//! This module owns transport representation behavior only. Application
//! handlers remain responsible for producing typed results and, when useful,
//! a strong `ETag`; the middleware supplies cache defaults, conditional
//! delivery, HEAD semantics, and negotiated gzip encoding.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, HttpBody};
use axum::extract::{Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, ETAG};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use headers::{ETag, HeaderMapExt, IfNoneMatch};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::Predicate;

const API_CACHE_POLICY: &str = "no-store";
const SSE_CACHE_POLICY: &str = "no-cache, no-transform";
const SHELL_CACHE_POLICY: &str = "no-cache";
const IMMUTABLE_CACHE_POLICY: &str = "public, max-age=31536000, immutable";
const MAX_EMBEDDED_ASSETS: usize = 4_096;
const MAX_EMBEDDED_ASSET_BYTES: usize = 16 * 1024 * 1024;
const MAX_EMBEDDED_PATH_BYTES: usize = 1_024;
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// One compile-time asset admitted to the bounded in-memory asset service.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddedAsset {
    path: &'static str,
    contents: &'static [u8],
}

impl EmbeddedAsset {
    /// Defines an embedded asset whose strong ETag is derived from its bytes.
    ///
    /// Router construction computes the validator; callers cannot supply an
    /// identity that diverges from the representation.
    pub const fn new(path: &'static str, contents: &'static [u8]) -> Self {
        Self { path, contents }
    }
}

/// Static asset admission failures are detected before the router can serve.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EmbeddedAssetError {
    #[error("embedded asset count exceeds the {MAX_EMBEDDED_ASSETS} asset limit")]
    TooManyAssets,
    #[error("embedded asset path is empty, absolute, or exceeds the path limit")]
    InvalidPath,
    #[error("embedded asset `{path}` occupies the reserved API namespace")]
    ApiPath { path: &'static str },
    #[error("embedded asset `{path}` exceeds the per-asset body limit")]
    BodyTooLarge { path: &'static str },
    #[error("embedded asset `{path}` validator could not be encoded as an HTTP header")]
    HeaderEncoding { path: &'static str },
    #[error("embedded asset path `{path}` is duplicated")]
    DuplicatePath { path: &'static str },
    #[error("embedded shell `{path}` is absent from the asset set")]
    MissingShell { path: &'static str },
}

#[derive(Clone)]
struct PreparedAsset {
    contents: &'static [u8],
    content_type: HeaderValue,
    cache_control: HeaderValue,
    etag: HeaderValue,
}

#[derive(Clone)]
struct EmbeddedAssetState {
    assets: Arc<HashMap<&'static str, PreparedAsset>>,
    shell_path: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct ApiDeliveryRequest;

/// Preserves API delivery policy when an outer transport strips its public
/// application prefix before dispatching to the canonical application router.
pub fn mark_api_delivery_request(request: &mut Request) {
    request.extensions_mut().insert(ApiDeliveryRequest);
}

/// Applies API/SSE cache policy, conditional request handling, HEAD semantics,
/// and gzip content negotiation to an Axum router.
pub fn http_delivery_router(router: Router) -> Router {
    router
        .layer(
            CompressionLayer::new()
                .gzip(true)
                .no_br()
                .no_deflate()
                .no_zstd()
                .compress_when(DeliveryCompressionPredicate),
        )
        .layer(middleware::from_fn(delivery_policy))
}

/// Builds a bounded, filesystem-independent static asset router.
///
/// Exact assets are always served. Missing non-API paths fall back to the
/// shell only for browser navigations that accept HTML; `/api` and `/api/**`
/// always remain not found.
pub fn embedded_asset_router(
    assets: impl IntoIterator<Item = EmbeddedAsset>,
    shell_path: &'static str,
) -> Result<Router, EmbeddedAssetError> {
    if !valid_asset_path(shell_path) {
        return Err(EmbeddedAssetError::InvalidPath);
    }

    let mut prepared = HashMap::new();
    for asset in assets {
        if prepared.len() == MAX_EMBEDDED_ASSETS {
            return Err(EmbeddedAssetError::TooManyAssets);
        }
        if !valid_asset_path(asset.path) {
            return Err(EmbeddedAssetError::InvalidPath);
        }
        if is_api_asset_path(asset.path) {
            return Err(EmbeddedAssetError::ApiPath { path: asset.path });
        }
        if asset.contents.len() > MAX_EMBEDDED_ASSET_BYTES {
            return Err(EmbeddedAssetError::BodyTooLarge { path: asset.path });
        }
        let etag = content_etag(asset.path, asset.contents)?;
        let prepared_asset = PreparedAsset {
            contents: asset.contents,
            content_type: HeaderValue::from_static(content_type_for_path(asset.path)),
            cache_control: HeaderValue::from_static(
                if is_content_fingerprinted(asset.path, asset.contents) {
                    IMMUTABLE_CACHE_POLICY
                } else {
                    SHELL_CACHE_POLICY
                },
            ),
            etag,
        };
        if prepared.insert(asset.path, prepared_asset).is_some() {
            return Err(EmbeddedAssetError::DuplicatePath { path: asset.path });
        }
    }
    if !prepared.contains_key(shell_path) {
        return Err(EmbeddedAssetError::MissingShell { path: shell_path });
    }

    let state = EmbeddedAssetState {
        assets: Arc::new(prepared),
        shell_path,
    };
    Ok(http_delivery_router(
        Router::new()
            .fallback(get(serve_embedded_asset))
            .with_state(state),
    ))
}

async fn delivery_policy(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let request_is_api = is_api_path(request.uri().path())
        || request.extensions().get::<ApiDeliveryRequest>().is_some();
    let if_none_match = request
        .headers()
        .typed_try_get::<IfNoneMatch>()
        .ok()
        .flatten();
    let mut response = next.run(request).await;

    if is_sse(response.headers()) {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(SSE_CACHE_POLICY));
    } else if request_is_api && !response.headers().contains_key(CACHE_CONTROL) {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(API_CACHE_POLICY));
    }

    let gzip_representation = response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("gzip"));
    if gzip_representation && let Some(etag) = response.headers().get(ETAG).cloned() {
        if let Some(gzip_etag) = gzip_representation_etag(&etag) {
            response.headers_mut().insert(ETAG, gzip_etag);
        } else {
            response.headers_mut().remove(ETAG);
        }
    }

    if method != Method::GET && method != Method::HEAD {
        return response;
    }

    if response.status().is_success()
        && if_none_match
            .as_ref()
            .is_some_and(|condition| if_none_match_fails(condition, response.headers()))
    {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response.headers_mut().remove(CONTENT_LENGTH);
        *response.body_mut() = Body::empty();
        return response;
    }

    if method == Method::HEAD {
        if !response.headers().contains_key(CONTENT_LENGTH)
            && let Some(length) = response.body().size_hint().exact()
            && let Ok(value) = HeaderValue::from_str(&length.to_string())
        {
            response.headers_mut().insert(CONTENT_LENGTH, value);
        }
        *response.body_mut() = Body::empty();
    }
    response
}

async fn serve_embedded_asset(
    State(state): State<EmbeddedAssetState>,
    request: Request,
) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    if is_api_asset_path(path) {
        return status_response(StatusCode::NOT_FOUND);
    }
    if path.is_empty() {
        return match state.assets.get(state.shell_path) {
            Some(shell) => asset_response(shell),
            None => status_response(StatusCode::NOT_FOUND),
        };
    }
    if let Some(asset) = state.assets.get(path) {
        return asset_response(asset);
    }

    if !is_html_navigation(&request) {
        return status_response(StatusCode::NOT_FOUND);
    }

    match state.assets.get(state.shell_path) {
        Some(shell) => asset_response(shell),
        None => status_response(StatusCode::NOT_FOUND),
    }
}

fn asset_response(asset: &PreparedAsset) -> Response {
    let mut response = Response::new(Body::from(asset.contents));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, asset.content_type.clone());
    headers.insert(CACHE_CONTROL, asset.cache_control.clone());
    headers.insert(ETAG, asset.etag.clone());
    if let Ok(length) = HeaderValue::from_str(&asset.contents.len().to_string()) {
        headers.insert(CONTENT_LENGTH, length);
    }
    response
}

fn status_response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

#[derive(Clone, Copy, Debug)]
struct DeliveryCompressionPredicate;

impl Predicate for DeliveryCompressionPredicate {
    fn should_compress<B>(&self, response: &axum::http::Response<B>) -> bool
    where
        B: HttpBody,
    {
        response.status() != StatusCode::NO_CONTENT
            && response.status() != StatusCode::NOT_MODIFIED
            && response.body().size_hint().exact() != Some(0)
            && is_compressible_content_type(response.headers())
            && !has_no_transform(response.headers())
    }
}

fn is_compressible_content_type(headers: &HeaderMap) -> bool {
    let Some(content_type) = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    if media_type.eq_ignore_ascii_case("text/event-stream") {
        return false;
    }
    media_type
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"))
        || media_type.eq_ignore_ascii_case("application/json")
        || media_type
            .rsplit_once('/')
            .is_some_and(|(_, subtype)| subtype.to_ascii_lowercase().ends_with("+json"))
        || media_type.eq_ignore_ascii_case("application/javascript")
        || media_type.eq_ignore_ascii_case("application/wasm")
        || media_type.eq_ignore_ascii_case("application/xml")
        || media_type.eq_ignore_ascii_case("image/svg+xml")
}

fn has_no_transform(headers: &HeaderMap) -> bool {
    headers.get_all(CACHE_CONTROL).iter().any(|value| {
        value.to_str().ok().is_some_and(|directives| {
            directives
                .split(',')
                .any(|directive| directive.trim().eq_ignore_ascii_case("no-transform"))
        })
    })
}

fn is_sse(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"))
}

fn if_none_match_fails(condition: &IfNoneMatch, headers: &HeaderMap) -> bool {
    if condition == &IfNoneMatch::any() {
        return true;
    }
    headers
        .typed_try_get::<ETag>()
        .ok()
        .flatten()
        .is_some_and(|current| !condition.precondition_passes(&current))
}

fn gzip_representation_etag(etag: &HeaderValue) -> Option<HeaderValue> {
    let etag = etag.to_str().ok()?;
    etag.parse::<ETag>().ok()?;
    if etag.starts_with("W/") {
        return None;
    }
    HeaderValue::from_str(&format!("{}--gzip\"", &etag[..etag.len() - 1])).ok()
}

fn valid_asset_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_EMBEDDED_PATH_BYTES
        && !path.starts_with('/')
        && !path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
}

fn content_etag(path: &'static str, contents: &[u8]) -> Result<HeaderValue, EmbeddedAssetError> {
    let digest = Sha256::digest(contents);
    let mut encoded = [b'0'; 66];
    encoded[0] = b'"';
    encoded[65] = b'"';
    for (index, byte) in digest.iter().copied().enumerate() {
        encoded[index * 2 + 1] = HEX_DIGITS[(byte >> 4) as usize];
        encoded[index * 2 + 2] = HEX_DIGITS[(byte & 0x0f) as usize];
    }
    HeaderValue::from_bytes(&encoded).map_err(|_| EmbeddedAssetError::HeaderEncoding { path })
}

fn is_content_fingerprinted(path: &str, contents: &[u8]) -> bool {
    let digest = Sha256::digest(contents);
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .split(['.', '-', '_'])
        .any(|part| fingerprint_matches_digest(part.as_bytes(), &digest))
}

fn fingerprint_matches_digest(fingerprint: &[u8], digest: &[u8]) -> bool {
    if fingerprint.len() < 8 || fingerprint.len() > digest.len() * 2 {
        return false;
    }
    fingerprint.iter().enumerate().all(|(index, encoded)| {
        let byte = digest[index / 2];
        let expected = if index % 2 == 0 {
            byte >> 4
        } else {
            byte & 0x0f
        };
        encoded.to_ascii_lowercase()
            == if expected < 10 {
                b'0' + expected
            } else {
                b'a' + expected - 10
            }
    })
}

fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn is_api_asset_path(path: &str) -> bool {
    path == "api" || path.starts_with("api/")
}

fn is_html_navigation(request: &Request) -> bool {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return false;
    }
    if request
        .headers()
        .get("sec-fetch-mode")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("navigate"))
    {
        return true;
    }
    request.headers().get_all("accept").iter().any(|value| {
        value.to_str().ok().is_some_and(|accepted| {
            accepted.split(',').any(|range| {
                let media_type = range.split(';').next().unwrap_or_default().trim();
                media_type.eq_ignore_ascii_case("text/html")
                    || media_type.eq_ignore_ascii_case("application/xhtml+xml")
            })
        })
    })
}

fn content_type_for_path(path: &str) -> &'static str {
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("html") {
        "text/html; charset=utf-8"
    } else if extension.eq_ignore_ascii_case("css") {
        "text/css; charset=utf-8"
    } else if extension.eq_ignore_ascii_case("js") || extension.eq_ignore_ascii_case("mjs") {
        "application/javascript; charset=utf-8"
    } else if extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("map") {
        "application/json"
    } else if extension.eq_ignore_ascii_case("svg") {
        "image/svg+xml"
    } else if extension.eq_ignore_ascii_case("png") {
        "image/png"
    } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
        "image/jpeg"
    } else if extension.eq_ignore_ascii_case("gif") {
        "image/gif"
    } else if extension.eq_ignore_ascii_case("webp") {
        "image/webp"
    } else if extension.eq_ignore_ascii_case("ico") {
        "image/x-icon"
    } else if extension.eq_ignore_ascii_case("woff") {
        "font/woff"
    } else if extension.eq_ignore_ascii_case("woff2") {
        "font/woff2"
    } else if extension.eq_ignore_ascii_case("ttf") {
        "font/ttf"
    } else if extension.eq_ignore_ascii_case("wasm") {
        "application/wasm"
    } else if extension.eq_ignore_ascii_case("xml") {
        "application/xml"
    } else if extension.eq_ignore_ascii_case("txt") {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use axum::body::{Body, to_bytes};
    use axum::http::header::{
        ACCEPT, ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE,
        ETAG, IF_NONE_MATCH, VARY,
    };
    use axum::http::{HeaderValue, Method, Request, StatusCode};
    use axum::routing::get;
    use axum::{Json, Router};
    use flate2::read::GzDecoder;
    use serde_json::json;
    use tower::ServiceExt;

    use super::{
        EmbeddedAsset, embedded_asset_router, http_delivery_router, mark_api_delivery_request,
    };

    const BODY_LIMIT: usize = 1024 * 1024;

    fn api_router() -> Router {
        let router = Router::new()
            .route(
                "/api/data",
                get(|| async {
                    (
                        [(ETAG, HeaderValue::from_static("\"data-v1\""))],
                        Json(json!({
                            "value": "a response large enough to exercise gzip compression"
                        })),
                    )
                }),
            )
            .route(
                "/api/events",
                get(|| async {
                    (
                        [(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))],
                        "event: item\ndata: this stream must never be compressed\n\n",
                    )
                }),
            )
            .route(
                "/api/no-transform",
                get(|| async {
                    (
                        [
                            (CONTENT_TYPE, HeaderValue::from_static("application/json")),
                            (
                                CACHE_CONTROL,
                                HeaderValue::from_static("private, no-transform"),
                            ),
                        ],
                        r#"{"value":"this response explicitly forbids transformation"}"#,
                    )
                }),
            )
            .route(
                "/api/precompressed",
                get(|| async {
                    (
                        [
                            (CONTENT_TYPE, HeaderValue::from_static("application/json")),
                            (CONTENT_ENCODING, HeaderValue::from_static("gzip")),
                            (CONTENT_LENGTH, HeaderValue::from_static("18")),
                        ],
                        "already-compressed",
                    )
                }),
            )
            .route(
                "/api/created",
                get(|| async {
                    (
                        StatusCode::CREATED,
                        Json(json!({"value": "an existing untagged representation"})),
                    )
                }),
            )
            .route(
                "/api/comma-tag",
                get(|| async {
                    (
                        [(ETAG, HeaderValue::from_static("\"release,2026\""))],
                        Json(json!({"value": "an opaque validator containing a comma"})),
                    )
                }),
            );
        http_delivery_router(router)
    }

    fn static_router() -> Router {
        embedded_asset_router(
            [
                EmbeddedAsset::new(
                    "index.html",
                    b"<!doctype html><html><body>TraceDecay shell</body></html>",
                ),
                EmbeddedAsset::new(
                    "static/app.56e5f3600934df7e.js",
                    b"globalThis.TRACEDECAY = 'embedded and compressible';",
                ),
                EmbeddedAsset::new("static/plain.css", b"body { color: #123456; }"),
            ],
            "index.html",
        )
        .expect("valid embedded asset set")
    }

    #[test]
    fn embedded_assets_reject_the_api_namespace_at_admission() {
        let result = embedded_asset_router(
            [
                EmbeddedAsset::new("index.html", b"shell"),
                EmbeddedAsset::new("api/status.json", b"{}"),
            ],
            "index.html",
        );
        assert!(matches!(
            result,
            Err(super::EmbeddedAssetError::ApiPath {
                path: "api/status.json"
            })
        ));
    }

    #[test]
    fn embedded_asset_etags_are_derived_from_exact_contents() {
        let shell =
            super::content_etag("index.html", b"shell").expect("digest is a valid strong ETag");
        let copy = super::content_etag("static/copy.txt", b"shell")
            .expect("digest is a valid strong ETag");
        let changed = super::content_etag("static/changed.txt", b"changed")
            .expect("digest is a valid strong ETag");
        assert_eq!(
            shell,
            "\"ce635c4eabff5e4f56dba8fb1e39ca235530aa2b6b18533eef1af3862016c577\""
        );
        assert_eq!(shell, copy);
        assert_ne!(shell, changed);
    }

    #[tokio::test]
    async fn conditional_get_distinguishes_stale_and_current_strong_etags() {
        let stale = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/data")
                    .header(IF_NONE_MATCH, "\"data-v0\"")
                    .body(Body::empty())
                    .expect("valid stale request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(stale.status(), StatusCode::OK);
        assert_eq!(stale.headers()[ETAG], "\"data-v1\"");
        assert_eq!(stale.headers()[CACHE_CONTROL], "no-store");
        assert!(
            !to_bytes(stale.into_body(), BODY_LIMIT)
                .await
                .expect("bounded body")
                .is_empty()
        );

        let current = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/data")
                    .header(IF_NONE_MATCH, "\"other\", W/\"data-v1\"")
                    .body(Body::empty())
                    .expect("valid current request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(current.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(current.headers()[ETAG], "\"data-v1\"");
        assert_eq!(current.headers()[CACHE_CONTROL], "no-store");
        assert!(
            to_bytes(current.into_body(), BODY_LIMIT)
                .await
                .expect("bounded body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rewritten_api_request_retains_no_store_delivery_policy() {
        let router = http_delivery_router(Router::new().route(
            "/data",
            get(|| async { Json(json!({"value": "rewritten"})) }),
        ));
        let mut request = Request::builder()
            .uri("/data")
            .body(Body::empty())
            .expect("valid rewritten API request");
        mark_api_delivery_request(&mut request);

        let response = router.oneshot(request).await.expect("infallible router");

        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn wildcard_if_none_match_matches_an_existing_untagged_success() {
        let response = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/created")
                    .header(IF_NONE_MATCH, "*")
                    .body(Body::empty())
                    .expect("valid wildcard conditional request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(!response.headers().contains_key(ETAG));
        assert!(
            to_bytes(response.into_body(), BODY_LIMIT)
                .await
                .expect("bounded body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn if_none_match_parses_a_quoted_opaque_tag_containing_a_comma() {
        let response = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/comma-tag")
                    .header(IF_NONE_MATCH, "\"release,2026\"")
                    .body(Body::empty())
                    .expect("valid comma-tag conditional request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn if_none_match_evaluates_all_repeated_header_fields() {
        let response = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/comma-tag")
                    .header(IF_NONE_MATCH, "\"stale\"")
                    .header(IF_NONE_MATCH, "\"release,2026\"")
                    .body(Body::empty())
                    .expect("valid repeated conditional request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn head_preserves_get_metadata_without_a_body() {
        let response = api_router()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/data")
                    .body(Body::empty())
                    .expect("valid HEAD request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[ETAG], "\"data-v1\"");
        assert!(response.headers().contains_key(CONTENT_LENGTH));
        assert!(
            to_bytes(response.into_body(), BODY_LIMIT)
                .await
                .expect("bounded body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn gzip_head_has_get_equivalent_encoding_metadata_without_a_body() {
        let response = api_router()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/data")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("valid gzip HEAD request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_ENCODING], "gzip");
        assert_eq!(response.headers()[VARY], "accept-encoding");
        assert_eq!(response.headers()[ETAG], "\"data-v1--gzip\"");
        assert!(
            to_bytes(response.into_body(), BODY_LIMIT)
                .await
                .expect("bounded body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn head_preserves_handler_length_for_an_already_encoded_representation() {
        let response = api_router()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/precompressed")
                    .body(Body::empty())
                    .expect("valid precompressed HEAD request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_ENCODING], "gzip");
        assert_eq!(response.headers()[CONTENT_LENGTH], "18");
        assert!(
            to_bytes(response.into_body(), BODY_LIMIT)
                .await
                .expect("bounded body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn eligible_json_gzip_round_trips() {
        let response = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/data")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("valid gzip request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_ENCODING], "gzip");
        let compressed = to_bytes(response.into_body(), BODY_LIMIT)
            .await
            .expect("bounded compressed body");
        let mut decoded = String::new();
        GzDecoder::new(compressed.as_ref())
            .read_to_string(&mut decoded)
            .expect("valid gzip response");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&decoded).expect("valid JSON"),
            json!({"value": "a response large enough to exercise gzip compression"})
        );
    }

    #[tokio::test]
    async fn gzip_uses_a_distinct_strong_etag_for_conditional_requests() {
        let identity = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/data")
                    .body(Body::empty())
                    .expect("valid identity request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(identity.headers()[ETAG], "\"data-v1\"");

        let gzip = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/data")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("valid gzip request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(gzip.headers()[CONTENT_ENCODING], "gzip");
        assert_eq!(gzip.headers()[ETAG], "\"data-v1--gzip\"");

        let identity_validator = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/data")
                    .header(ACCEPT_ENCODING, "gzip")
                    .header(IF_NONE_MATCH, "\"data-v1\"")
                    .body(Body::empty())
                    .expect("valid conditional gzip request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(identity_validator.status(), StatusCode::OK);

        let current = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/data")
                    .header(ACCEPT_ENCODING, "gzip")
                    .header(IF_NONE_MATCH, "\"data-v1--gzip\"")
                    .body(Body::empty())
                    .expect("valid conditional gzip request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(current.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(current.headers()[CONTENT_ENCODING], "gzip");
        assert_eq!(current.headers()[VARY], "accept-encoding");
        assert_eq!(current.headers()[ETAG], "\"data-v1--gzip\"");
    }

    #[tokio::test]
    async fn sse_and_no_transform_responses_are_never_compressed() {
        let sse = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/events")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("valid SSE request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(sse.headers()[CACHE_CONTROL], "no-cache, no-transform");
        assert!(!sse.headers().contains_key(CONTENT_ENCODING));

        let no_transform = api_router()
            .oneshot(
                Request::builder()
                    .uri("/api/no-transform")
                    .header(ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("valid no-transform request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(
            no_transform.headers()[CACHE_CONTROL],
            "private, no-transform"
        );
        assert!(!no_transform.headers().contains_key(CONTENT_ENCODING));
    }

    #[tokio::test]
    async fn embedded_assets_apply_content_type_and_cache_policies() {
        let shell = static_router()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("valid shell request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(shell.headers()[CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(shell.headers()[CACHE_CONTROL], "no-cache");
        assert_eq!(
            shell.headers()[ETAG],
            "\"6e60d3062f6ed12a2e9cd2d3e4445cd68dbc92b124d9b8201e782acf98b3b285\""
        );

        let fingerprinted = static_router()
            .oneshot(
                Request::builder()
                    .uri("/static/app.56e5f3600934df7e.js")
                    .body(Body::empty())
                    .expect("valid asset request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(
            fingerprinted.headers()[CONTENT_TYPE],
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            fingerprinted.headers()[CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );

        let plain = static_router()
            .oneshot(
                Request::builder()
                    .uri("/static/plain.css")
                    .body(Body::empty())
                    .expect("valid asset request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(plain.headers()[CONTENT_TYPE], "text/css; charset=utf-8");
        assert_eq!(plain.headers()[CACHE_CONTROL], "no-cache");
    }

    #[tokio::test]
    async fn hex_like_filename_without_matching_content_digest_is_not_immutable() {
        let router = embedded_asset_router(
            [
                EmbeddedAsset::new("index.html", b"shell"),
                EmbeddedAsset::new(
                    "static/app.0123456789abcdef.js",
                    b"content whose digest is not the filename token",
                ),
            ],
            "index.html",
        )
        .expect("valid embedded asset set");
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/static/app.0123456789abcdef.js")
                    .body(Body::empty())
                    .expect("valid asset request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-cache");
    }

    #[tokio::test]
    async fn spa_fallback_is_navigation_only_and_never_handles_api_paths() {
        let navigation = static_router()
            .oneshot(
                Request::builder()
                    .uri("/brain")
                    .header(ACCEPT, "text/html,application/xhtml+xml")
                    .body(Body::empty())
                    .expect("valid navigation"),
            )
            .await
            .expect("infallible router");
        assert_eq!(navigation.status(), StatusCode::OK);
        let body = to_bytes(navigation.into_body(), BODY_LIMIT)
            .await
            .expect("bounded shell body");
        assert!(body.starts_with(b"<!doctype html>"));

        for path in ["/missing.js", "/api/missing", "/api"] {
            let response = static_router()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(ACCEPT, "text/html")
                        .body(Body::empty())
                        .expect("valid missing request"),
                )
                .await
                .expect("infallible router");
            let expected = if path == "/missing.js" {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            };
            assert_eq!(response.status(), expected, "{path}");
        }

        let non_navigation = static_router()
            .oneshot(
                Request::builder()
                    .uri("/missing.js")
                    .header(ACCEPT, "application/javascript")
                    .body(Body::empty())
                    .expect("valid non-navigation request"),
            )
            .await
            .expect("infallible router");
        assert_eq!(non_navigation.status(), StatusCode::NOT_FOUND);
    }
}

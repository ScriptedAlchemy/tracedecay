//! Static single-page application transport policy.
//!
//! The executable owns the embedded bytes because its build script is the only
//! place that can resolve its `OUT_DIR`. This module owns the HTTP behavior
//! around those bytes: asset lookup, cache headers, entity tags, and the rule
//! that an API request can never be answered with the single-page app.
//!
//! Compressible dashboard assets may be gzip-embedded (`StaticAssetEncoding::Gzip`).
//! When the client advertises `Accept-Encoding: gzip`, those bytes are served
//! with `Content-Encoding: gzip` so the binary keeps the compressed form and
//! the browser inflates. Clients that omit gzip are served an identity body
//! decoded here.

use std::io::Read;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, http::StatusCode};
use flate2::read::GzDecoder;
use http_encoding_headers::{Encoding, decode_header_value};

/// Wire encoding of [`StaticDashboardAsset::contents`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticAssetEncoding {
    /// `contents` are the exact bytes clients receive for an identity response.
    Identity,
    /// `contents` are gzip-compressed; inflate before identity responses, or
    /// pass through with `Content-Encoding: gzip` when the client accepts it.
    Gzip,
}

/// One immutable embedded dashboard asset supplied by the owning binary.
#[derive(Clone, Copy)]
pub struct StaticDashboardAsset {
    pub path: &'static str,
    pub contents: &'static [u8],
    pub content_type: &'static str,
    pub encoding: StaticAssetEncoding,
}

/// Byte authority for a dashboard bundle embedded by an executable build.
///
/// The API crate deliberately receives this narrow source instead of reading
/// the filesystem or depending on the binary crate. That keeps generated
/// `OUT_DIR` ownership at the build-script boundary while keeping all HTTP
/// presentation behavior in the canonical API crate.
pub trait DashboardAssetSource: Send + Sync + 'static {
    fn asset_by_path(&self, path: &str) -> Option<StaticDashboardAsset>;
    fn cache_tag(&self) -> &str;
}

/// A static asset source for binaries that can expose their generated manifest
/// as a static slice. It also makes the adapter directly testable without a
/// filesystem or a second router implementation.
#[derive(Clone, Copy)]
pub struct StaticDashboardAssets {
    pub assets: &'static [StaticDashboardAsset],
    pub cache_tag: &'static str,
}

impl DashboardAssetSource for StaticDashboardAssets {
    fn asset_by_path(&self, path: &str) -> Option<StaticDashboardAsset> {
        self.assets.iter().copied().find(|asset| asset.path == path)
    }

    fn cache_tag(&self) -> &str {
        self.cache_tag
    }
}

/// Build the complete static dashboard router.
///
/// It owns `/`, `/static/{*tail}`, and the fallback for client-side routes.
/// `/api` and `/api/**` deliberately answer `404` from the fallback, so a
/// mistyped or unavailable API path never becomes a successful HTML response.
pub fn static_dashboard_router(source: Arc<dyn DashboardAssetSource>) -> Router {
    Router::new()
        .route("/", get(app_index))
        .route("/static/{*tail}", get(app_static))
        .fallback(get(app_spa_fallback))
        .with_state(source)
}

async fn app_index(
    State(source): State<Arc<dyn DashboardAssetSource>>,
    headers: HeaderMap,
) -> Response {
    {
        let _span = tracing::trace_span!("api.http.assets").entered();
        {
            match source.asset_by_path("index.html") {
                Some(asset) => {
                    app_response(&headers, asset, source.cache_tag(), CachePolicy::Revalidate)
                }
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}

async fn app_static(
    State(source): State<Arc<dyn DashboardAssetSource>>,
    headers: HeaderMap,
    Path(tail): Path<String>,
) -> Response {
    {
        let _span = tracing::trace_span!("api.http.static").entered();
        {
            let asset_path = format!("static/{tail}");
            let cache_policy = if fingerprinted_static_asset_path(&asset_path) {
                CachePolicy::Immutable
            } else {
                CachePolicy::Revalidate
            };
            match source.asset_by_path(&asset_path) {
                Some(asset) => app_response(&headers, asset, source.cache_tag(), cache_policy),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}

async fn app_spa_fallback(
    State(source): State<Arc<dyn DashboardAssetSource>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    {
        let _span = tracing::trace_span!("api.http.spa").entered();
        {
            if uri.path() == "/api" || uri.path().starts_with("/api/") {
                return StatusCode::NOT_FOUND.into_response();
            }
            match source.asset_by_path("index.html") {
                Some(asset) => {
                    app_response(&headers, asset, source.cache_tag(), CachePolicy::Revalidate)
                }
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum CachePolicy {
    Revalidate,
    Immutable,
}

impl CachePolicy {
    const fn header_value(self) -> &'static str {
        match self {
            Self::Revalidate => "no-cache",
            Self::Immutable => "public, max-age=31536000, immutable",
        }
    }
}

fn fingerprinted_static_asset_path(path: &str) -> bool {
    path.strip_prefix("static/").is_some_and(|relative| {
        relative.split('.').any(|segment| {
            segment.len() >= 8 && segment.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    })
}

fn response_encoding(
    headers: &HeaderMap,
    stored: StaticAssetEncoding,
) -> Result<StaticAssetEncoding, StatusCode> {
    if !headers.contains_key(header::ACCEPT_ENCODING) {
        return Ok(StaticAssetEncoding::Identity);
    }
    let mut preferences = Vec::new();
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let value = value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?;
        if !value.trim().is_empty() {
            preferences.extend(decode_header_value(value).map_err(|_| StatusCode::BAD_REQUEST)?);
        }
    }
    if preferences
        .iter()
        .any(|(_, quality)| !(0.0..=1.0).contains(quality))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let quality = |encoding| {
        preferences
            .iter()
            .find_map(|(candidate, quality)| (candidate == &encoding).then_some(*quality))
    };
    let wildcard = quality(Encoding::Wildcard);
    let gzip = quality(Encoding::Gzip).or(wildcard).unwrap_or(0.0);
    // Identity remains acceptable unless explicitly excluded. An explicit
    // identity preference can outrank gzip; otherwise prefer the stored bytes.
    let identity =
        quality(Encoding::Identity).unwrap_or(if wildcard == Some(0.0) { 0.0 } else { 1.0 });
    if stored == StaticAssetEncoding::Gzip
        && gzip > 0.0
        && quality(Encoding::Identity).is_none_or(|weight| gzip >= weight)
    {
        Ok(StaticAssetEncoding::Gzip)
    } else if identity > 0.0 {
        Ok(StaticAssetEncoding::Identity)
    } else {
        Err(StatusCode::NOT_ACCEPTABLE)
    }
}

fn inflate_gzip(bytes: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut decoder = GzDecoder::new(bytes);
    let mut inflated = Vec::new();
    decoder.read_to_end(&mut inflated)?;
    Ok(inflated)
}

fn app_response(
    headers: &HeaderMap,
    asset: StaticDashboardAsset,
    cache_tag: &str,
    cache_policy: CachePolicy,
) -> Response {
    let encoding = match response_encoding(headers, asset.encoding) {
        Ok(encoding) => encoding,
        Err(status) => return status.into_response(),
    };
    let gzip = encoding == StaticAssetEncoding::Gzip;
    let entity_tag = if gzip {
        format!("\"{cache_tag}-gzip\"")
    } else {
        format!("\"{cache_tag}\"")
    };
    let hit = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value == "*"
                || value
                    .split(',')
                    .any(|tag| tag.trim().trim_start_matches("W/") == entity_tag)
        });
    let mut response = if hit {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        match asset.encoding {
            StaticAssetEncoding::Identity => asset.contents.into_response(),
            StaticAssetEncoding::Gzip if gzip => asset.contents.into_response(),
            StaticAssetEncoding::Gzip => match inflate_gzip(asset.contents) {
                Ok(inflated) => inflated.into_response(),
                Err(error) => {
                    tracing::error!(
                        path = asset.path,
                        error = %error,
                        "embedded gzip dashboard asset could not be decoded"
                    );
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            },
        }
    };
    let response_headers = response.headers_mut();
    if asset.encoding == StaticAssetEncoding::Gzip {
        response_headers.insert(
            header::VARY,
            header::HeaderValue::from_static("accept-encoding"),
        );
    }
    if gzip {
        response_headers.insert(
            header::CONTENT_ENCODING,
            header::HeaderValue::from_static("gzip"),
        );
    }
    if let Ok(value) = header::HeaderValue::from_str(asset.content_type) {
        response_headers.insert(header::CONTENT_TYPE, value);
    }
    response_headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(cache_policy.header_value()),
    );
    if let Ok(etag) = header::HeaderValue::from_str(&entity_tag) {
        response_headers.insert(header::ETAG, etag);
    }
    response
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tower::ServiceExt;

    use super::{
        StaticAssetEncoding, StaticDashboardAsset, StaticDashboardAssets, static_dashboard_router,
    };

    const ASSETS: &[StaticDashboardAsset] = &[
        StaticDashboardAsset {
            path: "index.html",
            contents: b"<html>TraceDecay</html>",
            content_type: "text/html; charset=utf-8",
            encoding: StaticAssetEncoding::Identity,
        },
        StaticDashboardAsset {
            path: "static/app.abc12345.js",
            contents: b"console.log('dashboard')",
            content_type: "application/javascript",
            encoding: StaticAssetEncoding::Identity,
        },
        StaticDashboardAsset {
            path: "static/unversioned.js",
            contents: b"console.log('must revalidate')",
            content_type: "application/javascript",
            encoding: StaticAssetEncoding::Identity,
        },
    ];

    fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(raw).expect("gzip");
        encoder.finish().expect("gzip finish")
    }

    fn router() -> axum::Router {
        static_dashboard_router(Arc::new(StaticDashboardAssets {
            assets: ASSETS,
            cache_tag: "bundle.1",
        }))
    }

    #[tokio::test]
    async fn api_fallback_never_returns_dashboard_html() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/api/not-a-real-route")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .expect("not-found body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn client_routes_revalidate_but_fingerprinted_assets_are_immutable() {
        let index = router()
            .oneshot(
                Request::builder()
                    .uri("/delivery")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("index response");
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(
            index
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
        assert_eq!(
            index
                .headers()
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok()),
            Some("\"bundle.1\"")
        );

        let static_asset = router()
            .oneshot(
                Request::builder()
                    .uri("/static/app.abc12345.js")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("asset response");
        assert_eq!(static_asset.status(), StatusCode::OK);
        assert_eq!(
            static_asset
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("public, max-age=31536000, immutable")
        );
    }

    #[tokio::test]
    async fn weak_matching_etag_returns_not_modified_for_the_html_shell() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::IF_NONE_MATCH, "W/\"bundle.1\"")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router response");

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
    }

    #[tokio::test]
    async fn unversioned_static_assets_revalidate_instead_of_being_immutable() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/static/unversioned.js")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
    }

    #[tokio::test]
    async fn gzip_embedded_assets_pass_through_when_client_accepts_gzip() {
        let raw = b"console.log('gzip-dashboard')";
        let compressed = gzip_bytes(raw);
        // Leak so the asset slice can be 'static for the router state.
        let compressed_static: &'static [u8] = Box::leak(compressed.into_boxed_slice());
        let assets: &'static [StaticDashboardAsset] = Box::leak(Box::new([StaticDashboardAsset {
            path: "index.html",
            contents: compressed_static,
            content_type: "text/html; charset=utf-8",
            encoding: StaticAssetEncoding::Gzip,
        }]));
        let response = static_dashboard_router(Arc::new(StaticDashboardAssets {
            assets,
            cache_tag: "gz.1",
        }))
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::ACCEPT_ENCODING, "gzip, deflate")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            Some("gzip")
        );
        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        assert_eq!(body.as_ref(), compressed_static);
    }

    #[tokio::test]
    async fn gzip_embedded_assets_inflate_when_client_omits_gzip() {
        let raw = b"<html>inflated</html>";
        let compressed = gzip_bytes(raw);
        let compressed_static: &'static [u8] = Box::leak(compressed.into_boxed_slice());
        let assets: &'static [StaticDashboardAsset] = Box::leak(Box::new([StaticDashboardAsset {
            path: "index.html",
            contents: compressed_static,
            content_type: "text/html; charset=utf-8",
            encoding: StaticAssetEncoding::Gzip,
        }]));
        let response = static_dashboard_router(Arc::new(StaticDashboardAssets {
            assets,
            cache_tag: "gz.2",
        }))
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        assert_eq!(body.as_ref(), raw);
    }
    #[tokio::test]
    async fn compressed_asset_validators_follow_the_selected_representation() {
        let raw = b"<html>cache variants</html>";
        let compressed = Box::leak(gzip_bytes(raw).into_boxed_slice());
        let asset = StaticDashboardAsset {
            path: "index.html",
            contents: compressed,
            content_type: "text/html; charset=utf-8",
            encoding: StaticAssetEncoding::Gzip,
        };
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
        let encoded =
            super::app_response(&headers, asset, "bundle", super::CachePolicy::Revalidate);
        let encoded_etag = encoded.headers()[header::ETAG].clone();
        assert_eq!(encoded.headers()[header::VARY], "accept-encoding");
        headers.insert(header::IF_NONE_MATCH, encoded_etag);
        let unchanged =
            super::app_response(&headers, asset, "bundle", super::CachePolicy::Revalidate);
        assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(unchanged.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(unchanged.headers()[header::VARY], "accept-encoding");
        headers.remove(header::ACCEPT_ENCODING);
        let identity =
            super::app_response(&headers, asset, "bundle", super::CachePolicy::Revalidate);
        assert_eq!(identity.status(), StatusCode::OK);
        assert_eq!(identity.headers()[header::VARY], "accept-encoding");
        assert!(identity.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(
            to_bytes(identity.into_body(), 1024).await.unwrap().as_ref(),
            raw
        );
    }

    #[test]
    fn corrupt_embedded_assets_do_not_receive_success_cache_headers() {
        let asset = StaticDashboardAsset {
            path: "static/bad.abc12345.js",
            contents: b"invalid gzip",
            content_type: "application/javascript",
            encoding: StaticAssetEncoding::Gzip,
        };
        let response = super::app_response(
            &axum::http::HeaderMap::new(),
            asset,
            "bundle",
            super::CachePolicy::Revalidate,
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().get(header::ETAG).is_none());
        assert!(response.headers().get(header::CACHE_CONTROL).is_none());
    }
    #[tokio::test]
    async fn asset_responses_respect_encoding_rejections_and_preferences() {
        let raw = b"<html>encoding preferences</html>";
        let compressed = Box::leak(gzip_bytes(raw).into_boxed_slice());
        let asset = StaticDashboardAsset {
            path: "index.html",
            contents: compressed,
            content_type: "text/html",
            encoding: StaticAssetEncoding::Gzip,
        };
        for (accept, status, encoded) in [
            ("gzip;q=0", StatusCode::OK, false),
            ("", StatusCode::OK, false),
            ("GZIP", StatusCode::OK, true),
            ("gzip;q=invalid", StatusCode::BAD_REQUEST, false),
            ("gzip;q=NaN", StatusCode::BAD_REQUEST, false),
            ("gzip;q=1.2", StatusCode::BAD_REQUEST, false),
            ("gzip;q=0, *;q=1", StatusCode::OK, false),
            ("gzip;q=0.5, identity;q=0.8", StatusCode::OK, false),
            ("gzip;q=0.8, identity;q=0.2", StatusCode::OK, true),
            ("*;q=0.5", StatusCode::OK, true),
            ("gzip;q=0, identity;q=0", StatusCode::NOT_ACCEPTABLE, false),
            ("*;q=0", StatusCode::NOT_ACCEPTABLE, false),
        ] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, accept.parse().unwrap());
            let response =
                super::app_response(&headers, asset, "bundle", super::CachePolicy::Revalidate);
            assert_eq!(response.status(), status, "{accept}");
            assert_eq!(
                response.headers().contains_key(header::CONTENT_ENCODING),
                encoded,
                "{accept}"
            );
            if status == StatusCode::OK {
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                assert_eq!(
                    body.as_ref(),
                    if encoded { &*compressed } else { raw },
                    "{accept}"
                );
            }
        }
    }
}

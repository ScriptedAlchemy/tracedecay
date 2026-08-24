//! Static single-page application transport policy.
//!
//! The executable owns the embedded bytes because its build script is the only
//! place that can resolve its `OUT_DIR`. This module owns the HTTP behavior
//! around those bytes: asset lookup, cache headers, entity tags, and the rule
//! that an API request can never be answered with the single-page app.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, http::StatusCode};

/// One immutable embedded dashboard asset supplied by the owning binary.
#[derive(Clone, Copy)]
pub struct StaticDashboardAsset {
    pub path: &'static str,
    pub contents: &'static [u8],
    pub content_type: &'static str,
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
    match source.asset_by_path("index.html") {
        Some(asset) => app_response(&headers, asset, source.cache_tag(), CachePolicy::Revalidate),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn app_static(
    State(source): State<Arc<dyn DashboardAssetSource>>,
    headers: HeaderMap,
    Path(tail): Path<String>,
) -> Response {
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

async fn app_spa_fallback(
    State(source): State<Arc<dyn DashboardAssetSource>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if uri.path() == "/api" || uri.path().starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    match source.asset_by_path("index.html") {
        Some(asset) => app_response(&headers, asset, source.cache_tag(), CachePolicy::Revalidate),
        None => StatusCode::NOT_FOUND.into_response(),
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

fn app_response(
    headers: &HeaderMap,
    asset: StaticDashboardAsset,
    cache_tag: &str,
    cache_policy: CachePolicy,
) -> Response {
    let entity_tag = format!("\"{cache_tag}\"");
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
        asset.contents.into_response()
    };
    let response_headers = response.headers_mut();
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
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::{StaticDashboardAsset, StaticDashboardAssets, static_dashboard_router};

    const ASSETS: &[StaticDashboardAsset] = &[
        StaticDashboardAsset {
            path: "index.html",
            contents: b"<html>TraceDecay</html>",
            content_type: "text/html; charset=utf-8",
        },
        StaticDashboardAsset {
            path: "static/app.abc12345.js",
            contents: b"console.log('dashboard')",
            content_type: "application/javascript",
        },
        StaticDashboardAsset {
            path: "static/unversioned.js",
            contents: b"console.log('must revalidate')",
            content_type: "application/javascript",
        },
    ];

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
}

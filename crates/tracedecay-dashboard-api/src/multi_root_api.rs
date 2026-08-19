//! Named multi-root collection resolution for the dashboard.
//!
//! The dashboard resolves a named collection through the daemon application
//! transport; selection precedence and read mapping are owned by the
//! application resolver. No default collection is currently configurable —
//! the retired `query.default_collection.v1` setting fails closed in old
//! stores and no replacement setting exists — so an unnamed resolution
//! reports the typed no-collection state instead of guessing a scope set.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tracedecay_api::read_model::multi_root::MultiRootCapabilityV1;
use tracedecay_application::{
    MultiRootCollectionResolutionV1, MultiRootCollectionSelectorV1,
    MultiRootCollectionUnavailableV1,
};
use tracedecay_domain::ScopeSetId;

use super::{DashboardHttpRequestControlV1, DashboardState};

#[derive(Deserialize)]
pub struct CollectionQueryV1 {
    pub collection: Option<String>,
}

/// `GET /api/multi-root/collection` — resolve the selected named collection.
///
/// An explicit `collection` query parameter names the target; without one the
/// selector falls through to the (currently absent) default collection and
/// reports the typed no-collection state.
pub async fn resolve_collection(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    Query(query): Query<CollectionQueryV1>,
) -> Response {
    let explicit_target = match query.collection {
        Some(raw) => match ScopeSetId::new(raw) {
            Ok(collection) => Some(collection),
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "code": "multi_root.invalid_collection",
                        "detail": format!("collection must name one canonical scope set: {error}"),
                    })),
                )
                    .into_response();
            }
        },
        None => None,
    };
    let capability = resolve_collection_capability(
        &state,
        control.map(|Extension(control)| control),
        explicit_target,
    )
    .await;
    Json(capability).into_response()
}

/// Shared resolution used by the collection route and `/api/capabilities`.
pub(crate) async fn resolve_collection_capability(
    state: &DashboardState,
    control: Option<DashboardHttpRequestControlV1>,
    explicit_target: Option<ScopeSetId>,
) -> MultiRootCapabilityV1 {
    let Some(runtime) = state.application_invocation_executor.as_deref() else {
        return MultiRootCapabilityV1::unavailable(
            MultiRootCollectionUnavailableV1::TransportNotAdmitted.reason(),
        );
    };
    let selector = MultiRootCollectionSelectorV1::new(explicit_target, None);
    let Some(target) = selector.target().cloned() else {
        return MultiRootCapabilityV1::unavailable(
            MultiRootCollectionUnavailableV1::NoCollectionNamed.reason(),
        );
    };
    let Some(control) = control else {
        return MultiRootCapabilityV1::unavailable(
            MultiRootCollectionUnavailableV1::AuthorityUnavailable {
                detail: "dashboard HTTP request admission is unavailable".to_owned(),
            }
            .reason(),
        );
    };
    let read = match runtime
        .read_multi_root_scope_set(control, target.clone())
        .await
    {
        Ok(read) => read,
        Err(unavailable) => {
            return MultiRootCapabilityV1::unavailable(
                MultiRootCollectionUnavailableV1::AuthorityUnavailable {
                    detail: unavailable.detail,
                }
                .reason(),
            );
        }
    };
    match MultiRootCollectionResolutionV1::from_persisted_read(&target, read) {
        MultiRootCollectionResolutionV1::Mounted { scope_set } => {
            MultiRootCapabilityV1::mounted(&scope_set)
        }
        MultiRootCollectionResolutionV1::Unavailable { reason } => {
            MultiRootCapabilityV1::unavailable(reason.reason())
        }
    }
}

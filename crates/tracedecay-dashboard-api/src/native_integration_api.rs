//! Read-only native-integration status for the dashboard.
//!
//! The dashboard consumes the same application result the CLI and MCP
//! surfaces project (`NativeIntegrationSurfaceResultV1`), resolved through
//! the daemon transport under the live request controls. No mutating
//! native-integration operation is reachable here: the dashboard can observe
//! a transaction but never advance one, apply edits, or mutate Git.

use axum::Json;
use axum::extract::{Extension, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tracedecay_application::{
    NativeIntegrationSurfaceResultV1, NativeIntegrationSurfaceUnavailableV1,
};
use tracedecay_domain::NativeIntegrationTransactionId;

use super::{DashboardHttpRequestControlV1, DashboardState};

#[derive(Deserialize)]
pub struct NativeIntegrationStatusQueryV1 {
    pub transaction_id: String,
}

/// `GET /api/native-integration/status` — read one transaction status.
pub async fn status(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
    Query(query): Query<NativeIntegrationStatusQueryV1>,
) -> Response {
    let transaction_id = match NativeIntegrationTransactionId::new(query.transaction_id) {
        Ok(transaction_id) => transaction_id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "code": "native_integration.invalid_transaction",
                    "detail": format!(
                        "transaction_id must name one canonical transaction: {error}"
                    ),
                })),
            )
                .into_response();
        }
    };
    // A dashboard without the daemon application transport, or without live
    // request admission, has no authority to consult; that is the same typed
    // unmounted state the daemon answers for a project without the runtime.
    let (Some(runtime), Some(Extension(control))) =
        (state.application_invocation_executor.as_deref(), control)
    else {
        return Json(NativeIntegrationSurfaceResultV1::unavailable(
            NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted,
        ))
        .into_response();
    };
    match runtime
        .native_integration_status(control, transaction_id)
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(unavailable) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "code": "native_integration.transport_unavailable",
                "detail": unavailable.detail,
            })),
        )
            .into_response(),
    }
}

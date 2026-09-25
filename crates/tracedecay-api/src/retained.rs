//! Public HTTP helpers shared by the retained application operations.

use axum::response::Response;
use tracedecay_contracts::RequestId;
use tracedecay_contracts::retained_surfaces::RetainedSurfaceOperation;

use crate::http::invalid_request_response;

pub fn retained_route_path(operation: RetainedSurfaceOperation) -> String {
    format!("/retained/{}", operation.as_str())
}

pub fn retained_invalid_request_response(request_id: RequestId) -> Response {
    invalid_request_response(
        request_id,
        "retained.invalid_request",
        "The retained application request is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broad_translator_names_are_not_callable_routes() {
        for name in ["session_refresh", "fact_store"] {
            assert_eq!(RetainedSurfaceOperation::from_operation_name(name), None);
        }
    }
}

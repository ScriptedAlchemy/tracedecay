//! Retained memory, session, and workflow request bodies.

use tracedecay_contracts::retained_surfaces::{
    FactFeedbackRequestV1, FactStoreAddRequestV1, FactStoreContradictRequestV1,
    FactStoreCurateRequestV1, FactStoreGetRequestV1, FactStoreListRequestV1,
    FactStoreProbeRequestV1, FactStoreReasonRequestV1, FactStoreRelatedRequestV1,
    FactStoreRemoveRequestV1, FactStoreSearchRequestV1, FactStoreSupersedeRequestV1,
    FactStoreUpdateRequestV1, LcmDescribeRequestV1, LcmDoctorRequestV1, LcmExpandQueryRequestV1,
    LcmExpandRequestV1, LcmGrepRequestV1, LcmLoadSessionRequestV1, LcmStatusRequestV1,
    MemoryStatusRequestV1, MessageSearchRequestV1, RetainedSurfaceOperation,
    RetainedSurfaceRequestV1, SessionRefreshActionRequestV1, SessionRefreshActionV1,
    SessionRefreshRequestV1, SessionsForRequestV1, WorkflowsRequestV1,
};

/// Decode one retained operation body into its typed request.
///
/// HTTP decodes the route body directly; MCP and the `tracedecay tool` CLI
/// decode the transport-normalized arguments through the same function, so
/// every surface lands on one canonical request. The returned error carries
/// the exact serde diagnostic (unknown field, unknown enum variant with the
/// admitted values, wrong type) so every dispatch surface can hand the caller
/// a corrective message instead of a blank "invalid request".
#[hotpath::measure(label = "application_surface.retained.decode")]
pub fn decode_retained_request(
    operation: RetainedSurfaceOperation,
    body: serde_json::Value,
) -> Result<RetainedSurfaceRequestV1, serde_json::Error> {
    macro_rules! decode {
        ($request:ty, $variant:ident) => {
            serde_path_to_error::deserialize::<_, $request>(body)
                .map(RetainedSurfaceRequestV1::$variant)
                .map_err(named_argument_error)
        };
    }
    match operation {
        RetainedSurfaceOperation::FactStoreCurate => {
            decode!(FactStoreCurateRequestV1, FactStoreCurate)
        }
        RetainedSurfaceOperation::FactStoreAdd => {
            decode!(FactStoreAddRequestV1, FactStoreAdd)
        }
        RetainedSurfaceOperation::FactStoreSearch => {
            decode!(FactStoreSearchRequestV1, FactStoreSearch)
        }
        RetainedSurfaceOperation::FactStoreProbe => {
            decode!(FactStoreProbeRequestV1, FactStoreProbe)
        }
        RetainedSurfaceOperation::FactStoreRelated => {
            decode!(FactStoreRelatedRequestV1, FactStoreRelated)
        }
        RetainedSurfaceOperation::FactStoreReason => {
            decode!(FactStoreReasonRequestV1, FactStoreReason)
        }
        RetainedSurfaceOperation::FactStoreContradict => {
            decode!(FactStoreContradictRequestV1, FactStoreContradict)
        }
        RetainedSurfaceOperation::FactStoreGet => {
            decode!(FactStoreGetRequestV1, FactStoreGet)
        }
        RetainedSurfaceOperation::FactStoreUpdate => {
            decode!(FactStoreUpdateRequestV1, FactStoreUpdate)
        }
        RetainedSurfaceOperation::FactStoreRemove => {
            decode!(FactStoreRemoveRequestV1, FactStoreRemove)
        }
        RetainedSurfaceOperation::FactStoreSupersede => {
            decode!(FactStoreSupersedeRequestV1, FactStoreSupersede)
        }
        RetainedSurfaceOperation::FactStoreList => {
            decode!(FactStoreListRequestV1, FactStoreList)
        }
        RetainedSurfaceOperation::FactFeedback => decode!(FactFeedbackRequestV1, FactFeedback),
        RetainedSurfaceOperation::MemoryStatus => decode!(MemoryStatusRequestV1, MemoryStatus),
        RetainedSurfaceOperation::SessionRefreshStatus => {
            decode_session_refresh(body, SessionRefreshActionV1::Status)
        }
        RetainedSurfaceOperation::SessionRefreshCancel => {
            decode_session_refresh(body, SessionRefreshActionV1::Cancel)
        }
        RetainedSurfaceOperation::SessionRefreshBegin => {
            decode_session_refresh(body, SessionRefreshActionV1::Begin)
        }
        RetainedSurfaceOperation::MessageSearch => decode!(MessageSearchRequestV1, MessageSearch),
        RetainedSurfaceOperation::SessionsFor => decode!(SessionsForRequestV1, SessionsFor),
        RetainedSurfaceOperation::Workflows => decode!(WorkflowsRequestV1, Workflows),
        RetainedSurfaceOperation::LcmStatus => decode!(LcmStatusRequestV1, LcmStatus),
        RetainedSurfaceOperation::LcmDoctor => decode!(LcmDoctorRequestV1, LcmDoctor),
        RetainedSurfaceOperation::LcmLoadSession => {
            decode!(LcmLoadSessionRequestV1, LcmLoadSession)
        }
        RetainedSurfaceOperation::LcmGrep => decode!(LcmGrepRequestV1, LcmGrep),
        RetainedSurfaceOperation::LcmDescribe => decode!(LcmDescribeRequestV1, LcmDescribe),
        RetainedSurfaceOperation::LcmExpand => decode!(LcmExpandRequestV1, LcmExpand),
        RetainedSurfaceOperation::LcmExpandQuery => {
            decode!(LcmExpandQueryRequestV1, LcmExpandQuery)
        }
    }
}

fn decode_session_refresh(
    body: serde_json::Value,
    action: SessionRefreshActionV1,
) -> Result<RetainedSurfaceRequestV1, serde_json::Error> {
    let request = serde_path_to_error::deserialize::<_, SessionRefreshActionRequestV1>(body)
        .map_err(named_argument_error)?;
    Ok(RetainedSurfaceRequestV1::SessionRefresh(
        SessionRefreshRequestV1::with_action(action, request),
    ))
}

/// Prefix the serde diagnostic with the offending argument path, so the
/// corrective message names the argument even for wrong-type errors, which
/// serde alone reports without the field.
fn named_argument_error(error: serde_path_to_error::Error<serde_json::Error>) -> serde_json::Error {
    let path = error.path().to_string();
    let inner = error.into_inner();
    if path == "." {
        inner
    } else {
        serde::de::Error::custom(format!("{path}: {inner}"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn route_selected_session_refresh_rejects_embedded_action() {
        assert!(
            decode_retained_request(
                RetainedSurfaceOperation::SessionRefreshStatus,
                json!({ "action": "status" }),
            )
            .is_err()
        );
    }

    #[test]
    fn fact_store_curate_rejects_caller_owned_authority() {
        for forbidden in [
            "operations",
            "proposal_id",
            "approve",
            "apply",
            "run_id",
            "task",
        ] {
            let mut value = serde_json::Map::new();
            value.insert(forbidden.to_owned(), serde_json::Value::Bool(true));
            assert!(
                decode_retained_request(
                    RetainedSurfaceOperation::FactStoreCurate,
                    serde_json::Value::Object(value),
                )
                .is_err()
            );
        }
    }
}

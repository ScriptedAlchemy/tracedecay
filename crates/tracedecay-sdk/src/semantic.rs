//! Cross-field result validation selected by generated operation metadata.

use serde_json::Value;
use tracedecay_application::RequestId;
use tracedecay_application::retained_surfaces::{
    AutomationRunResultV1, FactStoreCurateRequestV1, SdkResultSemanticsV1,
};

pub(crate) fn response_matches(
    semantics: SdkResultSemanticsV1,
    request_id: &str,
    expected_request_id: Option<&RequestId>,
    request: &Value,
    result: &Value,
) -> bool {
    if expected_request_id.is_some_and(|expected| expected.as_str() != request_id) {
        return false;
    }
    match semantics {
        SdkResultSemanticsV1::SchemaOnly => true,
        SdkResultSemanticsV1::FactStoreCurateTerminal => {
            let Ok(request_id) = RequestId::new(request_id.to_owned()) else {
                return false;
            };
            let Ok(request) = serde_json::from_value::<FactStoreCurateRequestV1>(request.clone())
            else {
                return false;
            };
            let Ok(admission) = request.automation_request(&request_id) else {
                return false;
            };
            serde_json::from_value::<AutomationRunResultV1>(result.clone())
                .is_ok_and(|result| result.matches_admission(&admission))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_application::retained_surfaces::SdkResultSemanticsV1;

    use super::response_matches;

    #[test]
    fn curate_semantics_reject_a_structural_terminal_with_foreign_run_identity() {
        let terminal = json!({
            "run_id": "request.foreign",
            "task": "memory_curator",
            "request_digest": concat!(
                "sha256:",
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
            "terminal": {
                "status": "completed",
                "summary": {
                    "reviewed_count": 0,
                    "accepted_count": 0,
                    "rejected_count": 0,
                    "skipped_count": 0
                }
            },
            "committed_receipts": []
        });
        assert!(!response_matches(
            SdkResultSemanticsV1::FactStoreCurateTerminal,
            "request.sdk.curate",
            None,
            &json!({}),
            &terminal,
        ));
    }
}

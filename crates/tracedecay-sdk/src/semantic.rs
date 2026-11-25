//! Cross-field result validation selected by generated operation metadata.

use serde_json::Value;
use tracedecay_contracts::retained_surfaces::{
    FactStoreCurateRequestV1, FactStoreCurateResultV1, SdkResultSemanticsV1,
};
use tracedecay_contracts::{RequestId, ResolvedScope};

pub(crate) fn response_matches(
    semantics: SdkResultSemanticsV1,
    request_id: &str,
    expected_request_id: Option<&RequestId>,
    request: &Value,
    scope: &Value,
    result: &Value,
) -> bool {
    if expected_request_id.is_some_and(|expected| expected.as_str() != request_id) {
        return false;
    }
    match semantics {
        SdkResultSemanticsV1::SchemaOnly => true,
        SdkResultSemanticsV1::FactStoreCurateReceipt => {
            // The receipt is bound to the scope the daemon resolved; a scope
            // whose digest does not commit to its identity is not that scope.
            let scope_is_canonical = serde_json::from_value::<ResolvedScope>(scope.clone())
                .is_ok_and(|scope| scope.validate().is_ok());
            if !scope_is_canonical {
                return false;
            }
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
            serde_json::from_value::<FactStoreCurateResultV1>(result.clone())
                .is_ok_and(|result| result.matches_admission(&admission))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_contracts::retained_surfaces::{
        FactStoreCurateRequestV1, FactStoreCurateResultV1, SdkResultSemanticsV1,
    };
    use tracedecay_contracts::{RequestId, ResolvedScope};
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

    use super::response_matches;

    #[test]
    fn curate_semantics_accept_only_the_receipt_for_the_admitted_run() {
        let request = json!({ "fact_review_limit": 4 });
        let admission = serde_json::from_value::<FactStoreCurateRequestV1>(request.clone())
            .expect("curate request")
            .automation_request(&RequestId::new("request.sdk.curate").expect("request id"))
            .expect("admission");
        let receipt =
            serde_json::to_value(FactStoreCurateResultV1::started(&admission).expect("receipt"))
                .expect("receipt json");
        assert_eq!(receipt["run_id"], "request.sdk.curate");
        assert_eq!(receipt["task"], "memory_curator");
        assert_eq!(receipt["state"], "started");
        let scope = serde_json::to_value(
            ResolvedScope::new(
                ProjectId::new("project.sdk").expect("project id"),
                RepositoryId::new("repository.sdk").expect("repository id"),
                WorktreeId::new("worktree.sdk").expect("worktree id"),
                None,
            )
            .expect("resolved scope"),
        )
        .expect("scope json");
        let matches_in =
            |scope: &serde_json::Value, request_id: &str, result: &serde_json::Value| {
                response_matches(
                    SdkResultSemanticsV1::FactStoreCurateReceipt,
                    request_id,
                    None,
                    &request,
                    scope,
                    result,
                )
            };
        let matches =
            |request_id: &str, result: &serde_json::Value| matches_in(&scope, request_id, result);
        assert!(matches("request.sdk.curate", &receipt));
        assert!(!matches("request.sdk.foreign", &receipt));
        let mut forged_scope = scope.clone();
        forged_scope["scope_digest"] = json!(format!("sha256:{}", "9".repeat(64)));
        assert!(
            !matches_in(&forged_scope, "request.sdk.curate", &receipt),
            "a scope digest that does not commit to its identity is refused"
        );

        let mut terminal = receipt.clone();
        let object = terminal.as_object_mut().expect("receipt object");
        object.remove("state");
        object.insert(
            "terminal".to_owned(),
            json!({"status": "completed", "summary": {
                "reviewed_count": 0, "accepted_count": 0, "rejected_count": 0, "skipped_count": 0
            }}),
        );
        object.insert("committed_receipts".to_owned(), json!([]));
        assert!(
            !matches("request.sdk.curate", &terminal),
            "the run terminal is no longer the curate result"
        );
    }
}

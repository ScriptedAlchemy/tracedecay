mod common;

use tracedecay_application::{
    ApplicationProblem, AuthorizationPortOutcome, AuthorizationService, ConcealedResourceCause,
    NonDisclosureHooks, RetryDirective,
};
use tracedecay_domain::UtcMicros;
use tracedecay_policy::authorization::SourceAuthorizationEvaluatorV1;

#[test]
fn absent_out_of_scope_and_policy_hidden_resources_share_one_public_problem() {
    let hooks = NonDisclosureHooks;
    let public_shapes = [
        ConcealedResourceCause::Absent,
        ConcealedResourceCause::OutsideScope,
        ConcealedResourceCause::PolicyHidden,
    ]
    .map(|cause| {
        serde_json::to_value(hooks.resource_problem(cause, RetryDirective::Never)).unwrap()
    });

    assert_eq!(public_shapes[0], public_shapes[1]);
    assert_eq!(public_shapes[1], public_shapes[2]);
    assert_eq!(public_shapes[0]["kind"], "not_found_or_not_authorized");
    assert!(public_shapes[0].get("detail").is_none());
    assert!(public_shapes[0].get("count").is_none());
    assert!(public_shapes[0].get("timing").is_none());
}

#[test]
fn cursor_and_anchor_rejections_use_the_same_non_disclosing_shape() {
    let hooks = NonDisclosureHooks;
    let cursor = hooks.cursor_problem(RetryDirective::AfterRevalidate);
    let anchor = hooks.anchor_problem(RetryDirective::AfterRevalidate);

    assert_eq!(cursor, anchor);
    assert_eq!(
        cursor,
        ApplicationProblem::not_found_or_not_authorized(RetryDirective::AfterRevalidate)
    );
}

#[test]
fn denied_and_absent_sources_are_indistinguishable_after_policy_evaluation() {
    let operation = common::operation();
    let context = common::context(&operation);
    let denied = AuthorizationService::new(
        common::StaticAuthorizationPort::new(AuthorizationPortOutcome::Snapshot(Box::new(
            common::source_snapshot(common::source_authorization_input("project_owner_mismatch")),
        ))),
        SourceAuthorizationEvaluatorV1::default(),
    )
    .admit(&context, &operation, UtcMicros(10))
    .unwrap_err();
    let absent = AuthorizationService::new(
        common::StaticAuthorizationPort::new(AuthorizationPortOutcome::Absent),
        SourceAuthorizationEvaluatorV1::default(),
    )
    .admit(&context, &operation, UtcMicros(10))
    .unwrap_err();

    assert_eq!(
        serde_json::to_value(denied).expect("problem serializes"),
        serde_json::to_value(absent).expect("problem serializes")
    );
}

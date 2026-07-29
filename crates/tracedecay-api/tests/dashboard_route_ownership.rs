use axum::http::StatusCode;
use serde_json::json;
use tracedecay_api::configuration::{
    configuration_revision_conflict_error, dashboard_configuration_write_routes,
    parse_project_settings_patch, parse_user_settings_patch,
};
use tracedecay_api::feedback::{
    FeedbackStatusCoverageV1, FeedbackStatusDenominatorsV1, FeedbackStatusPresentationV1,
    dashboard_feedback_read_route, feedback_status_envelope,
};
use tracedecay_api::read_model::{
    DashboardCoverageCompletenessV1, DashboardDomainStateV1, DashboardScopeV1,
};
use tracedecay_api::remediation::{
    DoctorRemediationErrorPresentationV1, DoctorRemediationOperationPresentationV1,
    doctor_remediation_envelope, doctor_remediation_routes,
};

fn scope() -> DashboardScopeV1 {
    DashboardScopeV1 {
        project_id: Some("project.route-ownership".to_owned()),
        storage_mode: "profile_sharded".to_owned(),
        store_root: "/tmp/route-ownership".to_owned(),
    }
}

#[test]
fn configuration_write_descriptors_and_cas_errors_are_api_owned() {
    let routes = dashboard_configuration_write_routes();
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].method, "PATCH");
    assert_eq!(routes[0].path, "/api/settings/project");
    assert_eq!(routes[0].operation, "configuration_batch");
    assert_eq!(routes[1].method, "PATCH");
    assert_eq!(routes[1].path, "/api/settings/user");
    assert_eq!(routes[1].operation, "user_settings_mutate");

    let project = parse_project_settings_patch(json!({
        "expected_revision_id": "revision.project.1",
        "include": ["src/**"],
        "sync": {"auto_track_pr_branches": true}
    }))
    .expect("valid project patch");
    assert_eq!(project.expected_revision_id, "revision.project.1");
    assert_eq!(project.include, Some(vec!["src/**".to_owned()]));
    assert_eq!(
        project
            .sync
            .expect("sync patch")
            .auto_track_pr_branches,
        Some(true)
    );

    let shape_error =
        parse_user_settings_patch(json!({"expected_revision_id": "revision.user.1", "unknown": true}))
            .expect_err("unknown field must remain a typed bad request");
    assert_eq!(shape_error.0, StatusCode::BAD_REQUEST);
    assert_eq!(shape_error.1 .0["validation_errors"][0]["field"], "unknown");

    let conflict = configuration_revision_conflict_error(
        "settings changed after this edit began; refresh and retry",
        "revision.expected",
        "revision.actual",
    );
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    assert_eq!(conflict.1 .0["code"], "configuration_revision_conflict");
    assert_eq!(conflict.1 .0["expected_revision_id"], "revision.expected");
    assert_eq!(conflict.1 .0["actual_revision_id"], "revision.actual");
}

#[test]
fn remediation_write_descriptors_and_presentation_preserve_truthfulness() {
    let routes = doctor_remediation_routes();
    assert_eq!(routes.len(), 3);
    assert_eq!(routes[0].method, "POST");
    assert_eq!(routes[0].path, "/api/doctor/remediations/preview");
    assert_eq!(routes[1].method, "POST");
    assert_eq!(routes[1].path, "/api/doctor/remediations/apply");
    assert_eq!(routes[2].method, "GET");
    assert_eq!(routes[2].path, "/api/doctor/remediations/{operation_id}");

    let operation = doctor_remediation_envelope(
        scope(),
        Ok::<_, (String, DoctorRemediationErrorPresentationV1)>((
            json!({"operation_id": "operation.preview.1"}),
            DoctorRemediationOperationPresentationV1::new(DashboardDomainStateV1::Ready, true),
        )),
    );
    let operation_wire = serde_json::to_value(operation).expect("operation envelope wire shape");
    assert_eq!(operation_wire["domain_state"], "ready");
    assert_eq!(operation_wire["coverage"]["completeness"], "complete");
    assert_eq!(operation_wire["payload"]["status"], "operation");
    assert_eq!(
        operation_wire["payload"]["operation"]["operation_id"],
        "operation.preview.1"
    );

    let unavailable = doctor_remediation_envelope(
        scope(),
        Err::<
            (
                serde_json::Value,
                DoctorRemediationOperationPresentationV1,
            ),
            _,
        >((
            "unsupported".to_owned(),
            DoctorRemediationErrorPresentationV1::new(
                DashboardDomainStateV1::Unsupported,
                true,
            ),
        )),
    );
    assert_eq!(
        unavailable.coverage.completeness,
        DashboardCoverageCompletenessV1::Unsupported
    );
    assert_eq!(unavailable.domain_state, DashboardDomainStateV1::Unsupported);
    let unavailable_wire = serde_json::to_value(unavailable).expect("unavailable wire shape");
    assert_eq!(unavailable_wire["payload"]["status"], "unavailable");
    assert_eq!(unavailable_wire["payload"]["reason"], "unsupported");
}

#[test]
fn feedback_read_descriptor_and_mapper_keep_unknowns_explicit() {
    let route =
        dashboard_feedback_read_route("POST", "feedback/get").expect("feedback get descriptor");
    assert_eq!(route.application_path, "/get");
    assert_eq!(route.operation.as_str(), "feedback_get");
    assert!(dashboard_feedback_read_route("GET", "feedback/get").is_none());
    assert!(dashboard_feedback_read_route("POST", "feedback/status").is_none());

    let partial = feedback_status_envelope(
        scope(),
        Ok::<_, ()>(FeedbackStatusPresentationV1 {
            payload: json!({"total": 3}),
            coverage: FeedbackStatusCoverageV1::Partial,
            total_count: 3,
            denominators: FeedbackStatusDenominatorsV1 {
                eligible: 5,
                persisted: 3,
                delayed: 1,
                dropped: 1,
                retention_dropped: 0,
                incomplete_boots: 0,
            },
            last_observed_at_micros: Some(100),
            observed_through_micros: Some(101),
            producer_sequence: Some(7),
        }),
        || json!({"total": 0}),
    );
    assert_eq!(partial.domain_state, DashboardDomainStateV1::Partial);
    assert_eq!(
        partial.coverage.completeness,
        DashboardCoverageCompletenessV1::Partial
    );
    assert_eq!(
        partial.coverage.omission_reasons,
        vec![
            "delayed_observations".to_owned(),
            "dropped_observations".to_owned(),
        ]
    );
    assert_eq!(
        partial
            .source_watermark
            .expect("source watermark")
            .watermark,
        "7"
    );

    let unavailable = feedback_status_envelope(
        scope(),
        Err::<FeedbackStatusPresentationV1<serde_json::Value>, _>("authority unavailable"),
        || json!({"total": 0}),
    );
    assert_eq!(unavailable.domain_state, DashboardDomainStateV1::Unknown);
    assert_eq!(
        unavailable.coverage.completeness,
        DashboardCoverageCompletenessV1::Unknown
    );
}

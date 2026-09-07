//! `GET /api/doctor/findings` over the real embedded-dashboard mount.
//!
//! The route path and every presentation axis come from
//! `tracedecay_api::doctor`. These tests exercise the mounted path rather than
//! the handler so a descriptor that stops being mounted is a failure, and they
//! pin the honest states an unadmitted Doctor source must produce.

use crate::dashboard_api_support::*;

#[test]
fn doctor_findings_endpoint_is_typed_unsupported_without_an_admitted_source() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!(
                "{}{}",
                fixture.base_url,
                tracedecay_api::doctor::DOCTOR_FINDINGS_ROUTE_PATH
            ),
        );

        assert_eq!(status, 200, "{envelope}");
        assert_eq!(
            envelope["domain_state"], "unsupported",
            "an unadmitted Doctor source must never render as ready or complete: {envelope}"
        );
        assert_eq!(envelope["coverage"]["completeness"], "unsupported");
        assert_eq!(envelope["freshness"]["state"], "unsupported");
        assert!(
            envelope["coverage"]["denominator"].is_null(),
            "an unsupported read has no denominator to render: {envelope}"
        );
        assert_eq!(
            envelope["payload"]["note"],
            tracedecay_api::doctor::DOCTOR_REPORT_SOURCE_UNSUPPORTED_NOTE
        );
        assert_eq!(
            envelope["payload"]["entries"]
                .as_array()
                .unwrap_or_else(|| panic!("entries should be an array: {envelope}"))
                .len(),
            0
        );

        let families = envelope["payload"]["known_families"]
            .as_array()
            .unwrap_or_else(|| panic!("known families should be an array: {envelope}"));
        assert_eq!(
            families.len(),
            tracedecay_api::doctor::KNOWN_DOCTOR_FINDING_FAMILIES.len(),
            "the closed family vocabulary must be advertised in full: {envelope}"
        );

        // A caller can always re-read, even from a typed unavailable state.
        assert_eq!(
            envelope["legal_actions"],
            serde_json::json!([{
                "kind": "refresh",
                "operation": tracedecay_api::doctor::DOCTOR_FINDINGS_REFRESH_OPERATION,
            }]),
            "{envelope}"
        );
    });
}

#[test]
fn doctor_findings_endpoint_rejects_a_family_outside_the_closed_vocabulary() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();

        let (status, envelope) = get_json(
            &agent,
            &format!(
                "{}{}?family=not_a_family",
                fixture.base_url,
                tracedecay_api::doctor::DOCTOR_FINDINGS_ROUTE_PATH
            ),
        );
        assert_eq!(status, 200, "{envelope}");
        assert_eq!(
            envelope["domain_state"], "error",
            "an unknown family is a typed error, never a silent all-families read: {envelope}"
        );
        assert!(envelope["payload"]["family_filter"].is_null(), "{envelope}");
        assert_eq!(
            envelope["payload"]["note"],
            "unknown doctor finding family: not_a_family"
        );

        // A valid family is echoed back and keeps the unadmitted-source state.
        let (status, envelope) = get_json(
            &agent,
            &format!(
                "{}{}?family=storage",
                fixture.base_url,
                tracedecay_api::doctor::DOCTOR_FINDINGS_ROUTE_PATH
            ),
        );
        assert_eq!(status, 200, "{envelope}");
        assert_eq!(envelope["payload"]["family_filter"], "storage");
        assert_eq!(envelope["domain_state"], "unsupported");
    });
}

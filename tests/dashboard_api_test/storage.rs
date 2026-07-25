use crate::dashboard_api_support::*;

#[test]
fn storage_telemetry_endpoint_reports_observed_or_typed_budget_states() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!("{}/api/storage/telemetry", fixture.base_url),
        );

        assert_eq!(status, 200, "{envelope}");
        assert_eq!(envelope["domain_state"], "ready");
        assert_eq!(envelope["coverage"]["completeness"], "complete");
        let stores = envelope["payload"]["stores"]
            .as_array()
            .unwrap_or_else(|| panic!("telemetry stores should be an array: {envelope}"));
        assert!(
            !stores.is_empty(),
            "the dashboard-held stores must be sampled"
        );
        for store in stores {
            assert_eq!(
                store["read"]["kind"], "observed",
                "fresh fixture stores should have real pragma samples: {store}"
            );
            assert!(
                store["total_bytes"].as_u64().is_some_and(|bytes| bytes > 0),
                "observed store size should be real: {store}"
            );
            assert_eq!(
                store["budget"]["state"], "unset",
                "an owner with no configured soft limit must be unset, never silently within budget: {store}"
            );
            assert!(
                store["budget"]["setting_key"]
                    .as_str()
                    .is_some_and(|key| key.contains("store_soft_budgets_bytes")),
                "unset state should identify its owner setting: {store}"
            );
            assert!(
                matches!(
                    store["growth"]["state"].as_str(),
                    Some("baseline" | "observed")
                ),
                "growth must be a measured daemon-lifetime state: {store}"
            );
        }
    });
}

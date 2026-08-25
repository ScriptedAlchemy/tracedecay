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
            // No daemon-owned store-size watermark authority exists yet;
            // the growth dimension is typed unknown with its reason rather
            // than a fabricated baseline. When the execution-owned watermark
            // producer lands, this pins to `baseline`/`observed`.
            assert_eq!(
                store["growth"]["state"], "unknown",
                "growth without an execution-owned watermark stays typed unknown: {store}"
            );
            assert!(
                store["growth"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("watermark")),
                "unknown growth must name the missing watermark authority: {store}"
            );
        }
    });
}

#[test]
fn storage_findings_endpoint_reports_every_producer_source_honestly() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();
        let (status, envelope) = get_json(
            &agent,
            &format!("{}/api/storage/findings", fixture.base_url),
        );

        assert_eq!(status, 200, "{envelope}");
        let statuses = envelope["payload"]["kind_statuses"]
            .as_array()
            .unwrap_or_else(|| panic!("storage producer statuses should be an array: {envelope}"));
        let producer_kinds = statuses
            .iter()
            .map(|status| {
                status["kind"]
                    .as_str()
                    .unwrap_or_else(|| panic!("storage producer kind must be typed: {status}"))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            producer_kinds,
            [
                "over_budget_store",
                "orphan_store",
                "incident_debris_present",
                "retention_backlog",
                "table_growth",
            ],
            "every canonical Plan 38 producer must appear exactly once"
        );

        let status_for = |kind: &str| {
            statuses
                .iter()
                .find(|entry| entry["kind"] == kind)
                .unwrap_or_else(|| panic!("missing storage producer status for {kind}: {envelope}"))
        };
        for kind in [
            "over_budget_store",
            "orphan_store",
            "incident_debris_present",
            "retention_backlog",
            "table_growth",
        ] {
            let producer = status_for(kind);
            assert_eq!(
                producer["state"], "unsupported",
                "dashboard telemetry must not override an unadmitted canonical Doctor source: {producer}"
            );
            assert!(
                producer["reason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "unsupported producer status needs a reason: {producer}"
            );
        }
    });
}

use crate::dashboard_api_support::*;

fn canonical_curation_request(operations: Value) -> Value {
    serde_json::json!({
        "memory_scope": "project",
        "min_confidence": 0.9,
        "operations": operations,
    })
}

#[test]
fn legacy_manual_curation_routes_are_not_mounted() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let apply_url = format!("{}/api/plugins/holographic/curate/apply", fixture.base_url);
        let response = crate::common::http_call_with_retry(
            &format!("POST {apply_url} (legacy route absence)"),
            || {
                agent
                    .post(&apply_url)
                    .send_json(&canonical_curation_request(serde_json::json!([])))
            },
        );
        assert_eq!(
            response.status().as_u16(),
            404,
            "legacy apply must be absent"
        );

        for path in [
            "/api/plugins/holographic/curation/status",
            "/api/plugins/holographic/curation/activity",
        ] {
            let url = format!("{}{path}", fixture.base_url);
            let response = crate::common::http_call_with_retry(
                &format!("GET {url} (legacy route absence)"),
                || agent.get(&url).call(),
            );
            assert_eq!(
                response.status().as_u16(),
                404,
                "legacy manual-effect read must be absent at {path}"
            );
        }
    });
}

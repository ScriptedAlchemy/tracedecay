use crate::dashboard_api_support::*;

fn entry_script(html: &str) -> String {
    html.split("src=\"")
        .skip(1)
        .filter_map(|tail| tail.split_once('"').map(|(value, _)| value))
        .find(|value| value.starts_with("/static/js/index."))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("dashboard index omitted its Rsbuild entry script: {html}"))
}

#[test]
fn dashboard_root_serves_the_embedded_single_app_bundle() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();

        let mut index_response = agent
            .get(&format!("{}/", fixture.base_url))
            .call()
            .expect("embedded dashboard index should be served");
        assert_eq!(index_response.status().as_u16(), 200);
        assert_eq!(
            index_response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        let index = index_response
            .body_mut()
            .read_to_string()
            .expect("embedded dashboard index should be readable");
        assert!(index.contains("<title>TraceDecay</title>"), "{index}");
        assert!(
            !index.contains("rebuild in progress"),
            "production root served the legacy placeholder"
        );

        let script = entry_script(&index);
        let mut script_response = agent
            .get(&format!("{}{}", fixture.base_url, script))
            .call()
            .expect("embedded dashboard entry script should be served");
        assert_eq!(script_response.status().as_u16(), 200);
        assert_eq!(
            script_response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/javascript")
        );
        let script_body = script_response
            .body_mut()
            .read_to_string()
            .expect("embedded dashboard entry script should be readable");
        assert!(
            script_body.len() > 704,
            "production JavaScript must not regress to the historical 704-byte placeholder"
        );

        let mut deep_link_response = agent
            .get(&format!("{}/delivery", fixture.base_url))
            .call()
            .expect("SPA deep link should return the embedded index");
        assert_eq!(deep_link_response.status().as_u16(), 200);
        assert_eq!(
            deep_link_response
                .body_mut()
                .read_to_string()
                .expect("SPA fallback should be readable"),
            index
        );
    });
}

#[test]
fn dashboard_http_admission_rejects_rebinding_and_cross_origin_shapes() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();
        let capabilities_url = format!("{}/api/capabilities", fixture.base_url);

        let (status, ordinary) = get_json(&agent, &capabilities_url);
        assert_eq!(
            status, 200,
            "ordinary dashboard reads must remain available"
        );
        assert!(ordinary["features"].is_object(), "{ordinary}");

        let (status, rebound) = response_to_json(
            agent
                .get(&capabilities_url)
                .header("Host", "attacker.example")
                .call(),
        );
        assert_eq!(status, 403, "a non-loopback Host must be rejected");
        assert_eq!(rebound["error"], "dashboard_request_forbidden");

        let (status, wrong_port) = response_to_json(
            agent
                .get(&capabilities_url)
                .header("Host", "127.0.0.1:1")
                .call(),
        );
        assert_eq!(status, 403, "Host must name the bound dashboard port");
        assert_eq!(wrong_port["error"], "dashboard_request_forbidden");

        let (status, cross_origin) = response_to_json(
            agent
                .get(&capabilities_url)
                .header("Origin", "http://attacker.example")
                .call(),
        );
        assert_eq!(
            status, 403,
            "a cross-origin browser request must be rejected"
        );
        assert_eq!(cross_origin["error"], "dashboard_request_forbidden");

        let (status, same_origin) = response_to_json(
            agent
                .get(&capabilities_url)
                .header("Origin", &fixture.base_url)
                .call(),
        );
        assert_eq!(
            status, 200,
            "the dashboard's own Origin must remain admitted"
        );
        assert!(same_origin["features"].is_object(), "{same_origin}");
    });
}

use crate::dashboard_api_support::*;

#[test]
fn automation_config_uses_revisioned_current_contract() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_configuration_fixture().await;
        let agent = http_agent();
        let config_url = format!(
            "{}/api/plugins/holographic/curation/config",
            fixture.base_url
        );

        let (status, initial) = get_json(&agent, &config_url);
        assert_eq!(status, 200, "{initial}");
        assert_eq!(initial["source"], "daemon_pinned_snapshot");
        let initial_revision = initial["configuration_revision_id"]
            .as_str()
            .expect("GET must return the pinned configuration revision");

        let mutation = serde_json::json!({
            "expected_revision_id": initial_revision,
            "idempotency_key": "dashboard-automation-config-current-contract",
            "enabled": true,
            "backend": "codex_app_server",
            "model_id": "gpt-5.6-mini",
            "timeout_secs": 90,
            "scheduler_tick_secs": 15,
            "memory_curator": {
                "enabled": true,
                "schedule": "manual"
            },
            "session_reflector": {
                "enabled": true,
                "schedule": "interval",
                "interval_secs": 1800
            },
            "skill_writer": {
                "enabled": true,
                "schedule": "interval",
                "interval_secs": 3600
            }
        });
        let (status, saved) = patch_json_body(&agent, &config_url, &mutation);
        assert_eq!(status, 200, "{saved}");
        assert_eq!(saved["source"], "daemon_pinned_snapshot");
        assert_ne!(
            saved["configuration_revision_id"], initial["configuration_revision_id"],
            "a settled automation mutation must advance the pinned revision"
        );
        assert_eq!(saved["effective"]["enabled"], true);
        assert_eq!(saved["effective"]["backend"], "codex_app_server");
        assert_eq!(saved["effective"]["model_id"], "gpt-5.6-mini");
        assert_eq!(saved["effective"]["timeout_secs"], 90);
        assert_eq!(saved["effective"]["scheduler_tick_secs"], 15);
        for task in ["memory_curator", "session_reflector", "skill_writer"] {
            assert_eq!(
                saved["effective"]["tasks"][task]["enabled"], true,
                "{task} must be represented by the current boolean task contract"
            );
        }
        assert!(
            saved["effective"].get("memory_apply_policy").is_none(),
            "{saved}"
        );
        assert!(
            saved["effective"].get("skill_activation_policy").is_none(),
            "{saved}"
        );

        let saved_revision = saved["configuration_revision_id"].clone();
        let saved_effective = saved["effective"].clone();
        let stale_mutation = serde_json::json!({
            "expected_revision_id": initial_revision,
            "idempotency_key": "dashboard-automation-config-stale-retry",
            "timeout_secs": 120
        });
        let (status, conflict) = patch_json_body(&agent, &config_url, &stale_mutation);
        assert_eq!(status, 409, "{conflict}");
        assert_eq!(conflict["code"], "configuration_revision_conflict");
        assert_eq!(conflict["expected_revision_id"], initial_revision);
        assert_eq!(conflict["actual_revision_id"], saved_revision);

        let (status, reread) = get_json(&agent, &config_url);
        assert_eq!(status, 200, "{reread}");
        assert_eq!(reread["configuration_revision_id"], saved_revision);
        assert_eq!(reread["effective"], saved_effective);

        let (status, capabilities) =
            get_json(&agent, &format!("{}/api/capabilities", fixture.base_url));
        assert_eq!(status, 200, "{capabilities}");
        assert_eq!(capabilities["features"]["automation"], true);
        assert_eq!(capabilities["features"]["llm_curation"], true);
        assert_eq!(capabilities["automation"]["available"], true);
        assert_eq!(capabilities["automation"]["mode"], "standalone_backend");
        assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
    });
}

#[test]
fn automation_config_rejects_retired_policy_fields() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_configuration_fixture().await;
        let agent = http_agent();
        let config_url = format!(
            "{}/api/plugins/holographic/curation/config",
            fixture.base_url
        );
        let (status, initial) = get_json(&agent, &config_url);
        assert_eq!(status, 200, "{initial}");
        let revision = initial["configuration_revision_id"]
            .as_str()
            .expect("GET must return the pinned configuration revision");

        for (index, (field, value)) in [
            (
                "memory_apply_policy",
                serde_json::json!("validate_then_apply"),
            ),
            (
                "skill_activation_policy",
                serde_json::json!("validate_then_activate"),
            ),
            ("auto_apply_memory_ops", serde_json::json!(true)),
            ("auto_enable_skills", serde_json::json!(true)),
        ]
        .into_iter()
        .enumerate()
        {
            let mut body = serde_json::json!({
                "expected_revision_id": revision,
                "idempotency_key": format!("dashboard-retired-policy-{index}")
            });
            body[field] = value;
            let (status, rejected) = patch_json_body(&agent, &config_url, &body);
            assert_eq!(status, 400, "{field}: {rejected}");
            assert!(
                rejected.to_string().contains(field),
                "rejection must identify {field}: {rejected}"
            );
        }
    });
}

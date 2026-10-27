use crate::dashboard_api_support::*;
use tracedecay_domain::configuration::{
    AUTOMATION_SETTINGS_SETTING_KEY, AutomationSettingsV1, ConfigurationGrantId,
    ConfigurationGrantReceiptId, ConfigurationIdempotencyKey, ConfigurationLayerIdV1,
    ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationValueV1,
    SettingKey,
};
use tracedecay_domain::{AccessPolicyDigest, UtcMicros};
use tracedecay_global_db::configuration::contracts::{
    ConfigurationControlStore, ConfigurationMutationAuthority, DirectConfigurationMutation,
};

#[test]
fn persisted_invalid_schedule_remains_readable_and_repairable_over_http() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_configuration_fixture().await;
        let sibling = start_dashboard_configuration_fixture().await;
        let options = tracedecay_project::project::TraceDecayOpenOptions::for_profile(
            fixture.host_runtime.profile(),
        );
        let cg = fixture
            .host_runtime
            .open_project_graph_for_test(&fixture.project_root, options.clone())
            .await
            .unwrap();
        let fact_id = seed_dashboard_fact(
            &cg,
            "Exact memory reads survive an invalid automation schedule.",
            FactCategoryV1::Project,
            0.95,
            &["automation-isolation"],
            &[],
        )
        .await;
        let current = cg.configuration_runtime().client().current().await.unwrap();
        let mut settings = current.config().automation.as_ref().clone();
        settings.enabled = true;
        settings.tasks.memory_curator.enabled = false;
        settings.tasks.memory_curator.schedule = Some("after lunch".to_owned());
        settings.validate().unwrap();
        let mutation = DirectConfigurationMutation::Set {
            layer: ConfigurationLayerIdV1::Project {
                project_id: current.target().project_id.clone(),
            },
            key: SettingKey::new(AUTOMATION_SETTINGS_SETTING_KEY).unwrap(),
            value: Box::new(ConfigurationValueV1::AutomationSettings(Box::new(settings))),
        };
        let authority = ConfigurationMutationAuthority {
            receipt: ConfigurationMutationGrantReceiptV1::issue(
                ConfigurationGrantReceiptId::new("configuration.grant-receipt.http-schedule").unwrap(),
                ConfigurationGrantId::new("configuration.grant.http-schedule").unwrap(),
                ActorId::new("actor.http-schedule").unwrap(),
                ConfigurationMutationOperationV1::DirectMutation,
                mutation.target_scope_digest().unwrap(),
                current.revision_id().clone(),
                1,
                AccessPolicyDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
                Some(ConfigurationIdempotencyKey::new("configuration.http-schedule").unwrap()),
                UtcMicros(1),
                UtcMicros(100),
            )
            .unwrap(),
        };
        let store = cg.configuration_runtime().configuration_store();
        let receipt = ConfigurationControlStore::commit_direct(
            &store, &authority, &mutation, current.revision_id(),
        )
        .await
        .unwrap();
        drop(cg);
        let reopened = fixture
            .host_runtime
            .open_project_graph_for_test(&fixture.project_root, options)
            .await
            .expect("a stored schedule error must not block full registered project reopen");
        let invalid = reopened.configuration_runtime().client().current().await.unwrap();
        assert_eq!(invalid.revision_id(), &receipt.result_revision_id);
        assert!(matches!(
            invalid.admitted_automation(),
            Err(tracedecay_domain::errors::TraceDecayError::Automation(_))
        ));
        tracedecay_configuration::config::publish_pinned_runtime_configuration(invalid.clone())
            .unwrap();
        assert_eq!(
            tracedecay_agent_hosts::agents::context_scout::model::context_scout_backend_from_automation_config(
                &invalid.config().automation,
            ),
            tracedecay_contracts::context_scout::ContextScoutModelBackendV1::CodexAppServer,
        );
        let agent = http_agent();
        let config_url = format!("{}/api/plugins/holographic/curation/config", fixture.base_url);
        let scheduler_url = format!("{}/api/automation/scheduler", fixture.base_url);
        let (status, raw) = get_json(&agent, &config_url);
        assert_eq!(status, 200, "{raw}");
        assert_eq!(raw["configuration_revision_id"], receipt.result_revision_id.as_str());
        assert_eq!(raw["effective"]["tasks"]["memory_curator"]["schedule"], "after lunch");
        let (status, exact_fact) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/{fact_id}", fixture.base_url),
        );
        assert_eq!(status, 200, "{exact_fact}");
        assert!(exact_fact.to_string().contains("Exact memory reads survive an invalid automation schedule."));
        let (status, settings) = get_json(&agent, &format!("{}/api/settings", fixture.base_url));
        assert_eq!(status, 200, "{settings}");
        assert_eq!(settings["payload"]["automation"]["availability"]["available"], true, "{settings}");
        assert_eq!(settings["payload"]["automation"]["enabled"], true, "{settings}");
        let control_path = tracedecay_automation_runtime::automation::scheduler::scheduler_control_path(
            &reopened.store_layout().dashboard_root,
        );
        let control_before = fs::read(&control_path).ok();
        let (status, refused) = get_json(&agent, &format!("{scheduler_url}/status"));
        assert_eq!(status, 500, "{refused}");
        assert!(refused.to_string().contains("memory_curator schedule is invalid"), "{refused}");
        for action in ["pause", "resume"] {
            let (status, refused) = post_json(&agent, &format!("{scheduler_url}/{action}"));
            assert_eq!(status, 500, "{refused}");
            assert!(refused.to_string().contains("memory_curator schedule is invalid"), "{refused}");
            assert_eq!(fs::read(&control_path).ok(), control_before);
        }
        let effects_root = reopened.store_layout().dashboard_root.join("automation_effects");
        assert!(!effects_root.exists());
        let (status, curator_refused) = post_json_body(
            &agent,
            &format!("{}/api/application/retained/fact_store_curate", fixture.base_url),
            &serde_json::json!({
                "fact_review_limit": 4,
                "min_confidence_millionths": 500_000
            }),
        );
        assert_eq!(status, 503, "{curator_refused}");
        assert_eq!(curator_refused["kind"], "problem", "{curator_refused}");
        let problem = &curator_refused["value"]["problem"];
        assert_eq!(problem["kind"], "unavailable", "{curator_refused}");
        assert_eq!(
            problem["code"],
            "application.retained.authority-unavailable",
            "{curator_refused}"
        );
        assert_eq!(problem["terminality"], "pre_admission", "{curator_refused}");
        assert_eq!(problem.get("committed_receipt"), Some(&serde_json::Value::Null), "{curator_refused}");
        assert!(curator_refused.to_string().contains("memory_curator schedule is invalid"), "{curator_refused}");
        assert!(!effects_root.exists(), "refused curator must not reserve a durable effect");
        assert!(tracedecay_automation_runtime::automation::run_ledger::load_run_records(
            &reopened.store_layout().dashboard_root, 10,
        ).await.unwrap().is_empty());
        let (status, rejected) = patch_json_body(
            &agent, &config_url,
            &serde_json::json!({
                "expected_revision_id": receipt.result_revision_id.as_str(),
                "idempotency_key": "dashboard-invalid-schedule-unrelated-edit",
                "timeout_secs": 90
            }),
        );
        assert_eq!(status, 400, "{rejected}");
        assert_eq!(rejected["detail"], "settings validation failed");
        assert!(rejected["validation_errors"].as_array().is_some_and(|issues| !issues.is_empty()));
        let (status, unchanged) = get_json(&agent, &config_url);
        assert_eq!(status, 200, "{unchanged}");
        assert_eq!(unchanged["configuration_revision_id"], raw["configuration_revision_id"]);
        assert_eq!(unchanged["effective"], raw["effective"]);
        let (status, repaired) = patch_json_body(
            &agent, &config_url,
            &serde_json::json!({
                "expected_revision_id": receipt.result_revision_id.as_str(),
                "idempotency_key": "dashboard-invalid-schedule-repair",
                "memory_curator": {"schedule": "every 10m"}
            }),
        );
        assert_eq!(status, 200, "{repaired}");
        assert_ne!(repaired["configuration_revision_id"], raw["configuration_revision_id"]);
        assert_eq!(repaired["effective"]["tasks"]["memory_curator"]["schedule"], "every 10m");
        let (status, recovered) = get_json(&agent, &format!("{scheduler_url}/status"));
        assert_eq!(status, 200, "{recovered}");
        assert_eq!(recovered["configuration_revision_id"], repaired["configuration_revision_id"]);
        assert!(invalid.admitted_automation().is_err());
        let (status, sibling_raw) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/curation/config", sibling.base_url),
        );
        assert_eq!(status, 200, "{sibling_raw}");
        assert_eq!(
            sibling_raw["effective"],
            serde_json::to_value(AutomationSettingsV1::default()).unwrap(),
        );
    });
}

#[test]
fn automation_config_uses_revisioned_current_contract() {
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

/// One checkout registered in two profiles has one repository-marker project
/// id. Each profile owns its own configuration store, so each dashboard must
/// read and settle against its own revision, never the other owner's.
#[test]
fn two_profiles_sharing_a_project_id_settle_their_own_configuration() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let first = start_dashboard_configuration_fixture().await;
        let second = start_dashboard_configuration_fixture().await;
        assert_eq!(first.project_id, second.project_id);
        let agent = http_agent();
        let config_url = |fixture: &DashboardFixture| {
            format!(
                "{}/api/plugins/holographic/curation/config",
                fixture.base_url
            )
        };

        let (status, first_initial) = get_json(&agent, &config_url(&first));
        assert_eq!(status, 200, "{first_initial}");
        let (status, first_saved) = patch_json_body(
            &agent,
            &config_url(&first),
            &serde_json::json!({
                "expected_revision_id": first_initial["configuration_revision_id"],
                "idempotency_key": "dashboard-shared-project-id-first",
                "enabled": true,
                "backend": "codex_app_server",
                "timeout_secs": 90
            }),
        );
        assert_eq!(status, 200, "{first_saved}");

        let (status, second_initial) = get_json(&agent, &config_url(&second));
        assert_eq!(status, 200, "{second_initial}");
        let (status, second_saved) = patch_json_body(
            &agent,
            &config_url(&second),
            &serde_json::json!({
                "expected_revision_id": second_initial["configuration_revision_id"],
                "idempotency_key": "dashboard-shared-project-id-second",
                "enabled": true,
                "backend": "codex_app_server",
                "timeout_secs": 120
            }),
        );
        assert_eq!(status, 200, "{second_saved}");
        assert_eq!(
            second_initial["effective"], first_initial["effective"],
            "the second profile must read its own untouched configuration"
        );
        assert_eq!(second_saved["effective"]["timeout_secs"], 120);

        let (status, first_reread) = get_json(&agent, &config_url(&first));
        assert_eq!(status, 200, "{first_reread}");
        assert_eq!(
            first_reread["configuration_revision_id"],
            first_saved["configuration_revision_id"]
        );
        assert_eq!(first_reread["effective"]["timeout_secs"], 90);
    });
}

#[test]
fn automation_config_rejects_retired_policy_fields() {
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

use tempfile::tempdir;

use tracedecay::automation::config::{
    effective_config, load_project_config, merge_project_config, save_project_config,
    AutomationBackend, AutomationConfig, AutomationConfigPatch, AutomationHostMode,
    AutomationTaskPatch,
};
use tracedecay::user_config::UserConfig;

#[test]
fn automation_defaults_are_conservative() {
    let config = AutomationConfig::default();

    assert!(!config.enabled);
    assert_eq!(config.backend, AutomationBackend::Disabled);
    assert_eq!(config.host_mode, AutomationHostMode::Standalone);
    assert_eq!(config.timeout_secs, 60);
    assert!(config.require_dashboard_approval);
    assert!(!config.auto_apply_memory_ops);
    assert!(!config.auto_enable_skills);
    assert!(!config.tasks.memory_curator.enabled);
    assert!(!config.tasks.session_reflector.enabled);
    assert!(!config.tasks.skill_writer.enabled);
}

#[test]
fn user_config_carries_global_automation_defaults_without_requiring_fields() {
    let parsed: UserConfig = toml::from_str("upload_enabled = false\n").unwrap();

    assert!(!parsed.upload_enabled);
    assert_eq!(parsed.automation, AutomationConfig::default());
}

#[test]
fn user_config_omits_default_automation_when_serialized() {
    let serialized = toml::to_string_pretty(&UserConfig::default()).unwrap();

    assert!(
        !serialized.contains("[automation]"),
        "default automation config should not churn the user config file: {serialized}"
    );
}

#[test]
fn effective_config_applies_project_sidecar_over_global_defaults() {
    let global = AutomationConfig {
        timeout_secs: 45,
        model: Some("global-model".to_string()),
        ..AutomationConfig::default()
    };
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        host_mode: Some(AutomationHostMode::HermesHosted),
        model: Some(Some("project-model".to_string())),
        memory_curator: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("manual".to_string())),
        },
        ..AutomationConfigPatch::default()
    };

    let config = effective_config(&global, Some(&patch)).unwrap();

    assert!(config.enabled);
    assert_eq!(config.backend, AutomationBackend::CodexAppServer);
    assert_eq!(config.host_mode, AutomationHostMode::HermesHosted);
    assert_eq!(config.timeout_secs, 45);
    assert_eq!(config.model.as_deref(), Some("project-model"));
    assert!(config.tasks.memory_curator.enabled);
    assert_eq!(
        config.tasks.memory_curator.schedule.as_deref(),
        Some("manual")
    );
    assert!(config.require_dashboard_approval);
    assert!(!config.auto_apply_memory_ops);
    assert!(!config.auto_enable_skills);
}

#[test]
fn project_config_patch_merges_without_clearing_omitted_fields() {
    let current = AutomationConfigPatch {
        enabled: Some(true),
        model: Some(Some("project-model".to_string())),
        memory_curator: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("manual".to_string())),
        },
        ..AutomationConfigPatch::default()
    };
    let patch = AutomationConfigPatch {
        timeout_secs: Some(120),
        memory_curator: AutomationTaskPatch {
            schedule: Some(None),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };

    let merged = merge_project_config(Some(current), patch);

    assert_eq!(merged.enabled, Some(true));
    assert_eq!(merged.model, Some(Some("project-model".to_string())));
    assert_eq!(merged.timeout_secs, Some(120));
    assert_eq!(merged.memory_curator.enabled, Some(true));
    assert_eq!(merged.memory_curator.schedule, Some(None));
}

#[tokio::test]
async fn project_sidecar_round_trips_without_touching_global_user_config() {
    let temp = tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        timeout_secs: Some(90),
        skill_writer: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("weekly".to_string())),
        },
        ..AutomationConfigPatch::default()
    };

    save_project_config(&dashboard_root, &patch).await.unwrap();
    let loaded = load_project_config(&dashboard_root).await.unwrap();

    assert_eq!(loaded, Some(patch));
    assert!(dashboard_root.join("automation_config.json").is_file());
}

#[test]
fn validation_rejects_unsafe_automatic_mutation_without_dashboard_approval() {
    let patch = AutomationConfigPatch {
        auto_apply_memory_ops: Some(true),
        require_dashboard_approval: Some(false),
        ..AutomationConfigPatch::default()
    };

    let err = effective_config(&AutomationConfig::default(), Some(&patch)).unwrap_err();

    assert!(
        err.to_string().contains("auto_apply_memory_ops"),
        "unexpected error: {err}"
    );
}

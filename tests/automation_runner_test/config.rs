use std::any::TypeId;

use tracedecay_agent_hosts::automation::config::{
    AutomationBackend, AutomationConfig, AutomationConfigPatch, AutomationHostMode,
    AutomationTaskPatch, effective_config,
};

#[test]
fn automation_defaults_mount_the_final_v2_curators() {
    let config = AutomationConfig::default();

    assert!(config.enabled);
    assert_eq!(config.backend, AutomationBackend::CodexAppServer);
    assert_eq!(config.host_mode, AutomationHostMode::Standalone);
    assert_eq!(config.timeout_secs, 60);
    assert_eq!(config.scheduler_tick_secs, 60);
    assert!(config.combine_due_tasks);
    assert_eq!(config.tasks.memory_curator.interval_secs, Some(900));
    assert_eq!(config.tasks.session_reflector.interval_secs, Some(900));
    assert_eq!(config.tasks.skill_writer.interval_secs, Some(3_600));
    assert_eq!(config.tasks.skill_writer.min_idle_secs, Some(900));
}

#[test]
fn effective_config_applies_typed_patch() {
    let global = AutomationConfig {
        timeout_secs: 45,
        scheduler_tick_secs: 30,
        ..AutomationConfig::default()
    };
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        host_mode: Some(AutomationHostMode::DelegatedHost),
        model_id: Some(Some("gpt-5.6-mini".to_owned())),
        memory_curator: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("manual".to_string())),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };

    let config = effective_config(&global, Some(&patch)).unwrap();

    assert!(config.enabled);
    assert_eq!(config.backend, AutomationBackend::CodexAppServer);
    assert_eq!(config.host_mode, AutomationHostMode::DelegatedHost);
    assert_eq!(config.model_id.as_deref(), Some("gpt-5.6-mini"));
    assert_eq!(config.timeout_secs, 45);
    assert_eq!(config.scheduler_tick_secs, 30);
    assert!(config.tasks.memory_curator.enabled);
    assert_eq!(
        config.tasks.memory_curator.schedule.as_deref(),
        Some("manual")
    );
}

#[test]
fn validation_rejects_zero_scheduler_tick_secs() {
    let patch = AutomationConfigPatch {
        scheduler_tick_secs: Some(0),
        ..AutomationConfigPatch::default()
    };

    let err = effective_config(&AutomationConfig::default(), Some(&patch)).unwrap_err();
    assert!(err.to_string().contains("scheduler_tick_secs"));
}

#[test]
fn validation_rejects_invalid_task_schedule() {
    let patch = AutomationConfigPatch {
        skill_writer: AutomationTaskPatch {
            enabled: Some(true),
            schedule: Some(Some("after lunch".to_string())),
            ..AutomationTaskPatch::default()
        },
        ..AutomationConfigPatch::default()
    };

    let err = effective_config(&AutomationConfig::default(), Some(&patch)).unwrap_err();
    assert!(err.to_string().contains("skill_writer schedule"));
}

#[test]
fn production_automation_contracts_use_leaf_owned_type_identity() {
    assert_eq!(
        TypeId::of::<tracedecay_agent_hosts::automation::config::AutomationConfig>(),
        TypeId::of::<tracedecay_automation::config::AutomationConfig>(),
    );
    assert_eq!(
        TypeId::of::<tracedecay_agent_hosts::automation::backend::AgentTaskKind>(),
        TypeId::of::<tracedecay_automation::backend::AgentTaskKind>(),
    );
    assert_eq!(
        TypeId::of::<tracedecay::retention::RetentionConfig>(),
        TypeId::of::<tracedecay_automation::config::RetentionConfig>(),
    );
}

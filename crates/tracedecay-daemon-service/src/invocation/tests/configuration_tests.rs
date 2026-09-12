use super::*;
use tracedecay_domain::configuration::{
    ConfigurationValueV1, SEMANTIC_RUNTIME_SETTING_KEY, SettingKey,
};
use tracedecay_semantic_contracts::{SemanticConfig, SemanticRuntimeScheduleStatusV1};

fn semantic_runtime_set(config: &SemanticConfig) -> DirectConfigurationMutation {
    DirectConfigurationMutation::Set {
        layer: ConfigurationLayerIdV1::Default,
        key: SettingKey::new(SEMANTIC_RUNTIME_SETTING_KEY).expect("semantic runtime key"),
        value: Box::new(ConfigurationValueV1::Text(
            serde_json::to_string(config).expect("semantic runtime JSON"),
        )),
    }
}

/// The configuration write boundary admits model ids against the production
/// catalog: a structurally valid unknown id is a typed validation refusal,
/// while the default selection and an explicit `None` pass through.
#[test]
fn semantic_runtime_writes_admit_only_cataloged_model_ids() {
    let unknown = SemanticConfig {
        selected_model: Some("NotARealModel".to_owned()),
        ..SemanticConfig::default()
    };
    unknown
        .validate()
        .expect("the contract crate accepts any well-formed id");
    match semantic_profile_transition(&semantic_runtime_set(&unknown)) {
        Err(ConfigurationError::Validation(message)) => assert!(
            message.contains("not in the catalog"),
            "refusal must name catalog admission: {message}"
        ),
        other => panic!("unknown model must be refused at the write boundary: {other:?}"),
    }

    assert_eq!(
        semantic_profile_transition(&semantic_runtime_set(&SemanticConfig::default()))
            .expect("default selection is cataloged"),
        Some(None)
    );
    let disabled = SemanticConfig {
        selected_model: None,
        ..SemanticConfig::default()
    };
    assert_eq!(
        semantic_profile_transition(&semantic_runtime_set(&disabled))
            .expect("a disabled selection needs no catalog entry"),
        Some(None)
    );
}

#[tokio::test]
async fn semantic_commit_wake_survives_deferred_reconciler_install() {
    let registered_configuration_wake = Arc::new(tokio::sync::Notify::new());
    notify_committed_semantic_activation(&registered_configuration_wake);

    let deferred_reconciler_wake = Arc::clone(&registered_configuration_wake);
    tokio::time::timeout(
        Duration::from_millis(100),
        deferred_reconciler_wake.notified(),
    )
    .await
    .expect("a semantic commit before reconciler installation must retain its wake");
}

#[tokio::test]
async fn semantic_scheduler_is_daemon_private_retained_state_not_a_wire_operation() {
    let service = DaemonInvocationService::default();
    let registrar = DaemonSemanticRuntimeRegistrar::new(&service);
    let project_root = PathBuf::from("/project/semantic-runtime");
    let handle = tracedecay_semantic::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20)
        .expect("semantic scheduler");

    registrar
        .register(project_root.clone(), handle.clone())
        .await
        .expect("mount semantic scheduler");
    assert_eq!(
        service
            .project_runtimes
            .get::<tracedecay_semantic::DaemonSemanticRuntimeHandleV1>(&project_root)
            .await
            .expect("retained semantic scheduler")
            .status(),
        SemanticRuntimeScheduleStatusV1::Unavailable
    );
    assert!(matches!(
        registrar.register(project_root, handle).await,
        Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered)
    ));
    assert!(
        serde_json::to_string(&DaemonInvocationOperation::LspOpen)
            .expect("serialize existing operation")
            .find("semantic")
            .is_none(),
        "semantic scheduling must not add a public daemon operation"
    );
}

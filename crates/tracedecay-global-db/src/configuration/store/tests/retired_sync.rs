//! Released sync settings retire without changing supported policy or history.

use super::super::mutation::commit_direct_in_transaction_with_registry;
use super::super::{
    ConfigurationControlStore, ConfigurationError, ConfigurationRevisionStore,
    GlobalDbConfigurationControlStore,
};
use super::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, control_authority, count, direct_project_layer,
    global_setup, id,
};
use crate::configuration::contracts::DirectConfigurationMutation;
use crate::configuration::registry::{ConfigurationRegistry, ConfigurationRegistryError};
use crate::configuration::resolver::resolve_configuration;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::configuration::{
    ConfigurationMutationOperationV1, ConfigurationSnapshotV1, ConfigurationValueKindV1,
    ConfigurationValueV1, DeprecationStateV1, RestartRequirementV1,
    SYNC_READ_COOLDOWN_SECS_SETTING_KEY, SettingDefinitionV1, SettingKey, SettingScopeV1,
    SettingSensitivityV1,
};

// These defaults and descriptors shipped before the controls were retired.
const RELEASED_SYNC_SETTINGS: [(&str, u64); 2] = [
    ("sync.full_sync_escalation_files.v1", 500),
    ("sync.max_concurrent_syncs.v1", 2),
];

#[asupersync::test]
async fn retired_sync_defaults_converge_without_reset_or_history_rewrite() {
    assert_sync_retirement_preserves_history(false).await;
}

#[asupersync::test]
async fn retired_sync_overrides_converge_without_losing_supported_policy() {
    assert_sync_retirement_preserves_history(true).await;
}

async fn assert_sync_retirement_preserves_history(override_retired: bool) {
    let mut released = ConfigurationRegistry::core().unwrap();
    for (raw_key, default) in RELEASED_SYNC_SETTINGS {
        released
            .register(SettingDefinitionV1 {
                key: SettingKey::new(raw_key).unwrap(),
                schema_revision: 7,
                value_kind: ConfigurationValueKindV1::Unsigned,
                default_value: ConfigurationValueV1::Unsigned(default),
                sensitivity: SettingSensitivityV1::Public,
                scope: SettingScopeV1::Project,
                restart_requirement: RestartRequirementV1::DaemonRestart,
                deprecation: DeprecationStateV1::Active,
            })
            .unwrap();
    }
    let directory = tempfile::tempdir().unwrap();
    let profile_root = directory.path().join("profile");
    let project_root = directory.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        id("project.configuration-store.fixture"),
    )
    .await
    .unwrap();
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let initial_revision = id("configuration.revision.released-sync");
    store
        .initialize_canonical_with_registry(
            &initial_revision,
            &resolve_configuration(&released, &[]).unwrap(),
            UtcMicros(1),
            &released,
        )
        .await
        .unwrap();
    let initial = store.current_revision().await.unwrap();

    let cooldown = SettingKey::new(SYNC_READ_COOLDOWN_SECS_SETTING_KEY).unwrap();
    let mut mutations = vec![DirectConfigurationMutation::Set {
        layer: direct_project_layer(),
        key: cooldown.clone(),
        value: Box::new(ConfigurationValueV1::Unsigned(83)),
    }];
    if override_retired {
        mutations.extend(RELEASED_SYNC_SETTINGS.into_iter().zip([900, 7]).map(
            |((raw_key, _), value)| DirectConfigurationMutation::Set {
                layer: direct_project_layer(),
                key: SettingKey::new(raw_key).unwrap(),
                value: Box::new(ConfigurationValueV1::Unsigned(value)),
            },
        ));
    }
    let authority = control_authority(
        ConfigurationMutationOperationV1::DirectMutation,
        &initial_revision,
    );
    let transaction = db.begin_write_transaction().await.unwrap();
    commit_direct_in_transaction_with_registry(
        &transaction,
        &authority,
        &DirectConfigurationMutation::Batch { mutations },
        &initial_revision,
        &released,
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    let historical = store.current_revision().await.unwrap();

    let mut expected_values = historical.snapshot.effective_values.clone();
    let mut expected_provenance = historical.snapshot.provenance.clone();
    for ((raw_key, default), overridden) in RELEASED_SYNC_SETTINGS.into_iter().zip([900, 7]) {
        let key = SettingKey::new(raw_key).unwrap();
        assert_eq!(
            expected_values.remove(&key),
            Some(ConfigurationValueV1::Unsigned(if override_retired {
                overridden
            } else {
                default
            }))
        );
        assert!(expected_provenance.remove(&key).is_some());
    }
    let expected = ConfigurationSnapshotV1::new(expected_values, expected_provenance).unwrap();

    let converged = store
        .converge_registered_registry_shape(&historical.revision_id, UtcMicros(20))
        .await
        .unwrap();
    assert_ne!(converged.revision_id, historical.revision_id);
    assert_eq!(converged.snapshot, expected);
    assert_eq!(
        converged.snapshot.effective_values[&cooldown],
        ConfigurationValueV1::Unsigned(83)
    );
    assert_eq!(
        converged.snapshot.provenance[&cooldown],
        historical.snapshot.provenance[&cooldown]
    );
    let current = store.current_revision().await.unwrap();
    assert_eq!(current.revision_id, converged.revision_id);
    assert_eq!(
        current.parent_revision_id,
        Some(historical.revision_id.clone())
    );
    assert_eq!(current.operation_kind, "registry_shape_convergence");
    assert_eq!(
        store.read_revision(&historical.revision_id).await.unwrap(),
        Some(historical)
    );
    assert_eq!(
        store.read_revision(&initial_revision).await.unwrap(),
        Some(initial)
    );
    assert_eq!(
        store
            .converge_registered_registry_shape(&converged.revision_id, UtcMicros(21))
            .await
            .unwrap(),
        converged,
        "reopening the current shape must not create another revision"
    );
}

#[asupersync::test]
async fn retired_sync_settings_are_absent_and_writes_are_rejected() {
    let registry = ConfigurationRegistry::core().unwrap();
    let (_directory, runtime, root) = Box::pin(global_setup()).await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let authority = control_authority(
        ConfigurationMutationOperationV1::DirectMutation,
        &root.revision_id,
    );

    for (raw_key, default) in RELEASED_SYNC_SETTINGS {
        let key = SettingKey::new(raw_key).unwrap();
        assert!(
            registry
                .definitions()
                .all(|definition| definition.key != key)
        );
        assert!(matches!(
            registry.definition(&key),
            Err(ConfigurationRegistryError::UnknownSetting(rejected)) if rejected == key
        ));
        assert!(!root.snapshot.effective_values.contains_key(&key));
        assert!(!root.snapshot.provenance.contains_key(&key));

        for mutation in [
            DirectConfigurationMutation::Set {
                layer: direct_project_layer(),
                key: key.clone(),
                value: Box::new(ConfigurationValueV1::Unsigned(default)),
            },
            DirectConfigurationMutation::Unset {
                layer: direct_project_layer(),
                key: key.clone(),
            },
        ] {
            assert_eq!(
                store
                    .commit_direct(&authority, &mutation, &root.revision_id)
                    .await,
                Err(ConfigurationError::Validation(format!(
                    "setting key is not registered: {raw_key}"
                )))
            );
            assert_eq!(store.current_revision().await.unwrap(), root);
        }
    }
    let read = db.read_snapshot().await.unwrap();
    assert_eq!(count(&read, "configuration_mutation_receipts").await, 0);
    assert_eq!(count(&read, "configuration_audit_events").await, 0);
}

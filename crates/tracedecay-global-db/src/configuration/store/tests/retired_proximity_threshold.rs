use super::{control_authority_with_key_for_layer, global_setup, point_revision_at_snapshot};
use crate::configuration::contracts::DirectConfigurationMutation;
use crate::configuration::contracts::ports::ConfigurationControlStore;
use crate::configuration::contracts::{ConfigurationCurrentStateV1, ConfigurationError};
use crate::configuration::store::{
    ConfigurationRevisionId, ConfigurationRevisionRecordV1, GlobalDbConfigurationControlStore,
};
use crate::tests::harness::HostAdmissionScope;
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationIdempotencyKey,
    ConfigurationLayerIdV1, ConfigurationMutationOperationV1, ConfigurationSnapshotV1,
    ConfigurationValueV1, SettingKey, TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_domain::feedback::PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1;
use tracedecay_domain::{ProjectId, UtcMicros};
use tracedecay_runtime_core::db::engine::params;

const REGISTRY_REVISION_9_STORE_FIXTURE: &str =
    include_str!("fixtures/configuration-registry-revision9.sql");

const RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY: &str = "feedback.proximity.risk_threshold";

const PROJECT_ID: &str = "project.configuration-store.fixture";

const REPLACED_TABLES: &str = "'configuration_format', 'configuration_revisions', \
     'configuration_entries', 'configuration_topology_policies', \
     'configuration_topology_protected_refs'";

async fn replaced_table_triggers(
    transaction: &crate::registered::RegisteredGlobalDbWriteTransaction<'_>,
) -> Result<Vec<(String, String)>, tracedecay_runtime_core::db::engine::Error> {
    let sql = format!(
        "SELECT name, sql FROM sqlite_master
         WHERE type = 'trigger' AND tbl_name IN ({REPLACED_TABLES})
         ORDER BY name"
    );
    let mut rows = transaction.query(&sql, ()).await?;
    let mut triggers = Vec::new();
    while let Some(row) = rows.next().await? {
        triggers.push((row.get::<String>(0)?, row.get::<String>(1)?));
    }
    Ok(triggers)
}

async fn trigger_names(
    db: &crate::registered::RegisteredGlobalDb,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let read = db.read_snapshot().await?;
    let mut rows = read
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name",
            (),
        )
        .await?;
    let mut names = Vec::new();
    while let Some(row) = rows.next().await? {
        names.push(row.get::<String>(0)?);
    }
    Ok(names)
}

async fn seed_released_registry_key_set(
    transaction: &crate::registered::RegisteredGlobalDbWriteTransaction<'_>,
) -> Result<(), tracedecay_runtime_core::db::engine::Error> {
    let triggers = replaced_table_triggers(transaction).await?;
    let mut replace_rows = String::new();
    for (name, _) in &triggers {
        replace_rows.push_str(&format!(
            "DROP TRIGGER \"{}\";\n",
            name.replace('"', "\"\"")
        ));
    }
    replace_rows.push_str(
        "DELETE FROM configuration_topology_protected_refs;
         DELETE FROM configuration_entries;
         DELETE FROM configuration_topology_policies;
         DELETE FROM configuration_revisions;
         DELETE FROM configuration_format;",
    );
    transaction.execute_batch(&replace_rows).await?;
    transaction
        .execute_batch(REGISTRY_REVISION_9_STORE_FIXTURE)
        .await?;
    for (_, definition) in triggers {
        transaction.execute_batch(&definition).await?;
    }
    Ok(())
}

fn retired_key() -> SettingKey {
    SettingKey::new(RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY).unwrap()
}

fn hints_key() -> SettingKey {
    SettingKey::new(PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1).unwrap()
}

async fn revision_count(db: &crate::registered::RegisteredGlobalDb) -> i64 {
    let read = db.read_snapshot().await.unwrap();
    let mut rows = read
        .query("SELECT COUNT(*) FROM configuration_revisions", ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// Rewrites the root revision to hold both proximity keys, with the retired
/// threshold carrying `retired_provenance`.
async fn seed_both_proximity_keys(
    db: &crate::registered::RegisteredGlobalDb,
    root: &ConfigurationRevisionRecordV1,
    retired_provenance: Vec<ConfigurationCandidateV1>,
) {
    assert_eq!(
        root.snapshot.effective_values.get(&hints_key()),
        Some(&ConfigurationValueV1::Boolean(true))
    );
    assert!(!root.snapshot.effective_values.contains_key(&retired_key()));
    let mut effective_values = root.snapshot.effective_values.clone();
    effective_values.insert(retired_key(), ConfigurationValueV1::Unsigned(7_000));
    let mut provenance = root.snapshot.provenance.clone();
    provenance.insert(retired_key(), retired_provenance);
    let legacy_snapshot = ConfigurationSnapshotV1::new(effective_values, provenance).unwrap();

    let transaction = db.begin_write_transaction().await.unwrap();
    transaction
        .execute_batch("DROP TRIGGER configuration_revisions_immutable_update;")
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO configuration_entries (
                revision_id, key, layer_kind, layer_id, schema_revision, typed_value
             ) VALUES (?1, ?2, 'default', '', 1, ?3)",
            tracedecay_runtime_core::db::engine::params![
                root.revision_id.as_str(),
                RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY,
                serde_json::json!({
                    "schema_version": 1,
                    "value": ConfigurationValueV1::Unsigned(7_000),
                    "provenance": legacy_snapshot.provenance[&retired_key()],
                })
                .to_string(),
            ],
        )
        .await
        .unwrap();
    point_revision_at_snapshot(&transaction, &root.revision_id, &legacy_snapshot).await;
    transaction.commit().await.unwrap();
}

fn assert_converged_without_threshold(
    converged: &ConfigurationCurrentStateV1,
    parent: &ConfigurationRevisionId,
    defaults: &ConfigurationSnapshotV1,
) {
    assert_ne!(
        &converged.revision_id, parent,
        "convergence publishes a child revision"
    );
    assert!(
        !converged
            .snapshot
            .effective_values
            .contains_key(&retired_key()),
        "the retired threshold held only its default and is dropped"
    );
    assert!(!converged.snapshot.provenance.contains_key(&retired_key()));
    assert_eq!(
        converged.snapshot.effective_values.get(&hints_key()),
        Some(&ConfigurationValueV1::Boolean(true)),
        "relationship hints follow the registry default"
    );
    assert_eq!(
        converged.snapshot.provenance.get(&hints_key()),
        defaults.provenance.get(&hints_key())
    );
}

const PR_TRACKING_KEYS: [&str; 2] = [
    "sync.auto_track_pr_branches.v1",
    "sync.auto_track_pr_poll_secs.v1",
];

async fn install_released_pr_tracking_snapshot(db: &crate::registered::RegisteredGlobalDb) {
    let triggers_before = trigger_names(db).await.unwrap();
    let transaction = db.begin_write_transaction().await.unwrap();
    seed_released_registry_key_set(&transaction).await.unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(trigger_names(db).await.unwrap(), triggers_before);
}

#[tokio::test]
async fn released_pr_tracking_defaults_converge_without_a_reset() {
    let (_directory, runtime, _) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    install_released_pr_tracking_snapshot(db).await;
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let before = store.current().await.unwrap();
    for (name, value) in PR_TRACKING_KEYS.into_iter().zip([
        ConfigurationValueV1::Boolean(false),
        ConfigurationValueV1::Unsigned(300),
    ]) {
        let key = SettingKey::new(name).unwrap();
        assert_eq!(before.snapshot.effective_values[&key], value);
        let registry = crate::configuration::registry::ConfigurationRegistry::core().unwrap();
        assert!(registry.validate_value(&key, &value).is_err());
    }
    assert!(
        store
            .reset_retired_pr_tracking_configuration(&before.revision_id, UtcMicros(2))
            .await
            .is_err()
    );
    assert_eq!(store.current().await.unwrap(), before);

    let converged = store
        .converge_registered_registry_shape(&before.revision_id, UtcMicros(3))
        .await
        .expect("never-overridden released settings must upgrade without a reset");
    assert_ne!(converged.revision_id, before.revision_id);
    assert_pr_tracking_absent(&converged);
    assert_eq!(
        store
            .converge_registered_registry_shape(&converged.revision_id, UtcMicros(4))
            .await
            .unwrap(),
        converged
    );
    assert_eq!(revision_count(db).await, 2);
}

fn assert_pr_tracking_absent(current: &ConfigurationCurrentStateV1) {
    for name in PR_TRACKING_KEYS {
        let key = SettingKey::new(name).unwrap();
        assert!(!current.snapshot.effective_values.contains_key(&key));
        assert!(!current.snapshot.provenance.contains_key(&key));
    }
}

async fn seed_released_override(
    db: &crate::registered::RegisteredGlobalDb,
    before: &ConfigurationCurrentStateV1,
    key: &SettingKey,
    value: ConfigurationValueV1,
) {
    let transaction = db.begin_write_transaction().await.unwrap();
    let triggers = replaced_table_triggers(&transaction).await.unwrap();
    for (name, _) in &triggers {
        transaction
            .execute_batch(&format!("DROP TRIGGER \"{}\";", name.replace('"', "\"\"")))
            .await
            .unwrap();
    }
    let mut effective_values = before.snapshot.effective_values.clone();
    effective_values.insert(key.clone(), value.clone());
    let mut provenance = before.snapshot.provenance.clone();
    let candidate = ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Project {
            project_id: ProjectId::new(PROJECT_ID).unwrap(),
        },
        revision_id: before.revision_id.clone(),
        disposition: CandidateDispositionV1::Winning,
        safe_reason: None,
    };
    provenance.insert(key.clone(), vec![candidate]);
    let snapshot = ConfigurationSnapshotV1::new(effective_values, provenance).unwrap();
    transaction
        .execute(
            "UPDATE configuration_entries SET typed_value = ?3
         WHERE revision_id = ?1 AND key = ?2",
            params![
                before.revision_id.as_str(),
                key.as_str(),
                serde_json::json!({
                    "schema_version": 1,
                    "value": value,
                    "provenance": snapshot.provenance[key],
                })
                .to_string()
            ],
        )
        .await
        .unwrap();
    point_revision_at_snapshot(&transaction, &before.revision_id, &snapshot).await;
    for (_, definition) in triggers {
        transaction.execute_batch(&definition).await.unwrap();
    }
    transaction.commit().await.unwrap();
}

#[tokio::test]
async fn explicit_pr_tracking_override_requires_scoped_reset_even_at_old_default_value() {
    let (_directory, runtime, _) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    install_released_pr_tracking_snapshot(db).await;
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let key = SettingKey::new(PR_TRACKING_KEYS[0]).unwrap();
    seed_released_override(
        db,
        &store.current().await.unwrap(),
        &key,
        ConfigurationValueV1::Boolean(false),
    )
    .await;
    let unrelated = SettingKey::new(TELEMETRY_TIMINGS_SETTING_KEY).unwrap();
    seed_released_override(
        db,
        &store.current().await.unwrap(),
        &unrelated,
        ConfigurationValueV1::Boolean(false),
    )
    .await;
    let before = store.current().await.unwrap();

    assert_eq!(
        store
            .converge_registered_registry_shape(&before.revision_id, UtcMicros(2))
            .await,
        Err(ConfigurationError::RetiredPrTrackingResetRequired)
    );
    let wrong_revision = ConfigurationRevisionId::new("configuration.revision.stale").unwrap();
    assert_eq!(
        store
            .reset_retired_pr_tracking_configuration(&wrong_revision, UtcMicros(3))
            .await,
        Err(ConfigurationError::RevisionConflict)
    );
    assert_eq!(store.current().await.unwrap(), before);
    assert_eq!(revision_count(db).await, 1);

    let reset = store
        .reset_retired_pr_tracking_configuration(&before.revision_id, UtcMicros(4))
        .await
        .unwrap();
    assert_pr_tracking_absent(&reset);
    assert_eq!(
        reset.snapshot.effective_values[&unrelated],
        ConfigurationValueV1::Boolean(false)
    );
    assert_eq!(
        reset.snapshot.provenance[&unrelated],
        before.snapshot.provenance[&unrelated]
    );
    assert_eq!(
        store
            .converge_registered_registry_shape(&reset.revision_id, UtcMicros(5))
            .await
            .unwrap(),
        reset
    );
    assert_eq!(revision_count(db).await, 2);
}

#[tokio::test]
async fn pr_tracking_reset_refuses_unrelated_retired_overrides() {
    let (_directory, runtime, _) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    install_released_pr_tracking_snapshot(db).await;
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let key = SettingKey::new(PR_TRACKING_KEYS[1]).unwrap();
    seed_released_override(
        db,
        &store.current().await.unwrap(),
        &key,
        ConfigurationValueV1::Unsigned(301),
    )
    .await;
    seed_released_override(
        db,
        &store.current().await.unwrap(),
        &retired_key(),
        ConfigurationValueV1::Unsigned(7000),
    )
    .await;
    let before = store.current().await.unwrap();
    assert!(matches!(
        store
            .reset_retired_pr_tracking_configuration(&before.revision_id, UtcMicros(2))
            .await,
        Err(ConfigurationError::ResetRequired { .. })
    ));
    assert_eq!(store.current().await.unwrap(), before);
    assert_eq!(revision_count(db).await, 1);
}

#[tokio::test]
async fn default_threshold_beside_relationship_hints_is_dropped() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .expect("registered project database");
    seed_both_proximity_keys(db, &root, root.snapshot.provenance[&hints_key()].clone()).await;
    let store = GlobalDbConfigurationControlStore::new_registered(db);

    let converged = store
        .converge_registered_registry_shape(&root.revision_id, UtcMicros(2))
        .await
        .expect("a default-held retired threshold converges");
    assert_converged_without_threshold(&converged, &root.revision_id, &root.snapshot);
    let layer = ConfigurationLayerIdV1::Project {
        project_id: ProjectId::new(PROJECT_ID).expect("project"),
    };
    let authority = control_authority_with_key_for_layer(
        ConfigurationMutationOperationV1::DirectMutation,
        &converged.revision_id,
        Some(
            ConfigurationIdempotencyKey::new("configuration.idempotency.retired-proximity")
                .expect("idempotency key"),
        ),
        layer.clone(),
    );
    let mutation = DirectConfigurationMutation::Set {
        layer,
        key: retired_key(),
        value: Box::new(ConfigurationValueV1::Unsigned(7_000)),
    };
    let error = store
        .commit_direct(&authority, &mutation, &converged.revision_id)
        .await
        .expect_err("a setter for the retired threshold must be refused as unknown");
    match error {
        ConfigurationError::Validation(message) => assert!(
            message.contains("is not registered"),
            "the retired setter must be refused as an unknown setting: {message}"
        ),
        other => panic!("unexpected setter refusal: {other:?}"),
    }
}

#[tokio::test]
async fn overridden_threshold_is_reset_required() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .expect("registered project database");
    let override_provenance = vec![ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Project {
            project_id: ProjectId::new(PROJECT_ID).expect("project"),
        },
        revision_id: root.revision_id.clone(),
        disposition: CandidateDispositionV1::Winning,
        safe_reason: None,
    }];
    seed_both_proximity_keys(db, &root, override_provenance).await;
    let store = GlobalDbConfigurationControlStore::new_registered(db);

    let error = store
        .converge_registered_registry_shape(&root.revision_id, UtcMicros(2))
        .await
        .expect_err("an overridden retired threshold cannot carry over");
    assert!(
        matches!(error, ConfigurationError::ResetRequired { .. }),
        "an override of a retired setting surfaces the typed reset state: {error:?}"
    );
    assert_eq!(
        revision_count(db).await,
        1,
        "the refused convergence must not write a revision"
    );
}

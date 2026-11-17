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
    ConfigurationValueV1, SettingKey,
};
use tracedecay_domain::feedback::PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1;
use tracedecay_domain::{ProjectId, UtcMicros};

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

#[tokio::test]
async fn released_snapshot_drops_the_default_threshold_and_gains_relationship_hints() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .expect("registered project database");
    let triggers_before = trigger_names(db).await.unwrap();
    let transaction = db.begin_write_transaction().await.unwrap();
    seed_released_registry_key_set(&transaction).await.unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(
        trigger_names(db).await.unwrap(),
        triggers_before,
        "seeding restores every immutability trigger it stepped around"
    );
    let store = GlobalDbConfigurationControlStore::new_registered(db);

    let converged = store
        .converge_registered_registry_shape(&root.revision_id, UtcMicros(2))
        .await
        .expect("a store every earlier release wrote converges without a reset");
    assert_converged_without_threshold(&converged, &root.revision_id, &root.snapshot);

    let again = store
        .converge_registered_registry_shape(&converged.revision_id, UtcMicros(3))
        .await
        .expect("a converged store opens as is");
    assert_eq!(again.revision_id, converged.revision_id);
    assert_eq!(
        revision_count(db).await,
        2,
        "convergence writes one revision"
    );

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

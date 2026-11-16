//! A released configuration key set that still holds
//! `feedback.proximity.risk_threshold`. Registry removal is a fail-closed
//! cutover, not a data migration: the store surfaces typed `ResetRequired`
//! and refuses the retired setter.

use super::{control_authority_with_key_for_layer, global_setup};
use crate::configuration::contracts::DirectConfigurationMutation;
use crate::configuration::contracts::ports::ConfigurationControlStore;
use crate::configuration::store::GlobalDbConfigurationControlStore;
use crate::tests::harness::HostAdmissionScope;
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationOperationV1,
    ConfigurationValueV1, SettingKey,
};
use tracedecay_domain::feedback::PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1;
use tracedecay_domain::{ProjectId, UtcMicros};

/// Configuration rows for the released registry key set that still holds the
/// retired threshold; see the fixture header for its capture.
const RETIRED_THRESHOLD_STORE_FIXTURE: &str =
    include_str!("fixtures/configuration-registry-proximity-threshold.sql");

const RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY: &str = "feedback.proximity.risk_threshold";

const PROJECT_ID: &str = "project.configuration-store.fixture";

/// The five populated configuration tables the released key set replaces.
const REPLACED_TABLES: &str = "'configuration_format', 'configuration_revisions', \
     'configuration_entries', 'configuration_topology_policies', \
     'configuration_topology_protected_refs'";

async fn immutability_triggers(
    transaction: &crate::registered::RegisteredGlobalDbWriteTransaction<'_>,
) -> Result<Vec<String>, tracedecay_runtime_core::db::engine::Error> {
    let sql = format!(
        "SELECT sql FROM sqlite_master
         WHERE type = 'trigger' AND tbl_name IN ({REPLACED_TABLES})
         ORDER BY name"
    );
    let mut rows = transaction.query(&sql, ()).await?;
    let mut triggers = Vec::new();
    while let Some(row) = rows.next().await? {
        triggers.push(row.get::<String>(0)?);
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

/// Replaces the rows of the five populated configuration tables with the
/// released key set. The delete requires the table's own immutability
/// triggers to step aside, so they are re-created from their production
/// definitions before this returns.
async fn seed_released_registry_key_set(
    transaction: &crate::registered::RegisteredGlobalDbWriteTransaction<'_>,
) -> Result<(), tracedecay_runtime_core::db::engine::Error> {
    let triggers = immutability_triggers(transaction).await?;
    transaction
        .execute_batch(
            "DROP TRIGGER IF EXISTS configuration_format_immutable_update;
             DROP TRIGGER IF EXISTS configuration_format_immutable_delete;
             DROP TRIGGER IF EXISTS configuration_revisions_immutable_update;
             DROP TRIGGER IF EXISTS configuration_revisions_immutable_delete;
             DROP TRIGGER IF EXISTS configuration_entries_immutable_update;
             DROP TRIGGER IF EXISTS configuration_entries_immutable_delete;
             DROP TRIGGER IF EXISTS configuration_topology_policy_immutable_update;
             DROP TRIGGER IF EXISTS configuration_topology_policy_immutable_delete;
             DROP TRIGGER IF EXISTS configuration_topology_protected_refs_immutable_update;
             DROP TRIGGER IF EXISTS configuration_topology_protected_refs_immutable_delete;
             DELETE FROM configuration_topology_protected_refs;
             DELETE FROM configuration_entries;
             DELETE FROM configuration_topology_policies;
             DELETE FROM configuration_revisions;
             DELETE FROM configuration_format;",
        )
        .await?;
    transaction
        .execute_batch(RETIRED_THRESHOLD_STORE_FIXTURE)
        .await?;
    for trigger in triggers {
        transaction.execute_batch(&trigger).await?;
    }
    Ok(())
}

#[tokio::test]
async fn snapshot_holding_the_retired_proximity_threshold_is_reset_required() {
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

    let error = store
        .converge_registered_registry_shape(&root.revision_id, UtcMicros(2))
        .await
        .expect_err(
            "a snapshot holding feedback.proximity.risk_threshold must return configuration ResetRequired",
        );

    assert!(
        matches!(
            error,
            crate::configuration::contracts::ConfigurationError::ResetRequired { .. }
        ),
        "the retired entry must surface the typed reset state: {error:?}"
    );

    let read = db.read_snapshot().await.unwrap();
    let mut rows = read
        .query(
            "SELECT COUNT(*) FROM configuration_entries WHERE key = ?1",
            tracedecay_runtime_core::db::engine::params![RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1,
        "the refused convergence must leave the retired entry untouched"
    );
    let mut rows = read
        .query("SELECT COUNT(*) FROM configuration_entries", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        33,
        "no entry is added, removed, or rewritten by the refused convergence"
    );
    let mut rows = read
        .query(
            "SELECT typeof(sealed_policy_value), length(sealed_policy_value)
             FROM configuration_topology_policies",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>(0).unwrap(),
        "blob",
        "production seals the topology policy as a JSON BLOB"
    );
    assert!(row.get::<i64>(1).unwrap() > 0);

    let layer = ConfigurationLayerIdV1::Project {
        project_id: ProjectId::new(PROJECT_ID).expect("project"),
    };
    let authority = control_authority_with_key_for_layer(
        ConfigurationMutationOperationV1::DirectMutation,
        &root.revision_id,
        Some(
            ConfigurationIdempotencyKey::new("configuration.idempotency.retired-proximity")
                .expect("idempotency key"),
        ),
        layer.clone(),
    );
    let mutation = DirectConfigurationMutation::Set {
        layer,
        key: SettingKey::new(RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY).expect("key"),
        value: Box::new(ConfigurationValueV1::Unsigned(7_000)),
    };
    let error = store
        .commit_direct(&authority, &mutation, &root.revision_id)
        .await
        .expect_err("a setter for the retired threshold must be refused as unknown");
    match error {
        crate::configuration::contracts::ConfigurationError::Validation(message) => assert!(
            message.contains("is not registered"),
            "the retired setter must be refused as an unknown setting: {message}"
        ),
        other => panic!("unexpected setter refusal: {other:?}"),
    }
}

#[tokio::test]
async fn canonical_snapshot_with_the_relationship_hint_key_converges() {
    let (_directory, runtime, root) = global_setup().await;
    let key = SettingKey::new(PROXIMITY_RELATIONSHIP_HINTS_SETTING_KEY_V1).unwrap();
    assert_eq!(
        root.snapshot.effective_values.get(&key),
        Some(&ConfigurationValueV1::Boolean(true))
    );
    let retired = SettingKey::new(RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY).unwrap();
    assert!(!root.snapshot.effective_values.contains_key(&retired));
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .expect("registered project database");
    let store = GlobalDbConfigurationControlStore::new_registered(db);

    let converged = store
        .converge_registered_registry_shape(&root.revision_id, UtcMicros(2))
        .await
        .expect("the registered relationship-hint key is an additive registered default");

    assert_eq!(converged.revision_id, root.revision_id);
    assert_eq!(
        converged.snapshot.effective_values.get(&key),
        Some(&ConfigurationValueV1::Boolean(true))
    );
}

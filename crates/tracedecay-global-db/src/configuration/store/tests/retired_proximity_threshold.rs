//! A project store captured before `feedback.proximity.risk_threshold` was
//! retired. Registry removal is a fail-closed cutover, not a data migration:
//! the store surfaces typed `ResetRequired` and refuses the retired setter.

use super::{control_authority_with_key_for_layer, global_setup};
use crate::configuration::contracts::DirectConfigurationMutation;
use crate::configuration::contracts::ports::ConfigurationControlStore;
use crate::configuration::store::GlobalDbConfigurationControlStore;
use crate::tests::harness::HostAdmissionScope;
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationMutationOperationV1,
    ConfigurationValueV1, SettingKey,
};
use tracedecay_domain::{ProjectId, UtcMicros};

/// Byte-exact configuration rows of a store canonically initialized by the
/// pre-change registry; see the fixture header for its capture.
const RETIRED_THRESHOLD_STORE_FIXTURE: &str =
    include_str!("fixtures/configuration-registry-proximity-threshold.sql");

const RETIRED_PROXIMITY_THRESHOLD_SETTING_KEY: &str = "feedback.proximity.risk_threshold";

const PROJECT_ID: &str = "project.configuration-store.fixture";

/// The captured store holds rows exactly as the pre-change registry wrote
/// them, so replaying them into the fixture store reproduces that store.
async fn seed_captured_configuration_store(
    transaction: &crate::registered::RegisteredGlobalDbWriteTransaction<'_>,
) -> Result<(), tracedecay_runtime_core::db::engine::Error> {
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
        .await
}

#[tokio::test]
async fn snapshot_holding_the_retired_proximity_threshold_is_reset_required() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .expect("registered project database");
    let transaction = db.begin_write_transaction().await.unwrap();
    seed_captured_configuration_store(&transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
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

//! Credential, revision, and audit persistence tests.

use super::super::mutation::{
    commit_configuration_transaction, map_store_error, validate_commit_bindings,
};
use super::super::{
    ActivationDriftV1, AuthorizedActor, ConfigurationControlStore, ConfigurationError,
    ConfigurationStoreError, CredentialWritePort, params,
};
use super::{
    ConfigurationAuditEventKindV1, ConfigurationSqlStore, ConfigurationValueV1,
    GlobalDbConfigurationControlStore, HostAdmissionScope, TestConnection, TransactionBehavior,
    UtcMicros, WriteOnlyCredentialMutation, control_authority, count, direct_project_layer,
    global_setup, id, protected_commit, root_revision, seed_revision, setup,
};
use crate::configuration::contracts::DirectConfigurationMutation;
use crate::configuration::registry::ConfigurationRegistry;
use crate::configuration::resolver::resolve_configuration;
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::configuration::{
    ConfigurationMutationOperationV1, CredentialKindV1, SettingKey,
};

#[tokio::test]
async fn canonical_initialization_publishes_one_final_revision_without_legacy_state() {
    let directory = tempfile::tempdir().unwrap();
    let profile_root = directory.path().join("profile");
    let project_root = directory.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let runtime = crate::tests::harness::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        tracedecay_domain::ProjectId::new("project.configuration-initialization").unwrap(),
    )
    .await
    .unwrap();
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let revision_id = tracedecay_domain::configuration::ConfigurationRevisionId::new(
        "configuration.revision.initial",
    )
    .unwrap();
    let resolution = resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[]).unwrap();

    store
        .initialize_canonical(&revision_id, &resolution, UtcMicros(1))
        .await
        .unwrap();

    let current = ConfigurationControlStore::current(&store).await.unwrap();
    assert_eq!(current.revision_id, revision_id);
    assert_eq!(current.snapshot, resolution.snapshot);
    let snapshot = db.read_snapshot().await.unwrap();
    let mut legacy = snapshot
        .query(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE name IN (
                'configuration_migration_receipts',
                'configuration_migration_quarantine'
             )",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        legacy.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

#[tokio::test]
async fn credential_references_are_opaque_and_activation_failure_preserves_last_working() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    store
        .record_component_activation(
            "gateway".to_owned(),
            Some(root.revision_id.clone()),
            None,
            UtcMicros(11),
        )
        .await
        .unwrap();

    let credential_authority = control_authority(
        ConfigurationMutationOperationV1::CredentialWrite,
        &root.revision_id,
    );
    let handle = "opaque-credential-write-handle";
    let write = WriteOnlyCredentialMutation {
        expected_reference_id: None,
        kind: CredentialKindV1::ApiToken,
        write_handle: crate::configuration::contracts::CredentialWriteHandleV1::new(handle)
            .unwrap(),
    };
    let expected_operation_digest = canonical_sha256(&(
        "tracedecay.configuration.credential-write.v1",
        &credential_authority.receipt.actor_id,
        credential_authority.idempotency_key().unwrap(),
        &root.revision_id,
        &write.kind,
        write.write_handle.as_str(),
        &write.expected_reference_id,
    ))
    .unwrap();
    let metadata = store
        .write_reference(&credential_authority, &write, &root.revision_id)
        .await
        .unwrap();
    assert_eq!(
        metadata.rotation, 0,
        "an absent prior reference starts the canonical rotation sequence"
    );
    assert_eq!(metadata.operation_digest, expected_operation_digest);
    assert_ne!(
        metadata.operation_digest, metadata.reference_digest,
        "effect input identity must not alias opaque reference metadata"
    );
    assert_eq!(
        metadata.effective_deadline_at,
        credential_authority.receipt.expires_at
    );
    assert_eq!(
        store
            .write_reference(&credential_authority, &write, &root.revision_id)
            .await
            .unwrap(),
        metadata,
        "an exact credential retry must replay its accepted digest and deadline"
    );
    assert_eq!(
        store
            .write_reference(
                &credential_authority,
                &WriteOnlyCredentialMutation {
                    expected_reference_id: None,
                    kind: CredentialKindV1::ApiToken,
                    write_handle: crate::configuration::contracts::CredentialWriteHandleV1::new(
                        "different-opaque-credential-write-handle",
                    )
                    .unwrap(),
                },
                &root.revision_id,
            )
            .await,
        Err(ConfigurationError::IdempotencyConflict),
        "same-key replay must bind the original opaque handle through its operation digest"
    );
    let read = db.read_snapshot().await.unwrap();
    let mut count_rows = read
        .query(
            "SELECT COUNT(*) FROM configuration_credential_references",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        count_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        1,
        "an absent prior reference must not manufacture a credential row"
    );
    drop(count_rows);
    let mut rows = read
        .query(
            "SELECT reference_digest, operation_digest, effective_deadline_at
             FROM configuration_credential_references
             WHERE reference_id = ?1",
            params![metadata.reference_id.as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let digest = row.get::<String>(0).unwrap();
    assert!(!digest.contains(handle));
    assert_eq!(
        row.get::<String>(1).unwrap(),
        expected_operation_digest.as_str()
    );
    assert_eq!(
        row.get::<i64>(2).unwrap(),
        credential_authority.receipt.expires_at.0
    );
    drop(rows);
    let mut rows = read
        .query(
            "SELECT sealed_target_reference FROM configuration_audit_events",
            (),
        )
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let target = row.get::<Option<Vec<u8>>>(0).unwrap().unwrap_or_default();
        assert!(!String::from_utf8_lossy(&target).contains(handle));
    }
    drop(rows);
    drop(read);

    assert_eq!(
        store
            .write_reference(
                &credential_authority,
                &WriteOnlyCredentialMutation {
                    expected_reference_id: Some(metadata.reference_id.clone()),
                    kind: CredentialKindV1::AccessToken,
                    write_handle: crate::configuration::contracts::CredentialWriteHandleV1::new(
                        "opaque-credential-kind-mismatch",
                    )
                    .unwrap(),
                },
                &root.revision_id,
            )
            .await,
        Err(ConfigurationError::IdempotencyConflict)
    );

    let direct_authority = control_authority(
        ConfigurationMutationOperationV1::DirectMutation,
        &root.revision_id,
    );
    let receipt = store
        .commit_direct(
            &direct_authority,
            &DirectConfigurationMutation::Set {
                layer: direct_project_layer(),
                key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
                value: Box::new(ConfigurationValueV1::Boolean(true)),
            },
            &root.revision_id,
        )
        .await
        .unwrap();
    let actor = AuthorizedActor {
        actor_id: id("actor.configuration.fixture"),
    };
    let state = store.observed_state(&actor).await.unwrap().pop().unwrap();
    assert_eq!(state.desired_revision_id, receipt.result_revision_id);
    assert_eq!(state.observed_revision_id, Some(root.revision_id.clone()));
    assert_eq!(
        state.last_working_revision_id,
        Some(root.revision_id.clone())
    );
    assert!(state.restart_required);
    assert_eq!(state.drift, ActivationDriftV1::PendingRestart);

    store
        .record_component_activation(
            "gateway".to_owned(),
            None,
            Some("gateway_activation_failed".to_owned()),
            UtcMicros(12),
        )
        .await
        .unwrap();
    let state = store.observed_state(&actor).await.unwrap().pop().unwrap();
    assert_eq!(state.observed_revision_id, Some(root.revision_id.clone()));
    assert_eq!(state.last_working_revision_id, Some(root.revision_id));
    assert!(state.restart_required);
    assert_eq!(state.drift, ActivationDriftV1::ActivationFailed);
    assert_eq!(
        state.activation_error_code.as_deref(),
        Some("gateway_activation_failed")
    );
}

#[tokio::test]
async fn revision_store_round_trips_typed_snapshot_plan_receipt_and_audit() {
    let (_directory, connection) = setup().await;
    let root = root_revision();
    seed_revision(&connection, &root).await;
    let store = ConfigurationSqlStore::new(&connection);
    let (plan, commit) = protected_commit(&root);

    assert_eq!(store.current_revision().await.unwrap(), root);
    store.save_change_plan(&plan).await.unwrap();
    store.save_change_plan(&plan).await.unwrap();
    assert_eq!(
        store.read_change_plan(&plan.plan.plan_id).await.unwrap(),
        Some(plan.clone())
    );

    let receipt = store.commit(commit.clone()).await.unwrap();
    assert_eq!(receipt, commit.receipt);
    assert_eq!(
        store
            .read_revision(&commit.next_revision.revision_id)
            .await
            .unwrap(),
        Some(commit.next_revision.clone())
    );
    assert_eq!(
        store.current_revision().await.unwrap(),
        commit.next_revision.clone()
    );
    assert_eq!(store.commit(commit.clone()).await.unwrap(), receipt);

    let mut changed_input = commit.clone();
    changed_input.audit_event.safe_reason_code = Some("changed_input".to_owned());
    assert_eq!(
        store.commit(changed_input).await,
        Err(ConfigurationStoreError::IdempotencyConflict)
    );

    let mut stale = commit.clone();
    stale.change_plan = None;
    stale.next_revision.revision_id = id("configuration.revision.stale");
    stale.receipt.receipt_id = id("configuration.receipt.stale");
    stale.receipt.idempotency_key = id("configuration.idempotency.stale");
    stale.receipt.result_revision_id = stale.next_revision.revision_id.clone();
    stale.audit_event.event_id = id("configuration.audit.stale");
    stale.audit_event.idempotency_key = Some(stale.receipt.idempotency_key.clone());
    stale.audit_event.result_revision_id = Some(stale.next_revision.revision_id.clone());
    stale.audit_event.receipt_id = Some(stale.receipt.receipt_id.clone());
    assert_eq!(
        store.commit(stale).await,
        Err(ConfigurationStoreError::RevisionConflict)
    );

    assert_eq!(
        store.audit(None, 1).await.unwrap(),
        vec![commit.audit_event.clone()]
    );
    assert!(
        store
            .audit(Some(&commit.audit_event.event_id), 1)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(count(&connection, "configuration_revisions").await, 2);
    assert_eq!(
        count(&connection, "configuration_mutation_receipts").await,
        1
    );
    assert_eq!(
        count(&connection, "configuration_change_plan_events").await,
        2
    );
    assert_eq!(count(&connection, "configuration_audit_events").await, 1);
}

#[tokio::test]
async fn rollback_terminal_event_is_persisted_and_visible_in_audit() {
    let (_directory, connection) = setup().await;
    let root = root_revision();
    seed_revision(&connection, &root).await;
    let store = ConfigurationSqlStore::new(&connection);
    let (plan, mut commit) = protected_commit(&root);
    commit.next_revision.operation_kind = "rollback_apply".to_owned();
    commit.audit_event.event_kind = ConfigurationAuditEventKindV1::RollbackApplied;

    store.save_change_plan(&plan).await.unwrap();
    store.commit(commit.clone()).await.unwrap();

    let mut rows = connection
        .query(
            "SELECT event_kind
                 FROM configuration_change_plan_events
                 WHERE plan_id = ?1 AND sequence = 1",
            params![plan.plan.plan_id.as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "rollback_applied"
    );
    assert_eq!(
        store.audit(None, 1).await.unwrap(),
        vec![commit.audit_event]
    );
}

#[tokio::test]
async fn failed_configuration_commit_leaves_no_partial_revision_receipt_or_audit() {
    let (directory, connection) = setup().await;
    let root = root_revision();
    seed_revision(&connection, &root).await;
    let store = ConfigurationSqlStore::new(&connection);
    let (plan, commit) = protected_commit(&root);
    store.save_change_plan(&plan).await.unwrap();

    validate_commit_bindings(&commit).unwrap();
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    assert!(
        commit_configuration_transaction(&transaction, &commit, true, None)
            .await
            .is_err()
    );
    drop(transaction);
    drop(connection);

    let connection = TestConnection::open(&directory.path().join("configuration.db"));
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .await
        .unwrap();
    assert_eq!(count(&connection, "configuration_revisions").await, 1);
    assert_eq!(
        count(&connection, "configuration_mutation_receipts").await,
        0
    );
    assert_eq!(count(&connection, "configuration_audit_events").await, 0);
    assert_eq!(
        count(&connection, "configuration_component_activation_events").await,
        0
    );
    assert_eq!(
        count(&connection, "configuration_change_plan_events").await,
        1
    );
}

/// Byte-exact dump of a store canonically initialized by registry revision 3
/// (tip aae945bf54), whose current snapshot carries the retired
/// `query.default_collection.v1` entry with genuine revision-3 digests.
const REVISION_THREE_STORE_FIXTURE: &str =
    include_str!("fixtures/configuration-registry-revision3.sql");

const RETIRED_DEFAULT_COLLECTION_SETTING_KEY: &str = "query.default_collection.v1";

async fn revision_three_store() -> (tempfile::TempDir, TestConnection) {
    let directory = tempfile::tempdir().unwrap();
    let connection = TestConnection::open(&directory.path().join("configuration.db"));
    connection
        .execute_batch(REVISION_THREE_STORE_FIXTURE)
        .await
        .unwrap();
    (directory, connection)
}

/// The registry-revision-4 cutover retired `query.default_collection.v1`
/// without a data migration: the configuration store's durable contract is
/// exact-final-shape fail-closed, so a store still carrying the retired entry
/// surfaces the typed reset state instead of being interpreted or repaired.
#[tokio::test]
async fn store_carrying_the_retired_default_collection_entry_fails_closed_typed() {
    let (_directory, connection) = revision_three_store().await;

    // The SQL shape is unchanged between registry revisions 3 and 4, so
    // schema admission still passes; the reset must come from the retired
    // entry itself, not from a DDL mismatch.
    assert!(
        crate::configuration::schema::fresh_configuration_store_evidence(&*connection)
            .await
            .unwrap()
            .is_none(),
        "a populated revision-3 store must not present as fresh"
    );
    crate::configuration::schema::admit_configuration_schema(&*connection, None)
        .await
        .unwrap();

    let store = ConfigurationSqlStore::new(&connection);
    let current = store.current_revision().await;
    let Err(ConfigurationStoreError::InvalidData(reason)) = current else {
        panic!("revision-3 snapshot with a retired entry must be invalid data: {current:?}");
    };
    assert!(
        reason.contains("decode configuration snapshot entry"),
        "reset reason must cite the snapshot entry decode: {reason}"
    );
    assert!(
        reason.contains("default_collection"),
        "reset reason must name the retired value: {reason}"
    );
    assert!(
        matches!(
            map_store_error(ConfigurationStoreError::InvalidData(reason)),
            ConfigurationError::ResetRequired { .. }
        ),
        "invalid store data must surface as the typed reset state"
    );

    // The rollback target read uses the same revision read path and must fail
    // closed the same way instead of resurrecting the retired value.
    let target = store
        .read_revision(&id("configuration.revision.root"))
        .await;
    assert!(
        matches!(target, Err(ConfigurationStoreError::InvalidData(_))),
        "rollback-target read of a retired-entry revision must fail closed: {target:?}"
    );

    // Fail-closed means untouched: the store is never silently repaired.
    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM configuration_entries WHERE key = ?1",
            params![RETIRED_DEFAULT_COLLECTION_SETTING_KEY],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1,
        "the retired entry must survive the failed read untouched"
    );
}

#[tokio::test]
async fn fresh_stores_resolve_without_the_retired_default_collection_setting() {
    let registry = ConfigurationRegistry::core().unwrap();
    let retired = SettingKey::new(RETIRED_DEFAULT_COLLECTION_SETTING_KEY).unwrap();
    assert!(
        registry.definition(&retired).is_err(),
        "registry revision 4 must not define the retired setting"
    );
    // Any ingest of the retired key is rejected at the registry boundary.
    assert!(
        registry
            .validate_value(&retired, &ConfigurationValueV1::Boolean(true))
            .is_err(),
        "values for the retired key must be rejected on ingest"
    );

    let resolution = resolve_configuration(&registry, &[]).unwrap();
    assert!(
        !resolution.snapshot.effective_values.contains_key(&retired),
        "fresh resolution must not manufacture the retired setting"
    );

    let (_directory, connection) = setup().await;
    seed_revision(&connection, &root_revision()).await;
    let current = ConfigurationSqlStore::new(&connection)
        .current_revision()
        .await
        .unwrap();
    assert!(
        !current.snapshot.effective_values.contains_key(&retired),
        "a canonically initialized revision-4 store must not carry the retired setting"
    );
}

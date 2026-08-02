//! Control-plane mutation and activation behavior tests.

use super::super::audit::audit_target_commitment;
use super::super::mutation::apply_direct_mutation_to_snapshot;
use super::super::read::read_change_plan_from_executor;
use super::super::revision::insert_revision;
use super::super::{
    ConfigurationControlStore, ConfigurationError, GlobalDbConfigurationControlStore,
    OwnedGlobalDbConfigurationControlStore, params,
};
use super::{
    HostAdmissionScope, control_authority, digest, direct_project_layer, evidence_for,
    global_setup, id, protected_plan_for, root_revision, source_binding_snapshot,
};
use crate::configuration::contracts::{ConfigurationRollbackRequest, DirectConfigurationMutation};
use crate::configuration::registry::ConfigurationRegistry;
use crate::configuration::resolver::registry_default_candidate;
use crate::configuration::resolver::resolve_configuration;
use crate::tests::harness::HostAdmissionTestRuntimeV1;
use std::collections::BTreeSet;
use tracedecay_domain::configuration::CredentialReferenceMetadataV1;
use tracedecay_domain::configuration::{
    AccessRuleId, AuthorityRef, ConfigurationLayerIdV1, ConfigurationMutationOperationV1,
    ConfigurationValueV1, CredentialKindV1, DIAGNOSTICS_PREWARM_SETTING_KEY, ProtectedApplyRequest,
    ProtectedChange, RollbackModeV1, RuleEffect, SOURCE_BINDINGS_SETTING_KEY, ScopeAccessRule,
    ScopeAccessSubjectV1, ScopeControlOperationV1, ScopeSourceBinding, SettingKey, SourceBindingId,
    SourceKindV1,
};
use tracedecay_domain::research::CapabilityId;
use tracedecay_domain::{ActorId, LocatorDigest, ProjectId, UtcMicros, canonical_sha256};
use tracedecay_store::configuration::ConfigurationProtectedOperationV1;

#[test]
fn direct_unset_restores_the_registry_default_and_its_provenance() {
    let registry = ConfigurationRegistry::core().unwrap();
    let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
    let key = SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap();
    let set_revision = id("configuration.revision.set");
    let set = apply_direct_mutation_to_snapshot(
        &current,
        &DirectConfigurationMutation::Set {
            layer: direct_project_layer(),
            key: key.clone(),
            value: ConfigurationValueV1::Boolean(true),
        },
        &set_revision,
        &registry,
    )
    .unwrap();
    let unset = apply_direct_mutation_to_snapshot(
        &set,
        &DirectConfigurationMutation::Unset {
            layer: direct_project_layer(),
            key: key.clone(),
        },
        &id("configuration.revision.unset"),
        &registry,
    )
    .unwrap();

    assert_eq!(
        unset.effective_values[&key],
        registry.definition(&key).unwrap().default_value
    );
    assert_eq!(
        unset.provenance[&key],
        vec![registry_default_candidate().unwrap()]
    );
}

#[test]
fn audit_target_commitments_are_keyed_and_event_scoped() {
    let event_one = id("configuration.audit.one");
    let event_two = id("configuration.audit.two");
    let target = br#"{"binding_id":"binding.fixture"}"#;
    let first = audit_target_commitment(&[1; 32], &event_one, target).unwrap();

    assert_ne!(
        first,
        audit_target_commitment(&[2; 32], &event_one, target).unwrap()
    );
    assert_ne!(
        first,
        audit_target_commitment(&[1; 32], &event_two, target).unwrap()
    );
    assert_ne!(
        first,
        canonical_sha256(&(
            "tracedecay.configuration.audit-target-commitment.v1",
            &event_one,
            target,
        ))
        .unwrap(),
        "a public digest is not an HMAC commitment"
    );
}

#[tokio::test]
async fn partial_rollback_is_typed_unavailable_until_selective_restore_exists() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let authority = control_authority(
        ConfigurationMutationOperationV1::RollbackDryRun,
        &root.revision_id,
    );
    let result = store
        .dry_run_rollback(
            &authority,
            &ConfigurationRollbackRequest {
                target_revision_id: root.revision_id,
                mode: RollbackModeV1::Partial,
            },
            UtcMicros(20),
        )
        .await;
    assert_eq!(result, Err(ConfigurationError::Unavailable));
}

#[tokio::test]
async fn global_control_adapter_enforces_direct_cas_and_exact_replay() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let authority = control_authority(
        ConfigurationMutationOperationV1::DirectMutation,
        &root.revision_id,
    );
    let mutation = DirectConfigurationMutation::Set {
        layer: direct_project_layer(),
        key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
        value: ConfigurationValueV1::Boolean(true),
    };
    let foreign_target = DirectConfigurationMutation::Set {
        layer: ConfigurationLayerIdV1::Project {
            project_id: id("project.foreign.fixture"),
        },
        key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
        value: ConfigurationValueV1::Boolean(true),
    };
    assert_eq!(
        store
            .commit_direct(&authority, &foreign_target, &root.revision_id)
            .await,
        Err(ConfigurationError::MutationAuthorityRejected)
    );

    let receipt = store
        .commit_direct(&authority, &mutation, &root.revision_id)
        .await
        .unwrap();
    assert_eq!(
        store
            .commit_direct(&authority, &mutation, &root.revision_id)
            .await
            .unwrap(),
        receipt
    );

    let conflicting = DirectConfigurationMutation::Set {
        layer: direct_project_layer(),
        key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
        value: ConfigurationValueV1::Boolean(false),
    };
    assert_eq!(
        store
            .commit_direct(&authority, &conflicting, &root.revision_id)
            .await,
        Err(ConfigurationError::RevisionConflict)
    );
    assert_eq!(
        store
            .commit_direct(
                &authority,
                &DirectConfigurationMutation::Set {
                    layer: direct_project_layer(),
                    key: SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
                    value: ConfigurationValueV1::SourceBindings(Vec::new()),
                },
                &root.revision_id,
            )
            .await,
        Err(ConfigurationError::PolicyWideningForbidden)
    );
    let credential_reference = CredentialReferenceMetadataV1::new(
        id("credential.reference.direct-rejection"),
        CredentialKindV1::ApiToken,
        digest('f'),
        UtcMicros(1),
        0,
    )
    .unwrap();
    assert_eq!(
        store
            .commit_direct(
                &authority,
                &DirectConfigurationMutation::Set {
                    layer: direct_project_layer(),
                    key: SettingKey::new("diagnostics.credential_reference.v1").unwrap(),
                    value: ConfigurationValueV1::CredentialReference(credential_reference),
                },
                &root.revision_id,
            )
            .await,
        Err(ConfigurationError::Validation(
            "credential references require the write-only credential operation".to_owned()
        ))
    );
}

fn assert_daemon_configuration_authority<T>()
where
    T: ConfigurationControlStore + Clone + Send + Sync + 'static,
{
}

#[test]
fn owned_global_control_adapter_satisfies_daemon_registration_bounds() {
    assert_daemon_configuration_authority::<OwnedGlobalDbConfigurationControlStore>();
}

#[tokio::test]
async fn owned_global_control_adapter_preserves_cas_while_daemon_scope_is_active() {
    let (_directory, runtime, root) = global_setup().await;
    let store = runtime
        .project_configuration_control_store_for_test()
        .unwrap();

    assert_eq!(store.current().await.unwrap().revision_id, root.revision_id);

    let authority = control_authority(
        ConfigurationMutationOperationV1::DirectMutation,
        &root.revision_id,
    );
    let mutation = DirectConfigurationMutation::Set {
        layer: direct_project_layer(),
        key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
        value: ConfigurationValueV1::Boolean(true),
    };
    let receipt = store
        .commit_direct(&authority, &mutation, &root.revision_id)
        .await
        .unwrap();

    assert_eq!(
        store
            .clone()
            .commit_direct(&authority, &mutation, &root.revision_id)
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        store
            .commit_direct(
                &authority,
                &DirectConfigurationMutation::Set {
                    layer: direct_project_layer(),
                    key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
                    value: ConfigurationValueV1::Boolean(false),
                },
                &root.revision_id,
            )
            .await,
        Err(ConfigurationError::RevisionConflict)
    );
}

#[tokio::test]
async fn owned_global_control_adapter_rejects_writes_after_daemon_scope_ends() {
    let (_directory, runtime, root) = global_setup().await;
    let store = runtime
        .project_configuration_control_store_for_test()
        .unwrap();
    drop(runtime);

    let authority = control_authority(
        ConfigurationMutationOperationV1::DirectMutation,
        &root.revision_id,
    );
    let mutation = DirectConfigurationMutation::Set {
        layer: direct_project_layer(),
        key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
        value: ConfigurationValueV1::Boolean(true),
    };

    assert_eq!(
        store
            .commit_direct(&authority, &mutation, &root.revision_id)
            .await,
        Err(ConfigurationError::Unavailable)
    );
}

#[tokio::test]
async fn daemon_binding_repair_rejects_matching_locator_with_noncanonical_id() {
    let directory = tempfile::tempdir().unwrap();
    let profile_root = directory.path().join("profile");
    let project_root = directory.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        ProjectId::new("project.configuration-binding-repair").unwrap(),
    )
    .await
    .unwrap();
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let mut root = root_revision();
    root.snapshot = source_binding_snapshot(&root.revision_id);
    let transaction = db.begin_write_transaction().await.unwrap();
    insert_revision(&transaction, &root).await.unwrap();
    transaction.commit().await.unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let canonical = ScopeSourceBinding::new(
        id::<SourceBindingId>("binding.canonical.fixture"),
        SourceKindV1::Cursor,
        LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        AuthorityRef::Project(id("project.authoritative.fixture")),
    )
    .unwrap();

    assert_eq!(
        store
            .ensure_daemon_source_binding(canonical, UtcMicros(20))
            .await,
        Err(ConfigurationError::Validation(
            "daemon source binding registry repair found a non-canonical binding id".to_owned()
        ))
    );
}

#[tokio::test]
async fn direct_audit_target_never_persists_sensitive_setting_values() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let authority = control_authority(
        ConfigurationMutationOperationV1::DirectMutation,
        &root.revision_id,
    );
    let secret_path = "private-customer-source/**";
    let receipt = store
        .commit_direct(
            &authority,
            &DirectConfigurationMutation::Set {
                layer: direct_project_layer(),
                key: SettingKey::new("index.exclude.v1").unwrap(),
                value: ConfigurationValueV1::StringList(vec![secret_path.to_owned()]),
            },
            &root.revision_id,
        )
        .await
        .unwrap();

    let read = db.read_snapshot().await.unwrap();
    let mut rows = read
        .query(
            "SELECT sealed_target_reference
                 FROM configuration_audit_events
                 WHERE result_revision_id = ?1",
            params![receipt.result_revision_id.as_str()],
        )
        .await
        .unwrap();
    let target = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Vec<u8>>(0)
        .unwrap();
    assert!(!String::from_utf8_lossy(&target).contains(secret_path));
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn protected_operation_survives_adapter_rebuild_populates_projections_and_rolls_back() {
    let (_directory, runtime, root) = global_setup().await;
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap();
    let actor_id: ActorId = id("actor.configuration.fixture");
    let source_change = ProtectedChange::BindSource(
        ScopeSourceBinding::new(
            id::<SourceBindingId>("binding.restart.fixture"),
            SourceKindV1::Cursor,
            LocatorDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
            AuthorityRef::Project(id::<ProjectId>("project.restart.fixture")),
        )
        .unwrap(),
    );
    let source_plan = protected_plan_for(
        "configuration.plan.restart.source",
        &actor_id,
        &root.revision_id,
        &source_change,
    );
    {
        let store = GlobalDbConfigurationControlStore::new_registered(db);
        store.save_plan(&source_plan, &source_change).await.unwrap();
    }
    let store = GlobalDbConfigurationControlStore::new_registered(db);
    let apply_authority = control_authority(
        ConfigurationMutationOperationV1::ProtectedApply,
        &root.revision_id,
    );
    let source_request = ProtectedApplyRequest {
        plan_id: source_plan.plan_id.clone(),
        actor_id: actor_id.clone(),
        expected_base_revision_id: root.revision_id.clone(),
        operation_digest: source_plan.operation_digest.clone(),
        idempotency_key: id("configuration.idempotency.restart.source"),
    };
    let source_receipt = store
        .commit_protected(
            &apply_authority,
            &source_request,
            &source_plan,
            &evidence_for(&source_plan),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .commit_protected(
                &apply_authority,
                &source_request,
                &source_plan,
                &evidence_for(&source_plan),
            )
            .await
            .unwrap(),
        source_receipt
    );

    let read = db.read_snapshot().await.unwrap();
    let record = read_change_plan_from_executor(&read, &source_plan.plan_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.operation,
        ConfigurationProtectedOperationV1::Change(Box::new(source_change.clone()))
    );
    let mut rows = read
        .query(
            "SELECT COUNT(*), SUM(sealed_target_reference IS NOT NULL)
                 FROM configuration_source_bindings
                 CROSS JOIN configuration_audit_events",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 2);
    assert_eq!(row.get::<i64>(1).unwrap(), 2);
    drop(rows);
    drop(read);

    let access_change = ProtectedChange::UpsertAccessRule(
        ScopeAccessRule::new(
            id::<AccessRuleId>("access-rule.restart.fixture"),
            ScopeAccessSubjectV1 {
                actor: Some(actor_id.clone()),
                operation: Some(ScopeControlOperationV1::Read),
                source_kind: Some(SourceKindV1::Cursor),
            },
            AuthorityRef::Project(id::<ProjectId>("project.restart.fixture")),
            BTreeSet::from([CapabilityId::new("capability.read.fixture").unwrap()]),
            RuleEffect::Deny,
            None,
        )
        .unwrap(),
    );
    let access_plan = protected_plan_for(
        "configuration.plan.restart.access",
        &actor_id,
        &source_receipt.result_revision_id,
        &access_change,
    );
    store.save_plan(&access_plan, &access_change).await.unwrap();
    let access_authority = control_authority(
        ConfigurationMutationOperationV1::ProtectedApply,
        &source_receipt.result_revision_id,
    );
    let access_request = ProtectedApplyRequest {
        plan_id: access_plan.plan_id.clone(),
        actor_id: actor_id.clone(),
        expected_base_revision_id: source_receipt.result_revision_id.clone(),
        operation_digest: access_plan.operation_digest.clone(),
        idempotency_key: id("configuration.idempotency.restart.access"),
    };
    let access_receipt = store
        .commit_protected(
            &access_authority,
            &access_request,
            &access_plan,
            &evidence_for(&access_plan),
        )
        .await
        .unwrap();

    let read = db.read_snapshot().await.unwrap();
    let mut rows = read
        .query("SELECT COUNT(*) FROM configuration_access_rules", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    drop(rows);
    drop(read);

    let rollback_authority = control_authority(
        ConfigurationMutationOperationV1::RollbackDryRun,
        &access_receipt.result_revision_id,
    );
    let rollback = ConfigurationRollbackRequest {
        target_revision_id: root.revision_id.clone(),
        mode: RollbackModeV1::AllOrNothing,
    };
    let rollback_plan = store
        .dry_run_rollback(&rollback_authority, &rollback, UtcMicros(20))
        .await
        .unwrap();
    let rollback_apply_authority = control_authority(
        ConfigurationMutationOperationV1::RollbackApply,
        &access_receipt.result_revision_id,
    );
    let rollback_request = ProtectedApplyRequest {
        plan_id: rollback_plan.plan_id.clone(),
        actor_id,
        expected_base_revision_id: access_receipt.result_revision_id,
        operation_digest: rollback_plan.operation_digest.clone(),
        idempotency_key: id("configuration.idempotency.restart.rollback"),
    };
    let rollback_receipt = store
        .apply_rollback(
            &rollback_apply_authority,
            &rollback_request,
            &rollback_plan,
            &evidence_for(&rollback_plan),
        )
        .await
        .unwrap();
    assert_ne!(rollback_receipt.result_revision_id, root.revision_id);
    assert_eq!(store.current().await.unwrap().snapshot, root.snapshot);
}

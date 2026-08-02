//! Direct configuration-store behavior tests.

use super::migration_store::complete_snapshot_for_current_registry;
use super::read::validate_snapshot_registry_completeness;
use super::revision::insert_revision;
use super::{
    CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME, ConfigurationAuditEvent,
    ConfigurationAuditEventKindV1, ConfigurationCommitV1, ConfigurationMigrationQuarantineEntryV1,
    ConfigurationMigrationReceiptV1, ConfigurationMutationAuthority,
    ConfigurationMutationReceiptV1, ConfigurationProtectedOperationV1,
    ConfigurationProtectedPlanRecordV1, ConfigurationResolutionV1, ConfigurationRevisionId,
    ConfigurationRevisionRecordV1, ConfigurationSnapshotV1, ConfigurationSqlStore,
    ConfigurationValueV1, Connection, GlobalDbConfigurationControlStore, ManifestDigest,
    TestConnection, TransactionBehavior, WriteOnlyCredentialMutation, ensure_configuration_schema,
};
use crate::configuration::contracts::ScopeRevalidationEvidenceV1;
use crate::configuration::migration::LegacyConfigurationSourceKindV1;
use crate::configuration::registry::ConfigurationRegistry;
use crate::configuration::resolver::resolve_configuration;
use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CandidateDispositionV1, ConfigurationCandidateV1,
    ConfigurationGrantId, ConfigurationGrantReceiptId, ConfigurationLayerIdV1,
    ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, DIAGNOSTICS_PREWARM_SETTING_KEY,
    ProtectedChange, ProtectedChangePlan, RedactedConfigurationChangeV1,
    SOURCE_BINDINGS_SETTING_KEY, ScopeControlOperationV1, ScopeSourceBinding, SettingKey,
    SourceBindingId, SourceKindV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, LocatorDigest, ProjectId, UtcMicros, canonical_sha256,
};

async fn setup() -> (tempfile::TempDir, TestConnection) {
    let directory = tempfile::tempdir().unwrap();
    let connection = TestConnection::open(&directory.path().join("configuration.db"));
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .await
        .unwrap();
    ensure_configuration_schema(&*connection).await.unwrap();
    (directory, connection)
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

#[test]
fn forward_repair_materializes_defaults_added_after_a_stored_revision() {
    let registry = ConfigurationRegistry::core().unwrap();
    let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
    let missing_key = SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap();
    let mut effective_values = current.effective_values;
    let mut provenance = current.provenance;
    effective_values.remove(&missing_key);
    provenance.remove(&missing_key);
    let incomplete = ConfigurationSnapshotV1::new(effective_values, provenance).unwrap();
    assert!(validate_snapshot_registry_completeness(&incomplete).is_err());

    let repaired = complete_snapshot_for_current_registry(&incomplete).unwrap();

    validate_snapshot_registry_completeness(&repaired).unwrap();
    assert_eq!(
        repaired.effective_values.get(&missing_key),
        Some(&registry.definition(&missing_key).unwrap().default_value),
    );
}

#[test]
fn forward_repair_disables_pre_digest_semantics_without_changing_install_intent() {
    let registry = ConfigurationRegistry::core().unwrap();
    let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
    let semantic_key =
        SettingKey::new(tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY).unwrap();
    let legacy_artifact_path = std::env::temp_dir().join("tracedecay-semantic-legacy");
    let legacy = serde_json::json!({
        "selected_model": tracedecay_semantic::DEFAULT_FASTEMBED_MODEL_ID,
        "auto_download": true,
        "active_profile": {
            "profile_id": "profile.semantic.legacy.v1",
            "artifact_digest": "a".repeat(64),
            "artifact_path": legacy_artifact_path
        },
        "rollback_profile": null,
        "resources": tracedecay_semantic::SemanticResourceCeilings::default()
    });
    let mut effective_values = current.effective_values;
    let provenance = current.provenance;
    effective_values.insert(
        semantic_key.clone(),
        ConfigurationValueV1::Text(serde_json::to_string(&legacy).unwrap()),
    );
    let legacy_snapshot = ConfigurationSnapshotV1::new(effective_values, provenance).unwrap();
    let ConfigurationValueV1::Text(legacy_text) =
        legacy_snapshot.effective_values.get(&semantic_key).unwrap()
    else {
        panic!("semantic setting must remain typed text");
    };
    assert!(
        serde_json::from_str::<crate::configuration::semantic::SemanticConfig>(legacy_text)
            .is_err(),
        "fixture must reproduce the pre-accepted-profile-digest snapshot"
    );

    let repaired = complete_snapshot_for_current_registry(&legacy_snapshot).unwrap();

    let ConfigurationValueV1::Text(repaired_text) =
        repaired.effective_values.get(&semantic_key).unwrap()
    else {
        panic!("semantic setting must remain typed text");
    };
    let semantic =
        serde_json::from_str::<crate::configuration::semantic::SemanticConfig>(repaired_text)
            .unwrap();
    semantic.validate().unwrap();
    assert_eq!(
        semantic.selected_model.as_deref(),
        Some(tracedecay_semantic::DEFAULT_FASTEMBED_MODEL_ID)
    );
    assert!(semantic.auto_download);
    assert_eq!(
        semantic.resources,
        tracedecay_semantic::SemanticResourceCeilings::default()
    );
    assert!(semantic.active_profile.is_none());
    assert!(semantic.rollback_profile.is_none());
}

#[test]
fn forward_repair_rejects_unrecognized_semantic_configuration() {
    let registry = ConfigurationRegistry::core().unwrap();
    let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
    let semantic_key =
        SettingKey::new(tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY).unwrap();
    let legacy_artifact_path = std::env::temp_dir().join("tracedecay-semantic-legacy");
    let mut effective_values = current.effective_values;
    effective_values.insert(
        semantic_key,
        ConfigurationValueV1::Text(
            serde_json::json!({
                "selected_model": tracedecay_semantic::DEFAULT_FASTEMBED_MODEL_ID,
                "active_profile": {
                    "profile_id": "profile.semantic.legacy.v1",
                    "artifact_digest": "a".repeat(64),
                    "artifact_path": legacy_artifact_path
                },
                "rollback_profile": null,
                "resources": tracedecay_semantic::SemanticResourceCeilings::default(),
                "x": true
            })
            .to_string(),
        ),
    );
    let malformed = ConfigurationSnapshotV1::new(effective_values, current.provenance).unwrap();

    assert!(complete_snapshot_for_current_registry(&malformed).is_err());
}

fn migration_fixture() -> (
    ConfigurationMigrationReceiptV1,
    ConfigurationResolutionV1,
    Vec<ConfigurationMigrationQuarantineEntryV1>,
) {
    let resolution = resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[]).unwrap();
    let receipt = ConfigurationMigrationReceiptV1 {
        receipt_name: CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME,
        source_snapshot_digest: digest('a'),
        initial_revision_id: ConfigurationRevisionId::new("configuration.revision.initial")
            .unwrap(),
        initial_snapshot_id: resolution.snapshot.snapshot_id.clone(),
        created_at: UtcMicros(1),
    };
    let quarantine = vec![ConfigurationMigrationQuarantineEntryV1 {
        source_kind: LegacyConfigurationSourceKindV1::ConfigJson,
        source_key_digest: digest('b'),
        reason: super::super::migration::ConfigurationMigrationQuarantineReasonV1::UnknownKey,
        redacted_value_digest: digest('c'),
        quarantined_at: UtcMicros(1),
    }];
    (receipt, resolution, quarantine)
}

async fn count(connection: &Connection, table: &str) -> i64 {
    let mut rows = connection
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn root_revision() -> ConfigurationRevisionRecordV1 {
    let snapshot = resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[])
        .unwrap()
        .snapshot;
    ConfigurationRevisionRecordV1 {
        revision_id: id("configuration.revision.root"),
        parent_revision_id: None,
        snapshot,
        actor_id: id("actor.configuration.fixture"),
        operation_kind: "migration".to_owned(),
        created_at: UtcMicros(1),
    }
}

fn source_binding_snapshot(revision_id: &ConfigurationRevisionId) -> ConfigurationSnapshotV1 {
    let mut snapshot = resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[])
        .unwrap()
        .snapshot;
    let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap();
    let project_id: ProjectId = id("project.authoritative.fixture");
    let binding = ScopeSourceBinding::new(
        id::<SourceBindingId>("binding.authoritative.fixture"),
        SourceKindV1::Cursor,
        LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        AuthorityRef::Project(project_id.clone()),
    )
    .unwrap();
    let candidate = ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Project { project_id },
        revision_id: revision_id.clone(),
        disposition: CandidateDispositionV1::Winning,
        safe_reason: None,
    };
    snapshot.effective_values.insert(
        key.clone(),
        ConfigurationValueV1::SourceBindings(vec![binding]),
    );
    snapshot.provenance.insert(key, vec![candidate]);
    ConfigurationSnapshotV1::new(snapshot.effective_values, snapshot.provenance).unwrap()
}

fn protected_plan(
    base_revision_id: &ConfigurationRevisionId,
) -> ConfigurationProtectedPlanRecordV1 {
    let operation =
        ConfigurationProtectedOperationV1::Change(Box::new(ProtectedChange::BindSource(
            ScopeSourceBinding::new(
                id::<SourceBindingId>("binding.plan.fixture"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                AuthorityRef::Project(id("project.authoritative.fixture")),
            )
            .unwrap(),
        )));
    let plan = ProtectedChangePlan {
        plan_id: id("configuration.plan.fixture"),
        actor_id: id("actor.configuration.fixture"),
        base_revision_id: base_revision_id.clone(),
        operation_digest: operation.operation_digest().unwrap(),
        resolved_scope_digest: digest('c'),
        membership_digest: Some(digest('d')),
        authorization_policy_digest: id::<AccessPolicyDigest>(&format!(
            "sha256:{}",
            "e".repeat(64)
        )),
        policy_epoch: 7,
        expires_at: UtcMicros(100),
        created_at: UtcMicros(10),
        redacted_changes: vec![RedactedConfigurationChangeV1 {
            setting_key: SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
            operation: ScopeControlOperationV1::SourceBind,
            before_digest: Some(digest('f')),
            after_digest: Some(digest('a')),
        }],
    };
    ConfigurationProtectedPlanRecordV1 { plan, operation }
}

fn protected_commit(
    root: &ConfigurationRevisionRecordV1,
) -> (ConfigurationProtectedPlanRecordV1, ConfigurationCommitV1) {
    let next_revision_id: ConfigurationRevisionId = id("configuration.revision.child");
    let next_revision = ConfigurationRevisionRecordV1 {
        revision_id: next_revision_id.clone(),
        parent_revision_id: Some(root.revision_id.clone()),
        snapshot: source_binding_snapshot(&next_revision_id),
        actor_id: root.actor_id.clone(),
        operation_kind: "protected_apply".to_owned(),
        created_at: UtcMicros(20),
    };
    let plan_record = protected_plan(&root.revision_id);
    let plan = plan_record.plan.clone();
    let receipt = ConfigurationMutationReceiptV1 {
        receipt_id: id("configuration.receipt.fixture"),
        actor_id: root.actor_id.clone(),
        idempotency_key: id("configuration.idempotency.fixture"),
        base_revision_id: root.revision_id.clone(),
        result_revision_id: next_revision_id.clone(),
        operation_digest: plan.operation_digest.clone(),
        receipt_digest: digest('9'),
        created_at: UtcMicros(21),
    };
    let audit_event = ConfigurationAuditEvent {
        event_id: id("configuration.audit.fixture"),
        event_kind: ConfigurationAuditEventKindV1::Applied,
        actor_id: root.actor_id.clone(),
        idempotency_key: Some(receipt.idempotency_key.clone()),
        base_revision_id: root.revision_id.clone(),
        result_revision_id: Some(next_revision_id),
        operation_digest: plan.operation_digest.clone(),
        target_commitment: digest('8'),
        receipt_id: Some(receipt.receipt_id.clone()),
        safe_reason_code: None,
        occurred_at: UtcMicros(22),
    };
    (
        plan_record,
        ConfigurationCommitV1 {
            expected_base_revision_id: root.revision_id.clone(),
            next_revision,
            receipt,
            change_plan: Some(plan),
            audit_event,
        },
    )
}

async fn seed_revision(connection: &Connection, revision: &ConfigurationRevisionRecordV1) {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    insert_revision(&transaction, revision).await.unwrap();
    transaction.commit().await.unwrap();
}

async fn global_setup() -> (
    tempfile::TempDir,
    HostAdmissionTestRuntimeV1,
    ConfigurationRevisionRecordV1,
) {
    let directory = tempfile::tempdir().unwrap();
    let profile_root = directory.path().join("profile");
    let project_root = directory.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        ProjectId::new("project.configuration-store.fixture").unwrap(),
    )
    .await
    .expect("registered configuration runtime");
    let db = runtime
        .registered_database(HostAdmissionScope::Project)
        .expect("registered project database");
    let root = root_revision();
    let transaction = db.begin_write_transaction().await.unwrap();
    insert_revision(&transaction, &root).await.unwrap();
    transaction.commit().await.unwrap();
    (directory, runtime, root)
}

fn policy_digest(byte: char) -> AccessPolicyDigest {
    AccessPolicyDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn direct_project_layer() -> ConfigurationLayerIdV1 {
    ConfigurationLayerIdV1::Project {
        project_id: id("project.configuration.fixture"),
    }
}

fn control_authority(
    operation: ConfigurationMutationOperationV1,
    expected_revision: &ConfigurationRevisionId,
) -> ConfigurationMutationAuthority {
    let (sink, effect) = match operation {
        ConfigurationMutationOperationV1::CredentialWrite => (
            ConfigurationMutationSinkV1::CredentialStore,
            ConfigurationMutationEffectV1::WriteCredentialReference,
        ),
        ConfigurationMutationOperationV1::ProtectedDryRun
        | ConfigurationMutationOperationV1::RollbackDryRun => (
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CreateProtectedChangePlan,
        ),
        ConfigurationMutationOperationV1::DirectMutation
        | ConfigurationMutationOperationV1::ProtectedApply
        | ConfigurationMutationOperationV1::RollbackApply => (
            ConfigurationMutationSinkV1::ConfigurationStore,
            ConfigurationMutationEffectV1::CommitConfigurationRevision,
        ),
    };
    let scope_digest = if operation == ConfigurationMutationOperationV1::DirectMutation {
        canonical_sha256(&(
            "tracedecay.configuration.direct-target-layer.v1",
            direct_project_layer(),
        ))
        .unwrap()
    } else {
        digest('a')
    };
    ConfigurationMutationAuthority {
        receipt: ConfigurationMutationGrantReceiptV1::issue(
            id::<ConfigurationGrantReceiptId>(
                &format!("configuration.grant-receipt.{operation:?}").to_lowercase(),
            ),
            id::<ConfigurationGrantId>("configuration.grant.fixture"),
            id::<ActorId>("actor.configuration.fixture"),
            operation,
            scope_digest,
            expected_revision.clone(),
            7,
            policy_digest('b'),
            sink,
            effect,
            UtcMicros(10),
            UtcMicros(1_000),
        )
        .unwrap(),
    }
}

fn protected_plan_for(
    plan_id: &str,
    actor_id: &ActorId,
    base_revision_id: &ConfigurationRevisionId,
    change: &ProtectedChange,
) -> ProtectedChangePlan {
    ProtectedChangePlan {
        plan_id: id(plan_id),
        actor_id: actor_id.clone(),
        base_revision_id: base_revision_id.clone(),
        operation_digest: change.compute_digest().unwrap(),
        resolved_scope_digest: digest('a'),
        membership_digest: None,
        authorization_policy_digest: policy_digest('b'),
        policy_epoch: 7,
        created_at: UtcMicros(10),
        expires_at: UtcMicros(1_000),
        redacted_changes: vec![RedactedConfigurationChangeV1 {
            setting_key: match change {
                ProtectedChange::BindSource(_)
                | ProtectedChange::RebindSource(_)
                | ProtectedChange::UnbindSource { .. } => {
                    SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap()
                }
                ProtectedChange::UpsertAccessRule(_) | ProtectedChange::RemoveAccessRule { .. } => {
                    SettingKey::new(ACCESS_RULES_SETTING_KEY).unwrap()
                }
                ProtectedChange::ReplaceWorkTopologyPolicy(_) => {
                    SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY).unwrap()
                }
            },
            operation: change.operation_kind(),
            before_digest: Some(digest('c')),
            after_digest: Some(digest('d')),
        }],
    }
}

fn evidence_for(plan: &ProtectedChangePlan) -> ScopeRevalidationEvidenceV1 {
    ScopeRevalidationEvidenceV1 {
        resolved_scope_digest: plan.resolved_scope_digest.clone(),
        membership_digest: plan.membership_digest.clone(),
        authorization_policy_digest: plan.authorization_policy_digest.clone(),
        policy_epoch: plan.policy_epoch,
    }
}

mod control;
mod persistence;

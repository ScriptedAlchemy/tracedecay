use std::collections::BTreeMap;

use tracedecay_domain::configuration::{
    ConfigurationRevisionId, ConfigurationSnapshotV1, ProtectedChangePlan,
    RedactedConfigurationChangeV1, RollbackModeV1, ScopeControlOperationV1, SettingKey,
};
use tracedecay_domain::{AccessPolicyDigest, ActorId, ManifestDigest, UtcMicros};
use tracedecay_store::configuration::{
    ConfigurationProtectedOperationV1, ConfigurationProtectedPlanRecordV1,
    ConfigurationRevisionRecordV1, ConfigurationStoreError,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

#[test]
fn revision_records_are_append_only_typed_values() {
    let snapshot = ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).unwrap();
    let record = ConfigurationRevisionRecordV1 {
        revision_id: id::<ConfigurationRevisionId>("revision.fixture"),
        parent_revision_id: None,
        snapshot,
        actor_id: id::<ActorId>("actor.fixture"),
        operation_kind: "migration".to_owned(),
        created_at: UtcMicros(1),
    };

    record.validate().unwrap();
}

#[test]
fn idempotency_conflicts_have_one_stable_store_outcome() {
    assert_eq!(
        ConfigurationStoreError::IdempotencyConflict.to_string(),
        "configuration idempotency key conflicts with prior input"
    );
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

#[test]
fn protected_plan_records_bind_the_redacted_plan_to_the_exact_operation() {
    let operation = ConfigurationProtectedOperationV1::Rollback {
        target_revision_id: id("revision.target"),
        mode: RollbackModeV1::AllOrNothing,
    };
    let record = ConfigurationProtectedPlanRecordV1 {
        plan: ProtectedChangePlan {
            plan_id: id("plan.exact-operation"),
            actor_id: id("actor.fixture"),
            base_revision_id: id("revision.base"),
            operation_digest: operation.operation_digest().unwrap(),
            resolved_scope_digest: digest('a'),
            membership_digest: None,
            authorization_policy_digest: AccessPolicyDigest::new(format!(
                "sha256:{}",
                "b".repeat(64)
            ))
            .unwrap(),
            policy_epoch: 7,
            created_at: UtcMicros(1),
            expires_at: UtcMicros(2),
            redacted_changes: vec![RedactedConfigurationChangeV1 {
                setting_key: SettingKey::new("scope.source_bindings.v1").unwrap(),
                operation: ScopeControlOperationV1::Rollback,
                before_digest: Some(digest('c')),
                after_digest: Some(digest('d')),
            }],
        },
        operation,
    };

    record.validate().unwrap();

    let mut conflicting = record;
    conflicting.operation = ConfigurationProtectedOperationV1::Rollback {
        target_revision_id: id("revision.other"),
        mode: RollbackModeV1::AllOrNothing,
    };
    assert!(conflicting.validate().is_err());
}

use std::collections::BTreeMap;

use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
use tracedecay_domain::{ActorId, UtcMicros};
use tracedecay_store::configuration::{ConfigurationRevisionRecordV1, ConfigurationStoreError};

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

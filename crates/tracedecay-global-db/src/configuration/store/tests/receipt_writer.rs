//! Configuration receipts and outbox effects through the production registry.
//!
//! Every journey publishes a profile store with `open_registered_test_database_fixture`
//! (the registry and schema installer production admission uses) and submits
//! through that publication's writer. Row checks read the committed tables.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value as Json, json};
use tracedecay_domain::configuration::{
    ConfigurationAuditEvent, ConfigurationAuditEventKindV1, ConfigurationIdempotencyKey,
    ConfigurationReceiptId, ConfigurationRevisionId,
};
use tracedecay_domain::{AccessPolicyDigest, ManifestDigest, UtcMicros};
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{
    ConfigurationCommitV1, ConfigurationMutationReceiptV1, ConfigurationRevisionRecordV1,
    ConfigurationRevisionStore, OutboxEffectStateV1, RepositoryOperationEnvelopeV1,
    RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, RuntimeTransactionIdV1,
    RuntimeTransactionScopeV1, StoreOperationMetadataV1, StoreRuntimeBindingV1,
    TransactionalInboxReceiptV1, TransactionalOutboxEntryV1,
};

use super::super::revision::insert_revision;
use super::super::{ConfigurationStoreError, GlobalDbConfigurationControlStore};
use super::{id, root_revision};
use crate::registered::RegisteredGlobalDb;
use crate::tests::harness::open_registered_test_database_fixture;

const ACTIVATION_DESIRED_RECORDED: &str = "desired_recorded_v1";
const COMPONENT: &str = "gateway";

struct Probe {
    cancellation: tracedecay_store::RuntimeCancellationIdentityV1,
    deadline: tracedecay_store::RuntimeDeadlineV1,
    commit_started: AtomicBool,
}

impl Probe {
    fn for_request(request: &RuntimeSubmitRequestV1) -> Arc<Self> {
        Arc::new(Self {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            commit_started: AtomicBool::new(false),
        })
    }
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &tracedecay_store::RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &tracedecay_store::RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        self.commit_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

async fn publish_profile(
    name: &str,
) -> (
    tempfile::TempDir,
    crate::RegisteredGlobalDbLeaseV1,
    crate::RegisteredGlobalDbOwnerV1,
) {
    let directory = tempfile::tempdir().expect("profile store directory");
    let path = directory.path().join(name);
    let (database, owner) =
        open_registered_test_database_fixture(&path, TestDatabaseRuntimeScope::Profile)
            .await
            .expect("publish profile store through the production registry");
    (directory, database, owner)
}

fn child_commit(root: &ConfigurationRevisionRecordV1, label: &str) -> ConfigurationCommitV1 {
    let next_revision_id: ConfigurationRevisionId = id(&format!("configuration.revision.{label}"));
    let next_revision = ConfigurationRevisionRecordV1 {
        revision_id: next_revision_id.clone(),
        parent_revision_id: Some(root.revision_id.clone()),
        snapshot: root.snapshot.clone(),
        actor_id: root.actor_id.clone(),
        operation_kind: "direct_mutation".to_owned(),
        created_at: UtcMicros(20),
    };
    let operation_digest: ManifestDigest = tracedecay_domain::test_fixtures::digest('a');
    let authorization_policy_digest = AccessPolicyDigest::new(format!("sha256:{}", "e".repeat(64)))
        .expect("authorization policy digest");
    let receipt = ConfigurationMutationReceiptV1 {
        receipt_id: id::<ConfigurationReceiptId>(&format!("configuration.receipt.{label}")),
        actor_id: root.actor_id.clone(),
        idempotency_key: id::<ConfigurationIdempotencyKey>(&format!(
            "configuration.idempotency.{label}"
        )),
        base_revision_id: root.revision_id.clone(),
        result_revision_id: next_revision_id.clone(),
        operation_digest: operation_digest.clone(),
        authorization_policy_epoch: 7,
        authorization_policy_digest,
        authority_revalidated_at: UtcMicros(20),
        receipt_digest: tracedecay_domain::test_fixtures::digest('9'),
        created_at: UtcMicros(21),
        effective_deadline_at: UtcMicros(100),
    };
    let audit_event = ConfigurationAuditEvent {
        event_id: id(&format!("configuration.audit.{label}")),
        event_kind: ConfigurationAuditEventKindV1::Applied,
        actor_id: receipt.actor_id.clone(),
        idempotency_key: Some(receipt.idempotency_key.clone()),
        base_revision_id: receipt.base_revision_id.clone(),
        result_revision_id: Some(next_revision_id),
        operation_digest,
        target_commitment: tracedecay_domain::test_fixtures::digest('8'),
        receipt_id: Some(receipt.receipt_id.clone()),
        safe_reason_code: None,
        occurred_at: UtcMicros(22),
    };
    ConfigurationCommitV1 {
        expected_base_revision_id: root.revision_id.clone(),
        next_revision,
        receipt,
        change_plan: None,
        audit_event,
    }
}

async fn seed_root(db: &RegisteredGlobalDb, root: &ConfigurationRevisionRecordV1) {
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("begin root revision transaction");
    insert_revision(&transaction, root)
        .await
        .expect("insert root revision");
    transaction
        .execute(
            "INSERT INTO configuration_component_activation_events (
                component, desired_revision_id, observed_revision_id,
                last_working_revision_id, restart_required, activation_error_code, occurred_at
             ) VALUES (?1, ?2, ?2, ?2, 0, NULL, 1)",
            params![COMPONENT, root.revision_id.as_str()],
        )
        .await
        .expect("seed component activation");
    transaction.commit().await.expect("commit root revision");
}

fn submit_request(
    binding: &StoreRuntimeBindingV1,
    operation_id: &str,
    key: &str,
    admitted_at: i64,
    payload: RepositoryWritePayloadV1,
) -> RuntimeSubmitRequestV1 {
    let metadata: StoreOperationMetadataV1 = serde_json::from_value(json!({
        "operation_id": operation_id,
        "client_id": "client.runtime",
        "shard_id": binding.shard_id,
        "incarnation": binding.incarnation,
        "authority_epoch": binding.authority_epoch,
        "idempotency": {
            "key": key,
            "command_digest": format!("sha256:{}", "a".repeat(64))
        },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 128,
        "admitted_at": admitted_at
    }))
    .expect("runtime metadata");
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!("transaction.{operation_id}"))
            .expect("transaction id"),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata)
            .expect("compatibility"),
        opened_at: metadata.admitted_at,
    };
    let control: RuntimeRequestControlV1 = serde_json::from_value(json!({
        "requested_at": admitted_at,
        "deadline": { "deadline_id": format!("deadline.{operation_id}") },
        "cancellation": {
            "cancellation_id": format!("cancellation.{operation_id}"),
            "generation": 1
        }
    }))
    .expect("runtime control");
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .expect("runtime submit request")
}

async fn dispatch(
    db: &RegisteredGlobalDb,
    request: RuntimeSubmitRequestV1,
) -> Result<
    RuntimeSubmitOutcomeV1,
    tracedecay_runtime_core::shard_runtime::registry::StoreRuntimeRegistryFailure,
> {
    let probe = Probe::for_request(&request);
    db.runtime_client().dispatch_submit(request, probe).await
}

async fn text_cell(db: &RegisteredGlobalDb, sql: &str, argument: &str) -> String {
    let read = db.read_snapshot().await.expect("read snapshot");
    let mut rows = read
        .query(sql, params![argument])
        .await
        .expect("query text cell");
    let row = rows
        .next()
        .await
        .expect("text cell row")
        .expect("text cell");
    row.get::<String>(0).expect("text cell value")
}

async fn i64_cell(db: &RegisteredGlobalDb, sql: &str, argument: &str) -> i64 {
    let read = db.read_snapshot().await.expect("read snapshot");
    let mut rows = read
        .query(sql, params![argument])
        .await
        .expect("query integer cell");
    let row = rows
        .next()
        .await
        .expect("integer cell row")
        .expect("integer cell");
    row.get::<i64>(0).expect("integer cell value")
}

async fn count_rows(db: &RegisteredGlobalDb, sql: &str) -> i64 {
    let read = db.read_snapshot().await.expect("read snapshot");
    let mut rows = read.query(sql, ()).await.expect("count query");
    let row = rows.next().await.expect("count row").expect("count");
    row.get::<i64>(0).expect("count value")
}

fn require_committed(outcome: RuntimeSubmitOutcomeV1, step: &str) {
    assert!(
        matches!(outcome, RuntimeSubmitOutcomeV1::Committed { .. }),
        "{step} did not commit: {outcome:?}"
    );
}

#[tokio::test]
async fn configuration_commit_admission_rejects_a_foreign_audit_actor() {
    let root = root_revision();
    let (_directory, db, _owner) = publish_profile("profile-audit-binding.db").await;
    seed_root(&db, &root).await;
    let mut commit = child_commit(&root, "audit-binding");
    let valid_request = submit_request(
        db.binding(),
        "operation.configuration.audit-binding",
        "key.configuration.audit-binding",
        21,
        RepositoryWritePayloadV1::Configuration(Box::new(commit.clone())),
    );
    commit.audit_event.actor_id = id("actor.foreign");
    let mut envelope = valid_request.envelope().clone();
    envelope.payload = RepositoryWritePayloadV1::Configuration(Box::new(commit.clone()));
    assert!(
        RuntimeSubmitRequestV1::new(
            envelope,
            valid_request.transaction_scope().clone(),
            valid_request.control().clone(),
        )
        .is_err(),
        "repository admission must reject an audit actor foreign to its receipt"
    );
    assert!(
        GlobalDbConfigurationControlStore::new_registered(&db)
            .commit(commit)
            .await
            .is_err()
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM configuration_revisions").await,
        1
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM configuration_mutation_receipts").await,
        0
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM configuration_audit_events").await,
        0
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM configuration_component_activation_events"
        )
        .await,
        1
    );
}

/// A repository commit and a global-db commit of the same idempotency key are
/// one receipt: each direction replays, and the row carries the store's
/// desired-activation status plus a desired-state event.
#[tokio::test]
async fn repository_and_store_configuration_commits_replay_exactly() {
    let root = root_revision();
    let (_directory, db, _owner) = publish_profile("profile-forward.db").await;
    seed_root(&db, &root).await;
    let commit = child_commit(&root, "forward");
    let request = submit_request(
        db.binding(),
        "operation.configuration.forward",
        "key.configuration.forward",
        21,
        RepositoryWritePayloadV1::Configuration(Box::new(commit.clone())),
    );
    let outcome = dispatch(&db, request)
        .await
        .unwrap_or_else(|error| panic!("repository configuration commit failed: {error:?}"));
    require_committed(outcome, "repository configuration commit");

    let receipt_id = commit.receipt.receipt_id.as_str();
    assert_eq!(
        i64_cell(
            &db,
            "SELECT authorization_policy_epoch FROM configuration_mutation_receipts
             WHERE receipt_id = ?1",
            receipt_id,
        )
        .await,
        7,
        "receipt authorization_policy_epoch"
    );
    assert_eq!(
        text_cell(
            &db,
            "SELECT authorization_policy_digest FROM configuration_mutation_receipts
             WHERE receipt_id = ?1",
            receipt_id,
        )
        .await,
        commit.receipt.authorization_policy_digest.as_str()
    );
    assert_eq!(
        i64_cell(
            &db,
            "SELECT authority_revalidated_at FROM configuration_mutation_receipts
             WHERE receipt_id = ?1",
            receipt_id,
        )
        .await,
        commit.receipt.authority_revalidated_at.0
    );
    assert_eq!(
        i64_cell(
            &db,
            "SELECT effective_deadline_at FROM configuration_mutation_receipts
             WHERE receipt_id = ?1",
            receipt_id,
        )
        .await,
        commit.receipt.effective_deadline_at.0
    );
    assert_eq!(
        text_cell(
            &db,
            "SELECT activation_status FROM configuration_mutation_receipts WHERE receipt_id = ?1",
            receipt_id,
        )
        .await,
        ACTIVATION_DESIRED_RECORDED
    );
    assert_eq!(
        text_cell(
            &db,
            "SELECT desired_revision_id FROM configuration_component_activation_events
             WHERE component = ?1 ORDER BY event_id DESC LIMIT 1",
            COMPONENT,
        )
        .await,
        commit.next_revision.revision_id.as_str(),
        "desired state follows the committed revision"
    );
    assert_eq!(
        i64_cell(
            &db,
            "SELECT restart_required FROM configuration_component_activation_events
             WHERE component = ?1 ORDER BY event_id DESC LIMIT 1",
            COMPONENT,
        )
        .await,
        1
    );
    assert_eq!(
        i64_cell(
            &db,
            "SELECT occurred_at FROM configuration_component_activation_events
             WHERE component = ?1 ORDER BY event_id DESC LIMIT 1",
            COMPONENT,
        )
        .await,
        commit.receipt.created_at.0,
        "desired-state advance uses the receipt time"
    );
    let child_revision = commit.next_revision.revision_id.as_str();
    let default_layers = i64_cell(
        &db,
        "SELECT COUNT(*) FROM configuration_entries
         WHERE revision_id = ?1 AND layer_kind = 'default' AND layer_id = ''",
        child_revision,
    )
    .await;
    assert!(
        default_layers > 0,
        "default-layer entries are stored as the empty layer id, found {default_layers}"
    );
    assert_eq!(
        i64_cell(
            &db,
            "SELECT COUNT(*) FROM configuration_entries
             WHERE revision_id = ?1 AND layer_kind = 'default' AND layer_id IS NULL",
            child_revision,
        )
        .await,
        0,
        "default-layer entries are not NULL"
    );

    let store = GlobalDbConfigurationControlStore::new_registered(&db);
    let replayed = store.commit(commit.clone()).await.unwrap_or_else(|error| {
        panic!("global-db replay of the repository commit failed: {error}")
    });
    assert_eq!(replayed, commit.receipt);
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM configuration_revisions").await,
        2
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM configuration_mutation_receipts").await,
        1
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM configuration_component_activation_events",
        )
        .await,
        2,
        "replay does not append another desired-state event"
    );

    let (_directory, reverse_db, _owner) = publish_profile("profile-reverse.db").await;
    seed_root(&reverse_db, &root).await;
    let reverse = child_commit(&root, "reverse");
    let store = GlobalDbConfigurationControlStore::new_registered(&reverse_db);
    let stored = store
        .commit(reverse.clone())
        .await
        .unwrap_or_else(|error| panic!("global-db configuration commit failed: {error}"));
    assert_eq!(stored, reverse.receipt);
    let request = submit_request(
        reverse_db.binding(),
        "operation.configuration.reverse",
        "key.configuration.reverse",
        30,
        RepositoryWritePayloadV1::Configuration(Box::new(reverse.clone())),
    );
    let outcome = dispatch(&reverse_db, request)
        .await
        .unwrap_or_else(|error| {
            panic!("repository replay of the global-db commit failed: {error:?}")
        });
    require_committed(outcome, "repository replay of the global-db commit");
    assert_eq!(
        count_rows(&reverse_db, "SELECT COUNT(*) FROM configuration_revisions").await,
        2
    );
    assert_eq!(
        count_rows(
            &reverse_db,
            "SELECT COUNT(*) FROM configuration_mutation_receipts",
        )
        .await,
        1
    );
    assert_eq!(
        text_cell(
            &reverse_db,
            "SELECT activation_status FROM configuration_mutation_receipts WHERE receipt_id = ?1",
            reverse.receipt.receipt_id.as_str(),
        )
        .await,
        ACTIVATION_DESIRED_RECORDED
    );
    assert_eq!(
        text_cell(
            &reverse_db,
            "SELECT desired_revision_id FROM configuration_component_activation_events
             WHERE component = ?1 ORDER BY event_id DESC LIMIT 1",
            COMPONENT,
        )
        .await,
        reverse.next_revision.revision_id.as_str()
    );
    let conflicting = {
        let mut conflicting = reverse.clone();
        conflicting.receipt.receipt_digest = tracedecay_domain::test_fixtures::digest('7');
        conflicting
    };
    let conflict = store.commit(conflicting).await;
    assert!(
        matches!(conflict, Err(ConfigurationStoreError::IdempotencyConflict)),
        "a different commit under the same idempotency key conflicts, got {conflict:?}"
    );
}

fn outbox_entry(binding: &StoreRuntimeBindingV1, effect_id: &str) -> TransactionalOutboxEntryV1 {
    let mut target = serde_json::to_value(&binding.shard_id).expect("shard json");
    target["scope"] = json!({ "kind": "project_sessions", "project_id": "project.outbox" });
    serde_json::from_value(json!({
        "identity": {
            "effect_id": effect_id,
            "command_digest": format!("sha256:{}", "e".repeat(64)),
            "ordering_key": "parity.ordering",
            "source_watermark": {
                "shard_id": binding.shard_id,
                "incarnation": binding.incarnation,
                "authority_epoch": binding.authority_epoch,
                "commit_sequence": 0
            },
            "target_watermark": {
                "shard_id": target,
                "incarnation": binding.incarnation,
                "authority_epoch": binding.authority_epoch,
                "commit_sequence": 0
            }
        },
        "effect": "publish_observation",
        "state": "pending",
        "acknowledgement": null,
        "enqueued_at": 1_000,
        "updated_at": 1_000
    }))
    .expect("outbox entry")
}

fn inbox_receipt(entry: &TransactionalOutboxEntryV1) -> TransactionalInboxReceiptV1 {
    let identity = serde_json::to_value(&entry.identity).expect("effect identity");
    let mut target_watermark = identity["target_watermark"].clone();
    target_watermark["commit_sequence"] = json!(1);
    serde_json::from_value(json!({
        "identity": identity,
        "disposition": "applied",
        "target_commit_watermark": target_watermark,
        "committed_at": 2_000
    }))
    .expect("inbox receipt")
}

#[tokio::test]
async fn outbox_transitions_without_an_enqueued_row_name_that_cause() {
    let (_directory, db, _owner) = publish_profile("profile-outbox-missing.db").await;
    let pending = outbox_entry(db.binding(), "effect.parity.missing");
    let mut dispatched = pending.clone();
    dispatched
        .transition(OutboxEffectStateV1::Dispatched, UtcMicros(2_000))
        .expect("dispatch transition");
    let transitions = [
        (
            "operation.outbox.missing.dispatch",
            RepositoryWritePayloadV1::EnqueueOutbox(Box::new(dispatched)),
        ),
        (
            "operation.outbox.missing.acknowledge",
            RepositoryWritePayloadV1::AcknowledgeOutbox(Box::new(inbox_receipt(&pending))),
        ),
    ];
    for (operation_id, payload) in transitions {
        let request = submit_request(db.binding(), operation_id, operation_id, 2_000, payload);
        let error = dispatch(&db, request)
            .await
            .expect_err("a transition of an effect that was never enqueued must fail");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("outbox effect was never enqueued"),
            "{operation_id} without an enqueued row reported: {rendered}"
        );
    }
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM td_runtime_writer_outbox_v1").await,
        0
    );
}

/// The repository attachment owns no effect domain, so it cannot apply an
/// inbox effect; recording an `applied` receipt here would claim work that
/// never ran.
#[tokio::test]
async fn repository_attachment_rejects_applying_an_inbox_effect() {
    let (_directory, db, _owner) = publish_profile("profile-inbox.db").await;
    let binding = db.binding().clone();
    let mut source = serde_json::to_value(&binding.shard_id).expect("shard json");
    source["scope"] = json!({ "kind": "project_sessions", "project_id": "project.inbox" });
    let mut entry = serde_json::to_value(outbox_entry(&binding, "effect.parity.inbox"))
        .expect("outbox entry json");
    entry["identity"]["target_watermark"]["shard_id"] =
        entry["identity"]["source_watermark"]["shard_id"].take();
    entry["identity"]["source_watermark"]["shard_id"] = source;
    entry["state"] = json!("dispatched");
    let entry: TransactionalOutboxEntryV1 =
        serde_json::from_value(entry).expect("inbound outbox entry");
    let request = submit_request(
        &binding,
        "operation.inbox.apply",
        "key.inbox.apply",
        2_000,
        RepositoryWritePayloadV1::ApplyInbox(Box::new(entry)),
    );
    let error = dispatch(&db, request)
        .await
        .expect_err("the repository attachment must not apply an inbox effect");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("repository attachment does not own apply inbox effect"),
        "apply inbox through the repository attachment reported: {rendered}"
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM td_runtime_writer_inbox_v1").await,
        0
    );
}

#[tokio::test]
async fn outbox_enqueue_dispatch_and_acknowledge_commit_through_the_registry() {
    let (_directory, db, _owner) = publish_profile("profile-outbox.db").await;
    let binding = db.binding().clone();
    let pending = outbox_entry(&binding, "effect.parity.lifecycle");
    let effect_id = pending.identity.effect_id.as_str().to_owned();
    let enqueue = submit_request(
        &binding,
        "operation.outbox.enqueue",
        "key.outbox.enqueue",
        1_000,
        RepositoryWritePayloadV1::EnqueueOutbox(Box::new(pending.clone())),
    );
    let outcome = dispatch(&db, enqueue)
        .await
        .unwrap_or_else(|error| panic!("enqueue pending outbox effect failed: {error:?}"));
    require_committed(outcome, "enqueue pending outbox effect");

    let mut dispatched = pending.clone();
    dispatched
        .transition(OutboxEffectStateV1::Dispatched, UtcMicros(2_000))
        .expect("dispatch transition");
    let dispatch_request = submit_request(
        &binding,
        "operation.outbox.dispatch",
        "key.outbox.dispatch",
        2_000,
        RepositoryWritePayloadV1::EnqueueOutbox(Box::new(dispatched)),
    );
    let outcome = dispatch(&db, dispatch_request)
        .await
        .unwrap_or_else(|error| panic!("dispatch outbox effect failed: {error:?}"));
    require_committed(outcome, "dispatch outbox effect");

    let acknowledge = submit_request(
        &binding,
        "operation.outbox.acknowledge",
        "key.outbox.acknowledge",
        3_000,
        RepositoryWritePayloadV1::AcknowledgeOutbox(Box::new(inbox_receipt(&pending))),
    );
    let outcome = dispatch(&db, acknowledge)
        .await
        .unwrap_or_else(|error| panic!("acknowledge outbox effect failed: {error:?}"));
    require_committed(outcome, "acknowledge outbox effect");

    let state = text_cell(
        &db,
        "SELECT state FROM td_runtime_writer_outbox_v1 WHERE effect_id = ?1",
        &effect_id,
    )
    .await;
    assert_eq!(state, "acknowledged");
    let entry_json = text_cell(
        &db,
        "SELECT entry_json FROM td_runtime_writer_outbox_v1 WHERE effect_id = ?1",
        &effect_id,
    )
    .await;
    let entry: Json = serde_json::from_str(&entry_json).expect("outbox entry json");
    assert_eq!(entry["state"], json!("acknowledged"));
    assert!(
        entry["acknowledgement"].is_object(),
        "acknowledged outbox row keeps its acknowledgement receipt: {entry}"
    );
    assert_eq!(
        count_rows(&db, "SELECT COUNT(*) FROM td_runtime_writer_outbox_v1").await,
        1
    );
}

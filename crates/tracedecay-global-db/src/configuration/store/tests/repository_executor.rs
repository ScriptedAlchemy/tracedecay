//! The repository `Configuration` payload committed through the production
//! registry writer onto the real profile schema.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tracedecay_store::{
    RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StoreOperationMetadataV1,
    StoreRuntimeBindingV1,
};

use super::super::mutation::receipt_for_idempotency_from_transaction;
use super::super::read::read_revision_from_executor;
use super::super::revision::insert_revision;
use super::super::{ConfigurationRevisionStore, GlobalDbConfigurationControlStore, params};
use super::{protected_commit, root_revision};
use crate::tests::harness::HostAdmissionTestRuntimeV1;
use tracedecay_domain::test_fixtures::repeated_sha256_text;

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        self.commit_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

fn submit_request(
    binding: &StoreRuntimeBindingV1,
    payload: RepositoryWritePayloadV1,
) -> RuntimeSubmitRequestV1 {
    let metadata: StoreOperationMetadataV1 = serde_json::from_value(json!({
        "operation_id": "operation.configuration.repository-executor",
        "client_id": "client.configuration.repository-executor",
        "shard_id": binding.shard_id,
        "incarnation": binding.incarnation,
        "authority_epoch": binding.authority_epoch,
        "idempotency": {
            "key": "key.configuration.repository-executor",
            "command_digest": repeated_sha256_text('a'),
        },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 4096,
        "admitted_at": 1_000_000
    }))
    .unwrap();
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(
            "transaction.configuration.repository-executor".to_owned(),
        )
        .unwrap(),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata).unwrap(),
        opened_at: metadata.admitted_at,
    };
    let control: RuntimeRequestControlV1 = serde_json::from_value(json!({
        "requested_at": 1_000_000,
        "deadline": { "deadline_id": "deadline.configuration.repository-executor" },
        "cancellation": {
            "cancellation_id": "cancellation.configuration.repository-executor",
            "generation": 1
        }
    }))
    .unwrap();
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .unwrap()
}

async fn scalar(
    connection: &impl tracedecay_runtime_core::db::engine::QueryExecutor,
    sql: &str,
    revision_or_plan_id: &str,
) -> i64 {
    let mut rows = connection
        .query(sql, params![revision_or_plan_id])
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// Catches the executor omitting a NOT NULL receipt column (the receipt
/// insert fails) or storing a default-layer entry under any `layer_id` other
/// than `''`, the value the native cutover keys the default layer by.
#[tokio::test]
async fn repository_configuration_commit_persists_the_full_receipt_on_the_profile_schema() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path().join("profile"))
        .await
        .unwrap();
    let db = runtime.profile_registry();
    let root = root_revision();
    let transaction = db.begin_write_transaction().await.unwrap();
    insert_revision(&transaction, &root).await.unwrap();
    transaction.commit().await.unwrap();
    let (plan, commit) = protected_commit(&root);
    GlobalDbConfigurationControlStore::new_registered(db)
        .save_change_plan(&plan)
        .await
        .unwrap();

    let client = db.runtime_client();
    let request = submit_request(
        client.binding(),
        RepositoryWritePayloadV1::Configuration(Box::new(commit.clone())),
    );
    let probe = Arc::new(Probe {
        cancellation: request.control().cancellation.clone(),
        deadline: request.control().deadline.clone(),
        commit_started: AtomicBool::new(false),
    });
    let outcome = client.dispatch_submit(request, probe).await.unwrap();
    assert!(
        matches!(outcome, RuntimeSubmitOutcomeV1::Committed { .. }),
        "{outcome:?}"
    );

    let read = db.read_snapshot().await.unwrap();
    let stored = receipt_for_idempotency_from_transaction(
        &read,
        &commit.receipt.actor_id,
        &commit.receipt.idempotency_key,
    )
    .await
    .unwrap()
    .expect("the committed receipt is stored");
    assert_eq!(stored.receipt, commit.receipt);
    assert_eq!(stored.plan_id.as_ref(), Some(&plan.plan.plan_id));
    assert_eq!(
        read_revision_from_executor(&read, &commit.next_revision.revision_id)
            .await
            .unwrap(),
        Some(commit.next_revision.clone())
    );
    assert_eq!(
        scalar(
            &read,
            "SELECT COUNT(*) FROM configuration_change_plan_events
             WHERE plan_id = ?1 AND event_kind = 'applied'",
            plan.plan.plan_id.as_str(),
        )
        .await,
        1
    );

    let snapshot = &commit.next_revision.snapshot;
    let keys = snapshot
        .effective_values
        .keys()
        .chain(snapshot.provenance.keys())
        .collect::<BTreeSet<_>>();
    let revision_id = commit.next_revision.revision_id.as_str();
    let default_entries = scalar(
        &read,
        "SELECT COUNT(*) FROM configuration_entries
         WHERE revision_id = ?1 AND layer_kind = 'default'",
        revision_id,
    )
    .await;
    assert_eq!(
        default_entries,
        i64::try_from(keys.len() - 1).unwrap(),
        "every key but the project-bound source bindings sits on the default layer"
    );
    assert_eq!(
        scalar(
            &read,
            "SELECT COUNT(*) FROM configuration_entries
             WHERE revision_id = ?1 AND layer_kind = 'default' AND layer_id = ''",
            revision_id,
        )
        .await,
        default_entries
    );
}

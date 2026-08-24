use sha2::{Digest, Sha256};
use tracedecay_store::{
    CommandDigestV1, RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeRequestControlV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StoreOperationMetadataV1,
    StoreRuntimeBindingV1, TransactionalOutboxEntryV1,
};

pub(crate) fn digest(byte: char) -> CommandDigestV1 {
    let digest = Sha256::digest(byte.to_string().as_bytes());
    let digest = digest
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect::<String>();
    CommandDigestV1::new(format!("sha256:{digest}")).unwrap()
}

pub(crate) fn metadata(
    operation_id: &str,
    key: &str,
    digest_byte: char,
) -> StoreOperationMetadataV1 {
    serde_json::from_value(serde_json::json!({
        "operation_id": operation_id,
        "client_id": "client.runtime",
        "shard_id": {
            "brain_id": "brain.runtime",
            "profile_id": "profile.runtime",
            "scope": { "kind": "project", "project_id": "project.runtime" }
        },
        "incarnation": 1,
        "authority_epoch": 7,
        "idempotency": { "key": key, "command_digest": digest(digest_byte) },
        "durability": "full",
        "priority": "foreground",
        "admission_bytes": 128,
        "admitted_at": 1
    }))
    .unwrap()
}

pub(crate) fn scope(metadata: &StoreOperationMetadataV1) -> RuntimeTransactionScopeV1 {
    RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .unwrap(),
        compatibility: RuntimeBatchCompatibilityV1::from_operation(metadata).unwrap(),
        opened_at: metadata.admitted_at,
    }
}

pub(crate) fn binding(metadata: &StoreOperationMetadataV1) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        metadata.shard_id.clone(),
        metadata.incarnation,
        metadata.authority_epoch,
    )
}

pub(crate) fn outbox(metadata: &StoreOperationMetadataV1) -> TransactionalOutboxEntryV1 {
    serde_json::from_value(serde_json::json!({
        "identity": {
            "effect_id": format!("effect.{}", metadata.operation_id.as_str()),
            "command_digest": digest('e'),
            "ordering_key": "project.runtime.observations",
            "source_watermark": {
                "shard_id": metadata.shard_id,
                "incarnation": metadata.incarnation,
                "authority_epoch": metadata.authority_epoch,
                "commit_sequence": 0
            },
            "target_watermark": {
                "shard_id": {
                    "brain_id": "brain.runtime",
                    "profile_id": "profile.runtime",
                    "scope": { "kind": "project_sessions", "project_id": "project.runtime" }
                },
                "incarnation": 1,
                "authority_epoch": 7,
                "commit_sequence": 0
            }
        },
        "effect": "publish_observation",
        "state": "pending",
        "acknowledgement": null,
        "enqueued_at": 1,
        "updated_at": 1
    }))
    .unwrap()
}

pub(crate) fn request(metadata: StoreOperationMetadataV1) -> RuntimeSubmitRequestV1 {
    let transaction_scope = scope(&metadata);
    let entry = outbox(&metadata);
    let control: RuntimeRequestControlV1 = serde_json::from_value(serde_json::json!({
        "requested_at": 1,
        "deadline": { "deadline_id": "deadline.runtime" },
        "cancellation": { "cancellation_id": "cancellation.runtime", "generation": 1 }
    }))
    .unwrap();
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: RepositoryWritePayloadV1::EnqueueOutbox(Box::new(entry)),
        },
        transaction_scope,
        control,
    )
    .unwrap()
}

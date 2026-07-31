//! Compatibility read/write transaction envelopes and operation-receipt record/replay.

use std::future::Future;
use std::pin::Pin;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use tracedecay_domain::{FactEventId, FactId, FactOwnerV1, ProvenanceId, UtcMicros};
use tracedecay_store::{
    CompatibilityFactTargetV1, FactCompatibilityResult, FactCompatibilityStoreError,
    FactStoreError, FactStoreResult,
};

use super::DatabaseFactStore;
use super::primitives::{
    COMPATIBILITY_READ_OPERATION, COMPATIBILITY_WRITE_OPERATION, OwnerKey, QUERY_OPERATION,
    from_json, row_optional_string, row_string, storage_error, storage_message, to_json,
};

pub(super) async fn finish_read_snapshot<T>(
    snapshot: Transaction<'_>,
    result: FactStoreResult<T>,
) -> FactStoreResult<T> {
    match result {
        Ok(value) => {
            snapshot
                .commit()
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            Ok(value)
        }
        Err(error) => match snapshot.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(storage_error(
                QUERY_OPERATION,
                std::io::Error::other(format!(
                    "{error}; read snapshot rollback also failed: {rollback}"
                )),
            )),
        },
    }
}

#[derive(Clone)]
pub(super) struct CompatibilityOperationReceiptV1 {
    pub(super) fact_id: Option<FactId>,
    pub(super) event_id: Option<FactEventId>,
    pub(super) receipt: Value,
}

pub(super) fn compatibility_digest(material: Value) -> FactStoreResult<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let encoded = to_json(&material, "serialize compatibility request digest")?;
    let digest = Sha256::digest(encoded.as_bytes());
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(value)
}

pub(super) async fn compatibility_lookup_operation_receipt_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    expected_kind: &'static str,
    request_digest: &str,
) -> FactStoreResult<Option<CompatibilityOperationReceiptV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT operation_kind, request_digest, fact_id, event_id, receipt_json
             FROM memory_v2_compatibility_operation_receipts
             WHERE owner_kind = ?1
               AND project_id = ?2
               AND (
                    operation_id = ?3
                    OR (
                        ?6 = 1
                        AND operation_kind = ?4
                        AND request_digest = ?5
                        AND operation_id LIKE 'memory-operation.v1.%'
                    )
               )
             ORDER BY
                CASE WHEN operation_id = ?3 THEN 0 ELSE 1 END,
                recorded_at ASC,
                operation_id ASC
             LIMIT 1",
            params![
                key.kind,
                key.project_id.as_str(),
                operation_id.as_str(),
                expected_kind,
                request_digest,
                i64::from(operation_id.as_str().starts_with("memory-operation.v2.")),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    let operation_kind = row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?;
    let stored_digest = row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?;
    if operation_kind != expected_kind || stored_digest != request_digest {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility operation id was reused with a different request",
        ));
    }
    let fact_id = row_optional_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?
        .map(FactId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let event_id = row_optional_string(&row, 3, COMPATIBILITY_WRITE_OPERATION)?
        .map(FactEventId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    if event_id.is_some() && fact_id.is_none() {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility receipt has an event without a fact",
        ));
    }
    let receipt = from_json::<Value>(
        &row_string(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
        COMPATIBILITY_WRITE_OPERATION,
    )?;
    Ok(Some(CompatibilityOperationReceiptV1 {
        fact_id,
        event_id,
        receipt,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn compatibility_record_operation_receipt_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    operation_kind: &'static str,
    request_digest: &str,
    fact_id: Option<&FactId>,
    event_id: Option<&FactEventId>,
    receipt: &Value,
    recorded_at: UtcMicros,
) -> FactStoreResult<()> {
    if event_id.is_some() && fact_id.is_none() {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility receipt cannot reference an event without a fact",
        ));
    }
    let key = OwnerKey::new(owner)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_compatibility_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                key.kind,
                key.project_id.as_str(),
                operation_id.as_str(),
                operation_kind,
                request_digest,
                fact_id.map(FactId::as_str),
                event_id.map(FactEventId::as_str),
                to_json(receipt, "serialize compatibility operation receipt")?,
                recorded_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

pub(super) fn compatibility_target_digest(
    target: &CompatibilityFactTargetV1,
) -> FactStoreResult<Value> {
    match target {
        CompatibilityFactTargetV1::Canonical(target) => Ok(json!({
            "canonical_fact_id": target.fact_id().as_str(),
        })),
        CompatibilityFactTargetV1::Legacy(query) => Ok(json!({
            "legacy_source_store_id": query.source_store_id().as_str(),
            "legacy_fact_id": query.legacy_fact_id(),
        })),
    }
}

pub(super) fn compatibility_receipt_u64(
    receipt: &Value,
    field: &'static str,
) -> FactStoreResult<u64> {
    receipt.get(field).and_then(Value::as_u64).ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("compatibility receipt {field} is malformed"),
        )
    })
}

impl DatabaseFactStore<'_> {
    pub(super) async fn compatibility_read<T>(
        &self,
        work: impl for<'tx> FnOnce(
            &'tx Transaction<'_>,
        ) -> Pin<
            Box<dyn Future<Output = FactCompatibilityResult<T>> + Send + 'tx>,
        >,
    ) -> FactCompatibilityResult<T> {
        let snapshot = self
            .db
            .begin_memory_read_transaction(COMPATIBILITY_READ_OPERATION)
            .await
            .map_err(|error| {
                FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_READ_OPERATION,
                    error,
                ))
            })?;
        let result = work(&snapshot).await;
        match result {
            Ok(value) => {
                snapshot.commit().await.map_err(|error| {
                    FactCompatibilityStoreError::Store(storage_error(
                        COMPATIBILITY_READ_OPERATION,
                        error,
                    ))
                })?;
                Ok(value)
            }
            Err(error) => match snapshot.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_READ_OPERATION,
                    std::io::Error::other(format!(
                        "{error}; read snapshot rollback also failed: {rollback}"
                    )),
                ))),
            },
        }
    }

    pub(super) async fn compatibility_write<T>(
        &self,
        work: impl for<'tx> FnOnce(
            &'tx Transaction<'_>,
        ) -> Pin<
            Box<dyn Future<Output = FactCompatibilityResult<T>> + Send + 'tx>,
        >,
    ) -> FactCompatibilityResult<T> {
        let transaction = self
            .db
            .begin_memory_write_transaction(COMPATIBILITY_WRITE_OPERATION)
            .await
            .map_err(|error| {
                FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_WRITE_OPERATION,
                    error,
                ))
            })?;
        let result = work(&transaction).await;
        match result {
            Ok(value) => {
                transaction.commit().await.map_err(|error| {
                    FactCompatibilityStoreError::Store(storage_error(
                        COMPATIBILITY_WRITE_OPERATION,
                        error,
                    ))
                })?;
                Ok(value)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_WRITE_OPERATION,
                    std::io::Error::other(format!(
                        "{error}; transaction rollback also failed: {rollback}"
                    )),
                ))),
            },
        }
    }
}

//! Canonical at-rest purge authority for superseded assertion payloads.

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::privacy::{
    MEMORY_FACT_SANITIZER_VERSION_V1, MemoryFactSanitizationV1, sanitize_memory_fact_payload,
};
use tracedecay_domain::{FactAssertionId, FactAssertionV1, FactId, FactOwnerV1, FactPayloadV1};
use tracedecay_store::{
    FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_PRIVACY_PURGE_PAYLOADS,
    ProjectMemoryPrivacyPurgeCursorV1, ProjectMemoryPrivacyPurgeReceiptV1,
};

use super::crud::{payload_material, payload_metadata};
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, QUERY_OPERATION, from_json, row_string,
    storage_error, storage_message, to_json,
};

struct SupersededPayload {
    assertion_id: FactAssertionId,
    fact_id: FactId,
    payload_reference_json: String,
    payload: FactPayloadV1,
}

pub(super) async fn purge_superseded_payloads_for_owner_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    after: Option<&ProjectMemoryPrivacyPurgeCursorV1>,
    limit: usize,
) -> FactStoreResult<ProjectMemoryPrivacyPurgeReceiptV1> {
    if limit == 0 || limit > MAX_PROJECT_MEMORY_PRIVACY_PURGE_PAYLOADS {
        return Err(FactStoreError::InvalidQueryLimit {
            limit,
            max: MAX_PROJECT_MEMORY_PRIVACY_PURGE_PAYLOADS,
        });
    }
    if after.is_some_and(|cursor| cursor.owner() != owner) {
        return Err(FactStoreError::OwnerMismatch);
    }
    let owner_key = OwnerKey::new(owner)?;
    let mut candidates =
        load_superseded_payloads(transaction, &owner_key, None, None, after, limit + 1).await?;
    let has_more = candidates.len() > limit;
    if has_more {
        candidates.pop();
    }
    let next_after = if has_more {
        candidates
            .last()
            .map(|candidate| {
                ProjectMemoryPrivacyPurgeCursorV1::new(
                    owner.clone(),
                    candidate.fact_id.clone(),
                    candidate.assertion_id.clone(),
                )
            })
            .transpose()?
    } else {
        None
    };
    let scanned = u64::try_from(candidates.len()).map_err(|_| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "superseded payload count exceeds the receipt range",
        )
    })?;
    let purged = purge_candidates(transaction, &owner_key, candidates).await?;
    ProjectMemoryPrivacyPurgeReceiptV1::new(
        owner.clone(),
        MEMORY_FACT_SANITIZER_VERSION_V1.to_owned(),
        scanned,
        purged,
        next_after,
    )
}

pub(super) async fn purge_superseded_payloads_for_fact_tx(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    superseding_assertion_id: &FactAssertionId,
) -> FactStoreResult<()> {
    let candidates = load_superseded_payloads(
        transaction,
        owner,
        Some(fact_id),
        Some(superseding_assertion_id),
        None,
        MAX_PROJECT_MEMORY_PRIVACY_PURGE_PAYLOADS + 1,
    )
    .await?;
    if candidates.len() > MAX_PROJECT_MEMORY_PRIVACY_PURGE_PAYLOADS {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "one assertion supersedes more payloads than the bounded purge contract permits",
        ));
    }
    purge_candidates(transaction, owner, candidates).await?;
    Ok(())
}

async fn load_superseded_payloads(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: Option<&FactId>,
    superseding_assertion_id: Option<&FactAssertionId>,
    after: Option<&ProjectMemoryPrivacyPurgeCursorV1>,
    limit: usize,
) -> FactStoreResult<Vec<SupersededPayload>> {
    let mut rows = transaction
        .query(
            "SELECT payloads.assertion_id, payloads.fact_id,
                    assertions.payload_reference_json, payloads.payload_json
             FROM memory_v2_assertion_payloads AS payloads
             JOIN memory_v2_assertions AS assertions
               ON assertions.assertion_id = payloads.assertion_id
              AND assertions.fact_id = payloads.fact_id
              AND assertions.owner_kind = payloads.owner_kind
              AND assertions.project_id = payloads.project_id
             WHERE payloads.owner_kind = ?1 AND payloads.project_id = ?2
               AND (?3 IS NULL OR payloads.fact_id = ?3)
               AND (
                   ?5 IS NULL OR payloads.fact_id > ?5 OR
                   (payloads.fact_id = ?5 AND payloads.assertion_id > ?6)
               )
               AND EXISTS (
                   SELECT 1 FROM memory_v2_assertion_supersession AS supersession
                   WHERE supersession.superseded_assertion_id = payloads.assertion_id
                     AND supersession.fact_id = payloads.fact_id
                     AND supersession.owner_kind = payloads.owner_kind
                     AND supersession.project_id = payloads.project_id
                     AND (?4 IS NULL OR supersession.assertion_id = ?4)
               )
             ORDER BY payloads.fact_id, payloads.assertion_id
             LIMIT ?7",
            params![
                owner.kind,
                owner.project_id.as_str(),
                fact_id.map(FactId::as_str),
                superseding_assertion_id.map(FactAssertionId::as_str),
                after.map(|cursor| cursor.fact_id().as_str()),
                after.map(|cursor| cursor.assertion_id().as_str()),
                i64::try_from(limit).map_err(|_| storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "privacy purge query limit exceeds SQLite range",
                ))?,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut candidates = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        candidates.push(SupersededPayload {
            assertion_id: FactAssertionId::new(row_string(
                &row,
                0,
                PROJECT_MEMORY_WRITE_OPERATION,
            )?)?,
            fact_id: FactId::new(row_string(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?)?,
            payload_reference_json: row_string(&row, 2, PROJECT_MEMORY_WRITE_OPERATION)?,
            payload: from_json(
                &row_string(&row, 3, PROJECT_MEMORY_WRITE_OPERATION)?,
                PROJECT_MEMORY_WRITE_OPERATION,
            )?,
        });
    }
    Ok(candidates)
}

async fn purge_candidates(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    candidates: Vec<SupersededPayload>,
) -> FactStoreResult<u64> {
    let mut flagged = Vec::new();
    for candidate in candidates {
        let payload = &candidate.payload;
        let metadata = payload_metadata(payload.metadata());
        let wire = payload_material(
            payload.content(),
            payload.category(),
            payload.tags(),
            payload.entities(),
            &metadata,
            payload.source_label(),
        );
        let sanitized = sanitize_memory_fact_payload(wire.clone())
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        let clean = matches!(
            sanitized,
            MemoryFactSanitizationV1::Durable { payload, .. } if payload == wire
        );
        if !clean {
            flagged.push(candidate);
        }
    }
    if flagged.is_empty() {
        return Ok(0);
    }

    transaction
        .execute_batch("PRAGMA secure_delete = ON;")
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    for candidate in &flagged {
        record_purge_receipt(transaction, owner, candidate).await?;
        let changed = transaction
            .execute(
                "DELETE FROM memory_v2_assertion_payloads
                 WHERE assertion_id = ?1 AND fact_id = ?2
                   AND owner_kind = ?3 AND project_id = ?4",
                params![
                    candidate.assertion_id.as_str(),
                    candidate.fact_id.as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        if changed != 1 {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "detector-flagged assertion payload disappeared before purge",
            ));
        }
    }
    u64::try_from(flagged.len()).map_err(|_| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "purged payload count exceeds the receipt range",
        )
    })
}

async fn record_purge_receipt(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    candidate: &SupersededPayload,
) -> FactStoreResult<()> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO memory_v2_assertion_payload_purges(
                assertion_id, fact_id, owner_kind, project_id,
                payload_reference_json, detector_revision, purge_reason
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'detector_flagged')",
            params![
                candidate.assertion_id.as_str(),
                candidate.fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                candidate.payload_reference_json.as_str(),
                MEMORY_FACT_SANITIZER_VERSION_V1,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut rows = transaction
        .query(
            "SELECT payload_reference_json, detector_revision, purge_reason
             FROM memory_v2_assertion_payload_purges
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                candidate.assertion_id.as_str(),
                candidate.fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "assertion payload purge receipt insert disappeared",
        ));
    };
    if row_string(&row, 0, QUERY_OPERATION)? != candidate.payload_reference_json
        || row_string(&row, 1, QUERY_OPERATION)? != MEMORY_FACT_SANITIZER_VERSION_V1
        || row_string(&row, 2, QUERY_OPERATION)? != "detector_flagged"
    {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "assertion payload purge receipt identity collision",
        ));
    }
    Ok(())
}

pub(super) async fn assertion_payload_is_explicitly_purged_tx(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT payload_reference_json, detector_revision, purge_reason
             FROM memory_v2_assertion_payload_purges
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(row_string(&row, 0, QUERY_OPERATION)?
        == to_json(
            &assertion.payload().payload_reference()?,
            "serialize assertion payload reference",
        )?
        && !row_string(&row, 1, QUERY_OPERATION)?.is_empty()
        && row_string(&row, 2, QUERY_OPERATION)? == "detector_flagged")
}

pub(super) async fn assertion_payload_exists_tx(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT 1 FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
        .is_some())
}

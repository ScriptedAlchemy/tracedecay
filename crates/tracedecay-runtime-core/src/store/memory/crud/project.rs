//! Canonical project-memory CRUD helpers over the fact/event authority.

use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION, QUERY_OPERATION,
    ensure_project_memory_read_active, from_json, nonnegative_u64, project_memory_category_label,
    project_memory_event_time, row_i64, row_string, storage_error, storage_message,
};
use super::super::projection::{
    load_project_memory_projection_controlled_tx, load_project_memory_projection_tx,
    load_project_memory_projections_controlled_tx, load_project_memory_projections_tx,
};
use super::{DEFAULT_TRUST, commit_fact_tx, query_fact_lineage_controlled_tx};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, verify_memory_fact_sanitization,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactId,
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, FactPayloadV1, LocatorDigest, PayloadAccessState, ProvenanceId, RetentionClass,
    SanitizationReceiptV1, SanitizerDispositionV1, UtcMicros,
};
use tracedecay_store::{
    FactCommitOutcome, FactCommitReceipt, FactLineageQuery, FactReadControl, FactStoreError,
    FactStoreResult, FactWriteBatch, ProjectMemoryFactContentDigestQueryV1,
    ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactHistoryV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactPageV1, ProjectMemoryFactProjectionV1,
};

const PROJECT_MEMORY_RETENTION_CLASS: &str = "project-memory-canonical";

fn ensure_optional_project_memory_read_active(
    read_control: Option<&FactReadControl>,
) -> FactStoreResult<()> {
    match read_control {
        Some(read_control) => ensure_project_memory_read_active(read_control),
        None => Ok(()),
    }
}

pub(in crate::store::memory) async fn list_project_memory_facts_controlled_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactListQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactPageV1> {
    list_project_memory_facts_inner_tx(transaction, query, Some(read_control)).await
}

async fn list_project_memory_facts_inner_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactListQueryV1,
    read_control: Option<&FactReadControl>,
) -> FactStoreResult<ProjectMemoryFactPageV1> {
    ensure_optional_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(query.owner())?;
    let category = query.category().map(project_memory_category_label);
    let min_trust = query.min_trust().map(Confidence::as_f64);
    let has_min_trust = if min_trust.is_some() { 1_i64 } else { 0_i64 };
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactStoreError::InvalidQueryLimit {
            limit: query.limit(),
            max: usize::MAX,
        }
    })?;
    let mut rows = match (query.after_fact_id(), category) {
        (Some(after), Some(category)) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                     FROM memory_v2_current_facts AS current_facts
                     JOIN memory_v2_facts AS facts
                       ON facts.fact_id = current_facts.fact_id
                      AND facts.owner_kind = current_facts.owner_kind
                      AND facts.project_id = current_facts.project_id
                     JOIN memory_v2_assertion_payloads AS payloads
                       ON payloads.assertion_id = current_facts.active_assertion_id
                      AND payloads.fact_id = current_facts.fact_id
                      AND payloads.owner_kind = current_facts.owner_kind
                      AND payloads.project_id = current_facts.project_id
                     WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                       AND facts.owner_json = ?3 AND current_facts.fact_id > ?4
                       AND current_facts.payload_access = 'eligible'
                       AND current_facts.active_assertion_id IS NOT NULL
                       AND current_facts.trust_score >= ?5
                       AND json_extract(payloads.payload_json, '$.category') = ?6
                     ORDER BY current_facts.fact_id ASC LIMIT ?7",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        min_trust.unwrap_or(0.0),
                        category,
                        fetch_limit,
                    ],
                )
                .await
        }
        (Some(after), None) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                     FROM memory_v2_current_facts AS current_facts
                     JOIN memory_v2_facts AS facts
                       ON facts.fact_id = current_facts.fact_id
                      AND facts.owner_kind = current_facts.owner_kind
                      AND facts.project_id = current_facts.project_id
                     WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                       AND facts.owner_json = ?3 AND current_facts.fact_id > ?4
                       AND current_facts.active_assertion_id IS NOT NULL
                       AND (?5 = 0 OR (
                           current_facts.payload_access = 'eligible'
                           AND current_facts.trust_score >= ?6
                       ))
                     ORDER BY current_facts.fact_id ASC LIMIT ?7",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        has_min_trust,
                        min_trust.unwrap_or(0.0),
                        fetch_limit,
                    ],
                )
                .await
        }
        (None, Some(category)) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                     FROM memory_v2_current_facts AS current_facts
                     JOIN memory_v2_facts AS facts
                       ON facts.fact_id = current_facts.fact_id
                      AND facts.owner_kind = current_facts.owner_kind
                      AND facts.project_id = current_facts.project_id
                     JOIN memory_v2_assertion_payloads AS payloads
                       ON payloads.assertion_id = current_facts.active_assertion_id
                      AND payloads.fact_id = current_facts.fact_id
                      AND payloads.owner_kind = current_facts.owner_kind
                      AND payloads.project_id = current_facts.project_id
                     WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                       AND facts.owner_json = ?3
                       AND current_facts.payload_access = 'eligible'
                       AND current_facts.active_assertion_id IS NOT NULL
                       AND current_facts.trust_score >= ?4
                       AND json_extract(payloads.payload_json, '$.category') = ?5
                     ORDER BY current_facts.fact_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        min_trust.unwrap_or(0.0),
                        category,
                        fetch_limit,
                    ],
                )
                .await
        }
        (None, None) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                     FROM memory_v2_current_facts AS current_facts
                     JOIN memory_v2_facts AS facts
                       ON facts.fact_id = current_facts.fact_id
                      AND facts.owner_kind = current_facts.owner_kind
                      AND facts.project_id = current_facts.project_id
                     WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                       AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL
                       AND (?4 = 0 OR (
                           current_facts.payload_access = 'eligible'
                           AND current_facts.trust_score >= ?5
                       ))
                     ORDER BY current_facts.fact_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        has_min_trust,
                        min_trust.unwrap_or(0.0),
                        fetch_limit,
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    ensure_optional_project_memory_read_active(read_control)?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        ensure_optional_project_memory_read_active(read_control)?;
        fact_ids.push(FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?);
    }
    drop(rows);
    ensure_optional_project_memory_read_active(read_control)?;
    let has_more = fact_ids.len() > query.limit();
    fact_ids.truncate(query.limit());
    let facts = match read_control {
        Some(read_control) => {
            load_project_memory_projections_controlled_tx(
                transaction,
                query.owner(),
                &fact_ids,
                read_control,
            )
            .await?
        }
        None => load_project_memory_projections_tx(transaction, query.owner(), &fact_ids).await?,
    };
    ensure_optional_project_memory_read_active(read_control)?;
    let next = has_more
        .then(|| facts.last().map(|fact| fact.fact_id().clone()))
        .flatten();
    ProjectMemoryFactPageV1::new(query.owner().clone(), facts, next).map_err(Into::into)
}

pub(in crate::store::memory) async fn get_project_memory_fact_controlled_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactIdV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
    ensure_project_memory_read_active(read_control)?;
    let projection = load_project_memory_projection_controlled_tx(
        transaction,
        target.owner(),
        target.fact_id(),
        read_control,
    )
    .await?;
    ensure_project_memory_read_active(read_control)?;
    Ok(projection)
}

fn content_digest(content: &str) -> FactStoreResult<LocatorDigest> {
    LocatorDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(content.as_bytes()))
    ))
    .map_err(FactStoreError::from)
}

pub(in crate::store::memory) async fn find_project_memory_fact_by_content_digest_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactContentDigestQueryV1,
) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
    find_project_memory_fact_by_content_digest_inner_tx(transaction, query, None).await
}

pub(in crate::store::memory) async fn find_project_memory_fact_by_content_digest_controlled_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactContentDigestQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
    find_project_memory_fact_by_content_digest_inner_tx(transaction, query, Some(read_control))
        .await
}

async fn find_project_memory_fact_by_content_digest_inner_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactContentDigestQueryV1,
    read_control: Option<&FactReadControl>,
) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
    ensure_optional_project_memory_read_active(read_control)?;
    let key = OwnerKey::new(query.owner())?;
    let mut rows = transaction
        .query(
            "SELECT current_facts.fact_id, payloads.payload_json
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
             ORDER BY current_facts.fact_id ASC",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_optional_project_memory_read_active(read_control)?;
    let mut matching_fact_id = None;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_optional_project_memory_read_active(read_control)?;
        let payload = from_json::<FactPayloadV1>(
            &row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?;
        if content_digest(payload.content())? == *query.content_digest() {
            matching_fact_id = Some(FactId::new(row_string(
                &row,
                0,
                PROJECT_MEMORY_READ_OPERATION,
            )?)?);
            break;
        }
    }
    drop(rows);
    ensure_optional_project_memory_read_active(read_control)?;
    match matching_fact_id {
        Some(fact_id) => {
            let projection = match read_control {
                Some(read_control) => {
                    load_project_memory_projection_controlled_tx(
                        transaction,
                        query.owner(),
                        &fact_id,
                        read_control,
                    )
                    .await?
                }
                None => {
                    load_project_memory_projection_tx(transaction, query.owner(), &fact_id).await?
                }
            };
            ensure_optional_project_memory_read_active(read_control)?;
            Ok(projection)
        }
        None => Ok(None),
    }
}

pub(in crate::store::memory) async fn project_memory_fact_history_controlled_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactHistoryQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactHistoryV1> {
    ensure_project_memory_read_active(read_control)?;
    let lineage = FactLineageQuery::new(
        query.target().owner().clone(),
        query.target().fact_id().clone(),
        query.after().cloned(),
        query.limit(),
    )?;
    let events = query_fact_lineage_controlled_tx(transaction, &lineage, read_control).await?;
    let history = ProjectMemoryFactHistoryV1::new(
        query.target().owner().clone(),
        query.target().fact_id().clone(),
        events,
        None,
    )?;
    ensure_project_memory_read_active(read_control)?;
    Ok(history)
}

pub(in crate::store::memory) struct SanitizedPayload {
    pub(in crate::store::memory) payload: FactPayloadV1,
    pub(in crate::store::memory) access: PayloadAccessState,
}

pub(in crate::store::memory) fn payload_metadata(metadata: &Value) -> Value {
    let mut metadata = metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove("automation_run_id");
    }
    metadata
}

pub(in crate::store::memory) fn sanitize_payload(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
    source_label: Option<&str>,
) -> FactStoreResult<Option<SanitizedPayload>> {
    let metadata = payload_metadata(metadata);
    let sanitized = sanitize_memory_fact_payload(payload_material(
        content,
        category,
        tags,
        entities,
        &metadata,
        source_label,
    ))
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitized else {
        return Ok(None);
    };
    payload_from_parts(payload, category, receipt).map(Some)
}

pub(in crate::store::memory) fn verified_payload(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
    source_label: Option<&str>,
    receipt: SanitizationReceiptV1,
) -> FactStoreResult<SanitizedPayload> {
    let metadata = payload_metadata(metadata);
    let payload = payload_material(content, category, tags, entities, &metadata, source_label);
    verify_memory_fact_sanitization(&payload, &receipt)
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    payload_from_parts(payload, category, receipt)
}

fn payload_material(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
    source_label: Option<&str>,
) -> Value {
    let mut material = serde_json::Map::new();
    material.insert("content".to_owned(), Value::String(content.to_owned()));
    material.insert(
        "category".to_owned(),
        Value::String(project_memory_category_label(category).to_owned()),
    );
    material.insert("tags".to_owned(), json!(tags));
    material.insert("entities".to_owned(), json!(entities));
    material.insert("metadata".to_owned(), metadata.clone());
    if let Some(source_label) = source_label {
        material.insert(
            "source_label".to_owned(),
            Value::String(source_label.to_owned()),
        );
    }
    Value::Object(material)
}

fn value_strings(value: &Value, field: &'static str) -> FactStoreResult<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                format!("{field} is not an array"),
            )
        })?
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    format!("{field} contains a non-string"),
                )
            })
        })
        .collect()
}

fn payload_from_parts(
    payload: Value,
    category: FactCategoryV1,
    receipt: SanitizationReceiptV1,
) -> FactStoreResult<SanitizedPayload> {
    let content = payload
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "sanitized content is missing",
            )
        })?
        .to_owned();
    let tags = value_strings(
        payload.get("tags").ok_or_else(|| {
            storage_message(PROJECT_MEMORY_WRITE_OPERATION, "sanitized tags are missing")
        })?,
        "sanitized tags",
    )?;
    let entities = value_strings(
        payload.get("entities").ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "sanitized entities are missing",
            )
        })?,
        "sanitized entities",
    )?;
    let metadata = payload.get("metadata").cloned().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "sanitized metadata is missing",
        )
    })?;
    let source_label = match payload.get("source_label") {
        Some(Value::String(value)) => Some(value.clone()),
        None => None,
        _ => {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "sanitized source label is malformed",
            ));
        }
    };
    let retention = RetentionClass::new(PROJECT_MEMORY_RETENTION_CLASS.to_owned())?;
    let fact_payload = FactPayloadV1::new(
        content,
        category,
        tags,
        entities,
        metadata,
        source_label,
        receipt,
        retention,
    )?;
    let access = match fact_payload.receipt().disposition() {
        SanitizerDispositionV1::Accepted => PayloadAccessState::Eligible,
        SanitizerDispositionV1::Redacted => PayloadAccessState::Redacted,
        SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "durable payload has a non-durable receipt disposition",
            ));
        }
    };
    Ok(SanitizedPayload {
        payload: fact_payload,
        access,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::store::memory) fn initial_batch(
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    payload: FactPayloadV1,
    access: PayloadAccessState,
    trust: Confidence,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: operation_id.clone(),
        },
    )?;
    let fact_id = FactId::derive(&identity)?;
    let asserted_at = project_memory_event_time(now, 0)?;
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload,
        Vec::new(),
        asserted_at,
        actor.clone(),
    )?;
    let mut events = vec![FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        asserted_at,
        actor.clone(),
    )?];
    let mut next_offset = 1;
    if access != PayloadAccessState::Eligible {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: access,
            },
            project_memory_event_time(now, next_offset)?,
            actor.clone(),
        )?);
        next_offset += 1;
    }
    let default_trust = Confidence::new(DEFAULT_TRUST)?;
    if trust != default_trust {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: default_trust,
                current: trust,
                evidence_ids: Vec::new(),
            },
            project_memory_event_time(now, next_offset)?,
            actor,
        )?);
    }
    FactWriteBatch::new(
        fact_id,
        owner.clone(),
        Some(assertion),
        events,
        Vec::new(),
        Vec::new(),
        None,
    )?
    .with_identity_material(identity)
}

pub(in crate::store::memory) async fn commit_batch_tx(
    transaction: &Transaction<'_>,
    batch: &FactWriteBatch,
) -> FactStoreResult<(FactCommitReceipt, bool)> {
    let attempt = commit_fact_tx(transaction, batch).await?;
    match attempt.outcome {
        FactCommitOutcome::Committed(receipt) => Ok((receipt, false)),
        FactCommitOutcome::IdempotentReplay(receipt) => Ok((receipt, true)),
        FactCommitOutcome::Conflict(conflict) => Err(FactStoreError::CommitConflict { conflict }),
        _ => Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "canonical fact write returned an unsupported outcome",
        )),
    }
}

pub(super) async fn active_fact_count_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
               AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL
               AND current_facts.payload_access != 'deleted'",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .ok_or_else(|| storage_message(PROJECT_MEMORY_WRITE_OPERATION, "fact count is missing"))?;
    nonnegative_u64(
        row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?,
        "active fact count",
    )
}

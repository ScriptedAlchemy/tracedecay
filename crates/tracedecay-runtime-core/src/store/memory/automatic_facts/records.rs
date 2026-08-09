//! Automatic fact-receipt parsing, projection, and bounded read queries.

use super::super::crud::compatibility_payload_metadata;
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, from_json, row_i64, row_optional_string, row_string,
    storage_error, storage_message, to_json,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionId, FactCategoryV1, FactEventId, FactId, FactOwnerV1,
    ProvenanceId, SanitizationReceiptV1, UtcMicros,
};
use tracedecay_store::{
    FactStoreError, FactStoreResult, ProjectMemoryAutomaticFactEffectV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptPageV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactIdV1, ProjectMemoryFactMappingV1,
    ProjectMemoryResult,
};

pub(super) const PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPT_PAGE_LIMIT: usize = 100;

fn automatic_fact_required_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactStoreResult<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                format!("automatic fact request {field} is missing or malformed"),
            )
        })
}

fn automatic_fact_optional_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactStoreResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            format!("automatic fact request {field} is malformed"),
        )),
    }
}

pub(super) fn project_memory_automatic_fact_request_value(
    request: &ProjectMemoryFactAddCommandV1,
) -> Value {
    json!({
        "owner": request.owner(),
        "operation_id": request.operation_id().as_str(),
        "content": request.content(),
        "category": super::super::primitives::project_memory_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": compatibility_payload_metadata(request.metadata()),
        "sanitization_receipt": request.sanitization_receipt(),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    })
}

fn automatic_fact_request_from_value(
    owner: &FactOwnerV1,
    value: Value,
) -> FactStoreResult<ProjectMemoryFactAddCommandV1> {
    let object = value.as_object().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "automatic fact request is not an object",
        )
    })?;
    let stored_owner = from_json::<FactOwnerV1>(
        &to_json(
            object.get("owner").ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_READ_OPERATION,
                    "automatic fact request owner is missing",
                )
            })?,
            "serialize automatic fact request owner",
        )?,
        PROJECT_MEMORY_READ_OPERATION,
    )?;
    if &stored_owner != owner {
        return Err(FactStoreError::OwnerMismatch);
    }
    let operation_id = ProvenanceId::new(automatic_fact_required_string(object, "operation_id")?)
        .map_err(FactStoreError::from)?;
    let content = automatic_fact_required_string(object, "content")?;
    let category = match automatic_fact_required_string(object, "category")?.as_str() {
        "general" => FactCategoryV1::General,
        "user_pref" => FactCategoryV1::UserPref,
        "project" => FactCategoryV1::Project,
        "tool" => FactCategoryV1::Tool,
        "decision" => FactCategoryV1::Decision,
        "code_area" => FactCategoryV1::CodeArea,
        _ => {
            return Err(storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "automatic fact request category is malformed",
            ));
        }
    };
    let strings = |field| {
        object
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_READ_OPERATION,
                    format!("automatic fact request {field} is missing"),
                )
            })?
            .iter()
            .map(|value| {
                value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                    storage_message(
                        PROJECT_MEMORY_READ_OPERATION,
                        format!("automatic fact request {field} is malformed"),
                    )
                })
            })
            .collect::<FactStoreResult<Vec<_>>>()
    };
    let metadata =
        compatibility_payload_metadata(&object.get("metadata").cloned().ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_READ_OPERATION,
                "automatic fact request metadata is missing",
            )
        })?);
    let sanitization_receipt = from_json::<SanitizationReceiptV1>(
        &to_json(
            object.get("sanitization_receipt").ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_READ_OPERATION,
                    "automatic fact request sanitization receipt is missing",
                )
            })?,
            "serialize automatic fact request sanitization receipt",
        )?,
        PROJECT_MEMORY_READ_OPERATION,
    )?;
    let trust = Confidence::new(
        object
            .get("default_trust")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_READ_OPERATION,
                    "automatic fact request default trust is missing",
                )
            })?,
    )
    .map_err(FactStoreError::from)?;
    let actor = automatic_fact_optional_string(object, "actor")?
        .map(ActorId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let request = ProjectMemoryFactAddCommandV1::new(
        owner.clone(),
        operation_id,
        content,
        category,
        automatic_fact_optional_string(object, "source")?,
        strings("tags")?,
        strings("entities")?,
        metadata,
        sanitization_receipt,
        trust,
        actor,
    )?;
    match automatic_fact_optional_string(object, "automation_run_id")? {
        Some(run_id) => request.with_automation_run_id(run_id),
        None => Ok(request),
    }
}

pub(in crate::store::memory) fn project_memory_automatic_fact_state_label(
    state: ProjectMemoryAutomaticFactStateV1,
) -> &'static str {
    match state {
        ProjectMemoryAutomaticFactStateV1::Applied => "applied",
        ProjectMemoryAutomaticFactStateV1::Quarantined => "quarantined",
    }
}

fn automatic_fact_state(value: &str) -> FactStoreResult<ProjectMemoryAutomaticFactStateV1> {
    match value {
        "applied" => Ok(ProjectMemoryAutomaticFactStateV1::Applied),
        "quarantined" => Ok(ProjectMemoryAutomaticFactStateV1::Quarantined),
        _ => Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "automatic fact receipt state is malformed",
        )),
    }
}

pub(in crate::store::memory) async fn project_memory_automatic_fact_receipt_record_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    apply_id: &ProvenanceId,
) -> ProjectMemoryResult<Option<ProjectMemoryAutomaticFactReceiptV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT apply_id, owner_json, request_json, evidence_json, state,
                    quarantine_reason, applied_fact_id, applied_assertion_id, applied_event_id,
                    recorded_at
             FROM memory_v2_automatic_fact_receipts
             WHERE apply_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![apply_id.as_str(), key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    else {
        return Ok(None);
    };
    let stored_id = ProvenanceId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
        .map_err(FactStoreError::from)?;
    if &stored_id != apply_id {
        return Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "automatic fact receipt identity mismatch",
        )
        .into());
    }
    if row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch.into());
    }
    let request = automatic_fact_request_from_value(
        owner,
        from_json::<Value>(
            &row_string(&row, 2, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?,
    )?;
    let evidence = from_json::<ProjectMemoryAutomaticFactEvidenceV1>(
        &row_string(&row, 3, PROJECT_MEMORY_READ_OPERATION)?,
        PROJECT_MEMORY_READ_OPERATION,
    )?;
    let state = automatic_fact_state(&row_string(&row, 4, PROJECT_MEMORY_READ_OPERATION)?)?;
    let effect = match state {
        ProjectMemoryAutomaticFactStateV1::Applied => {
            let fact_id = row_optional_string(&row, 6, PROJECT_MEMORY_READ_OPERATION)?
                .ok_or_else(|| {
                    storage_message(
                        PROJECT_MEMORY_READ_OPERATION,
                        "applied receipt is missing its fact",
                    )
                })
                .and_then(|value| FactId::new(value).map_err(FactStoreError::from))?;
            let assertion_id = row_optional_string(&row, 7, PROJECT_MEMORY_READ_OPERATION)?
                .ok_or_else(|| {
                    storage_message(
                        PROJECT_MEMORY_READ_OPERATION,
                        "applied receipt is missing its assertion",
                    )
                })
                .and_then(|value| FactAssertionId::new(value).map_err(FactStoreError::from))?;
            let event_id = row_optional_string(&row, 8, PROJECT_MEMORY_READ_OPERATION)?
                .ok_or_else(|| {
                    storage_message(
                        PROJECT_MEMORY_READ_OPERATION,
                        "applied receipt is missing its event",
                    )
                })
                .and_then(|value| FactEventId::new(value).map_err(FactStoreError::from))?;
            let mapping = ProjectMemoryFactMappingV1::new(
                ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone())?,
                None,
            )?;
            ProjectMemoryAutomaticFactEffectV1::Applied {
                fact_id,
                mapping,
                assertion_id,
                event_id,
            }
        }
        ProjectMemoryAutomaticFactStateV1::Quarantined => {
            let reason =
                row_optional_string(&row, 5, PROJECT_MEMORY_READ_OPERATION)?.ok_or_else(|| {
                    storage_message(
                        PROJECT_MEMORY_READ_OPERATION,
                        "quarantined receipt is missing its reason",
                    )
                })?;
            ProjectMemoryAutomaticFactEffectV1::Quarantined { reason }
        }
    };
    ProjectMemoryAutomaticFactReceiptV1::new(
        stored_id,
        owner.clone(),
        state,
        request,
        evidence,
        effect,
        UtcMicros(row_i64(&row, 9, PROJECT_MEMORY_READ_OPERATION)?),
    )
    .map(Some)
    .map_err(Into::into)
}

pub(in crate::store::memory) async fn get_project_memory_automatic_fact_receipt_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    apply_id: &ProvenanceId,
) -> ProjectMemoryResult<Option<ProjectMemoryAutomaticFactReceiptV1>> {
    project_memory_automatic_fact_receipt_record_tx(transaction, owner, apply_id).await
}

pub(in crate::store::memory) async fn list_project_memory_automatic_fact_receipts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    state: Option<ProjectMemoryAutomaticFactStateV1>,
    after_apply_id: Option<&ProvenanceId>,
    limit: usize,
) -> ProjectMemoryResult<ProjectMemoryAutomaticFactReceiptPageV1> {
    if limit == 0 || limit > PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPT_PAGE_LIMIT {
        return Err(FactStoreError::InvalidQueryLimit {
            limit,
            max: PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPT_PAGE_LIMIT,
        }
        .into());
    }
    let key = OwnerKey::new(owner)?;
    let fetch_limit =
        i64::try_from(limit.saturating_add(1)).map_err(|_| FactStoreError::InvalidQueryLimit {
            limit,
            max: PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPT_PAGE_LIMIT,
        })?;
    let state = state.map(project_memory_automatic_fact_state_label);
    let mut rows = match (state, after_apply_id) {
        (Some(state), Some(after)) => {
            transaction
                .query(
                    "SELECT apply_id FROM memory_v2_automatic_fact_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
               AND state = ?4 AND apply_id > ?5
             ORDER BY apply_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        state,
                        after.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
        (Some(state), None) => {
            transaction
                .query(
                    "SELECT apply_id FROM memory_v2_automatic_fact_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3 AND state = ?4
             ORDER BY apply_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        state,
                        fetch_limit
                    ],
                )
                .await
        }
        (None, Some(after)) => {
            transaction
                .query(
                    "SELECT apply_id FROM memory_v2_automatic_fact_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3 AND apply_id > ?4
             ORDER BY apply_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
        (None, None) => {
            transaction
                .query(
                    "SELECT apply_id FROM memory_v2_automatic_fact_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
             ORDER BY apply_id ASC LIMIT ?4",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut ids = Vec::with_capacity(limit.saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ids.push(
            ProvenanceId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    let mut receipts = Vec::with_capacity(ids.len());
    for apply_id in &ids {
        receipts.push(
            project_memory_automatic_fact_receipt_record_tx(transaction, owner, apply_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        PROJECT_MEMORY_READ_OPERATION,
                        "automatic fact receipt disappeared from its snapshot",
                    )
                })?,
        );
    }
    ProjectMemoryAutomaticFactReceiptPageV1::new(
        owner.clone(),
        receipts,
        has_more.then(|| ids.last().cloned()).flatten(),
    )
    .map_err(Into::into)
}

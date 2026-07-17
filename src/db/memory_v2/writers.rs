use libsql::{Connection, params};
use tracedecay_domain::{
    Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactEventId, FactId,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, LegacyFactMappingV1,
    PayloadAccessState, SourceStoreId, UtcMicros,
};

use crate::errors::Result;
use crate::tracedecay::current_timestamp;

use super::types::{OwnerKey, StoredAssertionHeaderV1};
use super::{
    OPERATION, V23_COMPATIBILITY_BANK_VECTOR_BYTES, V23_COMPATIBILITY_BANK_VECTOR_HEADER,
    canonical_replay, current_fact_state, db_error, db_message, json_text, optional_i64,
    optional_string, owner_key, payload_access_label, row_exists, scalar_i64_params,
    validate_scope, validate_v1_compatibility_source,
};
#[cfg(test)]
use super::{begin, finish_transaction};

pub(super) async fn insert_fact_identity(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    identity_json: &str,
    created_at: i64,
) -> Result<()> {
    if let Some(existing) = optional_string(
        conn,
        "SELECT identity_json FROM memory_v2_facts WHERE fact_id = ?1",
        params![fact_id.as_str()],
    )
    .await?
    {
        return canonical_replay(existing, identity_json, "fact identity");
    }
    conn.execute(
        "INSERT INTO memory_v2_facts(
            fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            identity_json,
            created_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

pub(super) async fn insert_mapping(
    conn: &Connection,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> Result<()> {
    let mapping_json = json_text(mapping)?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT mapping_json FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2
           AND source_store_id = ?3 AND legacy_fact_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            mapping.source_store_id().as_str(),
            mapping.legacy_fact_id()
        ],
    )
    .await?
    {
        return canonical_replay(existing, &mapping_json, "legacy mapping");
    }
    conn.execute(
        "INSERT INTO memory_v2_legacy_map(
            owner_kind, project_id, owner_json, source_store_id,
            legacy_fact_id, fact_id, mapping_json
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            mapping.source_store_id().as_str(),
            mapping.legacy_fact_id(),
            mapping.fact_id().as_str(),
            mapping_json
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_legacy_feedback_event_mapping(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    legacy_feedback_event_id: i64,
    fact_id: &FactId,
    event_id: &FactEventId,
) -> Result<()> {
    validate_v1_compatibility_source(source_store_id)?;
    let mut rows = conn
        .query(
            "SELECT fact_id, event_id FROM memory_v2_legacy_feedback_event_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_feedback_event_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str(),
                legacy_feedback_event_id
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let existing_fact_id = row
            .get::<String>(0)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_event_id = row
            .get::<String>(1)
            .map_err(|error| db_error(OPERATION, error))?;
        if existing_fact_id == fact_id.as_str() && existing_event_id == event_id.as_str() {
            return Ok(());
        }
        return Err(db_message(
            OPERATION,
            "legacy feedback event mapping identity collision",
        ));
    }
    drop(rows);
    if let Some(existing_legacy_id) = optional_i64(
        conn,
        "SELECT legacy_feedback_event_id FROM memory_v2_legacy_feedback_event_map
         WHERE owner_kind = ?1 AND project_id = ?2
           AND source_store_id = ?3 AND event_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            event_id.as_str()
        ],
    )
    .await?
    {
        if existing_legacy_id == legacy_feedback_event_id {
            return Ok(());
        }
        return Err(db_message(
            OPERATION,
            "canonical feedback event maps to a different legacy event",
        ));
    }
    conn.execute(
        "INSERT INTO memory_v2_legacy_feedback_event_map(
            owner_kind, project_id, source_store_id, legacy_feedback_event_id, fact_id, event_id
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            legacy_feedback_event_id,
            fact_id.as_str(),
            event_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

/// Conflicting legacy numeric rows must not turn a resumable V22 repair or
/// V1 backfill into a permanent error. The first canonical mapping wins;
/// divergent replays are quarantined while the caller advances its cursor.
#[allow(clippy::too_many_arguments)]
pub(super) async fn legacy_feedback_mapping_can_be_recorded(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    legacy_feedback_event_id: i64,
    fact_id: &FactId,
    event_id: &FactEventId,
    recorded_at: i64,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT fact_id, event_id FROM memory_v2_legacy_feedback_event_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_feedback_event_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str(),
                legacy_feedback_event_id
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let existing_fact_id = row
            .get::<String>(0)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_event_id = row
            .get::<String>(1)
            .map_err(|error| db_error(OPERATION, error))?;
        if existing_fact_id != fact_id.as_str() || existing_event_id != event_id.as_str() {
            insert_quarantine(
                conn,
                owner,
                source_store_id,
                "memory_feedback_events",
                legacy_feedback_event_id,
                "feedback_mapping_collision",
                recorded_at,
            )
            .await?;
            return Ok(false);
        }
    }
    drop(rows);
    if let Some(existing_legacy_event_id) = optional_i64(
        conn,
        "SELECT legacy_feedback_event_id FROM memory_v2_legacy_feedback_event_map
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND event_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            event_id.as_str()
        ],
    )
    .await?
        && existing_legacy_event_id != legacy_feedback_event_id
    {
        insert_quarantine(
            conn,
            owner,
            source_store_id,
            "memory_feedback_events",
            legacy_feedback_event_id,
            "feedback_event_duplicate",
            recorded_at,
        )
        .await?;
        return Ok(false);
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_feedback_history(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    event_id: &FactEventId,
    action: &str,
    old_trust: Confidence,
    new_trust: Confidence,
    occurred_at: UtcMicros,
    source: Option<&str>,
    note: Option<&str>,
    details_availability: &str,
) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT action, old_trust, new_trust, occurred_at, source, note, details_availability
             FROM memory_v2_feedback_history
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3 AND event_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                fact_id.as_str(),
                event_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        let existing_action = row
            .get::<String>(0)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_old_trust = row
            .get::<f64>(1)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_new_trust = row
            .get::<f64>(2)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_occurred_at = row
            .get::<i64>(3)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_source = row
            .get::<Option<String>>(4)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_note = row
            .get::<Option<String>>(5)
            .map_err(|error| db_error(OPERATION, error))?;
        let existing_availability = row
            .get::<String>(6)
            .map_err(|error| db_error(OPERATION, error))?;
        if existing_action != action
            || existing_old_trust != old_trust.as_f64()
            || existing_new_trust != new_trust.as_f64()
            || existing_occurred_at != occurred_at.0
        {
            return Err(db_message(OPERATION, "feedback history identity collision"));
        }
        if existing_source.as_deref() == source
            && existing_note.as_deref() == note
            && existing_availability == details_availability
        {
            return Ok(());
        }
        if existing_source.is_none()
            && existing_note.is_none()
            && matches!(
                existing_availability.as_str(),
                "legacy_redacted" | "unknown"
            )
        {
            return Ok(());
        }
        return Err(db_message(OPERATION, "feedback history detail collision"));
    }
    drop(rows);
    conn.execute(
        "INSERT INTO memory_v2_feedback_history(
            owner_kind, project_id, fact_id, event_id, action, old_trust, new_trust,
            occurred_at, source, note, details_availability
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            fact_id.as_str(),
            event_id.as_str(),
            action,
            old_trust.as_f64(),
            new_trust.as_f64(),
            occurred_at.0,
            source,
            note,
            details_availability
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

pub(super) async fn insert_event(
    conn: &Connection,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
    recorded_at: i64,
) -> Result<()> {
    let event_json = json_text(event)?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT event_json FROM memory_v2_lineage_events WHERE event_id = ?1",
        params![event.event_id().as_str()],
    )
    .await?
    {
        return canonical_replay(existing, &event_json, "lineage event");
    }
    conn.execute(
        "INSERT INTO memory_v2_lineage_events(
            event_id, fact_id, owner_kind, project_id, event_json, occurred_at, recorded_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            event.event_id().as_str(),
            event.fact_id().as_str(),
            owner.kind,
            owner.project_id.as_str(),
            event_json,
            event.occurred_at().0,
            recorded_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

pub(super) async fn insert_assertion(
    conn: &Connection,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> Result<()> {
    let payload_reference = assertion
        .payload()
        .payload_reference()
        .map_err(|_| db_message(OPERATION, "typed payload reference construction failed"))?;
    let header = StoredAssertionHeaderV1 {
        assertion_id: assertion.assertion_id(),
        fact_id: assertion.fact_id(),
        owner: assertion.owner(),
        kind: assertion.kind(),
        payload_reference: &payload_reference,
        evidence: assertion.evidence(),
        asserted_at: assertion.asserted_at(),
        actor_id: assertion.actor_id(),
    };
    let header_json = json_text(&header)?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT assertion_header_json FROM memory_v2_assertions WHERE assertion_id = ?1",
        params![assertion.assertion_id().as_str()],
    )
    .await?
    {
        canonical_replay(existing, &header_json, "assertion")?;
    } else {
        conn.execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                header_json,
                json_text(assertion.kind())?,
                json_text(&payload_reference)?,
                json_text(assertion.payload().receipt())?,
                assertion.asserted_at().0,
                assertion.actor_id().map(|actor| actor.as_str())
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    insert_assertion_supersession(conn, owner, assertion).await?;
    insert_assertion_evidence(conn, owner, assertion).await?;
    let payload_json = json_text(assertion.payload())?;
    if let Some(existing) = optional_string(
        conn,
        "SELECT payload_json FROM memory_v2_assertion_payloads WHERE assertion_id = ?1",
        params![assertion.assertion_id().as_str()],
    )
    .await?
    {
        canonical_replay(existing, &payload_json, "assertion payload")?;
    } else {
        conn.execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                payload_json,
                assertion.payload().content()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

async fn insert_assertion_supersession(
    conn: &Connection,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> Result<()> {
    let superseded: Vec<&FactAssertionId> = match assertion.kind() {
        FactAssertionKindV1::Correction { supersedes } => vec![supersedes],
        FactAssertionKindV1::Merge { supersedes } => supersedes.iter().collect(),
        FactAssertionKindV1::Initial | FactAssertionKindV1::LegacyImport => Vec::new(),
    };
    for (ordinal, superseded_id) in superseded.iter().enumerate() {
        let existing = optional_string(
            conn,
            "SELECT superseded_assertion_id
             FROM memory_v2_assertion_supersession
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 AND ordinal = ?5",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                ordinal as i64
            ],
        )
        .await?;
        if let Some(existing) = existing {
            canonical_replay(existing, superseded_id.as_str(), "assertion supersession")?;
        } else {
            conn.execute(
                "INSERT INTO memory_v2_assertion_supersession(
                    assertion_id, fact_id, owner_kind, project_id,
                    superseded_assertion_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    superseded_id.as_str(),
                    ordinal as i64
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        }
    }
    let child_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*) FROM memory_v2_assertion_supersession
         WHERE assertion_id = ?1 AND fact_id = ?2
           AND owner_kind = ?3 AND project_id = ?4",
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await?;
    if child_count != superseded.len() as i64 {
        return Err(db_message(
            OPERATION,
            "assertion supersession child collision",
        ));
    }
    Ok(())
}

async fn insert_assertion_evidence(
    conn: &Connection,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> Result<()> {
    for (ordinal, evidence) in assertion.evidence().iter().enumerate() {
        let evidence_json = json_text(evidence)?;
        if let Some(existing) = optional_string(
            conn,
            "SELECT evidence_json FROM memory_v2_evidence WHERE evidence_id = ?1",
            params![evidence.evidence_id().as_str()],
        )
        .await?
        {
            canonical_replay(existing, &evidence_json, "fact evidence")?;
        } else {
            conn.execute(
                "INSERT INTO memory_v2_evidence(
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    evidence.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    owner.json.as_str(),
                    evidence.anchor_id().as_str(),
                    evidence_json
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        }
        let existing = optional_string(
            conn,
            "SELECT evidence_id FROM memory_v2_assertion_evidence
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 AND ordinal = ?5",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                ordinal as i64
            ],
        )
        .await?;
        if let Some(existing) = existing {
            canonical_replay(
                existing,
                evidence.evidence_id().as_str(),
                "assertion evidence",
            )?;
        } else {
            conn.execute(
                "INSERT INTO memory_v2_assertion_evidence(
                    assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    ordinal as i64
                ],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
        }
    }
    let child_count = scalar_i64_params(
        conn,
        "SELECT COUNT(*) FROM memory_v2_assertion_evidence
         WHERE assertion_id = ?1 AND fact_id = ?2
           AND owner_kind = ?3 AND project_id = ?4",
        params![
            assertion.assertion_id().as_str(),
            assertion.fact_id().as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await?;
    if child_count != assertion.evidence().len() as i64 {
        return Err(db_message(OPERATION, "assertion evidence child collision"));
    }
    Ok(())
}

pub(super) async fn ensure_current(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    event_id: &FactEventId,
    updated_at: i64,
) -> Result<()> {
    if row_exists(
        conn,
        "SELECT 1 FROM memory_v2_current_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await?
    {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO memory_v2_current_facts(
            fact_id, owner_kind, project_id, payload_access, trust_score,
            active_assertion_id, last_event_id, updated_at
         ) VALUES(?1, ?2, ?3, 'unavailable', NULL, NULL, ?4, ?5)",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            event_id.as_str(),
            updated_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn update_current(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_access: Option<(&FactAssertionId, PayloadAccessState)>,
    trust: Option<f64>,
    event_id: &FactEventId,
    updated_at: i64,
) -> Result<()> {
    let (assertion_id, access) = assertion_access.map_or((None, None), |(id, access)| {
        (Some(id.as_str()), Some(payload_access_label(access)))
    });
    conn.execute(
        "UPDATE memory_v2_current_facts SET
            payload_access = COALESCE(?1, payload_access),
            trust_score = COALESCE(?2, trust_score),
            active_assertion_id = COALESCE(?3, active_assertion_id),
            last_event_id = ?4,
            updated_at = MAX(updated_at, ?5)
         WHERE fact_id = ?6 AND owner_kind = ?7 AND project_id = ?8",
        params![
            access,
            trust,
            assertion_id,
            event_id.as_str(),
            updated_at,
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

/// Marks one owner-bound V23 compatibility-bank projection dirty inside the
/// caller's authoritative writer transaction.
pub(crate) async fn mark_memory_v2_compatibility_bank_dirty_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    conn.execute(
        "INSERT INTO memory_v2_compatibility_bank_dirty(
            owner_kind, project_id, source_store_id, owner_json, bank_name, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(owner_kind, project_id, source_store_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            updated_at = max(
                excluded.updated_at,
                memory_v2_compatibility_bank_dirty.updated_at + 1
            )",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name,
            updated_at.0
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Replaces one owner-bound V23 compatibility-bank projection inside the
/// caller's authoritative writer transaction. The strict binary shape is the
/// canonical f32-2048 FHRR encoding, never a legacy global-bank payload.
pub(crate) async fn upsert_memory_v2_compatibility_bank_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    vector: &[u8],
    fact_count: u64,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    if vector.len() != V23_COMPATIBILITY_BANK_VECTOR_BYTES
        || vector[..8] != V23_COMPATIBILITY_BANK_VECTOR_HEADER
    {
        return Err(db_message(
            OPERATION,
            "compatibility bank vector is not canonical f32-2048 FHRR data",
        ));
    }
    let fact_count = i64::try_from(fact_count)
        .map_err(|_| db_message(OPERATION, "compatibility bank fact count is out of range"))?;
    if fact_count == 0 {
        return Err(db_message(
            OPERATION,
            "compatibility bank fact count must be positive",
        ));
    }
    conn.execute(
        "INSERT INTO memory_v2_compatibility_banks(
            owner_kind, project_id, source_store_id, owner_json, bank_name,
            vector, hrr_algebra, hrr_dim, fact_count, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'amari_fhrr', 2048, ?7, ?8)
         ON CONFLICT(owner_kind, project_id, source_store_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            vector = excluded.vector,
            hrr_algebra = excluded.hrr_algebra,
            hrr_dim = excluded.hrr_dim,
            fact_count = excluded.fact_count,
            updated_at = excluded.updated_at
         WHERE excluded.updated_at >= memory_v2_compatibility_banks.updated_at",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name,
            vector,
            fact_count,
            updated_at.0
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Deletes an empty owner-bound V23 compatibility-bank projection inside the
/// caller's authoritative writer transaction.
pub(crate) async fn delete_memory_v2_compatibility_bank_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    conn.execute(
        "DELETE FROM memory_v2_compatibility_banks
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND owner_json = ?4 AND bank_name = ?5",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Clears a V23 dirty projection only when the caller rebuilt the exact owner
/// generation it observed. A concurrent mark therefore remains pending.
pub(crate) async fn clear_memory_v2_compatibility_bank_dirty_in_transaction(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    expected_updated_at: UtcMicros,
) -> Result<bool> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    let changed = conn
        .execute(
            "DELETE FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
               AND owner_json = ?4 AND bank_name = ?5 AND updated_at = ?6",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str(),
                owner.json.as_str(),
                bank_name,
                expected_updated_at.0
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(changed == 1)
}

fn compatibility_bank_owner_key(
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
) -> Result<OwnerKey> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    if !matches!(
        bank_name,
        "all" | "general" | "user_pref" | "project" | "tool" | "decision" | "code_area"
    ) {
        return Err(db_message(
            OPERATION,
            "compatibility bank name is unsupported",
        ));
    }
    owner_key(owner)
}

/// Purges payload, FTS, and vector material for one exact owner/store/fact.
/// Immutable identity, assertion headers, mapping, and typed lineage remain.
///
/// Standalone transaction wrapper retained for owner-bound purge tests; the
/// production purge path drives `purge_memory_v2_fact_inner` inside a
/// caller-owned authority transaction.
#[cfg(test)]
pub(super) async fn purge_memory_v2_fact(
    conn: &Connection,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: &FactEventId,
    occurred_at: UtcMicros,
) -> Result<bool> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    fact_id
        .validate()
        .map_err(|_| db_message("memory_v2_purge", "fact identity is invalid"))?;
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let owner_key = owner_key(owner)?;
    begin(conn, "memory_v2_purge").await?;
    let result = purge_memory_v2_fact_inner(
        conn,
        owner,
        &owner_key,
        source_store_id,
        fact_id,
        Some(expected_last_event_id),
        occurred_at,
    )
    .await;
    let purged = finish_transaction(conn, result, "memory_v2_purge").await?;
    if purged {
        conn.execute_batch("PRAGMA incremental_vacuum(64)")
            .await
            .map_err(|error| db_error("memory_v2_purge", error))?;
    }
    Ok(purged)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn quarantine_fact(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    legacy_fact_id: i64,
    reason: &'static str,
    recorded_at: i64,
) -> Result<()> {
    insert_quarantine(
        conn,
        owner_key,
        source_store_id,
        "memory_facts",
        legacy_fact_id,
        reason,
        recorded_at,
    )
    .await?;
    purge_payload_rows(conn, owner_key, fact_id).await?;
    let previous = current_fact_state(conn, owner_key, fact_id).await?.access;
    let event_id =
        if previous != PayloadAccessState::Deleted && previous != PayloadAccessState::Quarantined {
            let event = FactLineageEventV1::new(
                fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::PayloadAccessChanged {
                    previous,
                    current: PayloadAccessState::Quarantined,
                },
                UtcMicros(recorded_at),
                None,
            )
            .map_err(|_| db_message(OPERATION, "typed quarantine event construction failed"))?;
            insert_event(conn, owner_key, &event, recorded_at).await?;
            Some(event.event_id().clone())
        } else {
            None
        };
    purge_legacy_fact(conn, legacy_fact_id).await?;
    if let Some(event_id) = event_id {
        conn.execute(
            "UPDATE memory_v2_current_facts SET
                payload_access = 'quarantined', active_assertion_id = NULL,
                last_event_id = ?1, updated_at = MAX(updated_at, ?2)
             WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
            params![
                event_id.as_str(),
                recorded_at,
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

pub(super) async fn purge_memory_v2_fact_inner(
    conn: &Connection,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    expected_last_event_id: Option<&FactEventId>,
    occurred_at: UtcMicros,
) -> Result<bool> {
    let legacy_fact_id = optional_i64(
        conn,
        "SELECT legacy_fact_id FROM memory_v2_legacy_map
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
           AND source_store_id = ?4",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str(),
            source_store_id.as_str()
        ],
    )
    .await?;
    let fact_exists = row_exists(
        conn,
        "SELECT 1 FROM memory_v2_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await?;
    if !fact_exists {
        return Ok(false);
    }
    if legacy_fact_id.is_none()
        && row_exists(
            conn,
            "SELECT 1 FROM memory_v2_legacy_map
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![
                fact_id.as_str(),
                owner_key.kind,
                owner_key.project_id.as_str()
            ],
        )
        .await?
    {
        return Ok(false);
    }
    let current = current_fact_state(conn, owner_key, fact_id).await?;
    if expected_last_event_id.is_some_and(|expected| expected != &current.last_event_id) {
        return Err(db_message(
            "memory_v2_purge",
            "fact lineage changed before payload purge",
        ));
    }
    if current.access == PayloadAccessState::Deleted {
        return Ok(false);
    }
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: current.access,
            current: PayloadAccessState::Deleted,
        },
        occurred_at,
        None,
    )
    .map_err(|_| {
        db_message(
            "memory_v2_purge",
            "typed deletion event construction failed",
        )
    })?;
    insert_event(conn, owner_key, &event, occurred_at.0).await?;
    purge_payload_rows(conn, owner_key, fact_id).await?;
    if let Some(legacy_fact_id) = legacy_fact_id {
        purge_legacy_fact(conn, legacy_fact_id).await?;
    }
    conn.execute(
        "UPDATE memory_v2_current_facts SET
            payload_access = 'deleted', active_assertion_id = NULL,
            last_event_id = ?1, updated_at = MAX(updated_at, ?2)
         WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
        params![
            event.event_id().as_str(),
            occurred_at.0,
            fact_id.as_str(),
            owner_key.kind,
            owner_key.project_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    Ok(true)
}

pub(super) async fn purge_payload_rows(
    conn: &Connection,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> Result<()> {
    // Backfill quarantine reaches this helper without passing through the
    // public purge entrypoint, so set the deletion policy at every destructive
    // payload path.
    conn.execute_batch("PRAGMA secure_delete = ON")
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "UPDATE memory_v2_feedback_history
         SET source = NULL, note = NULL,
             details_availability = CASE
                 WHEN details_availability = 'available' THEN 'legacy_redacted'
                 ELSE details_availability
             END
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "DELETE FROM memory_v2_assertion_vectors
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute(
        "DELETE FROM memory_v2_assertion_payloads
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
        params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    Ok(())
}

async fn purge_legacy_fact(conn: &Connection, legacy_fact_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO memory_bank_dirty(bank_name, updated_at)
         SELECT bank_name, ?1 FROM memory_banks
         WHERE 1
         ON CONFLICT(bank_name) DO UPDATE SET updated_at = excluded.updated_at",
        params![current_timestamp()],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    conn.execute("DELETE FROM memory_banks", ())
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let mut rows = conn
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    let mut entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?
    {
        entity_ids.push(
            row.get::<i64>(0)
                .map_err(|error| db_error("memory_v2_purge", error))?,
        );
    }
    conn.execute(
        "DELETE FROM memory_facts WHERE fact_id = ?1",
        params![legacy_fact_id],
    )
    .await
    .map_err(|error| db_error("memory_v2_purge", error))?;
    for entity_id in entity_ids {
        conn.execute(
            "DELETE FROM memory_entities
             WHERE entity_id = ?1
               AND NOT EXISTS(
                   SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
               )",
            params![entity_id],
        )
        .await
        .map_err(|error| db_error("memory_v2_purge", error))?;
    }
    Ok(())
}

pub(super) async fn insert_quarantine(
    conn: &Connection,
    owner: &OwnerKey,
    source_store_id: &SourceStoreId,
    source_table: &'static str,
    source_row_id: i64,
    reason_code: &'static str,
    recorded_at: i64,
) -> Result<()> {
    if let Some(existing) = optional_string(
        conn,
        "SELECT reason_code FROM memory_v2_legacy_quarantine
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND source_table = ?4 AND source_row_id = ?5",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            source_table,
            source_row_id
        ],
    )
    .await?
    {
        return canonical_replay(existing, reason_code, "legacy quarantine record");
    }
    conn.execute(
        "INSERT INTO memory_v2_legacy_quarantine(
            owner_kind, project_id, source_store_id, source_table,
            source_row_id, reason_code, recorded_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            source_table,
            source_row_id,
            reason_code,
            recorded_at
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

//! Fact identity, mapping, feedback, event, assertion, and current-state
//! writers.

use tracedecay_domain::{
    FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactEventId, FactEvidenceRelationV1,
    FactId, FactLineageEventV1, LegacyFactMappingV1, PayloadAccessState,
};

use crate::db::engine::params;
use crate::db::{AnchorDerivativeKindV1, RetrievalAnchorDerivativeV1, publish_anchor_derivative};
use crate::errors::Result;

use super::super::types::{OwnerKey, StoredAssertionHeaderV1};
use super::super::{
    MemoryV2Executor, OPERATION, canonical_mapping_replay, canonical_replay, db_error, db_message,
    json_text, optional_string, payload_access_label, row_exists, scalar_i64_params,
};

pub(in crate::db::memory_v2) async fn insert_fact_identity(
    conn: &impl MemoryV2Executor,
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

pub(in crate::db::memory_v2) async fn insert_mapping(
    conn: &impl MemoryV2Executor,
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
        return canonical_mapping_replay(existing, &mapping_json);
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
/// Conflicting legacy numeric rows must not turn a resumable V22 repair or
/// V1 backfill into a permanent error. The first canonical mapping wins;
/// divergent replays are quarantined while the caller advances its cursor.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub(in crate::db::memory_v2) async fn insert_event(
    conn: &impl MemoryV2Executor,
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

pub(in crate::db::memory_v2) async fn insert_assertion(
    conn: &impl MemoryV2Executor,
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
                assertion.actor_id().map(tracedecay_domain::ActorId::as_str)
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
    conn: &impl MemoryV2Executor,
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
    conn: &impl MemoryV2Executor,
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
        let direct_evidence = matches!(
            evidence.relation(),
            FactEvidenceRelationV1::Supports
                | FactEvidenceRelationV1::Contradicts
                | FactEvidenceRelationV1::Corrects
        );
        let derivative = RetrievalAnchorDerivativeV1::new(
            evidence.anchor_id().clone(),
            assertion.owner().clone(),
            AnchorDerivativeKindV1::Contribution,
            evidence.evidence_id().as_str(),
            direct_evidence,
        )?;
        publish_anchor_derivative(conn, &derivative).await?;
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

pub(in crate::db::memory_v2) async fn ensure_current(
    conn: &impl MemoryV2Executor,
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
pub(in crate::db::memory_v2) async fn update_current(
    conn: &impl MemoryV2Executor,
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

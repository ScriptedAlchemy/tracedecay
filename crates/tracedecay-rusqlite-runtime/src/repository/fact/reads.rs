//! The two fact read operations.

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params, params_from_iter};
use tracedecay_domain::{
    Confidence, FactAssertionId, FactEventId, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    PayloadAccessState, UtcMicros,
};
use tracedecay_store::{FactCurrentQuery, FactLineageQuery, StoredFactV1};

use super::super::support::{decode, invalid, usize_to_i64};
use super::OwnerColumns;

pub(super) fn read_current(
    connection: &rusqlite::Connection,
    query: &FactCurrentQuery,
) -> rusqlite::Result<Option<StoredFactV1>> {
    let owner = OwnerColumns::new(query.owner())?;
    let row = connection
        .query_row(
            "SELECT facts.owner_json, current.payload_access, current.trust_score,
                    current.active_assertion_id, current.last_event_id, current.updated_at,
                    payload.payload_json
             FROM memory_v2_current_facts AS current
             JOIN memory_v2_facts AS facts
               USING(fact_id, owner_kind, project_id)
             LEFT JOIN memory_v2_assertion_payloads AS payload
               ON payload.assertion_id = current.active_assertion_id
              AND payload.fact_id = current.fact_id
              AND payload.owner_kind = current.owner_kind
              AND payload.project_id = current.project_id
             WHERE current.fact_id = ?1
               AND current.owner_kind = ?2
               AND current.project_id = ?3",
            params![query.fact_id().as_str(), owner.kind, owner.project_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((owner_json, access, trust, active_assertion, last_event, updated_at, payload)) = row
    else {
        return Ok(None);
    };
    let owner_value: FactOwnerV1 = decode(owner_json)?;
    if &owner_value != query.owner() {
        return Err(invalid("stored fact owner does not match read authority"));
    }
    let access: PayloadAccessState = decode(format!("\"{access}\""))?;
    let payload = if access == PayloadAccessState::Eligible {
        payload.map(decode::<FactPayloadV1>).transpose()?
    } else {
        None
    };
    let Some(active_assertion) = active_assertion else {
        return Ok(None);
    };
    StoredFactV1::new(
        query.fact_id().clone(),
        owner_value,
        payload,
        access,
        Confidence::new(trust.unwrap_or(0.5)).map_err(invalid)?,
        FactAssertionId::new(active_assertion).map_err(invalid)?,
        FactEventId::new(last_event).map_err(invalid)?,
        None,
        UtcMicros(updated_at),
    )
    .map(Some)
    .map_err(invalid)
}

pub(super) fn read_lineage(
    connection: &rusqlite::Connection,
    query: &FactLineageQuery,
) -> rusqlite::Result<Vec<FactLineageEventV1>> {
    let owner = OwnerColumns::new(query.owner())?;
    let limit = usize_to_i64(query.limit(), "fact lineage limit")?;
    let mut bindings = vec![
        Value::Text(query.fact_id().as_str().to_owned()),
        Value::Text(owner.kind.to_owned()),
        Value::Text(owner.project_id),
    ];
    // The keyset cursor is the only optional predicate, so fold it into the
    // one statement rather than carrying two near-identical copies.
    let cursor = match query.after() {
        Some(after) => {
            bindings.push(Value::Integer(after.occurred_at().0));
            bindings.push(Value::Text(after.event_id().as_str().to_owned()));
            "AND (occurred_at > ?4 OR (occurred_at = ?4 AND event_id > ?5))"
        }
        None => "",
    };
    let limit_index = bindings.len() + 1;
    bindings.push(Value::Integer(limit));
    let mut statement = connection.prepare(&format!(
        "SELECT event_json FROM memory_v2_lineage_events
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
           {cursor}
         ORDER BY occurred_at, event_id LIMIT ?{limit_index}"
    ))?;
    let rows = statement.query_map(params_from_iter(bindings), |row| row.get::<_, String>(0))?;
    let mut events: Vec<FactLineageEventV1> = Vec::new();
    for row in rows {
        events.push(decode(row?)?);
    }
    if events
        .iter()
        .any(|event| event.fact_id() != query.fact_id() || event.owner() != query.owner())
    {
        return Err(invalid("stored lineage event identity mismatch"));
    }
    Ok(events)
}

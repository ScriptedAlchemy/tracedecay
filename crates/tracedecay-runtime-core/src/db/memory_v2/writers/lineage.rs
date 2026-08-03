//! Owner-scoped V2 fact-lineage event writer.
//!
//! The identity/mapping/assertion/current-state writers that once lived here
//! belonged to the V1→V2 backfill, whose sole production writer was removed
//! with the V2 fresh-store cutover. Only the lineage-event append survives,
//! because the live legacy-payload purge path records a typed
//! `PayloadAccessChanged` event through it.

use tracedecay_domain::FactLineageEventV1;

use crate::db::engine::params;
use crate::errors::Result;

use super::super::types::OwnerKey;
use super::super::{
    MemoryV2Executor, OPERATION, canonical_replay, db_error, json_text, optional_string,
};

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

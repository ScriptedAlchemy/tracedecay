use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
use std::cell::Cell;

use libsql::{Connection, params};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalObservationIdV1, MessageOccurrenceIdV1, ProjectionOutputOrdinalV1,
};

use crate::errors::Result;

use super::{db_error, db_message, query_i64, quote_identifier};

#[cfg(test)]
thread_local! {
    static FORWARD_MIGRATE_FAULT_AFTER_IMPORT: Cell<bool> = const { Cell::new(false) };
}

/// Test-only fault injection: abort after the first successful temporal import
/// inside forward-migrate so the outer write TX must roll back.
#[cfg(test)]
pub(super) fn set_forward_migrate_fault_after_import(enabled: bool) {
    FORWARD_MIGRATE_FAULT_AFTER_IMPORT.with(|flag| flag.set(enabled));
}

/// Deterministic domain tag for consolidate legacy→temporal source digests.
const LEGACY_SOURCE_DIGEST_DOMAIN: &[u8] = b"tracedecay.session.temporal.legacy-source.v1\0";
const MAX_MIGRATION_BATCH_ITEMS: usize = 1_000;
const MIGRATION_WATERMARKS_JSON: &str = concat!(
    r#"{"active_generation":1,"cursor_key":null,"#,
    r#""projection_frontier":0,"source_frontier":0,"summary_frontier":0}"#
);

macro_rules! row_value {
    ($row:expr, $index:literal, $operation:literal) => {
        $row.get($index)
            .map_err(|error| db_error($operation, error))?
    };
}

struct UnionSpec {
    table: &'static str,
    identities: &'static [&'static str],
    label: &'static str,
}

const IMMUTABLE_UNIONS: &[UnionSpec] = &[
    UnionSpec {
        table: "session_summary_nodes",
        identities: &["summary_id"],
        label: "summary node",
    },
    UnionSpec {
        table: "session_summary_sources",
        identities: &["summary_id,source_ordinal"],
        label: "summary source",
    },
    UnionSpec {
        table: "session_summary_successors",
        identities: &["predecessor_summary_id,successor_summary_id"],
        label: "summary successor",
    },
    UnionSpec {
        table: "session_external_payload_manifests",
        identities: &["payload_ref"],
        label: "external payload manifest",
    },
    UnionSpec {
        table: "session_refresh_bindings",
        identities: &["session_id,operation_id", "session_id,generation"],
        label: "refresh binding",
    },
    UnionSpec {
        table: "session_refresh_progress",
        identities: &["session_id,operation_id,progress_ordinal"],
        label: "refresh progress",
    },
    UnionSpec {
        table: "session_refresh_batch_bindings",
        identities: &[
            "session_id,operation_id,progress_ordinal",
            "session_id,generation,batch_ordinal",
        ],
        label: "refresh batch binding",
    },
    UnionSpec {
        table: "session_refresh_receipts",
        identities: &["session_id,operation_id"],
        label: "refresh receipt",
    },
    UnionSpec {
        table: "session_temporal_projection_receipts",
        identities: &[
            "session_id,generation,batch_ordinal",
            "session_id,generation,batch_digest",
        ],
        label: "projection receipt",
    },
    UnionSpec {
        table: "session_turns",
        identities: &["session_id,generation,turn_id"],
        label: "temporal turn",
    },
    UnionSpec {
        table: "session_threads",
        identities: &["session_id,generation,thread_id"],
        label: "temporal thread",
    },
    UnionSpec {
        table: "session_agents",
        identities: &["session_id,generation,agent_id"],
        label: "temporal agent",
    },
    UnionSpec {
        table: "session_occurrences",
        identities: &["session_id,generation,occurrence_id"],
        label: "temporal occurrence",
    },
    UnionSpec {
        table: "session_logical_copy_edges",
        identities: &["session_id,generation,occurrence_id,copied_from_occurrence_id"],
        label: "logical copy edge",
    },
    UnionSpec {
        table: "session_turn_members",
        identities: &["session_id,generation,turn_id,occurrence_id"],
        label: "turn member",
    },
    UnionSpec {
        table: "session_thread_hierarchy_edges",
        identities: &["session_id,generation,parent_thread_id,child_thread_id"],
        label: "thread hierarchy edge",
    },
    UnionSpec {
        table: "session_agent_hierarchy_edges",
        identities: &["session_id,generation,parent_agent_id,child_agent_id"],
        label: "agent hierarchy edge",
    },
    UnionSpec {
        table: "session_assertions",
        identities: &["session_id,generation,assertion_id"],
        label: "temporal assertion",
    },
    UnionSpec {
        table: "session_summary_availability",
        identities: &["session_id,generation,summary_id"],
        label: "summary availability",
    },
    UnionSpec {
        table: "session_temporal_migration_receipts",
        identities: &["session_id,generation,batch_ordinal"],
        label: "migration receipt",
    },
];

const ANCHOR_UNIONS: &[UnionSpec] = &[
    UnionSpec {
        table: "retrieval_anchors",
        identities: &["anchor_id"],
        label: "retrieval anchor",
    },
    UnionSpec {
        table: "retrieval_anchor_aliases",
        identities: &["owner_json,alias_kind,locator_digest"],
        label: "retrieval anchor alias",
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct GenerationRow {
    session_id: String,
    generation: i64,
    state: String,
    frozen_watermarks_json: String,
    created_at: i64,
    ready_at: Option<i64>,
    activated_at: Option<i64>,
    completed_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RefreshRow {
    session_id: String,
    operation_id: String,
    request_digest: String,
    target_frontier_json: String,
    state: String,
    created_at: i64,
    updated_at: i64,
    terminal_at: Option<i64>,
    failure_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyRow {
    key_id: String,
    key_version: i64,
    key_material: Vec<u8>,
    created_at: i64,
    retired_at: Option<i64>,
}

pub(super) async fn preflight(conn: &Connection) -> Result<()> {
    for spec in IMMUTABLE_UNIONS {
        reject_row_collisions(conn, spec).await?;
    }
    for spec in ANCHOR_UNIONS {
        reject_row_collisions(conn, spec).await?;
    }
    preflight_summary_graph(conn).await?;
    preflight_observation_effects(conn).await?;
    generation_union(conn).await?;
    refresh_union(conn).await?;
    key_union(conn).await?;
    Ok(())
}

pub(super) async fn merge(conn: &Connection) -> Result<()> {
    let generations = generation_union(conn).await?;
    let refreshes = refresh_union(conn).await?;
    let keys = key_union(conn).await?;

    merge_plain(conn, "session_summary_nodes", "summary_id").await?;
    merge_plain(
        conn,
        "session_summary_sources",
        "summary_id, source_ordinal",
    )
    .await?;
    merge_plain(
        conn,
        "session_summary_successors",
        "predecessor_summary_id, successor_summary_id",
    )
    .await?;
    merge_plain(conn, "session_external_payload_manifests", "payload_ref").await?;

    seed_generations(conn, &generations).await?;
    seed_refresh_operations(conn, &refreshes).await?;

    // Projection receipts require generation state='building' on INSERT.
    // INSERT OR IGNORE still fires the BEFORE INSERT guard, so rematch must
    // skip identities already present on main once the generation is active.
    merge_projection_receipts(conn).await?;
    merge_plain(
        conn,
        "session_turns",
        "session_id, generation, ordinal, turn_id",
    )
    .await?;
    merge_plain(conn, "session_threads", "session_id, generation, thread_id").await?;
    merge_plain(conn, "session_agents", "session_id, generation, agent_id").await?;
    merge_plain(
        conn,
        "session_occurrences",
        "session_id, generation, knowledge_at, occurrence_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_logical_copy_edges",
        "session_id, generation, occurrence_id, copied_from_occurrence_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_turn_members",
        "session_id, generation, turn_id, ordinal, occurrence_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_thread_hierarchy_edges",
        "session_id, generation, parent_thread_id, ordinal, child_thread_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_agent_hierarchy_edges",
        "session_id, generation, parent_agent_id, ordinal, child_agent_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_assertions",
        "session_id, generation, knowledge_at, assertion_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_summary_availability",
        "session_id, generation, summary_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_temporal_migration_receipts",
        "session_id, generation, batch_ordinal",
    )
    .await?;

    merge_plain(conn, "session_refresh_bindings", "session_id, operation_id").await?;
    merge_plain(
        conn,
        "session_refresh_progress",
        "session_id, operation_id, progress_ordinal",
    )
    .await?;
    merge_plain(
        conn,
        "session_refresh_batch_bindings",
        "session_id, operation_id, progress_ordinal",
    )
    .await?;

    replay_generations(conn, &generations).await?;
    replay_refresh_operations(conn, &refreshes).await?;
    merge_plain(conn, "session_refresh_receipts", "session_id, operation_id").await?;
    merge_observation_effects(conn).await?;
    replay_keys(conn, &keys).await?;
    forward_migrate_legacy_sources(conn).await
}

async fn preflight_summary_graph(conn: &Connection) -> Result<()> {
    let edges = "SELECT predecessor_summary_id, successor_summary_id
                 FROM main.session_summary_successors
                 UNION
                 SELECT predecessor_summary_id, successor_summary_id
                 FROM source.session_summary_successors";
    if query_i64(
        conn,
        &format!(
            "WITH edges AS ({edges})
             SELECT COUNT(*) FROM (
                 SELECT predecessor_summary_id FROM edges
                 GROUP BY predecessor_summary_id HAVING COUNT(*) > 1
             )"
        ),
    )
    .await?
        != 0
    {
        return Err(db_message(
            "preflight_temporal_authority",
            "summary successor fork conflict",
        ));
    }

    let nodes = "SELECT summary_id, session_id FROM main.session_summary_nodes
                 UNION
                 SELECT summary_id, session_id FROM source.session_summary_nodes";
    if query_i64(
        conn,
        &format!(
            "WITH nodes AS ({nodes}), edges AS ({edges}),
             sources AS (
                 SELECT summary_id, source_summary_id
                 FROM main.session_summary_sources WHERE source_kind = 'summary'
                 UNION
                 SELECT summary_id, source_summary_id
                 FROM source.session_summary_sources WHERE source_kind = 'summary'
             )
             SELECT
               (SELECT COUNT(*) FROM edges
                JOIN nodes AS predecessor
                  ON predecessor.summary_id = edges.predecessor_summary_id
                JOIN nodes AS successor
                  ON successor.summary_id = edges.successor_summary_id
                WHERE predecessor.session_id IS NOT successor.session_id)
             + (SELECT COUNT(*) FROM sources
                JOIN nodes AS summary ON summary.summary_id = sources.summary_id
                JOIN nodes AS source ON source.summary_id = sources.source_summary_id
                WHERE summary.session_id IS NOT source.session_id)"
        ),
    )
    .await?
        != 0
    {
        return Err(db_message(
            "preflight_temporal_authority",
            "summary authority crosses session ownership",
        ));
    }

    if query_i64(
        conn,
        &format!(
            "WITH RECURSIVE edges AS ({edges}),
             walk(root, node) AS (
                 SELECT predecessor_summary_id, successor_summary_id FROM edges
                 UNION
                 SELECT walk.root, edges.successor_summary_id
                 FROM walk JOIN edges ON edges.predecessor_summary_id = walk.node
             )
             SELECT COUNT(*) FROM walk WHERE root = node"
        ),
    )
    .await?
        != 0
    {
        return Err(db_message(
            "preflight_temporal_authority",
            "summary successor cycle conflict",
        ));
    }
    Ok(())
}

async fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let sql = format!("PRAGMA source.table_info({})", quote_identifier(table));
    let mut rows = conn
        .query(&sql, ())
        .await
        .map_err(|error| db_error("inspect temporal authority table", error))?;
    let mut columns = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("inspect temporal authority table", error))?
    {
        columns.push(
            row.get::<String>(1)
                .map_err(|error| db_error("inspect temporal authority table", error))?,
        );
    }
    if columns.is_empty() {
        return Err(db_message(
            "preflight_temporal_authority",
            format!("{} table is missing", table),
        ));
    }
    Ok(columns)
}

async fn reject_row_collisions(conn: &Connection, spec: &UnionSpec) -> Result<()> {
    let table = quote_identifier(spec.table);
    let columns = table_columns(conn, spec.table).await?;
    let same_row = columns
        .iter()
        .map(|column| {
            let column = quote_identifier(column);
            format!("source_row.{column} IS target_row.{column}")
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    for identity in spec.identities {
        let same_identity = identity
            .split(',')
            .map(|column| {
                let column = quote_identifier(column);
                format!("source_row.{column} IS target_row.{column}")
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!(
            "SELECT COUNT(*)
             FROM source.{table} AS source_row
             JOIN main.{table} AS target_row ON {same_identity}
             WHERE NOT ({same_row})"
        );
        if query_i64(conn, &sql).await? != 0 {
            return Err(db_message(
                "preflight_temporal_authority",
                format!("{} identity collision", spec.label),
            ));
        }
    }
    Ok(())
}

async fn merge_plain(conn: &Connection, table: &str, order: &str) -> Result<()> {
    let table = quote_identifier(table);
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO main.{table}
             SELECT * FROM source.{table} ORDER BY {order}"
        ),
        (),
    )
    .await
    .map_err(|error| db_error("merge_temporal_authority", error))?;
    Ok(())
}

async fn merge_projection_receipts(conn: &Connection) -> Result<()> {
    let columns = table_columns(conn, "session_temporal_projection_receipts").await?;
    let column_list = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let source_columns = columns
        .iter()
        .map(|column| format!("source_row.{}", quote_identifier(column)))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute(
        &format!(
            "INSERT INTO main.session_temporal_projection_receipts ({column_list})
             SELECT {source_columns}
             FROM source.session_temporal_projection_receipts AS source_row
             WHERE NOT EXISTS (
                 SELECT 1
                 FROM main.session_temporal_projection_receipts AS target_row
                 WHERE target_row.session_id = source_row.session_id
                   AND target_row.generation = source_row.generation
                   AND target_row.batch_ordinal = source_row.batch_ordinal
             )
             ORDER BY source_row.session_id, source_row.generation, source_row.batch_ordinal"
        ),
        (),
    )
    .await
    .map_err(|error| db_error("merge_temporal_authority", error))?;
    Ok(())
}

async fn preflight_observation_effects(conn: &Connection) -> Result<()> {
    for schema in ["main", "source"] {
        let schema = quote_identifier(schema);
        if query_i64(
            conn,
            &format!(
                "SELECT COUNT(*)
                 FROM {schema}.session_temporal_observation_effects AS effect
                 LEFT JOIN {schema}.observations AS observation
                   ON observation.observation_id = effect.observation_id
                  AND observation.sequence = effect.observation_sequence
                  AND observation.receipt_id = effect.receipt_id
                 WHERE observation.observation_id IS NULL"
            ),
        )
        .await?
            != 0
        {
            return Err(db_message(
                "preflight_temporal_authority",
                "observation effect authority mismatch",
            ));
        }
    }
    if query_i64(
        conn,
        "SELECT COUNT(*)
         FROM source.session_temporal_observation_effects AS source_row
         JOIN main.session_temporal_observation_effects AS target_row
           USING(observation_id)
         WHERE source_row.session_id IS NOT target_row.session_id
            OR source_row.receipt_id IS NOT target_row.receipt_id
            OR source_row.effect_digest IS NOT target_row.effect_digest
            OR source_row.output_count IS NOT target_row.output_count
            OR source_row.recorded_at IS NOT target_row.recorded_at",
    )
    .await?
        != 0
    {
        return Err(db_message(
            "preflight_temporal_authority",
            "observation effect identity collision",
        ));
    }
    Ok(())
}

async fn merge_observation_effects(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "INSERT OR IGNORE INTO session_temporal_observation_effects(
             observation_id, observation_sequence, session_id, receipt_id,
             effect_digest, output_count, recorded_at
         )
         SELECT effect.observation_id, observation.sequence, effect.session_id,
                effect.receipt_id, effect.effect_digest, effect.output_count,
                effect.recorded_at
         FROM source.session_temporal_observation_effects AS effect
         JOIN main.observations AS observation
           ON observation.observation_id = effect.observation_id
         ORDER BY observation.sequence;",
    )
    .await
    .map_err(|error| db_error("merge_temporal_observation_effects", error))?;
    Ok(())
}

async fn read_generations(
    conn: &Connection,
    schema: &str,
) -> Result<BTreeMap<(String, i64), GenerationRow>> {
    let schema = quote_identifier(schema);
    let mut rows = conn
        .query(
            &format!(
                "SELECT session_id, generation, state, frozen_watermarks_json,
                        created_at, ready_at, activated_at, completed_at
                 FROM {schema}.session_temporal_generations
                 ORDER BY session_id, generation"
            ),
            (),
        )
        .await
        .map_err(|error| db_error("read temporal generations", error))?;
    let mut result = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("read temporal generations", error))?
    {
        let value = GenerationRow {
            session_id: row_value!(row, 0, "read temporal generations"),
            generation: row_value!(row, 1, "read temporal generations"),
            state: row_value!(row, 2, "read temporal generations"),
            frozen_watermarks_json: row_value!(row, 3, "read temporal generations"),
            created_at: row_value!(row, 4, "read temporal generations"),
            ready_at: row_value!(row, 5, "read temporal generations"),
            activated_at: row_value!(row, 6, "read temporal generations"),
            completed_at: row_value!(row, 7, "read temporal generations"),
        };
        result.insert((value.session_id.clone(), value.generation), value);
    }
    Ok(result)
}

fn generation_prefix(earlier: &GenerationRow, later: &GenerationRow) -> bool {
    if earlier.session_id != later.session_id
        || earlier.generation != later.generation
        || earlier.frozen_watermarks_json != later.frozen_watermarks_json
        || earlier.created_at != later.created_at
        || earlier
            .ready_at
            .is_some_and(|value| later.ready_at != Some(value))
        || earlier
            .activated_at
            .is_some_and(|value| later.activated_at != Some(value))
        || earlier
            .completed_at
            .is_some_and(|value| later.completed_at != Some(value))
    {
        return false;
    }
    matches!(
        (earlier.state.as_str(), later.state.as_str()),
        (
            "building",
            "building" | "ready" | "active" | "superseded" | "failed" | "cancelled"
        ) | (
            "ready",
            "ready" | "active" | "superseded" | "failed" | "cancelled"
        ) | ("active", "active" | "superseded")
            | ("superseded", "superseded")
            | ("failed", "failed")
            | ("cancelled", "cancelled")
    )
}

async fn generation_union(conn: &Connection) -> Result<BTreeMap<(String, i64), GenerationRow>> {
    let target_rows = read_generations(conn, "main").await?;
    let mut merged = target_rows.clone();
    for (key, source) in read_generations(conn, "source").await? {
        if let Some(target) = merged.get(&key) {
            let resolved = if generation_prefix(target, &source) {
                source
            } else if generation_prefix(&source, target) {
                target.clone()
            } else {
                return Err(db_message(
                    "preflight_temporal_authority",
                    "temporal generation lifecycle conflict",
                ));
            };
            merged.insert(key, resolved);
        } else {
            merged.insert(key, source);
        }
    }
    let mut active = BTreeSet::new();
    for row in merged.values().filter(|row| row.state == "active") {
        if !active.insert(row.session_id.as_str()) {
            return Err(db_message(
                "preflight_temporal_authority",
                "temporal generation active-state conflict",
            ));
        }
    }
    for active in target_rows.values().filter(|row| {
        row.state == "active"
            && merged
                .get(&(row.session_id.clone(), row.generation))
                .is_some_and(|desired| desired.state == "active")
    }) {
        let blocked_history = merged.values().any(|desired| {
            desired.session_id == active.session_id
                && desired.generation != active.generation
                && matches!(desired.state.as_str(), "active" | "superseded")
                && target_rows
                    .get(&(desired.session_id.clone(), desired.generation))
                    .is_none_or(|current| {
                        !matches!(current.state.as_str(), "active" | "superseded")
                    })
        });
        if blocked_history {
            return Err(db_message(
                "preflight_temporal_authority",
                "temporal generation history is not appendable",
            ));
        }
    }
    Ok(merged)
}

async fn seed_generations(
    conn: &Connection,
    generations: &BTreeMap<(String, i64), GenerationRow>,
) -> Result<()> {
    let existing = read_generations(conn, "main").await?;
    for (key, row) in generations {
        if existing.contains_key(key) {
            continue;
        }
        conn.execute(
            "INSERT INTO session_temporal_generations(
                 session_id, generation, state, frozen_watermarks_json, created_at,
                 ready_at, activated_at, completed_at
             ) VALUES (?1, ?2, 'building', ?3, ?4, NULL, NULL, NULL)",
            params![
                row.session_id.clone(),
                row.generation,
                row.frozen_watermarks_json.clone(),
                row.created_at
            ],
        )
        .await
        .map_err(|error| db_error("replay temporal generation", error))?;
    }
    Ok(())
}

async fn replay_generations(
    conn: &Connection,
    generations: &BTreeMap<(String, i64), GenerationRow>,
) -> Result<()> {
    let mut current = read_generations(conn, "main").await?;

    for (key, desired) in generations {
        let row = current.get(key).ok_or_else(|| {
            db_message(
                "merge_temporal_authority",
                "seeded temporal generation is missing",
            )
        })?;
        if row.state == "building"
            && matches!(desired.state.as_str(), "ready" | "active" | "superseded")
        {
            conn.execute(
                "UPDATE session_temporal_generations
                 SET state = 'ready', ready_at = ?3
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
                params![
                    desired.session_id.clone(),
                    desired.generation,
                    desired.ready_at
                ],
            )
            .await
            .map_err(|error| db_error("replay temporal generation", error))?;
        } else if row.state == "building"
            && matches!(desired.state.as_str(), "failed" | "cancelled")
        {
            conn.execute(
                "UPDATE session_temporal_generations
                 SET state = ?3, completed_at = ?4
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
                params![
                    desired.session_id.clone(),
                    desired.generation,
                    desired.state.clone(),
                    desired.completed_at
                ],
            )
            .await
            .map_err(|error| db_error("replay temporal generation", error))?;
        }
    }
    current = read_generations(conn, "main").await?;
    for (key, desired) in generations {
        let row = current.get(key).ok_or_else(|| {
            db_message(
                "merge_temporal_authority",
                "seeded temporal generation is missing",
            )
        })?;
        if row.state == "ready" && matches!(desired.state.as_str(), "failed" | "cancelled") {
            conn.execute(
                "UPDATE session_temporal_generations
                 SET state = ?3, completed_at = ?4
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
                params![
                    desired.session_id.clone(),
                    desired.generation,
                    desired.state.clone(),
                    desired.completed_at
                ],
            )
            .await
            .map_err(|error| db_error("replay temporal generation", error))?;
        }
    }
    current = read_generations(conn, "main").await?;
    for (key, desired) in generations {
        let row = current.get(key).ok_or_else(|| {
            db_message(
                "merge_temporal_authority",
                "seeded temporal generation is missing",
            )
        })?;
        if row.state == "active" && desired.state == "superseded" {
            conn.execute(
                "UPDATE session_temporal_generations
                 SET state = 'superseded', completed_at = ?3
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'active'",
                params![
                    desired.session_id.clone(),
                    desired.generation,
                    desired.completed_at
                ],
            )
            .await
            .map_err(|error| db_error("replay temporal generation", error))?;
        }
    }
    current = read_generations(conn, "main").await?;
    let mut activations = generations
        .iter()
        .filter(|(key, desired)| {
            current.get(*key).is_some_and(|row| row.state == "ready")
                && matches!(desired.state.as_str(), "active" | "superseded")
        })
        .map(|(_, row)| row)
        .collect::<Vec<_>>();
    activations.sort_by(|left, right| {
        (left.activated_at, left.session_id.as_str(), left.generation).cmp(&(
            right.activated_at,
            right.session_id.as_str(),
            right.generation,
        ))
    });
    for desired in activations {
        conn.execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = ?3
             WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
            params![
                desired.session_id.clone(),
                desired.generation,
                desired.activated_at
            ],
        )
        .await
        .map_err(|error| db_error("replay temporal generation", error))?;
        if desired.state == "superseded" {
            conn.execute(
                "UPDATE session_temporal_generations
                 SET state = 'superseded', completed_at = ?3
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'active'",
                params![
                    desired.session_id.clone(),
                    desired.generation,
                    desired.completed_at
                ],
            )
            .await
            .map_err(|error| db_error("replay temporal generation", error))?;
        }
    }
    Ok(())
}

async fn read_refreshes(
    conn: &Connection,
    schema: &str,
) -> Result<BTreeMap<(String, String), RefreshRow>> {
    let schema = quote_identifier(schema);
    let mut rows = conn
        .query(
            &format!(
                "SELECT session_id, operation_id, request_digest, target_frontier_json,
                        state, created_at, updated_at, terminal_at, failure_code
                 FROM {schema}.session_refresh_operations
                 ORDER BY session_id, operation_id"
            ),
            (),
        )
        .await
        .map_err(|error| db_error("read refresh operations", error))?;
    let mut result = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("read refresh operations", error))?
    {
        let value = RefreshRow {
            session_id: row_value!(row, 0, "read refresh operations"),
            operation_id: row_value!(row, 1, "read refresh operations"),
            request_digest: row_value!(row, 2, "read refresh operations"),
            target_frontier_json: row_value!(row, 3, "read refresh operations"),
            state: row_value!(row, 4, "read refresh operations"),
            created_at: row_value!(row, 5, "read refresh operations"),
            updated_at: row_value!(row, 6, "read refresh operations"),
            terminal_at: row_value!(row, 7, "read refresh operations"),
            failure_code: row_value!(row, 8, "read refresh operations"),
        };
        result.insert(
            (value.session_id.clone(), value.operation_id.clone()),
            value,
        );
    }
    Ok(result)
}

fn merge_refresh_rows(left: &RefreshRow, right: &RefreshRow) -> Option<RefreshRow> {
    if left.session_id != right.session_id
        || left.operation_id != right.operation_id
        || left.request_digest != right.request_digest
        || left.target_frontier_json != right.target_frontier_json
        || left.created_at != right.created_at
    {
        return None;
    }
    match (left.state.as_str(), right.state.as_str()) {
        ("running", "running") => Some(if left.updated_at >= right.updated_at {
            left.clone()
        } else {
            right.clone()
        }),
        ("running", _) if right.updated_at >= left.updated_at => Some(right.clone()),
        (_, "running") if left.updated_at >= right.updated_at => Some(left.clone()),
        _ if left == right => Some(left.clone()),
        _ => None,
    }
}

async fn refresh_union(conn: &Connection) -> Result<BTreeMap<(String, String), RefreshRow>> {
    let mut merged = read_refreshes(conn, "main").await?;
    for (key, source) in read_refreshes(conn, "source").await? {
        if let Some(target) = merged.get(&key) {
            let resolved = merge_refresh_rows(target, &source).ok_or_else(|| {
                db_message(
                    "preflight_temporal_authority",
                    "refresh operation lifecycle conflict",
                )
            })?;
            merged.insert(key, resolved);
        } else {
            merged.insert(key, source);
        }
    }
    let mut running = BTreeSet::new();
    for row in merged.values().filter(|row| row.state == "running") {
        if !running.insert(row.session_id.as_str()) {
            return Err(db_message(
                "preflight_temporal_authority",
                "refresh operation running-state conflict",
            ));
        }
    }
    Ok(merged)
}

async fn seed_refresh_operations(
    conn: &Connection,
    refreshes: &BTreeMap<(String, String), RefreshRow>,
) -> Result<()> {
    let existing = read_refreshes(conn, "main").await?;
    for (key, row) in refreshes {
        if existing.contains_key(key) {
            continue;
        }
        conn.execute(
            "INSERT INTO session_refresh_operations(
                 session_id, operation_id, request_digest, target_frontier_json,
                 state, created_at, updated_at, terminal_at, failure_code
             ) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?5, NULL, NULL)",
            params![
                row.session_id.clone(),
                row.operation_id.clone(),
                row.request_digest.clone(),
                row.target_frontier_json.clone(),
                row.created_at
            ],
        )
        .await
        .map_err(|error| db_error("replay refresh operation", error))?;
    }
    Ok(())
}

async fn replay_refresh_operations(
    conn: &Connection,
    refreshes: &BTreeMap<(String, String), RefreshRow>,
) -> Result<()> {
    let current = read_refreshes(conn, "main").await?;
    for (key, desired) in refreshes {
        let row = current.get(key).ok_or_else(|| {
            db_message(
                "merge_temporal_authority",
                "seeded refresh operation is missing",
            )
        })?;
        if row == desired {
            continue;
        }
        if desired.state == "running" {
            conn.execute(
                "UPDATE session_refresh_operations
                 SET updated_at = ?3
                 WHERE session_id = ?1 AND operation_id = ?2 AND state = 'running'",
                params![
                    desired.session_id.clone(),
                    desired.operation_id.clone(),
                    desired.updated_at
                ],
            )
            .await
            .map_err(|error| db_error("replay refresh operation", error))?;
        } else {
            conn.execute(
                "UPDATE session_refresh_operations
                 SET state = ?3, updated_at = ?4, terminal_at = ?5, failure_code = ?6
                 WHERE session_id = ?1 AND operation_id = ?2 AND state = 'running'",
                params![
                    desired.session_id.clone(),
                    desired.operation_id.clone(),
                    desired.state.clone(),
                    desired.updated_at,
                    desired.terminal_at,
                    desired.failure_code.clone()
                ],
            )
            .await
            .map_err(|error| db_error("replay refresh operation", error))?;
        }
    }
    Ok(())
}

async fn read_keys(conn: &Connection, schema: &str) -> Result<Vec<KeyRow>> {
    let schema = quote_identifier(schema);
    let mut rows = conn
        .query(
            &format!(
                "SELECT key_id, key_version, key_material, created_at, retired_at
                 FROM {schema}.session_query_cursor_keys ORDER BY key_version"
            ),
            (),
        )
        .await
        .map_err(|error| db_error("read cursor key history", error))?;
    let mut result = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("read cursor key history", error))?
    {
        result.push(KeyRow {
            key_id: row_value!(row, 0, "read cursor key history"),
            key_version: row_value!(row, 1, "read cursor key history"),
            key_material: row_value!(row, 2, "read cursor key history"),
            created_at: row_value!(row, 3, "read cursor key history"),
            retired_at: row_value!(row, 4, "read cursor key history"),
        });
    }
    Ok(result)
}

fn validate_key_history(rows: &[KeyRow]) -> bool {
    rows.iter().enumerate().all(|(index, row)| {
        if index > 0 {
            let previous = &rows[index - 1];
            if row.key_version <= previous.key_version
                || row.created_at <= previous.created_at
                || previous.retired_at != Some(row.created_at)
            {
                return false;
            }
        }
        index + 1 != rows.len() || row.retired_at.is_none()
    })
}

fn key_prefix(prefix: &[KeyRow], full: &[KeyRow]) -> bool {
    prefix.len() <= full.len()
        && prefix.iter().enumerate().all(|(index, row)| {
            let expected = &full[index];
            row.key_id == expected.key_id
                && row.key_version == expected.key_version
                && row.key_material == expected.key_material
                && row.created_at == expected.created_at
                && (row.retired_at == expected.retired_at
                    || (index + 1 == prefix.len()
                        && row.retired_at.is_none()
                        && expected.retired_at == full.get(index + 1).map(|next| next.created_at)))
        })
}

async fn key_union(conn: &Connection) -> Result<Vec<KeyRow>> {
    let target = read_keys(conn, "main").await?;
    let source = read_keys(conn, "source").await?;
    if !validate_key_history(&target) || !validate_key_history(&source) {
        return Err(db_message(
            "preflight_temporal_authority",
            "cursor key history is invalid",
        ));
    }
    let merged = if target.len() >= source.len() {
        target.clone()
    } else {
        source.clone()
    };
    if !key_prefix(&target, &merged) || !key_prefix(&source, &merged) {
        return Err(db_message(
            "preflight_temporal_authority",
            "cursor key prefix collision",
        ));
    }
    Ok(merged)
}

async fn replay_keys(conn: &Connection, keys: &[KeyRow]) -> Result<()> {
    let current = read_keys(conn, "main").await?;
    for row in keys.iter().skip(current.len()) {
        conn.execute(
            "INSERT INTO session_query_cursor_keys(
                 key_id, key_version, key_material, created_at, retired_at
             ) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![
                row.key_id.clone(),
                row.key_version,
                row.key_material.clone(),
                row.created_at
            ],
        )
        .await
        .map_err(|error| db_error("replay cursor key rotation", error))?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct LegacyImportItem {
    provider: String,
    message_id: String,
    content_hash: String,
    role: String,
    knowledge_at: i64,
    snippet_text: String,
    index_text: String,
    observation_id: String,
    retrieval_anchor_id: String,
}

/// Forward-migrate eligible legacy LCM/session rows into canonical temporal
/// generations with immutable migration receipts.
///
/// Binding authority is projected provenance + retrieval-anchor (not raw LCM
/// rows and not consolidate-only `observation_projection_aliases`). Replay is
/// idempotent via `source_digest` receipts; quarantined / `legacy_source` /
/// `legacy_truncated` rows never enter temporal sinks. Recovery is whole-TX
/// rematch (no mid-batch resume). Receipt rows are PR19 deletion-gate evidence
/// that eligible legacy sources have a canonical temporal generation covering
/// the migrated digest.
async fn forward_migrate_legacy_sources(conn: &Connection) -> Result<()> {
    if !main_table_exists(conn, "lcm_raw_messages").await? {
        return Ok(());
    }
    if !main_table_exists(conn, "observations").await?
        || !main_table_exists(conn, "observation_projection_provenance").await?
        || !main_table_exists(conn, "observation_retrieval_anchors").await?
    {
        return Ok(());
    }

    let sessions = legacy_migration_sessions(conn).await?;
    for session_id in sessions {
        migrate_session_legacy_sources(conn, &session_id).await?;
    }
    Ok(())
}

async fn main_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM main.sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            params![table],
        )
        .await
        .map_err(|error| db_error("inspect main temporal table", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error("inspect main temporal table", error))?
        .is_some())
}

async fn legacy_migration_sessions(conn: &Connection) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT session_id
             FROM lcm_raw_messages
             WHERE COALESCE(legacy_source, 0) = 0
               AND COALESCE(legacy_truncated, 0) = 0
               AND COALESCE(json_extract(metadata_json, '$.payload_access'), '')
                   NOT IN ('quarantined', 'redacted', 'deleted')
             ORDER BY session_id",
            (),
        )
        .await
        .map_err(|error| db_error("list legacy migration sessions", error))?;
    let mut sessions = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("list legacy migration sessions", error))?
    {
        sessions.push(row_value!(row, 0, "list legacy migration sessions"));
    }
    Ok(sessions)
}

async fn migrate_session_legacy_sources(conn: &Connection, session_id: &str) -> Result<()> {
    let items = load_importable_legacy_items(conn, session_id).await?;
    if items.is_empty() {
        return Ok(());
    }

    let source_digest = legacy_source_digest(session_id, &items);
    if migration_receipt_covers(conn, session_id, &source_digest).await? {
        return Ok(());
    }

    let generation = ensure_migration_generation(conn, session_id).await?;
    let batch_ordinal = next_migration_batch_ordinal(conn, session_id, generation).await?;
    let committed_at = query_i64(
        conn,
        "SELECT CAST(strftime('%s','now') AS INTEGER) * 1000000",
    )
    .await
    .unwrap_or(0);

    let mut imported = 0_i64;
    for chunk in items.chunks(MAX_MIGRATION_BATCH_ITEMS) {
        for item in chunk {
            imported += import_legacy_item(conn, session_id, generation, item).await?;
            #[cfg(test)]
            if FORWARD_MIGRATE_FAULT_AFTER_IMPORT.with(|flag| flag.get()) && imported > 0 {
                return Err(db_message(
                    "merge_temporal_authority",
                    "injected forward-migrate fault after import",
                ));
            }
        }
    }
    conn.execute(
        "INSERT INTO session_temporal_migration_receipts(
             session_id, generation, batch_ordinal, source_digest,
             frozen_watermarks_json, imported_items, committed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            session_id,
            generation,
            batch_ordinal,
            source_digest,
            MIGRATION_WATERMARKS_JSON,
            imported,
            committed_at
        ],
    )
    .await
    .map_err(|error| db_error("write temporal migration receipt", error))?;
    Ok(())
}

async fn load_importable_legacy_items(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<LegacyImportItem>> {
    let mut rows = conn
        .query(
            "SELECT raw.provider, raw.message_id, raw.content_hash, raw.role,
                    COALESCE(raw.timestamp, raw.ordinal, 0),
                    COALESCE(raw.snippet_text, raw.content, ''),
                    COALESCE(raw.index_text, raw.snippet_text, raw.content, ''),
                    provenance.observation_id, anchor.anchor_id
             FROM lcm_raw_messages AS raw
             JOIN observation_projection_provenance AS provenance
               ON provenance.output_provider = raw.provider
              AND provenance.output_message_id = raw.message_id
             JOIN observations AS observation
               ON observation.observation_id = provenance.observation_id
             JOIN observation_retrieval_anchors AS anchor
               ON anchor.observation_id = observation.observation_id
             WHERE raw.session_id = ?1
               AND COALESCE(raw.legacy_source, 0) = 0
               AND COALESCE(raw.legacy_truncated, 0) = 0
               AND COALESCE(json_extract(raw.metadata_json, '$.payload_access'), '')
                   NOT IN ('quarantined', 'redacted', 'deleted')
             ORDER BY raw.provider, raw.message_id, provenance.observation_id",
            params![session_id],
        )
        .await
        .map_err(|error| db_error("load importable legacy temporal sources", error))?;

    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("load importable legacy temporal sources", error))?
    {
        let provider: String = row_value!(row, 0, "load importable legacy temporal sources");
        let message_id: String = row_value!(row, 1, "load importable legacy temporal sources");
        let key = (provider.clone(), message_id.clone());
        if !seen.insert(key) {
            continue;
        }
        items.push(LegacyImportItem {
            provider,
            message_id,
            content_hash: row_value!(row, 2, "load importable legacy temporal sources"),
            role: row_value!(row, 3, "load importable legacy temporal sources"),
            knowledge_at: row_value!(row, 4, "load importable legacy temporal sources"),
            snippet_text: row_value!(row, 5, "load importable legacy temporal sources"),
            index_text: row_value!(row, 6, "load importable legacy temporal sources"),
            observation_id: row_value!(row, 7, "load importable legacy temporal sources"),
            retrieval_anchor_id: row_value!(row, 8, "load importable legacy temporal sources"),
        });
    }
    Ok(items)
}

fn legacy_source_digest(session_id: &str, items: &[LegacyImportItem]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(LEGACY_SOURCE_DIGEST_DOMAIN);
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    for item in items {
        hasher.update(item.provider.as_bytes());
        hasher.update([0]);
        hasher.update(item.message_id.as_bytes());
        hasher.update([0]);
        hasher.update(item.content_hash.as_bytes());
        hasher.update([0]);
        hasher.update(item.observation_id.as_bytes());
        hasher.update([0]);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

async fn migration_receipt_covers(
    conn: &Connection,
    session_id: &str,
    source_digest: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM session_temporal_migration_receipts
             WHERE session_id = ?1 AND source_digest = ?2
             LIMIT 1",
            params![session_id, source_digest],
        )
        .await
        .map_err(|error| db_error("lookup temporal migration receipt", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error("lookup temporal migration receipt", error))?
        .is_some())
}

async fn ensure_migration_generation(conn: &Connection, session_id: &str) -> Result<i64> {
    if let Some(generation) = active_or_building_generation(conn, session_id).await? {
        return Ok(generation);
    }

    let generation = next_generation_number(conn, session_id).await?;
    let created_at = query_i64(
        conn,
        "SELECT CAST(strftime('%s','now') AS INTEGER) * 1000000",
    )
    .await
    .unwrap_or(1);
    conn.execute(
        "INSERT INTO session_temporal_generations(
             session_id, generation, state, frozen_watermarks_json, created_at,
             ready_at, activated_at, completed_at
         ) VALUES (?1, ?2, 'building', ?3, ?4, NULL, NULL, NULL)",
        params![
            session_id,
            generation,
            MIGRATION_WATERMARKS_JSON,
            created_at
        ],
    )
    .await
    .map_err(|error| db_error("seed temporal migration generation", error))?;
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = ?3
         WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
        params![session_id, generation, created_at],
    )
    .await
    .map_err(|error| db_error("ready temporal migration generation", error))?;
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'active', activated_at = ?3
         WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
        params![session_id, generation, created_at],
    )
    .await
    .map_err(|error| db_error("activate temporal migration generation", error))?;
    Ok(generation)
}

async fn active_or_building_generation(conn: &Connection, session_id: &str) -> Result<Option<i64>> {
    let mut rows = conn
        .query(
            "SELECT generation FROM session_temporal_generations
             WHERE session_id = ?1 AND state IN ('active', 'building', 'ready')
             ORDER BY
               CASE state
                 WHEN 'active' THEN 0
                 WHEN 'ready' THEN 1
                 ELSE 2
               END,
               generation DESC
             LIMIT 1",
            params![session_id],
        )
        .await
        .map_err(|error| db_error("read temporal migration generation", error))?;
    Ok(
        match rows
            .next()
            .await
            .map_err(|error| db_error("read temporal migration generation", error))?
        {
            Some(row) => Some(row_value!(row, 0, "read temporal migration generation")),
            None => None,
        },
    )
}

async fn next_generation_number(conn: &Connection, session_id: &str) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(generation), 0) + 1
             FROM session_temporal_generations
             WHERE session_id = ?1",
            params![session_id],
        )
        .await
        .map_err(|error| db_error("allocate temporal migration generation", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error("allocate temporal migration generation", error))?
        .ok_or_else(|| {
            db_message(
                "allocate temporal migration generation",
                "missing generation allocation row",
            )
        })?;
    Ok(row_value!(row, 0, "allocate temporal migration generation"))
}

async fn next_migration_batch_ordinal(
    conn: &Connection,
    session_id: &str,
    generation: i64,
) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(batch_ordinal) + 1, 0)
             FROM session_temporal_migration_receipts
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id, generation],
        )
        .await
        .map_err(|error| db_error("allocate temporal migration batch ordinal", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| db_error("allocate temporal migration batch ordinal", error))?
        .ok_or_else(|| {
            db_message(
                "allocate temporal migration batch ordinal",
                "missing batch ordinal allocation row",
            )
        })?;
    Ok(row_value!(
        row,
        0,
        "allocate temporal migration batch ordinal"
    ))
}

async fn import_legacy_item(
    conn: &Connection,
    session_id: &str,
    generation: i64,
    item: &LegacyImportItem,
) -> Result<i64> {
    let observation_id =
        CanonicalObservationIdV1::new(item.observation_id.clone()).map_err(|error| {
            db_message(
                "import legacy temporal occurrence",
                &format!("invalid observation identity: {error}"),
            )
        })?;
    let occurrence_id =
        MessageOccurrenceIdV1::derive(&observation_id, ProjectionOutputOrdinalV1::new(0));

    let mut existing = conn
        .query(
            "SELECT 1 FROM session_occurrences
             WHERE session_id = ?1 AND generation = ?2 AND occurrence_id = ?3
             LIMIT 1",
            params![session_id, generation, occurrence_id.as_str()],
        )
        .await
        .map_err(|error| db_error("lookup temporal occurrence", error))?;
    if existing
        .next()
        .await
        .map_err(|error| db_error("lookup temporal occurrence", error))?
        .is_some()
    {
        return Ok(0);
    }

    let valid_time_json = format!(r#"{{"kind":"known","valid_at":{}}}"#, item.knowledge_at);
    let evidence_json = format!(
        concat!(
            r#"{{"authority":"legacy_migration","evidence_class":"provider_declared","#,
            r#""source_anchor_id":"{}","provider":"{}","message_id":"{}"}}"#
        ),
        item.retrieval_anchor_id, item.provider, item.message_id
    );

    conn.execute(
        "INSERT INTO session_occurrences(
             session_id, generation, occurrence_id, source_observation_id,
             projection_output_ordinal, retrieval_anchor_id, message_id,
             role, knowledge_at, valid_time_json, evidence_json,
             snippet_text, index_text
         ) VALUES (
             ?1, ?2, ?3, ?4, 0, ?5, ?6,
             ?7, ?8, ?9, ?10, ?11, ?12
         )",
        params![
            session_id,
            generation,
            occurrence_id.as_str(),
            item.observation_id.clone(),
            item.retrieval_anchor_id.clone(),
            item.message_id.clone(),
            item.role.clone(),
            item.knowledge_at,
            valid_time_json,
            evidence_json,
            item.snippet_text.clone(),
            item.index_text.clone()
        ],
    )
    .await
    .map_err(|error| db_error("import legacy temporal occurrence", error))?;

    conn.execute(
        "INSERT OR IGNORE INTO session_current_entities(
             session_id, generation, entity_kind, entity_id,
             current_assertion_id, current_occurrence_id, coverage_json
         ) VALUES (
             ?1, ?2, 'occurrence_anchor', ?3, NULL, ?4,
             '{\"occurrence_count\":1,\"source\":\"legacy_migration\"}'
         )",
        params![
            session_id,
            generation,
            item.retrieval_anchor_id.clone(),
            occurrence_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error("import legacy temporal current entity", error))?;

    Ok(1)
}

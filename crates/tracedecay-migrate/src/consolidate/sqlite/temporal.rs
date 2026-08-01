use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
use std::cell::Cell;

use tracedecay_runtime_core::db::engine::{Executor, params};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalObservationIdV1, MessageOccurrenceIdV1, ProjectionOutputOrdinalV1,
};

use tracedecay_runtime_core::errors::Result;

use super::{db_error, db_message, query_i64, quote_identifier};

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TemporalMergeFaultPhase {
    None,
    AfterImport,
    AfterSupersessionMerge,
    AfterFtsParity,
}

#[cfg(test)]
thread_local! {
    static TEMPORAL_MERGE_FAULT_PHASE: Cell<TemporalMergeFaultPhase> =
        const { Cell::new(TemporalMergeFaultPhase::None) };
}

/// Test-only fault injection: abort after the first successful temporal import
/// inside forward-migrate so the outer write TX must roll back.
#[cfg(test)]
pub(super) fn set_forward_migrate_fault_after_import(enabled: bool) {
    TEMPORAL_MERGE_FAULT_PHASE.with(|flag| {
        flag.set(if enabled {
            TemporalMergeFaultPhase::AfterImport
        } else {
            TemporalMergeFaultPhase::None
        });
    });
}

/// Test-only fault injection across consolidate temporal merge phases.
#[cfg(test)]
pub(super) fn set_temporal_merge_fault_phase(phase: &str) {
    let mapped = match phase {
        "after_import" => TemporalMergeFaultPhase::AfterImport,
        "after_supersession_merge" => TemporalMergeFaultPhase::AfterSupersessionMerge,
        "after_fts_parity" => TemporalMergeFaultPhase::AfterFtsParity,
        _ => TemporalMergeFaultPhase::None,
    };
    TEMPORAL_MERGE_FAULT_PHASE.with(|flag| flag.set(mapped));
}

#[cfg(test)]
fn inject_temporal_merge_fault(phase: TemporalMergeFaultPhase) -> Result<()> {
    if TEMPORAL_MERGE_FAULT_PHASE.with(std::cell::Cell::get) == phase {
        return Err(db_message(
            "merge_temporal_authority",
            match phase {
                TemporalMergeFaultPhase::None => "injected temporal merge fault",
                TemporalMergeFaultPhase::AfterImport => {
                    "injected forward-migrate fault after import"
                }
                TemporalMergeFaultPhase::AfterSupersessionMerge => {
                    "injected temporal merge fault after supersession"
                }
                TemporalMergeFaultPhase::AfterFtsParity => {
                    "injected temporal merge fault after fts parity"
                }
            },
        ));
    }
    Ok(())
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
        table: "session_assertion_supersession",
        identities: &["session_id,generation,superseded_assertion_id,superseding_assertion_id"],
        label: "assertion supersession",
    },
    UnionSpec {
        table: "session_current_entities",
        identities: &["session_id,generation,entity_kind,entity_id"],
        label: "current entity",
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
    UnionSpec {
        table: "session_temporal_migration_dispositions",
        identities: &["session_id,generation,batch_ordinal,disposition_ordinal"],
        label: "migration disposition",
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

pub(super) async fn preflight(conn: &impl Executor) -> Result<()> {
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

pub(super) async fn merge(conn: &impl Executor) -> Result<()> {
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
        "session_assertion_supersession",
        "session_id, generation, superseded_assertion_id, superseding_assertion_id",
    )
    .await?;
    merge_plain(
        conn,
        "session_current_entities",
        "session_id, generation, entity_kind, entity_id",
    )
    .await?;
    #[cfg(test)]
    inject_temporal_merge_fault(TemporalMergeFaultPhase::AfterSupersessionMerge)?;
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
    merge_plain(
        conn,
        "session_temporal_migration_dispositions",
        "session_id, generation, batch_ordinal, disposition_ordinal",
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
    forward_migrate_legacy_sources(conn).await?;
    rebuild_migrated_current_entities(conn).await?;
    parity_check_temporal_fts_derivatives(conn).await?;
    #[cfg(test)]
    inject_temporal_merge_fault(TemporalMergeFaultPhase::AfterFtsParity)?;
    assert_zero_legacy_temporal_authority(conn).await
}

async fn preflight_summary_graph(conn: &impl Executor) -> Result<()> {
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

async fn table_columns(conn: &impl Executor, table: &str) -> Result<Vec<String>> {
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
            format!("{table} table is missing"),
        ));
    }
    Ok(columns)
}

async fn reject_row_collisions(conn: &impl Executor, spec: &UnionSpec) -> Result<()> {
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

async fn merge_plain(conn: &impl Executor, table: &str, order: &str) -> Result<()> {
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

async fn merge_projection_receipts(conn: &impl Executor) -> Result<()> {
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

async fn preflight_observation_effects(conn: &impl Executor) -> Result<()> {
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

async fn merge_observation_effects(conn: &impl Executor) -> Result<()> {
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
    conn: &impl Executor,
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

async fn generation_union(conn: &impl Executor) -> Result<BTreeMap<(String, i64), GenerationRow>> {
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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

async fn refresh_union(conn: &impl Executor) -> Result<BTreeMap<(String, String), RefreshRow>> {
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
    conn: &impl Executor,
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
    conn: &impl Executor,
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

async fn read_keys(conn: &impl Executor, schema: &str) -> Result<Vec<KeyRow>> {
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

async fn key_union(conn: &impl Executor) -> Result<Vec<KeyRow>> {
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

async fn replay_keys(conn: &impl Executor, keys: &[KeyRow]) -> Result<()> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LegacyRowDisposition {
    Eligible,
    Quarantined,
    PolicyExcluded,
    Unbound,
    Ineligible,
}

impl LegacyRowDisposition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Quarantined => "quarantined",
            Self::PolicyExcluded => "policy_excluded",
            Self::Unbound => "unbound",
            Self::Ineligible => "ineligible",
        }
    }
}

struct LegacyCandidateRow {
    provider: String,
    message_id: String,
    content_hash: String,
    role: String,
    knowledge_at: i64,
    snippet_text: String,
    index_text: String,
    output_ordinal: i64,
    observation_id: Option<String>,
    retrieval_anchor_id: Option<String>,
    disposition: LegacyRowDisposition,
    reason: &'static str,
}

/// Forward-migrate eligible legacy LCM/session rows into canonical temporal
/// generations with immutable migration receipts and typed skip dispositions.
///
/// Binding authority is projected provenance + retrieval-anchor (not raw LCM
/// rows and not consolidate-only `observation_projection_aliases`). Replay is
/// idempotent via `source_digest` receipts; quarantined / policy-excluded /
/// unbound / ineligible rows emit disposition receipts and never enter temporal
/// sinks. Multi-output observations keep their projection output ordinals.
/// Recovery is whole-TX rematch (no mid-batch resume). Receipt rows are PR19
/// deletion-gate evidence that every legacy row has an explicit disposition.
async fn forward_migrate_legacy_sources(conn: &impl Executor) -> Result<()> {
    if !main_table_exists(conn, "lcm_raw_messages").await? {
        return Ok(());
    }
    if !main_table_exists(conn, "session_temporal_migration_dispositions").await? {
        return Ok(());
    }

    let sessions = legacy_migration_sessions(conn).await?;
    for session_id in sessions {
        migrate_session_legacy_sources(conn, &session_id).await?;
    }
    Ok(())
}

async fn main_table_exists(conn: &impl Executor, table: &str) -> Result<bool> {
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

async fn legacy_migration_sessions(conn: &impl Executor) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT session_id
             FROM lcm_raw_messages
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

async fn migrate_session_legacy_sources(conn: &impl Executor, session_id: &str) -> Result<()> {
    let candidates = load_legacy_candidate_rows(conn, session_id).await?;
    if candidates.is_empty() {
        return Ok(());
    }

    let source_digest = legacy_source_digest(session_id, &candidates);
    if migration_receipt_covers(conn, session_id, &source_digest).await? {
        return Ok(());
    }

    let generation = ensure_migration_generation(conn, session_id).await?;
    let batch_ordinal = next_migration_batch_ordinal(conn, session_id, generation).await?;
    let committed_at = query_i64(
        conn,
        "SELECT CAST(strftime('%s','now') AS INTEGER) * 1000000",
    )
    .await?;

    write_migration_dispositions(conn, session_id, generation, batch_ordinal, &candidates).await?;

    let mut imported = 0_i64;
    for chunk in candidates.chunks(MAX_MIGRATION_BATCH_ITEMS) {
        for item in chunk {
            if item.disposition != LegacyRowDisposition::Eligible {
                continue;
            }
            imported += import_legacy_item(conn, session_id, generation, item).await?;
            #[cfg(test)]
            if imported > 0 {
                inject_temporal_merge_fault(TemporalMergeFaultPhase::AfterImport)?;
            }
        }
    }

    let watermarks_json = migration_watermarks_json(&candidates);
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
            watermarks_json,
            imported,
            committed_at
        ],
    )
    .await
    .map_err(|error| db_error("write temporal migration receipt", error))?;
    Ok(())
}

async fn load_legacy_candidate_rows(
    conn: &impl Executor,
    session_id: &str,
) -> Result<Vec<LegacyCandidateRow>> {
    let has_observation_binding = main_table_exists(conn, "observations").await?
        && main_table_exists(conn, "observation_projection_provenance").await?
        && main_table_exists(conn, "observation_retrieval_anchors").await?;

    let sql = if has_observation_binding {
        "SELECT raw.provider, raw.message_id, raw.content_hash, raw.role,
                COALESCE(raw.timestamp, raw.ordinal, 0),
                COALESCE(raw.snippet_text, raw.content, ''),
                COALESCE(raw.index_text, raw.snippet_text, raw.content, ''),
                COALESCE(raw.legacy_source, 0),
                COALESCE(raw.legacy_truncated, 0),
                COALESCE(json_extract(raw.metadata_json, '$.payload_access'), ''),
                CASE
                  WHEN json_extract(raw.metadata_json, '$.migration_origin') IS NOT NULL
                    THEN 1 ELSE 0
                END,
                provenance.output_ordinal,
                provenance.observation_id,
                anchor.anchor_id
         FROM lcm_raw_messages AS raw
         LEFT JOIN observation_projection_provenance AS provenance
           ON provenance.output_provider = raw.provider
          AND provenance.output_message_id = raw.message_id
         LEFT JOIN observations AS observation
           ON observation.observation_id = provenance.observation_id
         LEFT JOIN observation_retrieval_anchors AS anchor
           ON anchor.observation_id = observation.observation_id
         WHERE raw.session_id = ?1
         ORDER BY raw.provider, raw.message_id,
                  COALESCE(provenance.output_ordinal, -1),
                  COALESCE(provenance.observation_id, '')"
    } else {
        "SELECT raw.provider, raw.message_id, raw.content_hash, raw.role,
                COALESCE(raw.timestamp, raw.ordinal, 0),
                COALESCE(raw.snippet_text, raw.content, ''),
                COALESCE(raw.index_text, raw.snippet_text, raw.content, ''),
                COALESCE(raw.legacy_source, 0),
                COALESCE(raw.legacy_truncated, 0),
                COALESCE(json_extract(raw.metadata_json, '$.payload_access'), ''),
                CASE
                  WHEN json_extract(raw.metadata_json, '$.migration_origin') IS NOT NULL
                    THEN 1 ELSE 0
                END,
                CAST(NULL AS INTEGER),
                CAST(NULL AS TEXT),
                CAST(NULL AS TEXT)
         FROM lcm_raw_messages AS raw
         WHERE raw.session_id = ?1
         ORDER BY raw.provider, raw.message_id"
    };

    let mut rows = conn
        .query(sql, params![session_id])
        .await
        .map_err(|error| db_error("load legacy temporal candidate rows", error))?;

    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("load legacy temporal candidate rows", error))?
    {
        let provider: String = row_value!(row, 0, "load legacy temporal candidate rows");
        let message_id: String = row_value!(row, 1, "load legacy temporal candidate rows");
        let content_hash: String = row_value!(row, 2, "load legacy temporal candidate rows");
        let role: String = row_value!(row, 3, "load legacy temporal candidate rows");
        let knowledge_at: i64 = row_value!(row, 4, "load legacy temporal candidate rows");
        let snippet_text: String = row_value!(row, 5, "load legacy temporal candidate rows");
        let index_text: String = row_value!(row, 6, "load legacy temporal candidate rows");
        let legacy_source: i64 = row_value!(row, 7, "load legacy temporal candidate rows");
        let legacy_truncated: i64 = row_value!(row, 8, "load legacy temporal candidate rows");
        let payload_access: String = row_value!(row, 9, "load legacy temporal candidate rows");
        let migration_origin: i64 = row_value!(row, 10, "load legacy temporal candidate rows");
        let output_ordinal: Option<i64> =
            row_value!(row, 11, "load legacy temporal candidate rows");
        let observation_id: Option<String> =
            row_value!(row, 12, "load legacy temporal candidate rows");
        let retrieval_anchor_id: Option<String> =
            row_value!(row, 13, "load legacy temporal candidate rows");

        let ordinal = output_ordinal.unwrap_or(0);
        let dedupe_key = (
            provider.clone(),
            message_id.clone(),
            ordinal,
            observation_id.clone().unwrap_or_default(),
        );
        if !seen.insert(dedupe_key) {
            continue;
        }

        let (disposition, reason) = classify_legacy_row(
            legacy_source != 0,
            legacy_truncated != 0,
            &payload_access,
            migration_origin != 0,
            observation_id.as_deref(),
            retrieval_anchor_id.as_deref(),
            output_ordinal,
        );

        items.push(LegacyCandidateRow {
            provider,
            message_id,
            content_hash,
            role,
            knowledge_at,
            snippet_text,
            index_text,
            output_ordinal: ordinal,
            observation_id,
            retrieval_anchor_id,
            disposition,
            reason,
        });
    }
    Ok(items)
}

fn classify_legacy_row(
    legacy_source: bool,
    legacy_truncated: bool,
    payload_access: &str,
    migration_origin: bool,
    observation_id: Option<&str>,
    retrieval_anchor_id: Option<&str>,
    output_ordinal: Option<i64>,
) -> (LegacyRowDisposition, &'static str) {
    if matches!(payload_access, "quarantined" | "redacted" | "deleted") {
        return (LegacyRowDisposition::Quarantined, "payload_access");
    }
    if migration_origin {
        return (LegacyRowDisposition::Ineligible, "migration_origin");
    }
    if legacy_source || legacy_truncated {
        return (LegacyRowDisposition::PolicyExcluded, "legacy_policy_flag");
    }
    if observation_id.is_none() || retrieval_anchor_id.is_none() || output_ordinal.is_none() {
        return (LegacyRowDisposition::Unbound, "missing_projection_binding");
    }
    (LegacyRowDisposition::Eligible, "importable")
}

fn legacy_source_digest(session_id: &str, items: &[LegacyCandidateRow]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(LEGACY_SOURCE_DIGEST_DOMAIN);
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    for item in items {
        hasher.update(item.provider.as_bytes());
        hasher.update([0]);
        hasher.update(item.message_id.as_bytes());
        hasher.update([0]);
        hasher.update(item.output_ordinal.to_be_bytes());
        hasher.update([0]);
        hasher.update(item.content_hash.as_bytes());
        hasher.update([0]);
        hasher.update(item.role.as_bytes());
        hasher.update([0]);
        hasher.update(item.knowledge_at.to_be_bytes());
        hasher.update([0]);
        hasher.update(item.snippet_text.as_bytes());
        hasher.update([0]);
        hasher.update(item.index_text.as_bytes());
        hasher.update([0]);
        if let Some(observation_id) = &item.observation_id {
            hasher.update(observation_id.as_bytes());
        }
        hasher.update([0]);
        if let Some(anchor_id) = &item.retrieval_anchor_id {
            hasher.update(anchor_id.as_bytes());
        }
        hasher.update([0]);
        hasher.update(item.disposition.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(item.reason.as_bytes());
        hasher.update([0]);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn migration_watermarks_json(items: &[LegacyCandidateRow]) -> String {
    let mut counts = BTreeMap::new();
    for disposition in [
        LegacyRowDisposition::Eligible,
        LegacyRowDisposition::Quarantined,
        LegacyRowDisposition::PolicyExcluded,
        LegacyRowDisposition::Unbound,
        LegacyRowDisposition::Ineligible,
    ] {
        counts.insert(disposition.as_str(), 0_i64);
    }
    for item in items {
        *counts.entry(item.disposition.as_str()).or_insert(0) += 1;
    }
    format!(
        concat!(
            r#"{{"active_generation":1,"cursor_key":null,"#,
            r#""projection_frontier":0,"source_frontier":0,"summary_frontier":0,"#,
            r#""dispositions":{{"eligible":{eligible},"quarantined":{quarantined},"#,
            r#""policy_excluded":{policy_excluded},"unbound":{unbound},"#,
            r#""ineligible":{ineligible}}}}}"#
        ),
        eligible = counts["eligible"],
        quarantined = counts["quarantined"],
        policy_excluded = counts["policy_excluded"],
        unbound = counts["unbound"],
        ineligible = counts["ineligible"],
    )
}

async fn write_migration_dispositions(
    conn: &impl Executor,
    session_id: &str,
    generation: i64,
    batch_ordinal: i64,
    items: &[LegacyCandidateRow],
) -> Result<()> {
    for (disposition_ordinal, item) in items.iter().enumerate() {
        let row_digest = legacy_row_digest(session_id, item);
        let ordinal = i64::try_from(disposition_ordinal).map_err(|error| {
            db_message(
                "write temporal migration disposition",
                format!("disposition ordinal overflow: {error}"),
            )
        })?;
        conn.execute(
            "INSERT INTO session_temporal_migration_dispositions(
                 session_id, generation, batch_ordinal, disposition_ordinal,
                 provider, message_id, output_ordinal, observation_id,
                 retrieval_anchor_id, disposition, reason, row_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                session_id,
                generation,
                batch_ordinal,
                ordinal,
                item.provider.clone(),
                item.message_id.clone(),
                item.output_ordinal,
                item.observation_id.clone(),
                item.retrieval_anchor_id.clone(),
                item.disposition.as_str(),
                item.reason,
                row_digest
            ],
        )
        .await
        .map_err(|error| db_error("write temporal migration disposition", error))?;
    }
    Ok(())
}

fn legacy_row_digest(session_id: &str, item: &LegacyCandidateRow) -> String {
    let mut hasher = Sha256::new();
    hasher.update(LEGACY_SOURCE_DIGEST_DOMAIN);
    hasher.update(b"row\0");
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    hasher.update(item.provider.as_bytes());
    hasher.update([0]);
    hasher.update(item.message_id.as_bytes());
    hasher.update([0]);
    hasher.update(item.output_ordinal.to_be_bytes());
    hasher.update([0]);
    hasher.update(item.content_hash.as_bytes());
    hasher.update([0]);
    hasher.update(item.role.as_bytes());
    hasher.update([0]);
    hasher.update(item.knowledge_at.to_be_bytes());
    hasher.update([0]);
    hasher.update(item.snippet_text.as_bytes());
    hasher.update([0]);
    hasher.update(item.index_text.as_bytes());
    hasher.update([0]);
    if let Some(observation_id) = &item.observation_id {
        hasher.update(observation_id.as_bytes());
    }
    hasher.update([0]);
    if let Some(anchor_id) = &item.retrieval_anchor_id {
        hasher.update(anchor_id.as_bytes());
    }
    hasher.update([0]);
    hasher.update(item.disposition.as_str().as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

async fn migration_receipt_covers(
    conn: &impl Executor,
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

async fn ensure_migration_generation(conn: &impl Executor, session_id: &str) -> Result<i64> {
    if let Some(generation) = active_or_building_generation(conn, session_id).await? {
        return Ok(generation);
    }

    let generation = next_generation_number(conn, session_id).await?;
    let created_at = query_i64(
        conn,
        "SELECT CAST(strftime('%s','now') AS INTEGER) * 1000000",
    )
    .await?;
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

async fn active_or_building_generation(
    conn: &impl Executor,
    session_id: &str,
) -> Result<Option<i64>> {
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

async fn next_generation_number(conn: &impl Executor, session_id: &str) -> Result<i64> {
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
    conn: &impl Executor,
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
    conn: &impl Executor,
    session_id: &str,
    generation: i64,
    item: &LegacyCandidateRow,
) -> Result<i64> {
    let observation_id_text = item.observation_id.as_deref().ok_or_else(|| {
        db_message(
            "import legacy temporal occurrence",
            "eligible row missing observation identity",
        )
    })?;
    let retrieval_anchor_id = item.retrieval_anchor_id.as_deref().ok_or_else(|| {
        db_message(
            "import legacy temporal occurrence",
            "eligible row missing retrieval anchor",
        )
    })?;
    let observation_id =
        CanonicalObservationIdV1::new(observation_id_text.to_string()).map_err(|error| {
            db_message(
                "import legacy temporal occurrence",
                format!("invalid observation identity: {error}"),
            )
        })?;
    let output_ordinal = u32::try_from(item.output_ordinal).map_err(|error| {
        db_message(
            "import legacy temporal occurrence",
            format!("invalid projection output ordinal: {error}"),
        )
    })?;
    let occurrence_id = MessageOccurrenceIdV1::derive(
        &observation_id,
        ProjectionOutputOrdinalV1::new(output_ordinal),
    );

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
            r#""source_anchor_id":"{}","provider":"{}","message_id":"{}","#,
            r#""output_ordinal":{}}}"#
        ),
        retrieval_anchor_id, item.provider, item.message_id, item.output_ordinal
    );

    conn.execute(
        "INSERT INTO session_occurrences(
             session_id, generation, occurrence_id, source_observation_id,
             projection_output_ordinal, retrieval_anchor_id, message_id,
             role, knowledge_at, valid_time_json, evidence_json,
             snippet_text, index_text
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7,
             ?8, ?9, ?10, ?11, ?12, ?13
         )",
        params![
            session_id,
            generation,
            occurrence_id.as_str(),
            observation_id_text,
            item.output_ordinal,
            retrieval_anchor_id,
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

    Ok(1)
}

async fn rebuild_migrated_current_entities(conn: &impl Executor) -> Result<()> {
    if !main_table_exists(conn, "session_occurrences").await?
        || !main_table_exists(conn, "session_current_entities").await?
    {
        return Ok(());
    }

    let mut rows = conn
        .query(
            "SELECT DISTINCT session_id, generation
             FROM session_occurrences
             ORDER BY session_id, generation",
            (),
        )
        .await
        .map_err(|error| db_error("list temporal generations for current rebuild", error))?;
    let mut scopes: Vec<(String, i64)> = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error("list temporal generations for current rebuild", error))?
    {
        scopes.push((
            row_value!(row, 0, "list temporal generations for current rebuild"),
            row_value!(row, 1, "list temporal generations for current rebuild"),
        ));
    }

    for (session_id, generation) in scopes {
        conn.execute(
            "DELETE FROM session_current_entities
             WHERE session_id = ?1 AND generation = ?2 AND entity_kind = 'occurrence_anchor'",
            params![session_id.clone(), generation],
        )
        .await
        .map_err(|error| db_error("rebuild temporal current occurrence entities", error))?;
        conn.execute(
            "WITH ranked AS (
                SELECT retrieval_anchor_id, occurrence_id,
                       COUNT(*) OVER (PARTITION BY retrieval_anchor_id) AS occurrence_count,
                       ROW_NUMBER() OVER (
                           PARTITION BY retrieval_anchor_id
                           ORDER BY
                               CASE json_extract(valid_time_json, '$.kind')
                                   WHEN 'known' THEN 1 ELSE 0
                               END DESC,
                               json_extract(valid_time_json, '$.valid_at') DESC,
                               knowledge_at DESC,
                               projection_output_ordinal DESC,
                               occurrence_id DESC
                       ) AS precedence
                FROM session_occurrences
                WHERE session_id = ?1 AND generation = ?2
             )
             INSERT INTO session_current_entities (
                session_id, generation, entity_kind, entity_id,
                current_assertion_id, current_occurrence_id, coverage_json
             )
             SELECT ?1, ?2, 'occurrence_anchor', retrieval_anchor_id,
                    NULL, occurrence_id,
                    json_object(
                        'occurrence_count', occurrence_count,
                        'source', 'legacy_migration_rebuild'
                    )
             FROM ranked WHERE precedence = 1",
            params![session_id.clone(), generation],
        )
        .await
        .map_err(|error| db_error("rebuild temporal current occurrence entities", error))?;

        if main_table_exists(conn, "session_assertions").await?
            && main_table_exists(conn, "session_assertion_supersession").await?
        {
            conn.execute(
                "DELETE FROM session_current_entities
                 WHERE session_id = ?1 AND generation = ?2 AND entity_kind = 'assertion_anchor'",
                params![session_id.clone(), generation],
            )
            .await
            .map_err(|error| db_error("rebuild temporal current assertion entities", error))?;
            conn.execute(
                "WITH superseded AS (
                    SELECT DISTINCT superseded_assertion_id AS assertion_id
                    FROM session_assertion_supersession
                    WHERE session_id = ?1 AND generation = ?2
                 ),
                 tips AS (
                    SELECT assertion.assertion_id, assertion.subject_anchor_id AS entity_id,
                           COUNT(*) OVER (PARTITION BY assertion.subject_anchor_id)
                             AS assertion_count,
                           ROW_NUMBER() OVER (
                               PARTITION BY assertion.subject_anchor_id
                               ORDER BY assertion.knowledge_at DESC, assertion.assertion_id DESC
                           ) AS precedence
                    FROM session_assertions AS assertion
                    LEFT JOIN superseded
                      ON superseded.assertion_id = assertion.assertion_id
                    WHERE assertion.session_id = ?1
                      AND assertion.generation = ?2
                      AND superseded.assertion_id IS NULL
                 )
                 INSERT INTO session_current_entities (
                    session_id, generation, entity_kind, entity_id,
                    current_assertion_id, current_occurrence_id, coverage_json
                 )
                 SELECT ?1, ?2, 'assertion_anchor', entity_id,
                        assertion_id, NULL,
                        json_object(
                            'assertion_count', assertion_count,
                            'source', 'legacy_migration_rebuild'
                        )
                 FROM tips WHERE precedence = 1",
                params![session_id, generation],
            )
            .await
            .map_err(|error| db_error("rebuild temporal current assertion entities", error))?;
        }
    }
    Ok(())
}

async fn parity_check_temporal_fts_derivatives(conn: &impl Executor) -> Result<()> {
    if !main_table_exists(conn, "session_occurrences").await?
        || !main_table_exists(conn, "session_occurrences_fts").await?
    {
        return Ok(());
    }

    let missing = query_i64(
        conn,
        "SELECT COUNT(*)
         FROM session_occurrences AS occurrence
         WHERE NOT EXISTS (
             SELECT 1
             FROM session_occurrences_fts AS fts
             WHERE fts.rowid = occurrence.rowid
         )",
    )
    .await?;
    if missing != 0 {
        conn.execute(
            "INSERT INTO session_occurrences_fts(session_occurrences_fts)
             VALUES('rebuild')",
            (),
        )
        .await
        .map_err(|error| db_error("rebuild temporal occurrence FTS", error))?;
    }

    let mismatched = query_i64(
        conn,
        "SELECT COUNT(*)
         FROM session_occurrences AS occurrence
         JOIN session_occurrences_fts AS fts ON fts.rowid = occurrence.rowid
         WHERE fts.index_text IS NOT occurrence.index_text
            OR fts.snippet_text IS NOT occurrence.snippet_text",
    )
    .await?;
    if mismatched != 0 {
        return Err(db_message(
            "merge_temporal_authority",
            "temporal occurrence FTS parity mismatch after rebuild",
        ));
    }

    if main_table_exists(conn, "session_temporal_generations").await?
        && main_table_exists(conn, "session_temporal_migration_receipts").await?
    {
        // Migration receipts must bind a generation that completed the legal
        // lifecycle into active/ready/completed — never a stranded building row.
        let stranded = query_i64(
            conn,
            "SELECT COUNT(*)
             FROM session_temporal_migration_receipts AS receipt
             JOIN session_temporal_generations AS generation
               ON generation.session_id = receipt.session_id
              AND generation.generation = receipt.generation
             WHERE generation.state IN ('failed', 'cancelled')",
        )
        .await?;
        if stranded != 0 {
            return Err(db_message(
                "merge_temporal_authority",
                "migration receipt bound to terminal-failed generation lifecycle",
            ));
        }
    }
    Ok(())
}

/// Executable PR19 deletion gate: every legacy raw row has an explicit
/// disposition receipt, and no eligible legacy row remains without a covering
/// temporal occurrence + migration receipt.
pub(super) async fn assert_zero_legacy_temporal_authority(conn: &impl Executor) -> Result<()> {
    if !main_table_exists(conn, "lcm_raw_messages").await? {
        return Ok(());
    }
    if !main_table_exists(conn, "session_temporal_migration_dispositions").await? {
        return Err(db_message(
            "pr19_zero_legacy_temporal_authority",
            "migration disposition receipts table is missing",
        ));
    }

    let undisposed = query_i64(
        conn,
        "SELECT COUNT(*)
         FROM lcm_raw_messages AS raw
         WHERE NOT EXISTS (
             SELECT 1
             FROM session_temporal_migration_dispositions AS disposition
             WHERE disposition.provider = raw.provider
               AND disposition.message_id = raw.message_id
               AND disposition.session_id = raw.session_id
         )",
    )
    .await?;
    if undisposed != 0 {
        return Err(db_message(
            "pr19_zero_legacy_temporal_authority",
            "legacy rows lack explicit migration dispositions",
        ));
    }

    let uncovered_eligible = query_i64(
        conn,
        "SELECT COUNT(*)
         FROM session_temporal_migration_dispositions AS disposition
         WHERE disposition.disposition = 'eligible'
           AND (
               NOT EXISTS (
                   SELECT 1
                   FROM session_occurrences AS occurrence
                   WHERE occurrence.session_id = disposition.session_id
                     AND occurrence.source_observation_id = disposition.observation_id
                     AND occurrence.projection_output_ordinal = disposition.output_ordinal
               )
               OR NOT EXISTS (
                   SELECT 1
                   FROM session_temporal_migration_receipts AS receipt
                   WHERE receipt.session_id = disposition.session_id
                     AND receipt.generation = disposition.generation
                     AND receipt.batch_ordinal = disposition.batch_ordinal
               )
           )",
    )
    .await?;
    if uncovered_eligible != 0 {
        return Err(db_message(
            "pr19_zero_legacy_temporal_authority",
            "eligible legacy rows lack temporal coverage receipts",
        ));
    }

    let skipped_imported = query_i64(
        conn,
        "SELECT COUNT(*)
         FROM session_temporal_migration_dispositions AS disposition
         JOIN session_occurrences AS occurrence
           ON occurrence.session_id = disposition.session_id
          AND occurrence.source_observation_id = disposition.observation_id
          AND occurrence.projection_output_ordinal = disposition.output_ordinal
         WHERE disposition.disposition IN (
             'quarantined', 'policy_excluded', 'unbound', 'ineligible'
         )",
    )
    .await?;
    if skipped_imported != 0 {
        return Err(db_message(
            "pr19_zero_legacy_temporal_authority",
            "non-eligible legacy dispositions were imported into temporal sinks",
        ));
    }
    Ok(())
}

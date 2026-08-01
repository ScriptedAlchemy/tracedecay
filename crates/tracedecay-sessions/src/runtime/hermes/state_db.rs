//! Bounded `state.db` access: schema probing, byte-capped page reads, and the
//! per-source ingest drivers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tracedecay_domain::{ObservationScopeV1, ObservationSourceGenerationV1, ProjectId};

use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::ingest_byte_budget::IngestByteBudget;
use crate::runtime::shared::{
    ProjectRootMatcher, SqliteReadConn, StoredCursor, TranscriptIngestStats,
};

use super::coverage::{
    admit_rows_with_admission, admit_rows_with_admission_and_cancellation, sqlite_incarnation,
};
use super::ingest::{HermesProfileSource, ProjectIngestDestination};
use super::observation::{HermesProjectionMetadata, project_projection_metadata};
use super::routing::{
    turn_project_locations, turn_project_locations_for_destinations, user_turn_locations,
};
use super::rows::{HermesPageRead, HermesRow, hermes_budget_bytes, hermes_page_row_charge};
use super::{CHUNK_ROWS, MAX_HERMES_IDENTITY_BYTES, MAX_HERMES_PAGE_BYTES, MAX_HERMES_VALUE_BYTES};

/// Column names of the `messages` table — `active` (v12 rewind soft-delete)
/// and `reasoning` arrived in later Hermes schema revisions, so the sweep
/// probes before selecting to stay readable on legacy stores.
pub async fn message_columns(
    conn: &SqliteReadConn,
) -> Result<std::collections::BTreeSet<String>, String> {
    table_columns(conn, "messages").await
}

pub async fn table_columns(
    conn: &SqliteReadConn,
    table: &str,
) -> Result<std::collections::BTreeSet<String>, String> {
    let table = table.to_string();
    conn.with(move |conn| table_columns_sync(conn, &table))
        .await
        .unwrap_or_else(|| Err("could not inspect Hermes SQLite schema".to_string()))
}

fn table_columns_sync(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<std::collections::BTreeSet<String>, String> {
    let mut out = std::collections::BTreeSet::new();
    let query = format!("SELECT name FROM pragma_table_info('{table}')");
    let mut statement = conn
        .prepare(&query)
        .map_err(|_| "could not inspect Hermes SQLite schema".to_string())?;
    let mut rows = statement
        .query(())
        .map_err(|_| "could not inspect Hermes SQLite schema".to_string())?;
    while let Some(row) = rows
        .next()
        .map_err(|_| "could not read Hermes SQLite schema".to_string())?
    {
        let name = row
            .get::<_, String>(0)
            .map_err(|_| "Hermes SQLite schema row is malformed".to_string())?;
        out.insert(name);
    }
    if out.is_empty() {
        return Err("Hermes SQLite authority is incomplete".to_string());
    }
    Ok(out)
}

fn validate_required_columns(
    table: &str,
    columns: &std::collections::BTreeSet<String>,
    required: &[&str],
) -> Result<(), String> {
    let missing = required
        .iter()
        .copied()
        .filter(|column| !columns.contains(*column))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Hermes SQLite table '{table}' is missing required columns: {}",
            missing.join(", ")
        ))
    }
}

#[cfg(test)]
pub async fn validate_required_schema(conn: &SqliteReadConn) -> Result<(), String> {
    let messages = message_columns(conn).await?;
    let sessions = table_columns(conn, "sessions").await?;
    validate_required_columns(
        "messages",
        &messages,
        &["id", "session_id", "role", "content", "timestamp"],
    )?;
    validate_required_columns("sessions", &sessions, &["id"])
}

fn sql_byte_len(expr: &str) -> String {
    format!("length(CAST({expr} AS BLOB))")
}

/// Returns the column only when it is TEXT, within `max_bytes`, and the whole
/// row fits the current cumulative page budget. Hostile BLOB/`zeroblob`
/// values and rows deferred to the next page never appear in the result set.
fn sql_bounded_text(expr: &str, max_bytes: usize, row_fits_budget: &str) -> String {
    let byte_len = sql_byte_len(expr);
    format!(
        "CASE WHEN ({row_fits_budget}) AND typeof({expr}) = 'text' \
              AND {byte_len} <= {max_bytes} THEN {expr} ELSE NULL END"
    )
}

/// Returns only `SQLite`'s fixed-size numeric representations. `SQLite` columns
/// are dynamically typed, so selecting a nominal REAL/INTEGER column directly
/// could otherwise materialize an attacker-controlled TEXT/BLOB value.
fn sql_bounded_number(expr: &str) -> String {
    format!("CASE WHEN typeof({expr}) IN ('integer', 'real') THEN {expr} ELSE NULL END")
}

/// SQL UTF-8/blob byte charge without returning the value. Caps each column at
/// `max_bytes + 1` so oversized/zeroblob sizes cannot inflate page accounting
/// unboundedly while still signaling oversize.
fn sql_capped_len(expr: &str, max_bytes: usize) -> String {
    let cap = max_bytes.saturating_add(1);
    let byte_len = sql_byte_len(expr);
    format!(
        "CASE
            WHEN {expr} IS NULL THEN 0
            WHEN typeof({expr}) IN ('text', 'blob') AND {byte_len} > {max_bytes} THEN {cap}
            WHEN typeof({expr}) IN ('text', 'blob') THEN {byte_len}
            WHEN typeof({expr}) IN ('integer', 'real') THEN length(CAST({expr} AS BLOB))
            ELSE {cap}
         END"
    )
}

fn sql_value_oversized(expr: &str, max_bytes: usize) -> String {
    let byte_len = sql_byte_len(expr);
    format!(
        "CASE
            WHEN {expr} IS NULL THEN 0
            WHEN typeof({expr}) = 'text' AND {byte_len} <= {max_bytes} THEN 0
            ELSE 1
         END"
    )
}

pub fn select_new_messages_sql(
    message_columns: &std::collections::BTreeSet<String>,
    session_columns: &std::collections::BTreeSet<String>,
) -> String {
    let reasoning_raw = if message_columns.contains("reasoning") {
        "m.reasoning"
    } else {
        "NULL"
    };
    let active_expr = if message_columns.contains("active") {
        "m.active"
    } else {
        "1"
    };
    let tool_name_raw = if message_columns.contains("tool_name") {
        "m.tool_name"
    } else {
        "NULL"
    };
    let tool_calls_raw = if message_columns.contains("tool_calls") {
        "m.tool_calls"
    } else {
        "NULL"
    };
    let session_model_raw = if session_columns.contains("model") {
        "s.model"
    } else {
        "NULL"
    };
    let parent_session_id_raw = if session_columns.contains("parent_session_id") {
        "s.parent_session_id"
    } else {
        "NULL"
    };
    let session_cwd_raw = if session_columns.contains("cwd") {
        "s.cwd"
    } else {
        "NULL"
    };
    let session_source_raw = if session_columns.contains("source") {
        "s.source"
    } else {
        "NULL"
    };
    let session_title_raw = if session_columns.contains("title") {
        "s.title"
    } else {
        "NULL"
    };
    let session_started_at = if session_columns.contains("started_at") {
        "s.started_at"
    } else {
        "NULL"
    };
    let session_ended_at = if session_columns.contains("ended_at") {
        "s.ended_at"
    } else {
        "NULL"
    };
    let input_tokens_raw = if session_columns.contains("input_tokens") {
        "s.input_tokens"
    } else {
        "NULL"
    };
    let output_tokens_raw = if session_columns.contains("output_tokens") {
        "s.output_tokens"
    } else {
        "NULL"
    };
    let cache_read_tokens_raw = if session_columns.contains("cache_read_tokens") {
        "s.cache_read_tokens"
    } else {
        "NULL"
    };
    let cache_write_tokens_raw = if session_columns.contains("cache_write_tokens") {
        "s.cache_write_tokens"
    } else {
        "NULL"
    };
    let reasoning_tokens_raw = if session_columns.contains("reasoning_tokens") {
        "s.reasoning_tokens"
    } else {
        "NULL"
    };
    let id_max = MAX_HERMES_IDENTITY_BYTES;
    let value_max = MAX_HERMES_VALUE_BYTES;
    let measured = format!(
        "{session_id_len} + {role_len} + {content_len} + {reasoning_len} + {tool_name_len} + {tool_calls_len} + {model_len} + {parent_len} + {cwd_len} + {source_len} + {title_len}",
        session_id_len = sql_capped_len("m.session_id", id_max),
        role_len = sql_capped_len("m.role", id_max),
        content_len = sql_capped_len("m.content", value_max),
        reasoning_len = sql_capped_len(reasoning_raw, value_max),
        tool_name_len = sql_capped_len(tool_name_raw, id_max),
        tool_calls_len = sql_capped_len(tool_calls_raw, value_max),
        model_len = sql_capped_len(session_model_raw, id_max),
        parent_len = sql_capped_len(parent_session_id_raw, id_max),
        cwd_len = sql_capped_len(session_cwd_raw, value_max),
        source_len = sql_capped_len(session_source_raw, id_max),
        title_len = sql_capped_len(session_title_raw, value_max),
    );
    let oversized = format!(
        "CASE WHEN ({session_id_os} + {role_os} + {content_os} + {reasoning_os} + {tool_name_os} + {tool_calls_os} + {model_os} + {parent_os} + {cwd_os} + {source_os} + {title_os}) > 0 THEN 1 ELSE 0 END",
        session_id_os = sql_value_oversized("m.session_id", id_max),
        role_os = sql_value_oversized("m.role", id_max),
        content_os = sql_value_oversized("m.content", value_max),
        reasoning_os = sql_value_oversized(reasoning_raw, value_max),
        tool_name_os = sql_value_oversized(tool_name_raw, id_max),
        tool_calls_os = sql_value_oversized(tool_calls_raw, value_max),
        model_os = sql_value_oversized(session_model_raw, id_max),
        parent_os = sql_value_oversized(parent_session_id_raw, id_max),
        cwd_os = sql_value_oversized(session_cwd_raw, value_max),
        source_os = sql_value_oversized(session_source_raw, id_max),
        title_os = sql_value_oversized(session_title_raw, value_max),
    );
    let row_fits_budget = format!("({measured}) <= ?2");
    let session_id = sql_bounded_text("m.session_id", id_max, &row_fits_budget);
    let role = sql_bounded_text("m.role", id_max, &row_fits_budget);
    let content = sql_bounded_text("m.content", value_max, &row_fits_budget);
    let reasoning = sql_bounded_text(reasoning_raw, value_max, &row_fits_budget);
    let tool_name = sql_bounded_text(tool_name_raw, id_max, &row_fits_budget);
    let tool_calls = sql_bounded_text(tool_calls_raw, value_max, &row_fits_budget);
    let model = sql_bounded_text(session_model_raw, id_max, &row_fits_budget);
    let parent_session_id = sql_bounded_text(parent_session_id_raw, id_max, &row_fits_budget);
    let session_cwd = sql_bounded_text(session_cwd_raw, value_max, &row_fits_budget);
    let session_source = sql_bounded_text(session_source_raw, id_max, &row_fits_budget);
    let session_title = sql_bounded_text(session_title_raw, value_max, &row_fits_budget);
    let timestamp = sql_bounded_number("m.timestamp");
    let session_started_at = sql_bounded_number(session_started_at);
    let session_ended_at = sql_bounded_number(session_ended_at);
    let input_tokens = sql_bounded_number(input_tokens_raw);
    let output_tokens = sql_bounded_number(output_tokens_raw);
    let cache_read_tokens = sql_bounded_number(cache_read_tokens_raw);
    let cache_write_tokens = sql_bounded_number(cache_write_tokens_raw);
    let reasoning_tokens = sql_bounded_number(reasoning_tokens_raw);
    let active = sql_bounded_number(active_expr);
    let typed_oversized = format!(
        "CASE WHEN ({oversized}) > 0 OR ({measured}) > {MAX_HERMES_PAGE_BYTES} \
              THEN 1 ELSE 0 END"
    );
    format!(
        "SELECT m.id,
                {session_id},
                {role},
                {content},
                {reasoning},
                {tool_name},
                {tool_calls},
                {timestamp},
                {model},
                {parent_session_id},
                {session_cwd},
                {session_source},
                {session_title},
                {session_started_at},
                {session_ended_at},
                {input_tokens}, {output_tokens}, {cache_read_tokens}, {cache_write_tokens},
                {reasoning_tokens}, {active},
                CAST(({measured}) AS INTEGER) AS measured_bytes,
                CAST(({typed_oversized}) AS INTEGER) AS value_oversized,
                CAST(({row_fits_budget}) AS INTEGER) AS row_fits_budget
         FROM messages m LEFT JOIN sessions s ON s.id = m.session_id
         WHERE m.id > ?1
         ORDER BY m.id
         LIMIT 1"
    )
}

/// Incrementally scans one Hermes `state.db`; each bounded page is admitted
/// against its session-scoped authoritative SQLite-row cursor. The caller
/// decides whether a source error is runtime noise or migration-blocking.
/// Opens a Hermes `state.db` read-only and derives everything a page sweep
/// needs before its first read: the physical incarnation and the column-probed
/// bounded SELECT.
async fn open_state_source(
    source: &HermesProfileSource,
) -> Result<
    (
        SqliteReadConn,
        ObservationSourceGenerationV1,
        u64,
        u64,
        String,
    ),
    String,
> {
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let (generation, file_identity, resume_fingerprint) = sqlite_incarnation(state_db)?;
    let message_columns = message_columns(&conn).await?;
    let session_columns = table_columns(&conn, "sessions").await?;
    validate_required_columns(
        "messages",
        &message_columns,
        &["id", "session_id", "role", "content", "timestamp"],
    )?;
    validate_required_columns("sessions", &session_columns, &["id"])?;
    let select_sql = select_new_messages_sql(&message_columns, &session_columns);
    Ok((
        conn,
        generation,
        file_identity,
        resume_fingerprint,
        select_sql,
    ))
}

/// Drives one single-destination bounded page sweep. Each page is truncated to
/// the shared byte budget, routed by `route_page`, and admitted against the
/// destination's authoritative SQLite-row cursor.
#[allow(clippy::too_many_arguments)]
async fn ingest_bounded_pages<F, R>(
    admission: &dyn HostAdmission,
    conn: &SqliteReadConn,
    select_sql: &str,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    resume_fingerprint: u64,
    budget: &mut IngestByteBudget,
    mut route_page: F,
    cancellation: &ObservationCancellation,
) -> Result<TranscriptIngestStats, String>
where
    F: FnMut(&[HermesRow]) -> R,
    R: Fn(&HermesRow) -> Option<HermesProjectionMetadata>,
{
    let mut read_cursor = StoredCursor::default();
    let mut stats = TranscriptIngestStats::default();
    loop {
        if cancellation.is_cancelled() {
            return Ok(stats);
        }
        let new = read_new_rows_strict(conn, select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        let bounded_count = new
            .items
            .iter()
            .take_while(|row| budget.try_consume(hermes_budget_bytes(row)))
            .count();
        if bounded_count == 0 {
            return Ok(stats);
        }
        let bounded = &new.items[..bounded_count];
        let route = route_page(bounded);
        let admitted = admit_rows_with_admission_and_cancellation(
            admission,
            bounded,
            scope.clone(),
            generation,
            file_identity,
            resume_fingerprint,
            route,
            cancellation,
        )
        .await?;
        stats.messages_upserted = stats
            .messages_upserted
            .saturating_add(admitted.messages_upserted);
        stats.sessions_upserted = stats
            .sessions_upserted
            .saturating_add(admitted.sessions_upserted);
        read_cursor.position = bounded
            .last()
            .and_then(|row| u64::try_from(row.id).ok())
            .unwrap_or(read_cursor.position);
        if bounded_count < row_count {
            return Ok(stats);
        }
        if new.truncated_by_byte_budget {
            continue;
        }
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

pub(super) async fn try_ingest_state_db_bounded_with_admission(
    source: &HermesProfileSource,
    project_root: &Path,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    budget: &mut IngestByteBudget,
    cancellation: &ObservationCancellation,
) -> Result<TranscriptIngestStats, String> {
    if cancellation.is_cancelled() {
        return Ok(TranscriptIngestStats::default());
    }
    let (conn, generation, file_identity, resume_fingerprint, select_sql) =
        open_state_source(source).await?;
    let scope = ObservationScopeV1::Project { project_id };
    ingest_bounded_pages(
        admission,
        &conn,
        &select_sql,
        scope,
        generation,
        file_identity,
        resume_fingerprint,
        budget,
        |bounded| {
            let locations = turn_project_locations(bounded, project_root, source);
            move |row: &HermesRow| {
                locations.get(&row.id).copied().map(|provenance| {
                    project_projection_metadata(row, source, project_root, provenance)
                })
            }
        },
        cancellation,
    )
    .await
}

/// Shared-source equivalent of [`try_ingest_state_db`]. The `SQLite` page is read
/// once, then each destination independently admits routed rows against its own
/// authoritative observation cursor.
pub(super) async fn try_ingest_state_db_for_projects(
    source: &HermesProfileSource,
    destinations: &[ProjectIngestDestination<'_>],
    budget: &mut IngestByteBudget,
) -> Result<TranscriptIngestStats, String> {
    let (conn, generation, file_identity, resume_fingerprint, select_sql) =
        open_state_source(source).await?;
    let scopes = destinations
        .iter()
        .map(|destination| ObservationScopeV1::Project {
            project_id: destination.project_id.clone(),
        })
        .collect::<Vec<_>>();
    let destination_matchers = destinations
        .par_iter()
        .map(|destination| ProjectRootMatcher::new(destination.project_root))
        .collect::<Vec<_>>();
    let mut read_cursor = StoredCursor::default();
    let mut stats = TranscriptIngestStats::default();
    loop {
        let new = read_new_rows_strict(&conn, &select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        let bounded_count = new
            .items
            .iter()
            .take_while(|row| budget.try_consume(hermes_budget_bytes(row)))
            .count();
        if bounded_count == 0 {
            return Ok(stats);
        }
        let bounded = &new.items[..bounded_count];
        // Per-page route cache: avoid unbounded growth across many SQLite pages.
        let mut destination_routes = HashMap::<PathBuf, Vec<usize>>::new();
        let locations = turn_project_locations_for_destinations(
            bounded,
            &destination_matchers,
            source,
            &mut destination_routes,
        );
        for (index, destination) in destinations.iter().enumerate() {
            let admitted = admit_rows_with_admission(
                destination.admission,
                bounded,
                scopes[index].clone(),
                generation,
                file_identity,
                resume_fingerprint,
                |row| {
                    locations[index]
                        .by_row_id
                        .get(&row.id)
                        .copied()
                        .map(|provenance| {
                            project_projection_metadata(
                                row,
                                source,
                                destination.project_root,
                                provenance,
                            )
                        })
                },
            )
            .await?;
            stats.messages_upserted = stats
                .messages_upserted
                .saturating_add(admitted.messages_upserted);
            stats.sessions_upserted = stats
                .sessions_upserted
                .saturating_add(admitted.sessions_upserted);
        }
        read_cursor.position = bounded
            .last()
            .and_then(|row| u64::try_from(row.id).ok())
            .unwrap_or(read_cursor.position);
        if bounded_count < row_count {
            return Ok(stats);
        }
        if new.truncated_by_byte_budget {
            continue;
        }
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

pub(super) async fn try_ingest_user_state_db_bounded_with_admission(
    admission: &dyn HostAdmission,
    source: &HermesProfileSource,
    _registered_roots: &[PathBuf],
    budget: &mut IngestByteBudget,
    cancellation: &ObservationCancellation,
) -> Result<TranscriptIngestStats, String> {
    if cancellation.is_cancelled() {
        return Ok(TranscriptIngestStats::default());
    }
    let (conn, generation, file_identity, resume_fingerprint, select_sql) =
        open_state_source(source).await?;
    ingest_bounded_pages(
        admission,
        &conn,
        &select_sql,
        ObservationScopeV1::Profile,
        generation,
        file_identity,
        resume_fingerprint,
        budget,
        |bounded| {
            let locations = user_turn_locations(bounded, source);
            let profile = source.profile.clone();
            let fallback_provenance = source
                .legacy_project_pin
                .as_ref()
                .map_or("session_cwd", |_| "profile_pin");
            move |row: &HermesRow| {
                locations
                    .contains(&row.id)
                    .then(|| HermesProjectionMetadata {
                        project_path: None,
                        location_path: None,
                        profile: profile.clone(),
                        location_provenance: Some(fallback_provenance),
                    })
            }
        },
        cancellation,
    )
    .await
}

/// Opens a Hermes `state.db` strictly read-only so the sweep can never write
/// to (or create) another agent's live store.
pub async fn open_read_only_strict(path: &Path) -> Result<SqliteReadConn, String> {
    let owned = path.to_path_buf();
    let opened = tokio::task::spawn_blocking(move || {
        tracedecay_rusqlite_runtime::open_immutable_reader(&owned)
    })
    .await
    .map_err(|error| format!("could not open '{}' read-only: {error}", path.display()))?;
    opened
        .map(SqliteReadConn::new)
        .map_err(|error| format!("could not open '{}' read-only: {error}", path.display()))
}

pub(super) async fn read_new_rows_strict(
    conn: &SqliteReadConn,
    select_sql: &str,
    prev: StoredCursor,
) -> Result<HermesPageRead, String> {
    let select_sql = select_sql.to_string();
    conn.with(move |conn| read_new_rows_strict_sync(conn, &select_sql, prev))
        .await
        .unwrap_or_else(|| Err("could not query legacy Hermes state rows".to_string()))
}

fn read_new_rows_strict_sync(
    conn: &rusqlite::Connection,
    select_sql: &str,
    prev: StoredCursor,
) -> Result<HermesPageRead, String> {
    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    let mut page_bytes = 0_u64;
    let mut truncated_by_byte_budget = false;
    while items.len() < CHUNK_ROWS {
        let remaining = MAX_HERMES_PAGE_BYTES.saturating_sub(page_bytes);
        let mut statement = conn
            .prepare(select_sql)
            .map_err(|error| format!("could not query legacy Hermes state rows: {error}"))?;
        let mut rows = statement
            .query(rusqlite::params![max_rowid as i64, remaining as i64])
            .map_err(|error| format!("could not query legacy Hermes state rows: {error}"))?;
        let row = rows
            .next()
            .map_err(|error| format!("could not read legacy Hermes state row: {error}"))?;
        let Some(row) = row else {
            break;
        };
        let rowid = row
            .get::<_, i64>(0)
            .map_err(|error| format!("legacy Hermes state row has no id: {error}"))?;
        // Columns 21..23 are SQL byte/typeof/budget aggregates — integers only.
        let measured = row_i64_flag(row, 21).max(0) as u64;
        let charge = hermes_page_row_charge(measured);
        let row_fits_budget = row_i64_flag(row, 23) != 0;
        if !row_fits_budget && !items.is_empty() {
            // SQL returned NULL for every text payload in this row, so defer it
            // without allocating the value that would cross the page budget.
            truncated_by_byte_budget = true;
            break;
        }
        let mapped = map_row(rowid, row, measured)
            .ok_or_else(|| format!("legacy Hermes state row {rowid} is malformed"))?;
        page_bytes = page_bytes.saturating_add(charge);
        max_rowid = max_rowid.max(rowid as u64);
        items.push(mapped);
        if page_bytes >= MAX_HERMES_PAGE_BYTES {
            truncated_by_byte_budget = true;
            break;
        }
    }
    if items.len() >= CHUNK_ROWS {
        truncated_by_byte_budget = true;
    }
    Ok(HermesPageRead {
        items,
        #[cfg(test)]
        new_cursor: StoredCursor {
            position: max_rowid,
            mtime: 0,
            file_id: 0,
        },
        truncated_by_byte_budget,
    })
}

fn row_i64_flag(row: &rusqlite::Row<'_>, idx: usize) -> i64 {
    row.get::<_, i64>(idx)
        .or_else(|_| {
            row.get::<_, Option<i64>>(idx)
                .map(|value| value.unwrap_or(0))
        })
        .or_else(|_| row.get::<_, f64>(idx).map(|value| value as i64))
        .unwrap_or(0)
}

fn row_optional_f64(row: &rusqlite::Row<'_>, idx: usize) -> Option<f64> {
    row.get::<_, Option<f64>>(idx).ok().flatten().or_else(|| {
        row.get::<_, Option<i64>>(idx)
            .ok()
            .flatten()
            .map(|value| value as f64)
    })
}

fn map_row(rowid: i64, row: &rusqlite::Row<'_>, sql_measured_bytes: u64) -> Option<HermesRow> {
    let sql_value_oversized = row_i64_flag(row, 22) != 0;
    let session_id = match row.get::<_, Option<String>>(1).ok().flatten() {
        Some(id) if !id.is_empty() => id,
        // Rejected/oversized session_id never materializes the hostile value; use a
        // deterministic cover identity so the row can advance without payload leakage.
        _ if sql_value_oversized => format!("hermes.oversized.{rowid}"),
        _ => return None,
    };
    Some(HermesRow {
        id: rowid,
        session_id,
        role: row
            .get::<_, Option<String>>(2)
            .ok()
            .flatten()
            .unwrap_or_default(),
        content: row.get::<_, Option<String>>(3).ok().flatten(),
        reasoning: row.get::<_, Option<String>>(4).ok().flatten(),
        tool_name: row.get::<_, Option<String>>(5).ok().flatten(),
        tool_calls: row.get::<_, Option<String>>(6).ok().flatten(),
        timestamp: row_optional_f64(row, 7),
        session_model: row.get::<_, Option<String>>(8).ok().flatten(),
        parent_session_id: row.get::<_, Option<String>>(9).ok().flatten(),
        session_cwd: row.get::<_, Option<String>>(10).ok().flatten(),
        session_source: row.get::<_, Option<String>>(11).ok().flatten(),
        session_title: row.get::<_, Option<String>>(12).ok().flatten(),
        session_started_at: row_optional_f64(row, 13),
        session_ended_at: row_optional_f64(row, 14),
        session_input_tokens: row.get::<_, Option<i64>>(15).ok().flatten(),
        session_output_tokens: row.get::<_, Option<i64>>(16).ok().flatten(),
        session_cache_read_tokens: row.get::<_, Option<i64>>(17).ok().flatten(),
        session_cache_write_tokens: row.get::<_, Option<i64>>(18).ok().flatten(),
        session_reasoning_tokens: row.get::<_, Option<i64>>(19).ok().flatten(),
        active: row.get::<_, Option<i64>>(20).ok().flatten().unwrap_or(1),
        sql_value_oversized,
        sql_measured_bytes,
    })
}

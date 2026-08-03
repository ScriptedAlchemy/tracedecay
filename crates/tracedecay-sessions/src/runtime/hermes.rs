//! Hermes Agent transcript source.
//!
//! Hermes does not write transcript files: every conversation lives in a
//! per-profile `SQLite` store at `<profile>/state.db` (tables `sessions` +
//! `messages`), where `<profile>` is `~/.hermes` for the default profile or
//! `~/.hermes/profiles/<name>` for named profiles. A profile maps to exactly
//! one ingest target only when provenance proves a real code project: a
//! legacy `plugins.tracedecay.project_root` pin or the session row's `cwd`.
//! For projectless/gateway sessions, one completed turn may instead prove its
//! project through structured tool-call routing (`project_path`,
//! `project_root`, or a nested project selector). Only that turn is projected;
//! an entire long-running multi-project chat is never assigned by inference.
//! Profile directories are never `TraceDecay` project identities.
//!
//! Unlike the file-based adapters this source holds *many* sessions in one
//! store, so it does not implement [`TranscriptSource`]; it drives the shared
//! `parse_offsets` cursor directly (`position` = last-seen `messages.id`, the
//! `RowCursor` kind) and upserts multi-session [`TranscriptBatch`]es in
//! bounded chunks.
//!
//! Hermes transcripts fill only the searchable `session_messages` projection
//! ([`GlobalDb::upsert_transcript_projection_batches`]): the raw LCM store is
//! already fed losslessly at runtime by the generated plugin's
//! `lcm_preflight` active-message ingest (and by the one-time legacy-store
//! migration) under its own message ids, so writing raw rows from this sweep
//! too would duplicate the LCM store.
//!
//! [`TranscriptSource`]: crate::runtime::source::TranscriptSource
//! [`TranscriptBatch`]: crate::runtime::hermes::TranscriptBatch

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use rayon::prelude::*;
use serde_json::{Map, Value};

use crate::runtime::shared::{
    NewRows, ProjectRootMatcher, StoredCursor, TranscriptIngestStats, TranscriptLocation,
    TranscriptLocationMetadataKeys, append_location_metadata, content_storage_text_and_tools,
    path_belongs_to_project, preview_title, title_from_messages,
};
use crate::{SessionMessageRecord, SessionRecord};

#[derive(Debug, Clone)]
pub struct TranscriptBatch {
    pub session: SessionRecord,
    pub messages: Vec<SessionMessageRecord>,
}

pub trait HermesStore: Sync {
    fn load_cursor<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = StoredCursor> + Send + 'a>>;
    fn advance_cursor<'a>(
        &'a self,
        path: &'a str,
        cursor: StoredCursor,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
    fn upsert_transcript_projection_batches<'a>(
        &'a self,
        batches: &'a [TranscriptBatch],
        path: &'a str,
        cursor: StoredCursor,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
    fn existing_session<'a>(
        &'a self,
        provider: &'a str,
        session_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<SessionRecord>> + Send + 'a>>;
}

const PROVIDER: &str = "hermes";
const HERMES_LOCATION_KEYS: TranscriptLocationMetadataKeys = TranscriptLocationMetadataKeys::new(
    "hermes_session_cwd",
    "hermes_session_worktree",
    "hermes_session_location_provenance",
);
/// Rows ingested per transaction. Keeps the first catch-up over a large
/// profile history (tens of thousands of rows) memory-bounded while letting
/// the cursor advance after every committed chunk, so an interrupted sweep
/// resumes where it stopped.
const CHUNK_ROWS: usize = 2000;
const CORRELATION_CURSOR_VERSION: &str = "turn-project-v2";
const USER_CURSOR_VERSION: &str = "user-turn-v2";

/// One project-store destination for a shared Hermes source sweep.
#[derive(Clone, Copy)]
pub struct ProjectIngestDestination<'a> {
    pub db: &'a dyn HermesStore,
    pub project_root: &'a Path,
}

/// Ingests Hermes history for several registered projects while opening and
/// scanning each profile `state.db` only once. The caller resolves the legacy
/// project pin for each profile, keeping agent-configuration parsing out of
/// this reusable runtime crate.
pub async fn ingest_homes_for_projects_with_project_pins<F>(
    hermes_homes: &[PathBuf],
    destinations: &[ProjectIngestDestination<'_>],
    project_pin: F,
) -> TranscriptIngestStats
where
    F: Fn(&Path) -> Option<PathBuf>,
{
    let mut stats = TranscriptIngestStats::default();
    for source in all_profile_sources(hermes_homes, &project_pin) {
        let eligible = destinations
            .iter()
            .copied()
            .filter(|destination| {
                source_is_candidate_for_project(&source, destination.project_root)
            })
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            continue;
        }
        match try_ingest_state_db_for_projects(&source, &eligible).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping shared Hermes transcript source"
            ),
        }
    }
    stats
}

/// Ingests Hermes sessions from explicit home directories. The caller supplies
/// the exact legacy profile-pin parser for its host configuration format.
pub async fn ingest_homes_with_project_pins<F>(
    db: &dyn HermesStore,
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_pin: F,
) -> TranscriptIngestStats
where
    F: Fn(&Path) -> Option<PathBuf>,
{
    let mut stats = TranscriptIngestStats::default();
    for source in candidate_state_dbs(hermes_homes, project_root, &project_pin) {
        match try_ingest_state_db(db, &source, project_root).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping Hermes transcript source"
            ),
        }
    }
    stats
}

/// Ingests canonical historical Hermes conversations into a profile-level
/// session store. The caller supplies the exact legacy profile-pin parser.
pub async fn ingest_user_homes_with_project_pins<F>(
    db: &dyn HermesStore,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
    project_pin: F,
) -> TranscriptIngestStats
where
    F: Fn(&Path) -> Option<PathBuf>,
{
    let mut stats = TranscriptIngestStats::default();
    for source in all_profile_sources(hermes_homes, &project_pin) {
        match try_ingest_user_state_db(db, &source, registered_roots).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping projectless Hermes transcript source"
            ),
        }
    }
    stats
}

/// Strict one-time import for a legacy profile whose project pin was already
/// resolved by the migration layer. Unlike the normal catch-up sweep, any
/// open/query/write failure is returned so callers retain the pin and source.
pub async fn ingest_legacy_pinned_profile_with_project_pin(
    db: &dyn HermesStore,
    profile_dir: &Path,
    project_root: &Path,
    legacy_project_pin: Option<PathBuf>,
) -> Result<TranscriptIngestStats, String> {
    let state_db = profile_dir.join("state.db");
    if !state_db.is_file() {
        return Ok(TranscriptIngestStats::default());
    }
    let legacy_project_pin = legacy_project_pin.ok_or_else(|| {
        format!(
            "legacy Hermes state store '{}' has no project pin",
            state_db.display()
        )
    })?;
    let profile = profile_dir
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "profiles"))
        .and_then(|_| profile_dir.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_string);
    let source = HermesProfileSource {
        state_db,
        profile,
        legacy_project_pin: Some(legacy_project_pin),
    };
    try_ingest_state_db(db, &source, project_root).await
}

/// Locates the `state.db` of every profile that maps to `project_root`.
///
/// A legacy project pin may associate an entire profile. Otherwise the
/// profile is only a bounded candidate source and each session must carry a
/// matching code-project cwd.
///
/// Returns `(state_db_path, profile_name)`; the default profile (the home
/// directory itself) has no profile name.
struct HermesProfileSource {
    state_db: PathBuf,
    profile: Option<String>,
    legacy_project_pin: Option<PathBuf>,
}

fn all_profile_sources<F>(hermes_homes: &[PathBuf], project_pin: &F) -> Vec<HermesProfileSource>
where
    F: Fn(&Path) -> Option<PathBuf>,
{
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for home in hermes_homes {
        let mut profiles = vec![(home.clone(), None)];
        if let Ok(entries) = std::fs::read_dir(home.join("profiles")) {
            profiles.extend(entries.filter_map(|entry| {
                let path = entry.ok()?.path();
                path.is_dir().then(|| {
                    let name = path.file_name()?.to_str()?.to_string();
                    Some((path, Some(name)))
                })?
            }));
        }
        for (profile_dir, profile) in profiles {
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push(HermesProfileSource {
                    state_db,
                    profile,
                    legacy_project_pin: project_pin(&profile_dir),
                });
            }
        }
    }
    out
}

fn candidate_state_dbs<F>(
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_pin: &F,
) -> Vec<HermesProfileSource>
where
    F: Fn(&Path) -> Option<PathBuf>,
{
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let project_is_real = tracedecay_runtime_core::worktree::git_worktree_root(project_root)
        .is_some()
        || tracedecay_runtime_core::config::has_project_database(project_root);
    for home in hermes_homes {
        let mut candidates: Vec<(PathBuf, Option<String>)> = vec![(home.clone(), None)];
        if let Ok(entries) = std::fs::read_dir(home.join("profiles")) {
            let mut profiles = entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    entry.file_type().ok()?.is_dir().then(|| entry.path())
                })
                .collect::<Vec<_>>();
            profiles.sort();
            for profile_dir in profiles {
                let name = profile_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
                candidates.push((profile_dir, name));
            }
        }
        for (profile_dir, profile_name) in candidates {
            let legacy_project_pin = project_pin(&profile_dir);
            if legacy_project_pin
                .as_deref()
                .is_some_and(|pin| !path_belongs_to_project(pin, project_root))
                || (legacy_project_pin.is_none() && !project_is_real)
            {
                continue;
            }
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push(HermesProfileSource {
                    state_db,
                    profile: profile_name,
                    legacy_project_pin,
                });
            }
        }
    }
    out
}

fn source_is_candidate_for_project(source: &HermesProfileSource, project_root: &Path) -> bool {
    if source
        .legacy_project_pin
        .as_deref()
        .is_some_and(|pin| !path_belongs_to_project(pin, project_root))
    {
        return false;
    }
    source.legacy_project_pin.is_some()
        || tracedecay_runtime_core::worktree::git_worktree_root(project_root).is_some()
        || tracedecay_runtime_core::config::has_project_database(project_root)
}

/// One joined `messages` × `sessions` row read past the cursor.
struct HermesRow {
    id: i64,
    session_id: String,
    role: String,
    content: Option<String>,
    tool_name: Option<String>,
    tool_calls: Option<String>,
    timestamp: Option<f64>,
    session_title: Option<String>,
    session_model: Option<String>,
    parent_session_id: Option<String>,
    session_started_at: Option<f64>,
    session_ended_at: Option<f64>,
    session_source: Option<String>,
    session_cwd: Option<String>,
    session_input_tokens: Option<i64>,
    session_output_tokens: Option<i64>,
    session_cache_read_tokens: Option<i64>,
    session_cache_write_tokens: Option<i64>,
    session_reasoning_tokens: Option<i64>,
    /// `messages.active` soft-delete flag (0 = rewound/undone turn). Legacy
    /// stores without the column read as 1.
    active: i64,
}

/// Column names of the `messages` table — `active` (v12 rewind soft-delete)
/// and `reasoning` arrived in later Hermes schema revisions, so the sweep
/// probes before selecting to stay readable on legacy stores.
async fn message_columns(conn: &libsql::Connection) -> std::collections::BTreeSet<String> {
    table_columns(conn, "messages").await
}

async fn table_columns(
    conn: &libsql::Connection,
    table: &str,
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let query = format!("SELECT name FROM pragma_table_info('{table}')");
    let Ok(mut rows) = conn.query(&query, ()).await else {
        return out;
    };
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(name) = row.get::<String>(0) {
            out.insert(name);
        }
    }
    out
}

fn select_new_messages_sql(
    message_columns: &std::collections::BTreeSet<String>,
    session_columns: &std::collections::BTreeSet<String>,
) -> String {
    // Reasoning-only assistant turns carry no `content`; surface the
    // reasoning text so the turn stays searchable.
    let content_expr = if message_columns.contains("reasoning") {
        "COALESCE(NULLIF(m.content, ''), m.reasoning)"
    } else {
        "m.content"
    };
    let active_expr = if message_columns.contains("active") {
        "m.active"
    } else {
        "1"
    };
    let session_cwd_expr = if session_columns.contains("cwd") {
        "s.cwd"
    } else {
        "NULL"
    };
    format!(
        "SELECT m.id, m.session_id, m.role, {content_expr}, m.tool_name,
                m.tool_calls, m.timestamp,
                s.title, s.model, s.parent_session_id, s.started_at, s.ended_at, s.source, {session_cwd_expr},
                s.input_tokens, s.output_tokens, s.cache_read_tokens, s.cache_write_tokens,
                s.reasoning_tokens, {active_expr}
         FROM messages m LEFT JOIN sessions s ON s.id = m.session_id
         WHERE m.id > ?
         ORDER BY m.id
         LIMIT {CHUNK_ROWS}"
    )
}

/// Incrementally ingests one Hermes `state.db`, advancing the shared parse
/// cursor after every committed chunk. The caller decides whether a source
/// error is fail-open runtime noise or a migration-blocking failure.
async fn try_ingest_state_db(
    db: &dyn HermesStore,
    source: &HermesProfileSource,
    project_root: &Path,
) -> Result<TranscriptIngestStats, String> {
    let mut stats = TranscriptIngestStats::default();
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let path_str = state_db.to_string_lossy().to_string();
    let cursor_path = format!("{path_str}#{CORRELATION_CURSOR_VERSION}");
    let mut cursor = { db.load_cursor(&cursor_path).await };
    let mut sessions_seen = BTreeSet::new();
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await,
        &table_columns(&conn, "sessions").await,
    );
    loop {
        let new = read_new_rows_strict(&conn, &select_sql, cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        let next_cursor = StoredCursor {
            position: new.new_cursor.position,
            mtime: file_mtime_secs(state_db),
            file_id: 0,
        };
        let batches = build_batches(db, &new.items, &path_str, project_root, source).await;
        if batches.is_empty() {
            // Only non-conversation rows (e.g. `session_meta`) — still advance
            // the cursor so the next sweep does not re-read them.
            db.advance_cursor(&cursor_path, next_cursor).await;
        } else {
            let message_count: u64 = batches
                .iter()
                .map(|batch| batch.messages.len() as u64)
                .sum();
            if !db
                .upsert_transcript_projection_batches(&batches, &cursor_path, next_cursor)
                .await
            {
                return Err(format!(
                    "could not persist legacy Hermes state rows from '{}'",
                    state_db.display()
                ));
            }
            for batch in &batches {
                sessions_seen.insert(batch.session.session_id.clone());
            }
            stats.messages_upserted = stats.messages_upserted.saturating_add(message_count);
            stats.sessions_upserted = sessions_seen.len() as u64;
        }
        cursor = next_cursor;
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

struct ProjectDestinationState<'a> {
    destination: ProjectIngestDestination<'a>,
    cursor: StoredCursor,
    sessions_seen: BTreeSet<String>,
    writable: bool,
    cursor_pending: bool,
}

/// Shared-source equivalent of [`try_ingest_state_db`]. Source rows are read
/// from the lowest destination cursor; destinations already ahead skip the
/// prefix and independently commit their projection plus cursor.
async fn try_ingest_state_db_for_projects(
    source: &HermesProfileSource,
    destinations: &[ProjectIngestDestination<'_>],
) -> Result<TranscriptIngestStats, String> {
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let path_str = state_db.to_string_lossy().to_string();
    let cursor_path = format!("{path_str}#{CORRELATION_CURSOR_VERSION}");
    let mut states = Vec::with_capacity(destinations.len());
    for destination in destinations {
        let prev = destination.db.load_cursor(&cursor_path).await;
        states.push(ProjectDestinationState {
            destination: *destination,
            cursor: prev,
            sessions_seen: BTreeSet::new(),
            writable: true,
            cursor_pending: false,
        });
    }
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await,
        &table_columns(&conn, "sessions").await,
    );
    let destination_matchers = states
        .par_iter()
        .map(|state| ProjectRootMatcher::new(state.destination.project_root))
        .collect::<Vec<_>>();
    let mut destination_routes = HashMap::<PathBuf, Vec<usize>>::new();
    let mut read_cursor = StoredCursor {
        position: states
            .iter()
            .map(|state| state.cursor.position)
            .min()
            .unwrap_or_default(),
        mtime: 0,
        file_id: 0,
    };
    let mut stats = TranscriptIngestStats::default();

    loop {
        let new = read_new_rows_strict(&conn, &select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            break;
        }
        let source_position = new.new_cursor.position;
        let mtime = file_mtime_secs(state_db);
        let destination_locations = turn_project_locations_for_destinations(
            &new.items,
            &destination_matchers,
            source,
            &mut destination_routes,
        );
        for (state_index, state) in states
            .iter_mut()
            .enumerate()
            .filter(|(_, state)| state.writable)
        {
            if source_position <= state.cursor.position {
                continue;
            }
            let destination = &destination_locations[state_index];
            let first_new = destination
                .row_indices
                .partition_point(|&index| new.items[index].id as u64 <= state.cursor.position);
            let next_cursor = StoredCursor {
                position: source_position,
                mtime,
                file_id: 0,
            };
            let batches = build_batches_with_locations(
                state.destination.db,
                &new.items,
                &path_str,
                state.destination.project_root,
                source,
                &destination.by_row_id,
                Some(&destination.row_indices[first_new..]),
            )
            .await;
            if batches.is_empty() {
                // Cursor-only transactions across every registered project
                // dominate cold catch-up. Defer them to one final write; a
                // crash before that point merely causes an idempotent rescan.
                state.cursor = next_cursor;
                state.cursor_pending = true;
                continue;
            }
            if !state
                .destination
                .db
                .upsert_transcript_projection_batches(&batches, &cursor_path, next_cursor)
                .await
            {
                state.writable = false;
                continue;
            }
            stats.messages_upserted = stats.messages_upserted.saturating_add(
                batches
                    .iter()
                    .map(|batch| batch.messages.len() as u64)
                    .sum::<u64>(),
            );
            for batch in &batches {
                state.sessions_seen.insert(batch.session.session_id.clone());
            }
            state.cursor = next_cursor;
            state.cursor_pending = false;
        }
        read_cursor.position = source_position;
        if row_count < CHUNK_ROWS {
            break;
        }
    }
    for state in states
        .iter()
        .filter(|state| state.writable && state.cursor_pending)
    {
        state
            .destination
            .db
            .advance_cursor(&cursor_path, state.cursor)
            .await;
    }
    stats.sessions_upserted = states
        .iter()
        .map(|state| state.sessions_seen.len() as u64)
        .sum();
    Ok(stats)
}

async fn try_ingest_user_state_db(
    db: &dyn HermesStore,
    source: &HermesProfileSource,
    registered_roots: &[PathBuf],
) -> Result<TranscriptIngestStats, String> {
    let mut stats = TranscriptIngestStats::default();
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let path_str = state_db.to_string_lossy().to_string();
    let cursor_path = format!("{path_str}#{USER_CURSOR_VERSION}");
    let mut cursor = { db.load_cursor(&cursor_path).await };
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await,
        &table_columns(&conn, "sessions").await,
    );
    loop {
        let new = read_new_rows_strict(&conn, &select_sql, cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        let next_cursor = StoredCursor {
            position: new.new_cursor.position,
            mtime: file_mtime_secs(state_db),
            file_id: 0,
        };
        let batches = build_user_batches(db, &new.items, &path_str, source, registered_roots).await;
        if batches.is_empty() {
            db.advance_cursor(&cursor_path, next_cursor).await;
        } else {
            let message_count = batches
                .iter()
                .map(|batch| batch.messages.len() as u64)
                .sum::<u64>();
            if !db
                .upsert_transcript_projection_batches(&batches, &cursor_path, next_cursor)
                .await
            {
                return Err(format!(
                    "could not persist projectless Hermes rows from '{}'",
                    state_db.display()
                ));
            }
            stats.messages_upserted = stats.messages_upserted.saturating_add(message_count);
            stats.sessions_upserted = stats.sessions_upserted.saturating_add(batches.len() as u64);
        }
        cursor = next_cursor;
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

/// Opens a Hermes `state.db` strictly read-only so the sweep can never write
/// to (or create) another agent's live store.
async fn open_read_only_strict(path: &Path) -> Result<libsql::Connection, String> {
    let db = libsql::Builder::new_local(path)
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .map_err(|error| format!("could not open '{}' read-only: {error}", path.display()))?;
    db.connect()
        .map_err(|error| format!("could not connect to '{}': {error}", path.display()))
}

async fn read_new_rows_strict(
    conn: &libsql::Connection,
    select_sql: &str,
    prev: StoredCursor,
) -> Result<NewRows<HermesRow>, String> {
    let mut rows = conn
        .query(select_sql, libsql::params![prev.position as i64])
        .await
        .map_err(|error| format!("could not query legacy Hermes state rows: {error}"))?;
    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    loop {
        let row = rows
            .next()
            .await
            .map_err(|error| format!("could not read legacy Hermes state row: {error}"))?;
        let Some(row) = row else {
            break;
        };
        let rowid = row
            .get::<i64>(0)
            .map_err(|error| format!("legacy Hermes state row has no id: {error}"))?;
        max_rowid = max_rowid.max(rowid as u64);
        items.push(
            map_row(rowid, &row)
                .ok_or_else(|| format!("legacy Hermes state row {rowid} is malformed"))?,
        );
    }
    Ok(NewRows {
        items,
        new_cursor: StoredCursor {
            position: max_rowid,
            mtime: 0,
            file_id: 0,
        },
    })
}

fn map_row(rowid: i64, row: &libsql::Row) -> Option<HermesRow> {
    Some(HermesRow {
        id: rowid,
        session_id: row.get::<String>(1).ok()?,
        role: row.get::<String>(2).unwrap_or_default(),
        content: row.get::<Option<String>>(3).ok().flatten(),
        tool_name: row.get::<Option<String>>(4).ok().flatten(),
        tool_calls: row.get::<Option<String>>(5).ok().flatten(),
        timestamp: row.get::<Option<f64>>(6).ok().flatten(),
        session_title: row.get::<Option<String>>(7).ok().flatten(),
        session_model: row.get::<Option<String>>(8).ok().flatten(),
        parent_session_id: row.get::<Option<String>>(9).ok().flatten(),
        session_started_at: row.get::<Option<f64>>(10).ok().flatten(),
        session_ended_at: row.get::<Option<f64>>(11).ok().flatten(),
        session_source: row.get::<Option<String>>(12).ok().flatten(),
        session_cwd: row.get::<Option<String>>(13).ok().flatten(),
        session_input_tokens: row.get::<Option<i64>>(14).ok().flatten(),
        session_output_tokens: row.get::<Option<i64>>(15).ok().flatten(),
        session_cache_read_tokens: row.get::<Option<i64>>(16).ok().flatten(),
        session_cache_write_tokens: row.get::<Option<i64>>(17).ok().flatten(),
        session_reasoning_tokens: row.get::<Option<i64>>(18).ok().flatten(),
        active: row.get::<Option<i64>>(19).ok().flatten().unwrap_or(1),
    })
}

/// Groups one chunk of rows into per-session [`TranscriptBatch`]es, merging
/// session metadata with any previously stored row (original `started_at` and
/// `title` survive incremental sweeps, mirroring the file-source driver).
async fn build_batches(
    db: &dyn HermesStore,
    rows: &[HermesRow],
    state_db_path: &str,
    project_root: &Path,
    source: &HermesProfileSource,
) -> Vec<TranscriptBatch> {
    let turn_locations = turn_project_locations(rows, project_root, source);
    build_batches_with_locations(
        db,
        rows,
        state_db_path,
        project_root,
        source,
        &turn_locations,
        None,
    )
    .await
}

async fn build_batches_with_locations(
    db: &dyn HermesStore,
    rows: &[HermesRow],
    state_db_path: &str,
    project_root: &Path,
    source: &HermesProfileSource,
    turn_locations: &HashMap<i64, HermesSessionLocation>,
    row_indices: Option<&[usize]>,
) -> Vec<TranscriptBatch> {
    let mut order = Vec::new();
    let mut by_session: HashMap<String, TranscriptBatch> = HashMap::new();

    {
        let mut add_row = |row: &HermesRow| {
            if row.role == "session_meta" || row.role.is_empty() {
                return;
            }
            if row.active == 0 {
                // Rewound/undone turns are soft-deleted in Hermes; surfacing
                // them as live history would misrepresent the conversation.
                return;
            }
            let Some(location) = turn_locations.get(&row.id) else {
                return;
            };
            let Some(message) = message_from_row(row, state_db_path, source, &location) else {
                return;
            };
            let batch = by_session.entry(row.session_id.clone()).or_insert_with(|| {
                order.push(row.session_id.clone());
                TranscriptBatch {
                    session: session_from_row(row, state_db_path, project_root, source, &location),
                    messages: Vec::new(),
                }
            });
            batch.messages.push(message);
        };
        if let Some(row_indices) = row_indices {
            for &index in row_indices {
                add_row(&rows[index]);
            }
        } else {
            for row in rows {
                add_row(row);
            }
        }
    }

    let mut batches = Vec::with_capacity(order.len());
    for session_id in order {
        let Some(mut batch) = by_session.remove(&session_id) else {
            continue;
        };
        merge_with_existing(db, &mut batch).await;
        batches.push(batch);
    }
    batches
}

async fn build_user_batches(
    db: &dyn HermesStore,
    rows: &[HermesRow],
    state_db_path: &str,
    source: &HermesProfileSource,
    _registered_roots: &[PathBuf],
) -> Vec<TranscriptBatch> {
    let mut order = Vec::new();
    let mut by_session: HashMap<String, TranscriptBatch> = HashMap::new();
    let locations = user_turn_locations(rows, source);
    for row in rows {
        if row.role == "session_meta" || row.role.is_empty() || row.active == 0 {
            continue;
        }
        let Some(location) = locations.get(&row.id) else {
            continue;
        };
        let Some(message) = message_from_row(row, state_db_path, source, location) else {
            continue;
        };
        let batch = by_session.entry(row.session_id.clone()).or_insert_with(|| {
            order.push(row.session_id.clone());
            TranscriptBatch {
                session: session_from_row(row, state_db_path, Path::new("user"), source, location),
                messages: Vec::new(),
            }
        });
        batch.messages.push(message);
    }
    let mut batches = Vec::with_capacity(order.len());
    for session_id in order {
        if let Some(mut batch) = by_session.remove(&session_id) {
            merge_with_existing(db, &mut batch).await;
            batches.push(batch);
        }
    }
    batches
}

fn user_turn_locations(
    rows: &[HermesRow],
    source: &HermesProfileSource,
) -> HashMap<i64, HermesSessionLocation> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = HashMap::new();
    for session_rows in by_session.into_values() {
        let recorded_cwd = session_rows.iter().find_map(|row| {
            let cwd = PathBuf::from(row.session_cwd.as_deref()?.trim());
            cwd.is_absolute().then_some(cwd)
        });
        let fallback = source
            .legacy_project_pin
            .clone()
            .or(recorded_cwd)
            .or_else(|| source.state_db.parent().map(Path::to_path_buf));
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_user_turn(&turn, fallback.as_deref(), &mut locations);
                turn.clear();
            }
            turn.push(row);
        }
        assign_user_turn(&turn, fallback.as_deref(), &mut locations);
    }
    locations
}

fn assign_user_turn(
    rows: &[&HermesRow],
    fallback: Option<&Path>,
    locations: &mut HashMap<i64, HermesSessionLocation>,
) {
    let explicit = rows
        .iter()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let cwd = explicit
        .last()
        .cloned()
        .or_else(|| fallback.map(Path::to_path_buf));
    let Some(cwd) = cwd else {
        return;
    };
    let location = HermesSessionLocation {
        cwd,
        provenance: "user_scope",
    };
    for row in rows {
        locations.insert(row.id, location.clone());
    }
}

fn turn_project_locations(
    rows: &[HermesRow],
    project_root: &Path,
    source: &HermesProfileSource,
) -> HashMap<i64, HermesSessionLocation> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = HashMap::new();
    for session_rows in by_session.into_values() {
        let fallback = session_rows
            .iter()
            .find_map(|row| session_location(row, project_root, source));
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_location(&turn, project_root, fallback.as_ref(), &mut locations);
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_location(&turn, project_root, fallback.as_ref(), &mut locations);
    }
    locations
}

struct DestinationTurnLocations {
    by_row_id: HashMap<i64, HermesSessionLocation>,
    row_indices: Vec<usize>,
}

fn turn_project_locations_for_destinations(
    rows: &[HermesRow],
    destination_matchers: &[ProjectRootMatcher],
    source: &HermesProfileSource,
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) -> Vec<DestinationTurnLocations> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    let row_indices = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.id, index))
        .collect::<HashMap<_, _>>();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = (0..destination_matchers.len())
        .map(|_| DestinationTurnLocations {
            by_row_id: HashMap::new(),
            row_indices: Vec::new(),
        })
        .collect::<Vec<_>>();
    for session_rows in by_session.into_values() {
        let fallback_candidates = if let Some(pin) = source.legacy_project_pin.as_ref() {
            vec![(pin.clone(), "profile_pin")]
        } else {
            let mut seen = BTreeSet::new();
            session_rows
                .iter()
                .filter_map(|row| {
                    let cwd = PathBuf::from(row.session_cwd.as_deref()?.trim());
                    (cwd.is_absolute() && seen.insert(cwd.clone())).then_some((cwd, "session_cwd"))
                })
                .collect::<Vec<_>>()
        };
        let mut fallbacks = vec![None; destination_matchers.len()];
        for (cwd, provenance) in fallback_candidates {
            for destination_index in
                matching_destinations(&cwd, destination_matchers, destination_routes)
            {
                fallbacks[destination_index].get_or_insert_with(|| HermesSessionLocation {
                    cwd: cwd.clone(),
                    provenance,
                });
            }
        }
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_locations_for_destinations(
                    &turn,
                    destination_matchers,
                    &fallbacks,
                    &row_indices,
                    &mut locations,
                    destination_routes,
                );
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_locations_for_destinations(
            &turn,
            destination_matchers,
            &fallbacks,
            &row_indices,
            &mut locations,
            destination_routes,
        );
    }
    for destination in &mut locations {
        destination.row_indices.sort_unstable();
    }
    locations
}

fn assign_turn_locations_for_destinations(
    rows: &[&HermesRow],
    destination_matchers: &[ProjectRootMatcher],
    fallbacks: &[Option<HermesSessionLocation>],
    row_indices: &HashMap<i64, usize>,
    locations: &mut [DestinationTurnLocations],
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let mut selected = vec![None; destination_matchers.len()];
    if explicit_paths.is_empty() {
        selected.clone_from_slice(fallbacks);
    } else {
        for path in explicit_paths {
            for destination_index in
                matching_destinations(&path, destination_matchers, destination_routes)
            {
                selected[destination_index].get_or_insert_with(|| HermesSessionLocation {
                    cwd: path.clone(),
                    provenance: "tool_project_path",
                });
            }
        }
    }
    for (location, destination) in selected.into_iter().zip(locations) {
        let Some(location) = location else {
            continue;
        };
        for row in rows {
            destination.by_row_id.insert(row.id, location.clone());
            if let Some(&index) = row_indices.get(&row.id) {
                destination.row_indices.push(index);
            }
        }
    }
}

fn matching_destinations(
    path: &Path,
    destination_matchers: &[ProjectRootMatcher],
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) -> Vec<usize> {
    if let Some(indices) = destination_routes.get(path) {
        return indices.clone();
    }
    let indices = destination_matchers
        .iter()
        .enumerate()
        .filter_map(|(index, matcher)| matcher.contains(path).then_some(index))
        .collect::<Vec<_>>();
    destination_routes.insert(path.to_path_buf(), indices.clone());
    indices
}

fn assign_turn_location(
    rows: &[&HermesRow],
    project_root: &Path,
    fallback: Option<&HermesSessionLocation>,
    locations: &mut HashMap<i64, HermesSessionLocation>,
) {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let location = if explicit_paths.is_empty() {
        fallback.cloned()
    } else {
        explicit_paths
            .into_iter()
            .find(|path| path_belongs_to_project(path, project_root))
            .map(|cwd| HermesSessionLocation {
                cwd,
                provenance: "tool_project_path",
            })
    };
    let Some(location) = location else {
        return;
    };
    for row in rows {
        locations.insert(row.id, location.clone());
    }
}

fn structured_tool_project_paths(row: &HermesRow) -> Vec<PathBuf> {
    let Some(raw) = row.tool_calls.as_deref() else {
        return Vec::new();
    };
    let Ok(calls) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let calls = calls.as_array().map(Vec::as_slice).unwrap_or(&[]);
    for call in calls {
        let arguments = call
            .pointer("/function/arguments")
            .or_else(|| call.get("arguments"));
        let parsed;
        let arguments = match arguments {
            Some(Value::String(raw)) => {
                parsed = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
                &parsed
            }
            Some(value) => value,
            None => continue,
        };
        for value in [
            arguments.get("project_root"),
            arguments.get("project_path"),
            arguments.pointer("/project_selector/path"),
            arguments.get("cwd"),
            arguments.get("workdir"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                paths.push(path);
            }
        }
    }
    paths
}

#[derive(Clone)]
struct HermesSessionLocation {
    cwd: PathBuf,
    provenance: &'static str,
}

fn session_location(
    row: &HermesRow,
    project_root: &Path,
    source: &HermesProfileSource,
) -> Option<HermesSessionLocation> {
    if let Some(pin) = source.legacy_project_pin.as_ref() {
        return Some(HermesSessionLocation {
            cwd: pin.clone(),
            provenance: "profile_pin",
        });
    }
    let cwd = PathBuf::from(row.session_cwd.as_deref()?.trim());
    if !cwd.is_absolute() || !path_belongs_to_project(&cwd, project_root) {
        return None;
    }
    Some(HermesSessionLocation {
        cwd,
        provenance: "session_cwd",
    })
}

fn session_from_row(
    row: &HermesRow,
    state_db_path: &str,
    project_root: &Path,
    source: &HermesProfileSource,
    location: &HermesSessionLocation,
) -> SessionRecord {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("hermes_state_db".to_string()),
    );
    if let Some(profile) = source.profile.as_deref() {
        metadata.insert("profile".to_string(), Value::String(profile.to_string()));
    }
    if let Some(source) = row.session_source.as_deref() {
        metadata.insert(
            "hermes_source".to_string(),
            Value::String(source.to_string()),
        );
    }
    if let Some(usage) = session_usage_counters(row) {
        metadata.insert("usage".to_string(), usage);
    }
    append_location_metadata(
        &mut metadata,
        HERMES_LOCATION_KEYS,
        TranscriptLocation::new(Some(&location.cwd), location.provenance),
    );
    let project = project_root.to_string_lossy().to_string();
    let parent_session_id = row
        .parent_session_id
        .as_deref()
        .filter(|parent| !parent.is_empty())
        .map(str::to_string);
    let is_subagent = parent_session_id.is_some();
    SessionRecord {
        provider: PROVIDER.to_string(),
        session_id: row.session_id.clone(),
        project_key: project.clone(),
        project_path: project,
        title: row
            .session_title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .map(preview_title),
        started_at: row.session_started_at.map(|secs| secs as i64),
        ended_at: row.session_ended_at.map(|secs| secs as i64),
        transcript_path: Some(state_db_path.to_string()),
        metadata_json: Some(Value::Object(metadata).to_string()),
        parent_session_id,
        is_subagent,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

/// Session-cumulative token counters from the Hermes `sessions` table, mapped
/// to the counter names the savings dashboard recognizes. Hermes records no
/// per-message usage (`messages.token_count` is never populated), so the
/// session row is the only honest granularity; the counters live in *session*
/// metadata — never message `usage` — so the per-message savings rollup
/// cannot double-count them. Re-sweeps refresh the values (cumulative
/// counters only grow).
fn session_usage_counters(row: &HermesRow) -> Option<Value> {
    let mut usage = Map::new();
    for (key, value) in [
        ("input_tokens", row.session_input_tokens),
        ("output_tokens", row.session_output_tokens),
        ("cache_read_input_tokens", row.session_cache_read_tokens),
        (
            "cache_creation_input_tokens",
            row.session_cache_write_tokens,
        ),
        ("reasoning_tokens", row.session_reasoning_tokens),
    ] {
        if let Some(count) = value.filter(|count| *count > 0) {
            usage.insert(key.to_string(), Value::from(count));
        }
    }
    (!usage.is_empty()).then_some(Value::Object(usage))
}

/// Preserve a previously stored session's original `started_at`, `title`,
/// and metadata keys (e.g. the `hermes_migration` marker left by the legacy
/// LCM-store import) across incremental sweeps, mirroring the file-source
/// driver's merge semantics.
async fn merge_with_existing(db: &dyn HermesStore, batch: &mut TranscriptBatch) {
    let existing = db
        .existing_session(PROVIDER, &batch.session.session_id)
        .await;
    let first_ts = batch.messages.first().and_then(|message| message.timestamp);
    let last_ts = batch.messages.last().and_then(|message| message.timestamp);

    if let Some(existing) = existing {
        if existing.title.is_some() {
            batch.session.title = existing.title;
        }
        if existing.started_at.is_some() {
            batch.session.started_at = existing.started_at;
        }
        if batch.session.ended_at.is_none() {
            batch.session.ended_at = last_ts.or(existing.ended_at);
        }
        if let Some(previous) = existing
            .metadata_json
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|value| value.as_object().cloned())
        {
            let mut merged = previous;
            if let Some(new) = batch
                .session
                .metadata_json
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .and_then(|value| value.as_object().cloned())
            {
                merged.extend(new);
            }
            batch.session.metadata_json = Some(Value::Object(merged).to_string());
        }
    }
    if batch.session.title.is_none() {
        batch.session.title = title_from_messages(&batch.messages);
    }
    if batch.session.started_at.is_none() {
        batch.session.started_at = first_ts;
    }
    if batch.session.ended_at.is_none() {
        batch.session.ended_at = last_ts;
    }
}

fn message_from_row(
    row: &HermesRow,
    state_db_path: &str,
    source: &HermesProfileSource,
    location: &HermesSessionLocation,
) -> Option<SessionMessageRecord> {
    let content = row
        .content
        .as_deref()
        .filter(|text| !text.trim().is_empty());
    let tool_calls_value = row
        .tool_calls
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .map(|text| {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.to_string()))
        });
    // Assistant tool-call turns carry no `content`; fall back to the compact
    // tool-call JSON so the turn stays searchable. Rows with neither carry no
    // conversational signal.
    let text = match (content, row.tool_calls.as_deref()) {
        (Some(content), _) => content.to_string(),
        (None, Some(tool_calls)) if !tool_calls.trim().is_empty() => tool_calls.to_string(),
        _ => return None,
    };

    let mut tool_names = Vec::new();
    if let Some(name) = row.tool_name.as_deref().filter(|name| !name.is_empty()) {
        tool_names.push(name.to_string());
    }
    if let Some(value) = tool_calls_value.as_ref() {
        let (_, mut from_calls) = content_storage_text_and_tools(&Value::Null, Some(value));
        tool_names.append(&mut from_calls);
    }
    tool_names.sort();
    tool_names.dedup();

    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("hermes_state_db".to_string()),
    );
    if let Some(profile) = source.profile.as_deref() {
        metadata.insert("profile".to_string(), Value::String(profile.to_string()));
    }
    append_location_metadata(
        &mut metadata,
        HERMES_LOCATION_KEYS,
        TranscriptLocation::new(Some(&location.cwd), location.provenance),
    );
    if let Some(value) = tool_calls_value {
        metadata.insert("tool_calls".to_string(), value);
    }

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{}", row.session_id, row.id),
        session_id: row.session_id.clone(),
        role: row.role.clone(),
        timestamp: row.timestamp.map(|secs| secs as i64),
        ordinal: row.id,
        text,
        kind: Some("message".to_string()),
        model: row.session_model.clone(),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(state_db_path.to_string()),
        source_offset: Some(row.id),
        metadata_json: Some(Value::Object(metadata).to_string()),
    })
}

fn file_mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

//! Hermes Agent transcript source.
//!
//! Hermes does not write transcript files: every conversation lives in a
//! per-profile `SQLite` store at `<profile>/state.db` (tables `sessions` +
//! `messages`), where `<profile>` is `~/.hermes` for the default profile or
//! `~/.hermes/profiles/<name>` for named profiles. A profile maps to exactly
//! one ingest target only when provenance proves a real code project: a
//! legacy `plugins.tracedecay.project_root` pin or the session row's `cwd`.
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
//! [`TranscriptSource`]: crate::sessions::source::TranscriptSource
//! [`TranscriptBatch`]: crate::global_db::TranscriptBatch

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::agents::hermes::read_config_pinned_project_root;
use crate::global_db::{GlobalDb, ParseOffset, TranscriptBatch};
use crate::sessions::shared::{
    NewRows, StoredCursor, TranscriptIngestStats, TranscriptLocation,
    TranscriptLocationMetadataKeys, append_location_metadata, content_storage_text_and_tools,
    path_belongs_to_project, preview_title, title_from_messages,
};
use crate::sessions::{SessionMessageRecord, SessionRecord};

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

/// Ingests Hermes sessions proven to belong to `project_root` into `db`.
///
/// Discovery is bounded to the default user integration (`~/.hermes`) and its
/// immediate named-profile children; environment overrides are ignored.
pub async fn ingest_for_project(db: &GlobalDb, project_root: &Path) -> TranscriptIngestStats {
    let homes = crate::sessions::home_dir()
        .map(|home| vec![home.join(".hermes")])
        .unwrap_or_default();
    ingest_homes(db, &homes, project_root).await
}

/// [`ingest_for_project`] with explicit Hermes home directories — the test
/// seam for pointing the sweep at a temporary home instead of the real
/// `~/.hermes`.
pub async fn ingest_homes(
    db: &GlobalDb,
    hermes_homes: &[PathBuf],
    project_root: &Path,
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    for source in candidate_state_dbs(hermes_homes, project_root) {
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

/// Strict one-time import for a legacy profile whose project pin was already
/// resolved by the migration layer. Unlike the normal catch-up sweep, any
/// open/query/write failure is returned so callers retain the pin and source.
pub(crate) async fn ingest_legacy_pinned_profile(
    db: &GlobalDb,
    profile_dir: &Path,
    project_root: &Path,
) -> Result<TranscriptIngestStats, String> {
    let state_db = profile_dir.join("state.db");
    if !state_db.is_file() {
        return Ok(TranscriptIngestStats::default());
    }
    let legacy_project_pin = read_config_pinned_project_root(&profile_dir.join("config.yaml"))
        .map(PathBuf::from)
        .ok_or_else(|| {
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

fn candidate_state_dbs(hermes_homes: &[PathBuf], project_root: &Path) -> Vec<HermesProfileSource> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let project_is_real = crate::worktree::git_worktree_root(project_root).is_some()
        || crate::config::has_project_database(project_root);
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
            let legacy_project_pin =
                read_config_pinned_project_root(&profile_dir.join("config.yaml"))
                    .map(PathBuf::from);
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
    db: &GlobalDb,
    source: &HermesProfileSource,
    project_root: &Path,
) -> Result<TranscriptIngestStats, String> {
    let mut stats = TranscriptIngestStats::default();
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let path_str = state_db.to_string_lossy().to_string();
    let mut cursor = {
        let prev = db.get_parse_offset(&path_str).await.unwrap_or_default();
        StoredCursor {
            position: prev.byte_offset,
            mtime: prev.mtime,
            file_id: prev.file_id,
        }
    };
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
        let offset = ParseOffset {
            byte_offset: next_cursor.position,
            mtime: next_cursor.mtime,
            file_id: next_cursor.file_id,
        };
        let batches = build_batches(db, &new.items, &path_str, project_root, source).await;
        if batches.is_empty() {
            // Only non-conversation rows (e.g. `session_meta`) — still advance
            // the cursor so the next sweep does not re-read them.
            db.set_parse_offset(&path_str, offset).await;
        } else {
            let message_count: u64 = batches
                .iter()
                .map(|batch| batch.messages.len() as u64)
                .sum();
            if !db
                .upsert_transcript_projection_batches(&batches, &path_str, offset)
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
    db: &GlobalDb,
    rows: &[HermesRow],
    state_db_path: &str,
    project_root: &Path,
    source: &HermesProfileSource,
) -> Vec<TranscriptBatch> {
    let mut order = Vec::new();
    let mut by_session: HashMap<String, TranscriptBatch> = HashMap::new();

    for row in rows {
        if row.role == "session_meta" || row.role.is_empty() {
            continue;
        }
        if row.active == 0 {
            // Rewound/undone turns are soft-deleted in Hermes; surfacing
            // them as live history would misrepresent the conversation.
            continue;
        }
        let Some(location) = session_location(row, project_root, source) else {
            continue;
        };
        let Some(message) = message_from_row(row, state_db_path, source, &location) else {
            continue;
        };
        let batch = by_session.entry(row.session_id.clone()).or_insert_with(|| {
            order.push(row.session_id.clone());
            TranscriptBatch {
                session: session_from_row(row, state_db_path, project_root, source, &location),
                messages: Vec::new(),
            }
        });
        batch.messages.push(message);
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
async fn merge_with_existing(db: &GlobalDb, batch: &mut TranscriptBatch) {
    let existing = db.get_session(PROVIDER, &batch.session.session_id).await;
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

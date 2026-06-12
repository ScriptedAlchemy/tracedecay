//! Hermes Agent transcript source.
//!
//! Hermes does not write transcript files: every conversation lives in a
//! per-profile `SQLite` store at `<profile>/state.db` (tables `sessions` +
//! `messages`), where `<profile>` is `~/.hermes` for the default profile or
//! `~/.hermes/profiles/<name>` for named profiles. A profile maps to exactly
//! one project through the `plugins.tracedecay.project_root` pin in its
//! `config.yaml` — the same pin the generated Hermes plugin resolves at
//! runtime — so the sweep ingests a profile's history only into the project
//! it is pinned to.
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
use crate::sessions::source::{
    content_storage_text_and_tools, paths_equal, preview_title, read_new_rows, title_from_messages,
    StoredCursor, TranscriptIngestStats,
};
use crate::sessions::{SessionMessageRecord, SessionRecord};

const PROVIDER: &str = "hermes";
/// Rows ingested per transaction. Keeps the first catch-up over a large
/// profile history (tens of thousands of rows) memory-bounded while letting
/// the cursor advance after every committed chunk, so an interrupted sweep
/// resumes where it stopped.
const CHUNK_ROWS: usize = 2000;

/// Ingests every Hermes profile pinned to `project_root` into `db`.
///
/// Discovery is bounded: the default profile directory (`~/.hermes`, plus an
/// optional `HERMES_HOME` override) and the immediate children of
/// `~/.hermes/profiles` are the only directories consulted — no recursive
/// scanning.
pub async fn ingest_for_project(db: &GlobalDb, project_root: &Path) -> TranscriptIngestStats {
    let mut homes = Vec::new();
    if let Some(home) = dirs::home_dir() {
        homes.push(home.join(".hermes"));
    }
    if let Some(env_home) = std::env::var_os("HERMES_HOME") {
        if !env_home.is_empty() {
            homes.push(PathBuf::from(env_home));
        }
    }
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
    for (state_db, profile) in pinned_state_dbs(hermes_homes, project_root) {
        stats = stats.merge(ingest_state_db(db, &state_db, project_root, profile.as_deref()).await);
    }
    stats
}

/// Locates the `state.db` of every profile whose `plugins.tracedecay`
/// `project_root` pin matches `project_root`. Returns `(state_db_path,
/// profile_name)`; the default profile (the home directory itself) has no
/// profile name.
fn pinned_state_dbs(
    hermes_homes: &[PathBuf],
    project_root: &Path,
) -> Vec<(PathBuf, Option<String>)> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
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
            let Some(pin) = read_config_pinned_project_root(&profile_dir.join("config.yaml"))
            else {
                continue;
            };
            if !paths_equal(Path::new(&pin), project_root) {
                continue;
            }
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push((state_db, profile_name));
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
    session_input_tokens: Option<i64>,
    session_output_tokens: Option<i64>,
    session_cache_read_tokens: Option<i64>,
    session_cache_write_tokens: Option<i64>,
    session_reasoning_tokens: Option<i64>,
}

fn select_new_messages_sql() -> String {
    format!(
        "SELECT m.id, m.session_id, m.role, m.content, m.tool_name,
                m.tool_calls, m.timestamp,
                s.title, s.model, s.parent_session_id, s.started_at, s.ended_at, s.source,
                s.input_tokens, s.output_tokens, s.cache_read_tokens, s.cache_write_tokens,
                s.reasoning_tokens
         FROM messages m LEFT JOIN sessions s ON s.id = m.session_id
         WHERE m.id > ?
         ORDER BY m.id
         LIMIT {CHUNK_ROWS}"
    )
}

/// Incrementally ingests one Hermes `state.db`, advancing the shared parse
/// cursor after every committed chunk. Fail-open: any open/query error yields
/// whatever was committed so far.
async fn ingest_state_db(
    db: &GlobalDb,
    state_db: &Path,
    project_root: &Path,
    profile: Option<&str>,
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    let Some(conn) = open_read_only(state_db).await else {
        return stats;
    };
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
    let select_sql = select_new_messages_sql();

    loop {
        let Some(new) = read_new_rows(&conn, &select_sql, cursor, map_row).await else {
            return stats;
        };
        let row_count = new.items.len();
        if row_count == 0 {
            return stats;
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
        let batches = build_batches(db, &new.items, &path_str, project_root, profile).await;
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
                return stats;
            }
            for batch in &batches {
                sessions_seen.insert(batch.session.session_id.clone());
            }
            stats.messages_upserted = stats.messages_upserted.saturating_add(message_count);
            stats.sessions_upserted = sessions_seen.len() as u64;
        }
        cursor = next_cursor;
        if row_count < CHUNK_ROWS {
            return stats;
        }
    }
}

/// Opens a Hermes `state.db` strictly read-only so the sweep can never write
/// to (or create) another agent's live store.
async fn open_read_only(path: &Path) -> Option<libsql::Connection> {
    let db = libsql::Builder::new_local(path)
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .ok()?;
    db.connect().ok()
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
        session_input_tokens: row.get::<Option<i64>>(13).ok().flatten(),
        session_output_tokens: row.get::<Option<i64>>(14).ok().flatten(),
        session_cache_read_tokens: row.get::<Option<i64>>(15).ok().flatten(),
        session_cache_write_tokens: row.get::<Option<i64>>(16).ok().flatten(),
        session_reasoning_tokens: row.get::<Option<i64>>(17).ok().flatten(),
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
    profile: Option<&str>,
) -> Vec<TranscriptBatch> {
    let mut order = Vec::new();
    let mut by_session: HashMap<String, TranscriptBatch> = HashMap::new();

    for row in rows {
        if row.role == "session_meta" || row.role.is_empty() {
            continue;
        }
        let Some(message) = message_from_row(row, state_db_path, profile) else {
            continue;
        };
        let batch = by_session.entry(row.session_id.clone()).or_insert_with(|| {
            order.push(row.session_id.clone());
            TranscriptBatch {
                session: session_from_row(row, state_db_path, project_root, profile),
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

fn session_from_row(
    row: &HermesRow,
    state_db_path: &str,
    project_root: &Path,
    profile: Option<&str>,
) -> SessionRecord {
    let mut metadata = Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("hermes_state_db".to_string()),
    );
    if let Some(profile) = profile {
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
    let project = project_root.to_string_lossy().to_string();
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
        parent_session_id: row
            .parent_session_id
            .as_deref()
            .filter(|parent| !parent.is_empty())
            .map(str::to_string),
        is_subagent: false,
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
    profile: Option<&str>,
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
    if let Some(profile) = profile {
        metadata.insert("profile".to_string(), Value::String(profile.to_string()));
    }
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

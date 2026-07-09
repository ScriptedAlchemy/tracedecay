//! One-off self-heal that re-derives per-message **timestamps** and **token
//! usage counters** for legacy messages ingested before extraction existed.
//!
//! Two gaps motivate this pass:
//!
//! * Cursor transcript JSONL carries no structured timestamps, so every row
//!   ingested by older builds has `timestamp = NULL` in both
//!   `session_messages` and `lcm_raw_messages` — which collapsed the
//!   dashboard's per-day timeline into a single bucket.
//! * No source extracted transcript-recorded token usage into
//!   `metadata_json.usage`, so the savings dashboard had to estimate costs
//!   (chars/4) even where the transcripts record real counters (Claude
//!   `message.usage`, Codex `token_count` events).
//!
//! Incremental parse offsets prevent a natural re-read from ever revisiting
//! those lines, so this pass re-reads each affected transcript file from the
//! start with the same derivation logic live ingest now uses, matching rows
//! by their stored `source_offset`. One re-read populates both facts.
//!
//! Mirrors the LCM schema self-heal pattern: runs once per store (marker row
//! in `session_schema_migrations`), is fail-open (a missing or unreadable
//! transcript file simply leaves its rows as-is), and never overwrites an
//! existing timestamp or usage object — Hermes-migrated messages keep the
//! values their migration derived.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use libsql::{Connection, params};
use serde_json::Value;

use crate::global_db::GlobalDb;
use crate::sessions::SessionMessageRecord;
use crate::sessions::codex::{CodexTurnUsage, merge_usage_counters};
use crate::sessions::cursor::TimestampCarry;
use crate::sessions::shared::usage_counters_from;
use crate::sessions::source::{StoredCursor, TranscriptSource};

const MARKER_NAME: &str = "transcript_facts_backfill";
const MARKER_VERSION: i64 = 1;
/// Superseded by [`MARKER_NAME`]: the timestamps-only pass shipped briefly on
/// this branch; its marker row is removed when the combined pass completes.
const LEGACY_MARKER_NAME: &str = "cursor_timestamp_backfill";

/// Providers whose transcripts are append-only JSONL matched by byte offset
/// (the `source_offset` live ingest stores). Cline-like sources rewrite whole
/// JSON arrays (index offsets) and their parsed file carries no counters, so
/// they are not re-read here.
const JSONL_PROVIDERS: [&str; 4] = ["cursor", "claude", "codex", "vibe"];

/// Facts re-derived for one transcript line.
#[derive(Default)]
struct LineFacts {
    timestamp: Option<i64>,
    usage: Option<Value>,
}

/// Counts of rows that gained each fact.
#[derive(Default, Clone, Copy)]
pub(crate) struct BackfillStats {
    pub(crate) dated: u64,
    pub(crate) usage_added: u64,
}

/// Runs the backfill if this store has not completed it yet. Returns the
/// number of rows that gained facts, or `None` on database errors (in which
/// case the marker is not written and a later open retries).
pub(crate) async fn backfill_transcript_facts(conn: &Connection) -> Option<BackfillStats> {
    if marker_version(conn, MARKER_NAME).await >= MARKER_VERSION {
        return Some(BackfillStats::default());
    }

    let candidates = load_candidates(conn).await?;

    // Re-derive per-line facts file by file *before* opening the write
    // transaction; transcripts that no longer exist drop out here and their
    // rows simply stay as they are. The first run after an upgrade re-reads
    // every affected transcript from byte 0 — easily hundreds of MB of
    // JSONL — so the pure read+parse loop runs on the blocking pool instead
    // of pinning the async runtime worker that called `open_at`.
    let mut by_file: HashMap<(String, String), Vec<(String, i64)>> = HashMap::new();
    for (provider, message_id, source_path, source_offset) in candidates {
        by_file
            .entry((provider, source_path))
            .or_default()
            .push((message_id, source_offset));
    }
    let updates = tokio::task::spawn_blocking(move || {
        let mut updates: Vec<(String, String, LineFacts)> = Vec::new();
        for ((provider, path), rows) in by_file {
            let Some(mut line_facts) = derive_line_facts(&provider, Path::new(&path)) else {
                continue;
            };
            for (message_id, source_offset) in rows {
                if let Some(facts) = line_facts.remove(&source_offset) {
                    if facts.timestamp.is_some() || facts.usage.is_some() {
                        updates.push((provider.clone(), message_id, facts));
                    }
                }
            }
        }
        updates
    })
    .await
    .ok()?;

    conn.execute("BEGIN IMMEDIATE", ()).await.ok()?;
    let applied = apply_updates(conn, &updates).await;
    let Some(stats) = applied else {
        let _ = conn.execute("ROLLBACK", ()).await;
        return None;
    };
    if conn.execute("COMMIT", ()).await.is_err() {
        let _ = conn.execute("ROLLBACK", ()).await;
        return None;
    }
    if stats.dated > 0 || stats.usage_added > 0 {
        eprintln!(
            "Backfilled {} timestamp(s) and {} usage record(s) for legacy messages from transcripts.",
            stats.dated, stats.usage_added
        );
    }
    Some(stats)
}

async fn marker_version(conn: &Connection, name: &str) -> i64 {
    let Ok(mut rows) = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![name],
        )
        .await
    else {
        return 0;
    };
    match rows.next().await {
        Ok(Some(row)) => row.get(0).unwrap_or(0),
        _ => 0,
    }
}

/// Messages that still know where they came from and are missing a fact this
/// pass can derive: `(provider, message_id, source_path, source_offset)`.
/// A row qualifies when either projection is undated or its metadata lacks a
/// `usage` object.
async fn load_candidates(conn: &Connection) -> Option<Vec<(String, String, String, i64)>> {
    let providers = JSONL_PROVIDERS
        .map(|provider| format!("'{provider}'"))
        .join(", ");
    let sql = format!(
        "SELECT sm.provider, sm.message_id, sm.source_path, sm.source_offset
         FROM session_messages sm
         WHERE sm.provider IN ({providers})
           AND sm.source_path IS NOT NULL
           AND sm.source_offset IS NOT NULL
           AND (sm.timestamp IS NULL
                OR sm.metadata_json IS NULL
                OR NOT json_valid(sm.metadata_json)
                OR json_extract(sm.metadata_json, '$.usage') IS NULL
                OR EXISTS (
                    SELECT 1 FROM lcm_raw_messages r
                    WHERE r.provider = sm.provider
                      AND r.message_id = sm.message_id
                      AND (r.timestamp IS NULL
                           OR r.metadata_json IS NULL
                           OR NOT json_valid(r.metadata_json)
                           OR json_extract(r.metadata_json, '$.usage') IS NULL)))"
    );
    let mut rows = conn.query(&sql, ()).await.ok()?;
    let mut candidates = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        let (Ok(provider), Ok(message_id), Ok(source_path), Ok(source_offset)) = (
            row.get::<String>(0),
            row.get::<String>(1),
            row.get::<String>(2),
            row.get::<i64>(3),
        ) else {
            continue;
        };
        candidates.push((provider, message_id, source_path, source_offset));
    }
    Some(candidates)
}

async fn apply_updates(
    conn: &Connection,
    updates: &[(String, String, LineFacts)],
) -> Option<BackfillStats> {
    let mut stats = BackfillStats::default();
    for (provider, message_id, facts) in updates {
        if let Some(timestamp) = facts.timestamp {
            stats.dated += conn
                .execute(
                    "UPDATE session_messages SET timestamp = ?1
                     WHERE provider = ?2 AND message_id = ?3 AND timestamp IS NULL",
                    params![timestamp, provider.as_str(), message_id.as_str()],
                )
                .await
                .ok()?;
            conn.execute(
                "UPDATE lcm_raw_messages SET timestamp = ?1
                 WHERE provider = ?2 AND message_id = ?3 AND timestamp IS NULL",
                params![timestamp, provider.as_str(), message_id.as_str()],
            )
            .await
            .ok()?;
        }
        if let Some(usage) = &facts.usage {
            let usage_json = serde_json::to_string(usage).ok()?;
            // `json_set` preserves the other metadata keys; invalid or
            // missing metadata degrades to a fresh `{"usage": …}` object.
            for table in ["session_messages", "lcm_raw_messages"] {
                let updated = conn
                    .execute(
                        &format!(
                            "UPDATE {table} SET metadata_json = json_set(
                                CASE WHEN metadata_json IS NOT NULL AND json_valid(metadata_json)
                                     THEN metadata_json ELSE '{{}}' END,
                                '$.usage', json(?1))
                             WHERE provider = ?2 AND message_id = ?3
                               AND (metadata_json IS NULL
                                    OR NOT json_valid(metadata_json)
                                    OR json_extract(metadata_json, '$.usage') IS NULL)"
                        ),
                        params![usage_json.as_str(), provider.as_str(), message_id.as_str()],
                    )
                    .await
                    .ok()?;
                if table == "session_messages" {
                    stats.usage_added += updated;
                }
            }
        }
    }

    // Sessions ingested while messages were undated also have NULL
    // started_at/ended_at; derive them from the freshly dated messages.
    let providers = JSONL_PROVIDERS
        .map(|provider| format!("'{provider}'"))
        .join(", ");
    conn.execute(
        &format!(
            "UPDATE sessions SET
                started_at = COALESCE(started_at,
                    (SELECT MIN(r.timestamp) FROM lcm_raw_messages r
                     WHERE r.provider = sessions.provider AND r.session_id = sessions.session_id)),
                ended_at = COALESCE(ended_at,
                    (SELECT MAX(r.timestamp) FROM lcm_raw_messages r
                     WHERE r.provider = sessions.provider AND r.session_id = sessions.session_id))
             WHERE provider IN ({providers}) AND (started_at IS NULL OR ended_at IS NULL)"
        ),
        (),
    )
    .await
    .ok()?;

    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET
            version = excluded.version,
            applied_at = unixepoch()",
        params![MARKER_NAME, MARKER_VERSION],
    )
    .await
    .ok()?;
    conn.execute(
        "DELETE FROM session_schema_migrations WHERE name = ?1",
        params![LEGACY_MARKER_NAME],
    )
    .await
    .ok()?;
    Some(stats)
}

/// Re-reads a transcript from byte 0 and derives per-line facts keyed by the
/// line's starting byte offset (the same offset live ingest stores as
/// `source_offset`), using the same extraction rules as live ingest.
fn derive_line_facts(provider: &str, path: &Path) -> Option<HashMap<i64, LineFacts>> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);

    let mut carry = TimestampCarry::new(mtime);
    let mut facts: HashMap<i64, LineFacts> = HashMap::new();
    // For Codex, a turn's `token_count` events are summed and flushed onto the
    // turn's `agent_message` line at turn boundaries, mirroring live ingest.
    let mut last_assistant_offset: Option<i64> = None;
    let mut codex_turn_usage = CodexTurnUsage::default();
    let mut offset = 0i64;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                // A trailing line without a newline was never ingested
                // (stream_new_jsonl defers partial writes), so skip it.
                if !line.ends_with('\n') {
                    break;
                }
                let line_offset = offset;
                offset = offset.saturating_add(read as i64);
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };

                let mut line_facts = LineFacts {
                    timestamp: derive_timestamp(provider, &value, &mut carry),
                    usage: derive_usage(provider, &value),
                };
                if provider == "codex" {
                    if codex_turn_usage.observe(&value) {
                        continue;
                    }
                    match value.pointer("/payload/type").and_then(Value::as_str) {
                        // A new user prompt closes the previous turn.
                        Some("user_message") => flush_codex_turn_usage(
                            &mut facts,
                            last_assistant_offset,
                            &mut codex_turn_usage,
                        ),
                        Some("agent_message") => last_assistant_offset = Some(line_offset),
                        _ => {}
                    }
                    line_facts.usage = None;
                }
                facts.insert(line_offset, line_facts);
            }
        }
    }
    if provider == "codex" {
        // The final turn's trailing token_count(s) follow its agent_message.
        flush_codex_turn_usage(&mut facts, last_assistant_offset, &mut codex_turn_usage);
    }
    Some(facts)
}

/// Attach a finished Codex turn's summed usage to its assistant line's facts,
/// merging additively when several flushes land on the same line.
fn flush_codex_turn_usage(
    facts: &mut HashMap<i64, LineFacts>,
    assistant_offset: Option<i64>,
    turn_usage: &mut CodexTurnUsage,
) {
    let Some(usage) = turn_usage.take() else {
        return;
    };
    let Some(offset) = assistant_offset else {
        return;
    };
    let entry = facts.entry(offset).or_default();
    match entry.usage.as_mut() {
        Some(existing) => merge_usage_counters(existing, &usage),
        None => entry.usage = Some(usage),
    }
}

/// Per-provider timestamp derivation, mirroring each source's live ingest.
fn derive_timestamp(provider: &str, record: &Value, carry: &mut TimestampCarry) -> Option<i64> {
    match provider {
        // Cursor: `<timestamp>` tag carry-forward with mtime fallback.
        "cursor" => carry.observe(record),
        // Claude/Codex: ISO-8601 `timestamp` on every line.
        "claude" | "codex" => record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(crate::accounting::parser::parse_timestamp)
            .and_then(|secs| i64::try_from(secs).ok()),
        // Vibe: numeric `ts`/`timestamp`/`created_at`.
        "vibe" => record
            .get("ts")
            .or_else(|| record.get("timestamp"))
            .or_else(|| record.get("created_at"))
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
            }),
        _ => None,
    }
}

/// Per-provider usage derivation (Codex's event-attached usage is handled in
/// [`derive_line_facts`] instead, because it lives on a *different* line).
fn derive_usage(provider: &str, record: &Value) -> Option<Value> {
    match provider {
        "claude" => usage_counters_from(record.get("message").unwrap_or(record)),
        "cursor" | "vibe" => usage_counters_from(record)
            .or_else(|| record.get("message").and_then(usage_counters_from)),
        _ => None,
    }
}

// Structured-row backfill replays stored Claude/Codex transcripts through the
// current parser and inserts message ids missing from legacy stores.

const STRUCTURED_MARKER_NAME: &str = "structured_rows_backfill";
const STRUCTURED_MARKER_VERSION: i64 = 2;
/// Base name of the sweep's path watermark. The live key is namespaced by the
/// marker version (see [`structured_cursor_key`]) so bumping
/// [`STRUCTURED_MARKER_VERSION`] naturally starts the re-sweep from a fresh
/// (never-written) cursor instead of resuming past the last file the prior
/// version already covered.
const STRUCTURED_CURSOR_KEY_PREFIX: &str = "structured_backfill_cursor";
const STRUCTURED_BACKFILL_BATCH: usize = 32;
/// Transcripts larger than this are skipped (with a logged warning and a cursor
/// advance) rather than materialized whole. Threading a byte offset through the
/// watermark would balloon the diff, so we cap file size instead — pathological
/// multi-hundred-MB JSONL transcripts are the only ones affected.
const STRUCTURED_BACKFILL_MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const STRUCTURED_PROVIDERS: [&str; 2] = ["claude", "codex"];

/// Version-namespaced watermark key. Because the version is part of the key, a
/// bump to [`STRUCTURED_MARKER_VERSION`] yields a key that has never been
/// written, so [`read_backfill_cursor`] returns the empty string and the sweep
/// re-parses the whole history from the start.
fn structured_cursor_key() -> String {
    format!("{STRUCTURED_CURSOR_KEY_PREFIX}:v{STRUCTURED_MARKER_VERSION}")
}

#[derive(Default, Clone, Copy)]
pub(crate) struct StructuredBackfillStats {
    pub(crate) inserted: u64,
    pub(crate) files_scanned: u64,
}

struct StructuredCandidate {
    provider: String,
    source_path: String,
}

/// Re-parses the next bounded transcript batch and inserts rows missing from
/// legacy stores.
pub(crate) async fn backfill_structured_rows(db: &GlobalDb) -> Option<StructuredBackfillStats> {
    let conn = db.conn();
    if marker_version(conn, STRUCTURED_MARKER_NAME).await >= STRUCTURED_MARKER_VERSION {
        return Some(StructuredBackfillStats::default());
    }
    ensure_backfill_meta_table(conn).await?;
    let cursor_key = structured_cursor_key();
    let cursor = read_backfill_cursor(conn, &cursor_key).await;
    let candidates = load_structured_candidates(conn, &cursor, STRUCTURED_BACKFILL_BATCH).await?;
    if candidates.is_empty() {
        mark_structured_backfill_complete(conn).await?;
        return Some(StructuredBackfillStats::default());
    }

    let mut stats = StructuredBackfillStats::default();
    for candidate in &candidates {
        // Bound memory cheaply: an oversized transcript would be materialized
        // whole by the full-file parse below, so skip it (and advance past it)
        // rather than risk pinning hundreds of MB per parse.
        if let Ok(meta) = std::fs::metadata(&candidate.source_path) {
            if meta.len() > STRUCTURED_BACKFILL_MAX_FILE_BYTES {
                eprintln!(
                    "Structured backfill: skipping oversized transcript ({} bytes > {STRUCTURED_BACKFILL_MAX_FILE_BYTES} cap): {}",
                    meta.len(),
                    candidate.source_path
                );
                stats.files_scanned += 1;
                write_backfill_cursor(conn, &cursor_key, &candidate.source_path).await?;
                continue;
            }
        }

        let project_paths =
            load_project_paths_for_source(conn, &candidate.provider, &candidate.source_path)
                .await?;
        for project_path in project_paths {
            let project_root = PathBuf::from(&project_path);
            let provider = candidate.provider.clone();
            let source_path = candidate.source_path.clone();
            let messages = match tokio::task::spawn_blocking(move || {
                parse_structured_messages(&provider, &source_path, &project_path)
            })
            .await
            {
                // The parser ran to completion: rows to insert, or a clean
                // decline (foreign/missing transcript) that yields nothing.
                Ok(parsed) => parsed.unwrap_or_default(),
                // The parser panicked on this file — a deterministic per-file
                // failure. Holding the cursor here would re-poison every future
                // open and starve all lexically-later files, so log it and fall
                // through to advance past the file (it self-heals on a future
                // marker-version bump). Environment errors take a different
                // path: `insert_absent_session_messages` returns `None` below,
                // which propagates and holds the cursor for a later retry.
                Err(join_error) => {
                    eprintln!(
                        "Structured backfill: skipping transcript that failed to re-parse ({}): {join_error}",
                        candidate.source_path
                    );
                    break;
                }
            };
            if messages.is_empty() {
                continue;
            }
            let commit_records =
                crate::sessions::git_correlation::direct_commit_records(&messages, &project_root);
            let span_observations =
                crate::sessions::git_correlation::ingest_span_observations(&messages);
            let inserted = db.insert_absent_session_messages(&messages).await?;
            stats.inserted += inserted;
            for record in &commit_records {
                db.git_upsert_commit_session(record).await.ok()?;
            }
            for observation in &span_observations {
                db.git_record_span_observation(
                    observation,
                    crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
                )
                .await
                .ok()?;
            }
        }
        stats.files_scanned += 1;
        write_backfill_cursor(conn, &cursor_key, &candidate.source_path).await?;
    }

    if stats.inserted > 0 {
        eprintln!(
            "Backfilled {} structured transcript row(s) across {} file(s).",
            stats.inserted, stats.files_scanned
        );
    }
    Some(stats)
}

fn parse_structured_messages(
    provider: &str,
    source_path: &str,
    project_path: &str,
) -> Option<Vec<SessionMessageRecord>> {
    let source = provider_source(provider)?;
    let parsed = source.parse_new(
        Path::new(source_path),
        StoredCursor::default(),
        Path::new(project_path),
        None,
    )?;
    Some(parsed.messages)
}

fn provider_source(provider: &str) -> Option<Box<dyn TranscriptSource>> {
    let home = crate::sessions::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    match provider {
        "claude" => Some(Box::new(crate::sessions::claude::ClaudeSource::with_home(
            &home,
        ))),
        "codex" => Some(Box::new(crate::sessions::codex::CodexSource::with_home(
            &home,
        ))),
        _ => None,
    }
}

async fn load_structured_candidates(
    conn: &Connection,
    after_path: &str,
    limit: usize,
) -> Option<Vec<StructuredCandidate>> {
    let providers = STRUCTURED_PROVIDERS
        .map(|provider| format!("'{provider}'"))
        .join(", ");
    let sql = format!(
        "SELECT DISTINCT sm.source_path, sm.provider
         FROM session_messages sm
         WHERE sm.provider IN ({providers})
           AND sm.source_path IS NOT NULL
           AND sm.source_path > ?1
         ORDER BY sm.source_path
         LIMIT ?2"
    );
    let mut rows = conn
        .query(&sql, params![after_path, limit as i64])
        .await
        .ok()?;
    let mut out = Vec::new();
    // Match on `next()` explicitly: a mid-iteration `Err` must abort with `None`
    // (this function's documented contract), not silently truncate — a partial
    // list looks like fewer candidates and, once empty, would wrongly mark the
    // whole sweep complete and advance the watermark past unscanned files.
    loop {
        match rows.next().await {
            Ok(Some(row)) => {
                let (Ok(source_path), Ok(provider)) = (row.get::<String>(0), row.get::<String>(1))
                else {
                    continue;
                };
                out.push(StructuredCandidate {
                    provider,
                    source_path,
                });
            }
            Ok(None) => break,
            Err(_) => return None,
        }
    }
    Some(out)
}

async fn load_project_paths_for_source(
    conn: &Connection,
    provider: &str,
    source_path: &str,
) -> Option<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT s.project_path
             FROM session_messages sm
             JOIN sessions s
               ON s.provider = sm.provider AND s.session_id = sm.session_id
             WHERE sm.provider = ?1
               AND sm.source_path = ?2
               AND s.project_path IS NOT NULL
               AND s.project_path <> ''",
            params![provider, source_path],
        )
        .await
        .ok()?;
    let mut out = Vec::new();
    // As above: a mid-iteration `Err` must abort with `None` rather than drop
    // project roots silently — a truncated list would parse against fewer cwds
    // and then advance the watermark past the file forever.
    loop {
        match rows.next().await {
            Ok(Some(row)) => {
                if let Ok(project_path) = row.get::<String>(0) {
                    out.push(project_path);
                }
            }
            Ok(None) => break,
            Err(_) => return None,
        }
    }
    Some(out)
}

async fn ensure_backfill_meta_table(conn: &Connection) -> Option<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS session_backfill_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        (),
    )
    .await
    .ok()?;
    Some(())
}

async fn read_backfill_cursor(conn: &Connection, key: &str) -> String {
    let Ok(mut rows) = conn
        .query(
            "SELECT value FROM session_backfill_meta WHERE key = ?1",
            params![key],
        )
        .await
    else {
        return String::new();
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<String>(0).unwrap_or_default(),
        _ => String::new(),
    }
}

async fn write_backfill_cursor(conn: &Connection, key: &str, value: &str) -> Option<()> {
    conn.execute(
        "INSERT INTO session_backfill_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
        params![key, value],
    )
    .await
    .ok()?;
    Some(())
}

async fn mark_structured_backfill_complete(conn: &Connection) -> Option<()> {
    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET
            version = excluded.version,
            applied_at = unixepoch()",
        params![STRUCTURED_MARKER_NAME, STRUCTURED_MARKER_VERSION],
    )
    .await
    .ok()?;
    // The sweep is done, so drop every watermark row for it: this version's
    // key and any stale prior-version (or legacy un-versioned) keys. Keeps the
    // meta table from accumulating dead cursors across marker-version bumps.
    conn.execute(
        "DELETE FROM session_backfill_meta WHERE key = ?1 OR key LIKE ?2",
        params![
            STRUCTURED_CURSOR_KEY_PREFIX,
            format!("{STRUCTURED_CURSOR_KEY_PREFIX}:%")
        ],
    )
    .await
    .ok()?;
    Some(())
}

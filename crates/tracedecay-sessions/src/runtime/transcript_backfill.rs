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
//! by their stored `source_offset`. Each bounded page retains facts only for
//! that page's source offsets.
//!
//! Mirrors the LCM schema self-heal pattern: background maintenance advances
//! one durable, atomic page per tick until it writes the store marker in
//! `session_schema_migrations`. A missing or unreadable transcript file simply
//! leaves its rows as-is, and the pass never overwrites an existing timestamp
//! or usage object — Hermes-migrated messages keep the values their migration
//! derived.

use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::ProjectId;
use tracedecay_store::StoreShardScopeV1;

use crate::db::engine::{
    Error as EngineError, Executor, QueryExecutor, Result as EngineResult, params,
};
use crate::global_db::RegisteredGlobalDb;
use crate::runtime::codex::{CodexTurnUsage, merge_usage_counters};
use crate::runtime::cursor::TimestampCarry;
use crate::runtime::shared::usage_counters_from;
use crate::runtime::source::{
    MAX_JSONL_RECORD_BYTES, ParsedTranscript, RawJsonlFrame, RawJsonlFrameReader, StoredCursor,
    TranscriptIngestResult, TranscriptSource,
};

const MARKER_NAME: &str = "transcript_facts_backfill";
const MARKER_VERSION: i64 = 1;
const CURSOR_KEY: &str = "transcript_facts_backfill_cursor:v1";
const TRANSCRIPT_FACTS_BATCH: usize = 256;
#[cfg(test)]
const SAVEPOINT_NAME: &str = "transcript_facts_backfill_batch";
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
pub struct BackfillStats {
    pub dated: u64,
    pub usage_added: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptFactsBackfillOutcome {
    NotRequired,
    Pending { cursor: i64 },
    Complete,
}

struct TranscriptFactCandidate {
    rowid: i64,
    provider: String,
    message_id: String,
    source_path: String,
    source_offset: i64,
}

#[cfg(test)]
#[derive(Debug)]
pub struct BackfillError {
    primary: EngineError,
    rollback_cleanup: Option<EngineError>,
}

#[cfg(test)]
impl BackfillError {
    pub const fn atomicity_preserved(&self) -> bool {
        self.rollback_cleanup.is_none()
    }
}

#[cfg(test)]
impl From<EngineError> for BackfillError {
    fn from(primary: EngineError) -> Self {
        Self {
            primary,
            rollback_cleanup: None,
        }
    }
}

#[cfg(test)]
impl std::fmt::Display for BackfillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.primary)?;
        if let Some(rollback_cleanup) = &self.rollback_cleanup {
            write!(
                formatter,
                "; transcript backfill rollback cleanup failed: {rollback_cleanup}"
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
impl std::error::Error for BackfillError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.primary)
    }
}

pub async fn transcript_facts_backfill_status(
    db: &RegisteredGlobalDb,
) -> crate::errors::Result<TranscriptFactsBackfillOutcome> {
    if !matches!(
        &db.binding().shard_id.scope,
        StoreShardScopeV1::ProjectSessions { .. } | StoreShardScopeV1::ProfileSessions
    ) {
        return Ok(TranscriptFactsBackfillOutcome::NotRequired);
    }
    let snapshot = db
        .read_snapshot()
        .await
        .map_err(|error| backfill_error("open transcript facts backfill status", error))?;
    if required_marker_version(&snapshot, MARKER_NAME)
        .await
        .map_err(|error| backfill_error("read transcript facts backfill marker", error))?
        >= MARKER_VERSION
    {
        return Ok(TranscriptFactsBackfillOutcome::NotRequired);
    }
    let cursor = read_transcript_facts_cursor(&snapshot)
        .await
        .map_err(|error| backfill_error("read transcript facts backfill cursor", error))?;
    Ok(TranscriptFactsBackfillOutcome::Pending { cursor })
}

pub async fn advance_transcript_facts_backfill(
    db: &RegisteredGlobalDb,
) -> crate::errors::Result<TranscriptFactsBackfillOutcome> {
    advance_transcript_facts_backfill_with_limit(db, TRANSCRIPT_FACTS_BATCH).await
}

async fn advance_transcript_facts_backfill_with_limit(
    db: &RegisteredGlobalDb,
    limit: usize,
) -> crate::errors::Result<TranscriptFactsBackfillOutcome> {
    let TranscriptFactsBackfillOutcome::Pending { cursor } =
        transcript_facts_backfill_status(db).await?
    else {
        return Ok(TranscriptFactsBackfillOutcome::NotRequired);
    };
    let snapshot = db
        .read_snapshot()
        .await
        .map_err(|error| backfill_error("open transcript facts candidate snapshot", error))?;
    let candidates = load_candidates(&snapshot, cursor, limit.max(1))
        .await
        .map_err(|error| backfill_error("load transcript facts backfill candidates", error))?;
    drop(snapshot);
    if candidates.is_empty() {
        mark_transcript_facts_backfill_complete(db).await?;
        return Ok(TranscriptFactsBackfillOutcome::Complete);
    }

    let next_cursor = candidates
        .last()
        .map_or(cursor, |candidate| candidate.rowid);
    let updates = tokio::task::spawn_blocking(move || derive_candidate_updates(candidates))
        .await
        .map_err(|error| {
            backfill_error(
                "parse transcript facts backfill batch",
                format!("parser task failed: {error}"),
            )
        })?;
    let transaction = db.begin_write_transaction().await?;
    let applied: EngineResult<BackfillStats> = async {
        let stats = apply_updates(&transaction, &updates).await?;
        write_transcript_facts_cursor(&transaction, next_cursor).await?;
        Ok(stats)
    }
    .await;
    let stats = match applied {
        Ok(stats) => {
            transaction.commit().await?;
            stats
        }
        Err(error) => {
            transaction.rollback().await.map_err(|rollback_error| {
                backfill_error(
                    "roll back transcript facts backfill batch",
                    format!("{rollback_error}; original batch failure: {error}"),
                )
            })?;
            return Err(backfill_error(
                "apply transcript facts backfill batch",
                error,
            ));
        }
    };
    if stats.dated > 0 || stats.usage_added > 0 {
        tracing::info!(
            cursor = next_cursor,
            timestamps = stats.dated,
            usage_records = stats.usage_added,
            "backfilled legacy transcript message facts batch"
        );
    }
    Ok(TranscriptFactsBackfillOutcome::Pending {
        cursor: next_cursor,
    })
}

fn derive_candidate_updates(
    candidates: Vec<TranscriptFactCandidate>,
) -> Vec<(String, String, LineFacts)> {
    let mut by_file: HashMap<(String, String), Vec<(String, i64)>> = HashMap::new();
    for candidate in candidates {
        by_file
            .entry((candidate.provider, candidate.source_path))
            .or_default()
            .push((candidate.message_id, candidate.source_offset));
    }
    let mut updates = Vec::new();
    for ((provider, path), rows) in by_file {
        let target_offsets = rows.iter().map(|(_, offset)| *offset).collect();
        let Some(mut line_facts) = derive_line_facts(&provider, Path::new(&path), &target_offsets)
        else {
            continue;
        };
        for (message_id, source_offset) in rows {
            if let Some(facts) = line_facts.remove(&source_offset)
                && (facts.timestamp.is_some() || facts.usage.is_some())
            {
                updates.push((provider.clone(), message_id, facts));
            }
        }
    }
    updates
}

fn backfill_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

async fn required_marker_version(
    conn: &(impl QueryExecutor + ?Sized),
    name: &str,
) -> EngineResult<i64> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![name],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(0);
    };
    row.get(0)
}

async fn marker_version(conn: &(impl QueryExecutor + ?Sized), name: &str) -> i64 {
    required_marker_version(conn, name).await.unwrap_or(0)
}

async fn read_transcript_facts_cursor(conn: &(impl QueryExecutor + ?Sized)) -> EngineResult<i64> {
    let mut rows = conn
        .query(
            "SELECT value FROM session_backfill_meta WHERE key = ?1",
            params![CURSOR_KEY],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(0);
    };
    let value = row.get::<String>(0)?;
    value.parse::<i64>().map_err(|error| {
        EngineError::Runtime(format!(
            "invalid transcript facts backfill cursor '{value}': {error}"
        ))
    })
}

async fn write_transcript_facts_cursor(
    conn: &(impl Executor + ?Sized),
    cursor: i64,
) -> EngineResult<()> {
    conn.execute(
        "INSERT INTO session_backfill_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at = unixepoch()
         WHERE CAST(excluded.value AS INTEGER) >
               CAST(session_backfill_meta.value AS INTEGER)",
        params![CURSOR_KEY, cursor.to_string()],
    )
    .await?;
    Ok(())
}

/// Messages that still know where they came from and are missing a fact this
/// pass can derive: `(provider, message_id, source_path, source_offset)`.
/// A row qualifies when either projection is undated or its metadata lacks a
/// `usage` object.
async fn load_candidates(
    conn: &(impl QueryExecutor + ?Sized),
    after_rowid: i64,
    limit: usize,
) -> EngineResult<Vec<TranscriptFactCandidate>> {
    let providers = JSONL_PROVIDERS
        .map(|provider| format!("'{provider}'"))
        .join(", ");
    let sql = format!(
        "SELECT sm.rowid, sm.provider, sm.message_id, sm.source_path, sm.source_offset
         FROM session_messages sm
         WHERE sm.rowid > ?1
           AND sm.provider IN ({providers})
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
                           OR json_extract(r.metadata_json, '$.usage') IS NULL)))
         ORDER BY sm.rowid
         LIMIT ?2"
    );
    let mut rows = conn.query(&sql, params![after_rowid, limit as i64]).await?;
    let mut candidates = Vec::new();
    while let Some(row) = rows.next().await? {
        candidates.push(TranscriptFactCandidate {
            rowid: row.get(0)?,
            provider: row.get(1)?,
            message_id: row.get(2)?,
            source_path: row.get(3)?,
            source_offset: row.get(4)?,
        });
    }
    Ok(candidates)
}

#[cfg(test)]
async fn apply_updates_atomically(
    conn: &(impl Executor + ?Sized),
    updates: &[(String, String, LineFacts)],
) -> Result<BackfillStats, BackfillError> {
    conn.execute_batch(&format!("SAVEPOINT {SAVEPOINT_NAME}"))
        .await?;
    match apply_updates(conn, updates).await {
        Ok(stats) => match conn
            .execute_batch(&format!("RELEASE SAVEPOINT {SAVEPOINT_NAME}"))
            .await
        {
            Ok(()) => Ok(stats),
            Err(error) => rollback_updates(conn, error).await,
        },
        Err(error) => rollback_updates(conn, error).await,
    }
}

#[cfg(test)]
async fn rollback_updates(
    conn: &(impl Executor + ?Sized),
    primary: EngineError,
) -> Result<BackfillStats, BackfillError> {
    if let Err(rollback) = conn
        .execute_batch(&format!("ROLLBACK TO SAVEPOINT {SAVEPOINT_NAME}"))
        .await
    {
        return Err(BackfillError {
            primary,
            rollback_cleanup: Some(rollback),
        });
    }
    if let Err(rollback_cleanup) = conn
        .execute_batch(&format!("RELEASE SAVEPOINT {SAVEPOINT_NAME}"))
        .await
    {
        return Err(BackfillError {
            primary,
            rollback_cleanup: Some(rollback_cleanup),
        });
    }
    Err(primary.into())
}

async fn apply_updates(
    conn: &(impl Executor + ?Sized),
    updates: &[(String, String, LineFacts)],
) -> EngineResult<BackfillStats> {
    let mut stats = BackfillStats::default();
    for (provider, message_id, facts) in updates {
        if let Some(timestamp) = facts.timestamp {
            stats.dated += conn
                .execute(
                    "UPDATE session_messages SET timestamp = ?1
                     WHERE provider = ?2 AND message_id = ?3 AND timestamp IS NULL",
                    params![timestamp, provider.as_str(), message_id.as_str()],
                )
                .await?;
            conn.execute(
                "UPDATE lcm_raw_messages SET timestamp = ?1
                 WHERE provider = ?2 AND message_id = ?3 AND timestamp IS NULL",
                params![timestamp, provider.as_str(), message_id.as_str()],
            )
            .await?;
        }
        if let Some(usage) = &facts.usage {
            let usage_json = serde_json::to_string(usage).map_err(|error| {
                EngineError::Runtime(format!("serialize transcript usage facts: {error}"))
            })?;
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
                    .await?;
                if table == "session_messages" {
                    stats.usage_added += updated;
                }
            }
        }
    }

    Ok(stats)
}

async fn update_session_windows(conn: &(impl Executor + ?Sized)) -> EngineResult<()> {
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
    .await?;
    Ok(())
}

async fn mark_transcript_facts_backfill_complete(
    db: &RegisteredGlobalDb,
) -> crate::errors::Result<()> {
    let transaction = db.begin_write_transaction().await?;
    let completed: EngineResult<()> = async {
        // Sessions ingested while messages were undated also have NULL
        // started_at/ended_at; derive them once after every message page is
        // durable, not once per bounded batch.
        update_session_windows(&transaction).await?;
        transaction
            .execute(
                "INSERT INTO session_schema_migrations(name, version)
                 VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET
                    version = excluded.version,
                    applied_at = unixepoch()",
                params![MARKER_NAME, MARKER_VERSION],
            )
            .await?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations WHERE name = ?1",
                params![LEGACY_MARKER_NAME],
            )
            .await?;
        transaction
            .execute(
                "DELETE FROM session_backfill_meta WHERE key = ?1",
                params![CURSOR_KEY],
            )
            .await?;
        Ok(())
    }
    .await;
    match completed {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|error| backfill_error("commit transcript facts backfill", error)),
        Err(error) => {
            transaction.rollback().await.map_err(|rollback_error| {
                backfill_error(
                    "roll back transcript facts backfill completion",
                    format!("{rollback_error}; original completion failure: {error}"),
                )
            })?;
            Err(backfill_error("complete transcript facts backfill", error))
        }
    }
}

/// Re-reads a transcript from byte 0 and derives per-line facts keyed by the
/// line's starting byte offset (the same offset live ingest stores as
/// `source_offset`), using the same extraction rules as live ingest.
fn derive_line_facts(
    provider: &str,
    path: &Path,
    target_offsets: &HashSet<i64>,
) -> Option<HashMap<i64, LineFacts>> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    let file = std::fs::File::open(path).ok()?;
    let mut frames = RawJsonlFrameReader::new(BufReader::new(file), MAX_JSONL_RECORD_BYTES);

    let mut carry = TimestampCarry::new(mtime);
    let mut facts: HashMap<i64, LineFacts> = HashMap::new();
    // For Codex, a turn's `token_count` events are summed and flushed onto the
    // turn's `agent_message` line at turn boundaries, mirroring live ingest.
    let mut last_assistant_offset: Option<i64> = None;
    let mut codex_turn_usage = CodexTurnUsage::default();
    let mut offset = 0i64;
    loop {
        match frames.next_frame() {
            Ok(RawJsonlFrame::Eof | RawJsonlFrame::Partial { .. }) | Err(_) => break,
            Ok(frame) => {
                let read = match frame {
                    RawJsonlFrame::Complete { byte_len }
                    | RawJsonlFrame::Oversized { byte_len, .. }
                    | RawJsonlFrame::BudgetExhausted { byte_len, .. } => byte_len,
                    RawJsonlFrame::Eof | RawJsonlFrame::Partial { .. } => 0,
                };
                let line_offset = offset;
                offset = offset.saturating_add(i64::try_from(read).unwrap_or(i64::MAX));
                if !matches!(frame, RawJsonlFrame::Complete { .. }) {
                    continue;
                }
                let Ok(value) = serde_json::from_slice::<Value>(frames.record()) else {
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
                if target_offsets.contains(&line_offset) {
                    facts.insert(line_offset, line_facts);
                }
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

// Structured-row backfill replays stored Codex transcripts through the current
// parser and inserts message ids missing from legacy stores. Claude is
// intentionally excluded: its observation pipeline is the sole production
// cursor authority and must not race an independent legacy replay.

/// Base name of the per-provider structured-backfill marker rows in
/// `session_schema_migrations`. Each provider gets its own row keyed
/// `structured_rows_backfill:<provider>` (see [`structured_marker_name`]); the
/// bare name is the retired global marker migrated away in
/// [`migrate_legacy_global_marker`].
const STRUCTURED_MARKER_NAME: &str = "structured_rows_backfill";
/// Per-provider structured-backfill target versions. Bumping one provider's
/// entry re-sweeps ONLY that provider's transcripts (its marker falls behind
/// its target and its version-namespaced cursor starts fresh); every other
/// provider stays untouched. This replaces the former single global
/// `STRUCTURED_MARKER_VERSION`, where any single-provider parser addition reset
/// the one shared cursor and re-parsed every provider's history.
///
/// Version history / in-flight-bump translation:
/// * `codex = 4` — v4 joins the Codex CLI `custom_tool_call` exec harness into
///   searchable `kind="tool_call"` rows. The version intentionally advances
///   past the former global v3 Claude bump while re-sweeping only Codex.
const STRUCTURED_BACKFILL_VERSIONS: &[(&str, i64)] = &[("codex", 4)];
/// Base name of the sweep's path watermark. The live key is namespaced by both
/// provider and target version (see [`structured_cursor_key`]) so bumping a
/// provider's entry in [`STRUCTURED_BACKFILL_VERSIONS`] naturally starts that
/// provider's re-sweep from a fresh (never-written) cursor instead of resuming
/// past the last file the prior version already covered.
const STRUCTURED_CURSOR_KEY_PREFIX: &str = "structured_backfill_cursor";
const WRITE_BACKFILL_CURSOR_SQL: &str =
    "INSERT INTO session_backfill_meta(key, value) VALUES (?1, ?2)
     ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()
        WHERE excluded.value > session_backfill_meta.value";
const STRUCTURED_BACKFILL_BATCH: usize = 32;
const STRUCTURED_BACKFILL_PARSE_BYTES: u64 = MAX_JSONL_RECORD_BYTES as u64 + 1;

/// Per-provider marker row name in `session_schema_migrations`.
fn structured_marker_name(provider: &str) -> String {
    format!("{STRUCTURED_MARKER_NAME}:{provider}")
}

/// Provider-scoped, version-namespaced watermark key. Because both the provider
/// and its target version are part of the key, bumping a provider's entry in
/// [`STRUCTURED_BACKFILL_VERSIONS`] yields a key that has never been written, so
/// [`read_backfill_cursor`] returns the empty string and only *that* provider's
/// sweep re-parses its whole history from the start.
fn structured_cursor_key(provider: &str, version: i64) -> String {
    format!("{STRUCTURED_CURSOR_KEY_PREFIX}:{provider}:v{version}")
}

/// Target version for one provider, or 0 when the provider is not tracked.
fn structured_backfill_target_version(provider: &str) -> i64 {
    STRUCTURED_BACKFILL_VERSIONS
        .iter()
        .find(|(name, _)| *name == provider)
        .map_or(0, |(_, version)| *version)
}

#[derive(Default, Clone, Copy)]
pub struct StructuredBackfillStats {
    pub inserted: u64,
    pub files_scanned: u64,
}

struct StructuredCandidate {
    provider: String,
    source_path: String,
}

/// Sibling `<store>.structured-backfill.lock` used to serialize the sweep
/// across processes. The in-process in-flight guard in
/// The daemon scheduler only excludes stacked passes within one process;
/// production runs many short-lived hook processes, so without a filesystem
/// lock two of them could sweep the same store at once and race the watermark
/// backwards.
fn structured_backfill_lock_path(db_path: &Path) -> PathBuf {
    let mut lock_name = db_path.file_name().map_or_else(
        || std::ffi::OsString::from("session"),
        std::ffi::OsStr::to_os_string,
    );
    lock_name.push(".structured-backfill.lock");
    db_path.with_file_name(lock_name)
}

/// Tries to claim the exclusive cross-process sweep lock for `db_path`. Returns
/// the held lock file on success (drop releases it; the OS also releases it if
/// the process dies), or `None` when another process/task already holds it — in
/// which case the caller simply skips its sweep. Uses an advisory `flock`
/// (`fs2`), the same primitive the branch-add and monitor single-instance
/// guards use, so a crashed holder never leaves a stale lock behind.
#[doc(hidden)]
pub fn try_acquire_structured_backfill_lock(db_path: &Path) -> Option<std::fs::File> {
    let lock_path = structured_backfill_lock_path(db_path);
    crate::storage::try_acquire_sidecar_lock(&lock_path)
        .ok()
        .flatten()
}

/// Re-parses the next bounded transcript batch and inserts rows missing from
/// legacy stores.
pub async fn backfill_structured_rows(
    db: &RegisteredGlobalDb,
) -> Option<StructuredBackfillStats> {
    if !matches!(
        &db.binding().shard_id.scope,
        StoreShardScopeV1::ProjectSessions { .. } | StoreShardScopeV1::ProfileSessions
    ) {
        return None;
    }
    let snapshot = db.read_snapshot().await.ok()?;
    // Cheap pre-check before the lock: skip entirely when every provider is
    // already at (or past) its target version and no legacy global marker
    // remains to migrate.
    if !structured_backfill_pending(&snapshot).await {
        return Some(StructuredBackfillStats::default());
    }
    drop(snapshot);
    // Claim the store cross-process before doing any parse or watermark work.
    // A process that loses the race skips its sweep entirely rather than
    // duplicating the whole-file re-parse and interleaving watermark writes
    // with the winner. Held for the whole batch; released on drop / on exit.
    let Some(_sweep_lock) = try_acquire_structured_backfill_lock(db.db_path()) else {
        return Some(StructuredBackfillStats::default());
    };
    let transaction = db.begin_write_transaction().await.ok()?;
    ensure_backfill_meta_table(&transaction).await?;
    // One-time migration from the single global marker to per-provider markers,
    // so a store that already completed the global sweep does not re-sweep.
    migrate_legacy_global_marker(&transaction).await?;
    transaction.commit().await.ok()?;

    // Sweep each provider independently against its own marker + cursor. A
    // provider already at its target version is skipped without touching its
    // watermark or re-parsing, so bumping one provider never disturbs another.
    let mut stats = StructuredBackfillStats::default();
    for &(provider, target_version) in STRUCTURED_BACKFILL_VERSIONS {
        sweep_provider(db, provider, target_version, &mut stats).await?;
    }

    if stats.inserted > 0 {
        tracing::info!(
            rows = stats.inserted,
            files = stats.files_scanned,
            "backfilled structured transcript rows"
        );
    }
    Some(stats)
}

/// Whether any structured-backfill work is outstanding: a leftover global
/// marker still needs migrating, or some provider is behind its target version.
async fn structured_backfill_pending(conn: &(impl QueryExecutor + ?Sized)) -> bool {
    if legacy_global_marker_version(conn).await.is_some() {
        return true;
    }
    for &(provider, target_version) in STRUCTURED_BACKFILL_VERSIONS {
        if marker_version(conn, &structured_marker_name(provider)).await < target_version {
            return true;
        }
    }
    false
}

/// Reads the retired global marker's version if its row still exists, else
/// `None`. Distinct from [`marker_version`] (which maps a missing row to 0) so
/// the migration seeds only when a genuine legacy marker is present.
async fn legacy_global_marker_version(conn: &(impl QueryExecutor + ?Sized)) -> Option<i64> {
    let Ok(mut rows) = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![STRUCTURED_MARKER_NAME],
        )
        .await
    else {
        return None;
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).ok(),
        _ => None,
    }
}

/// One-time migration from the single global `structured_rows_backfill` marker
/// to per-provider markers. When a store carries the legacy global marker at
/// version N (it already finished the global sweep up to N, which covered every
/// provider), seed every provider's marker to N so no provider spuriously
/// re-sweeps, then retire the global marker and its global/un-versioned cursor
/// rows. Providers whose target now exceeds N still re-sweep on their own.
async fn migrate_legacy_global_marker(conn: &(impl Executor + ?Sized)) -> Option<()> {
    let Some(legacy_version) = legacy_global_marker_version(conn).await else {
        return Some(());
    };
    // `ON CONFLICT DO NOTHING` preserves any per-provider progress a prior run
    // already recorded (the migration only ever seeds a first baseline).
    for &(provider, _) in STRUCTURED_BACKFILL_VERSIONS {
        conn.execute(
            "INSERT INTO session_schema_migrations(name, version)
             VALUES (?1, ?2)
             ON CONFLICT(name) DO NOTHING",
            params![structured_marker_name(provider), legacy_version],
        )
        .await
        .ok()?;
    }
    conn.execute(
        "DELETE FROM session_schema_migrations WHERE name = ?1",
        params![STRUCTURED_MARKER_NAME],
    )
    .await
    .ok()?;
    // Retire legacy cursor rows: the bare un-versioned key and the old
    // global-versioned `…:v{N}` keys. Per-provider cursors (`…:<provider>:v{N}`)
    // do not exist yet at first migration, and the `:v%` pattern would not match
    // them anyway (their segment after the prefix is a provider name, not `v…`).
    conn.execute(
        "DELETE FROM session_backfill_meta WHERE key = ?1 OR key LIKE ?2",
        params![
            STRUCTURED_CURSOR_KEY_PREFIX,
            format!("{STRUCTURED_CURSOR_KEY_PREFIX}:v%")
        ],
    )
    .await
    .ok()?;
    Some(())
}

/// Sweeps one provider's next bounded transcript batch, advancing that
/// provider's own version-namespaced cursor and marking it complete when it
/// drains. A provider already at its target version returns immediately.
async fn sweep_provider(
    db: &RegisteredGlobalDb,
    provider: &str,
    target_version: i64,
    stats: &mut StructuredBackfillStats,
) -> Option<()> {
    let snapshot = db.read_snapshot().await.ok()?;
    if marker_version(&snapshot, &structured_marker_name(provider)).await >= target_version {
        return Some(());
    }
    let cursor_key = structured_cursor_key(provider, target_version);
    let cursor = read_backfill_cursor(&snapshot, &cursor_key).await;
    let candidates =
        load_structured_candidates(&snapshot, provider, &cursor, STRUCTURED_BACKFILL_BATCH).await?;
    if candidates.is_empty() {
        drop(snapshot);
        let transaction = db.begin_write_transaction().await.ok()?;
        mark_structured_backfill_complete(&transaction, provider, target_version).await?;
        transaction.commit().await.ok()?;
        return Some(());
    }

    for candidate in &candidates {
        let target_size = std::fs::metadata(&candidate.source_path).ok()?.len();
        let project_paths =
            load_project_paths_for_source(&snapshot, &candidate.provider, &candidate.source_path)
                .await?;
        for project_path in project_paths {
            let project_root = PathBuf::from(&project_path);
            let mut parse_cursor = StoredCursor::default();
            while parse_cursor.position < target_size {
                let provider = candidate.provider.clone();
                let source_path = candidate.source_path.clone();
                let project_path = project_path.clone();
                let parsed = tokio::task::spawn_blocking(move || {
                    parse_structured_messages(&provider, &source_path, &project_path, parse_cursor)
                })
                .await
                .ok()?
                .ok()?;
                let parsed = parsed?;
                if parsed.new_cursor.position <= parse_cursor.position {
                    return None;
                }
                parse_cursor = parsed.new_cursor;
                let messages = parsed.messages;
                if messages.is_empty() {
                    continue;
                }
                let commit_records = crate::runtime::git_correlation::direct_commit_records(
                    &messages,
                    &project_root,
                );
                let span_observations =
                    crate::runtime::git_correlation::ingest_span_observations(&messages);
                let transaction = db.begin_write_transaction().await.ok()?;
                stats.inserted += insert_absent_session_messages(&transaction, &messages).await?;
                for record in &commit_records {
                    crate::runtime::git_correlation::upsert_commit_session(&transaction, record)
                        .await
                        .ok()?;
                }
                for observation in &span_observations {
                    crate::runtime::git_correlation::record_span_observation_in_transaction(
                        &transaction,
                        observation,
                        crate::runtime::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
                    )
                    .await
                    .ok()?;
                }
                transaction.commit().await.ok()?;
            }
        }
        stats.files_scanned += 1;
        let transaction = db.begin_write_transaction().await.ok()?;
        transaction
            .execute(
                WRITE_BACKFILL_CURSOR_SQL,
                params![cursor_key.as_str(), candidate.source_path.as_str()],
            )
            .await
            .ok()?;
        transaction.commit().await.ok()?;
    }

    Some(())
}

async fn insert_absent_session_messages(
    conn: &(impl Executor + ?Sized),
    messages: &[crate::runtime::SessionMessageRecord],
) -> Option<u64> {
    let mut inserted = 0u64;
    for message in messages {
        inserted = inserted.saturating_add(
            conn.execute(
                "INSERT OR IGNORE INTO session_messages
                     (provider, message_id, session_id, role, timestamp, ordinal, text, kind,
                      model, tool_names, source_path, source_offset, metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    message.provider.as_str(),
                    message.message_id.as_str(),
                    message.session_id.as_str(),
                    message.role.as_str(),
                    message.timestamp,
                    message.ordinal,
                    message.text.as_str(),
                    message.kind.as_deref(),
                    message.model.as_deref(),
                    message.tool_names.as_deref(),
                    message.source_path.as_deref(),
                    message.source_offset,
                    message.metadata_json.as_deref(),
                ],
            )
            .await
            .ok()?,
        );
    }
    Some(inserted)
}

fn parse_structured_messages(
    provider: &str,
    source_path: &str,
    project_path: &str,
    previous: StoredCursor,
) -> TranscriptIngestResult<Option<ParsedTranscript>> {
    let Some(source) = provider_source(provider) else {
        return Ok(None);
    };
    source.try_parse_new(
        Path::new(source_path),
        previous,
        Path::new(project_path),
        Some(STRUCTURED_BACKFILL_PARSE_BYTES),
    )
}

fn provider_source(provider: &str) -> Option<Box<dyn TranscriptSource>> {
    let home = crate::runtime::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    match provider {
        "codex" => Some(Box::new(crate::runtime::codex::CodexSource::with_home(
            &home,
        ))),
        _ => None,
    }
}

async fn load_structured_candidates(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    after_path: &str,
    limit: usize,
) -> Option<Vec<StructuredCandidate>> {
    // `provider` is always an allowlisted entry from `STRUCTURED_BACKFILL_VERSIONS`
    // and is passed as a bound parameter, so no interpolation/injection concern.
    let sql = "SELECT DISTINCT sm.source_path, sm.provider
         FROM session_messages sm
         WHERE sm.provider = ?1
           AND sm.source_path IS NOT NULL
           AND sm.source_path > ?2
         ORDER BY sm.source_path
         LIMIT ?3";
    let mut rows = conn
        .query(sql, params![provider, after_path, limit as i64])
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
    conn: &(impl QueryExecutor + ?Sized),
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

async fn ensure_backfill_meta_table(conn: &(impl Executor + ?Sized)) -> Option<()> {
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

async fn read_backfill_cursor(conn: &(impl QueryExecutor + ?Sized), key: &str) -> String {
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

async fn write_backfill_cursor(
    conn: &(impl Executor + ?Sized),
    key: &str,
    value: &str,
) -> Option<()> {
    // Compare-and-set: only ever move the watermark forward. Candidates are
    // selected with `source_path > cursor` and ordered ascending, so a greater
    // stored value means more files covered. The `WHERE excluded.value > …`
    // guard makes a slower concurrent sweep writing an earlier path a no-op
    // instead of regressing the cursor and re-queuing already-covered files.
    // Binary (default) TEXT collation matches the candidate query's ordering.
    conn.execute(WRITE_BACKFILL_CURSOR_SQL, params![key, value])
        .await
        .ok()?;
    Some(())
}

/// Test-only accessor: writes the Codex structured-backfill watermark for `db`
/// exactly as the sweep does, so tests can assert the compare-and-set
/// monotonicity guard rejects backwards moves.
#[doc(hidden)]
pub async fn write_structured_backfill_cursor_for_test(
    db: &RegisteredGlobalDb,
    value: &str,
) -> Option<()> {
    let transaction = db.begin_write_transaction().await.ok()?;
    ensure_backfill_meta_table(&transaction).await?;
    let key = structured_cursor_key("codex", structured_backfill_target_version("codex"));
    write_backfill_cursor(&transaction, &key, value).await?;
    transaction.commit().await.ok()?;
    Some(())
}

/// Test-only accessor: reads the Codex structured-backfill watermark for `db`.
#[doc(hidden)]
pub async fn read_structured_backfill_cursor_for_test(db: &RegisteredGlobalDb) -> String {
    let key = structured_cursor_key("codex", structured_backfill_target_version("codex"));
    let Ok(snapshot) = db.read_snapshot().await else {
        return String::new();
    };
    read_backfill_cursor(&snapshot, &key).await
}

/// Marks one provider's sweep complete at `target_version` and drops that
/// provider's watermark rows (this version's key and any stale prior-version
/// per-provider keys). Other providers' in-flight cursors are left intact.
async fn mark_structured_backfill_complete(
    conn: &(impl Executor + ?Sized),
    provider: &str,
    target_version: i64,
) -> Option<()> {
    conn.execute(
        "INSERT INTO session_schema_migrations(name, version)
         VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET
            version = excluded.version,
            applied_at = unixepoch()",
        params![structured_marker_name(provider), target_version],
    )
    .await
    .ok()?;
    conn.execute(
        "DELETE FROM session_backfill_meta WHERE key LIKE ?1",
        params![format!("{STRUCTURED_CURSOR_KEY_PREFIX}:{provider}:%")],
    )
    .await
    .ok()?;
    Some(())
}

/// Opaque registered ProjectSessions fixture for transcript-facts backfill tests.
#[doc(hidden)]
pub struct TranscriptFactsBackfillTestRuntimeV1 {
    authority: crate::application::host_admission::HostAdmissionTestRuntimeV1,
}

impl TranscriptFactsBackfillTestRuntimeV1 {
    pub async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> crate::errors::Result<Self> {
        Ok(Self {
            authority: crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
                profile_root,
                project_root,
                project_id,
            )
            .await?,
        })
    }

    fn database(&self) -> &RegisteredGlobalDb {
        match self
            .authority
            .registered_database(crate::application::host_admission::HostAdmissionScope::Project)
        {
            Some(database) => database,
            None => panic!("transcript facts test runtime has ProjectSessions authority"),
        }
    }

    pub async fn transcript_facts_backfill_status_for_test(
        &self,
    ) -> crate::errors::Result<TranscriptFactsBackfillOutcome> {
        transcript_facts_backfill_status(self.database()).await
    }

    pub async fn advance_transcript_facts_backfill_for_test(
        &self,
        limit: usize,
    ) -> crate::errors::Result<TranscriptFactsBackfillOutcome> {
        advance_transcript_facts_backfill_with_limit(self.database(), limit).await
    }
}

impl Deref for TranscriptFactsBackfillTestRuntimeV1 {
    type Target = crate::application::host_admission::HostAdmissionTestRuntimeV1;

    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}

/// Opaque registered ProjectSessions fixture for structured-backfill integration tests.
#[doc(hidden)]
pub struct StructuredBackfillTestRuntimeV1 {
    authority: crate::application::host_admission::HostAdmissionTestRuntimeV1,
}

impl StructuredBackfillTestRuntimeV1 {
    pub async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> crate::errors::Result<Self> {
        Ok(Self {
            authority: crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
                profile_root,
                project_root,
                project_id,
            )
            .await?,
        })
    }

    fn database(&self) -> &RegisteredGlobalDb {
        match self
            .authority
            .registered_database(crate::application::host_admission::HostAdmissionScope::Project)
        {
            Some(database) => database,
            None => panic!("structured backfill test runtime has ProjectSessions authority"),
        }
    }

    pub fn database_path(&self) -> &Path {
        self.database().db_path()
    }

    pub async fn seed_source(
        &self,
        source: &dyn TranscriptSource,
        project_root: &Path,
    ) -> Result<crate::runtime::shared::TranscriptIngestStats, String> {
        let discovery = source.discover_transcript_paths(
            project_root,
            crate::runtime::source::TranscriptDiscoveryBounds::default_walk(),
        );
        let mut stats = crate::runtime::shared::TranscriptIngestStats::default();
        for path in discovery.paths {
            let Some(parsed) = source
                .try_parse_new(&path, StoredCursor::default(), project_root, None)
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            let started_at = parsed
                .messages
                .iter()
                .filter_map(|message| message.timestamp)
                .min();
            let ended_at = parsed
                .messages
                .iter()
                .filter_map(|message| message.timestamp)
                .max();
            let transaction = self
                .database()
                .begin_write_transaction()
                .await
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO sessions
                         (provider, session_id, project_key, project_path, title, started_at,
                          ended_at, transcript_path, metadata_json, parent_session_id,
                          is_subagent, agent_id, parent_tool_use_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(provider, session_id) DO UPDATE SET
                        project_key = excluded.project_key,
                        project_path = excluded.project_path,
                        title = excluded.title,
                        started_at = excluded.started_at,
                        ended_at = excluded.ended_at,
                        transcript_path = excluded.transcript_path,
                        metadata_json = excluded.metadata_json,
                        parent_session_id = excluded.parent_session_id,
                        is_subagent = excluded.is_subagent,
                        agent_id = excluded.agent_id,
                        parent_tool_use_id = excluded.parent_tool_use_id",
                    params![
                        source.provider(),
                        parsed.draft.session_id.as_str(),
                        parsed.draft.project_key.as_str(),
                        parsed.draft.project_path.as_str(),
                        parsed.draft.title.as_deref(),
                        started_at,
                        ended_at,
                        path.to_string_lossy().as_ref(),
                        parsed.draft.metadata_json.as_deref(),
                        parsed.draft.parent_session_id.as_deref(),
                        i64::from(parsed.draft.is_subagent),
                        parsed.draft.agent_id.as_deref(),
                        parsed.draft.parent_tool_use_id.as_deref(),
                    ],
                )
                .await
                .map_err(|error| error.to_string())?;
            let inserted = insert_absent_session_messages(&transaction, &parsed.messages)
                .await
                .ok_or_else(|| "seed structured transcript messages".to_string())?;
            transaction
                .commit()
                .await
                .map_err(|error| error.to_string())?;
            stats.sessions_upserted = stats.sessions_upserted.saturating_add(1);
            stats.messages_upserted = stats.messages_upserted.saturating_add(inserted);
        }
        Ok(stats)
    }

    pub async fn run(&self) -> Option<u64> {
        backfill_structured_rows(self.database())
            .await
            .map(|stats| stats.inserted)
    }

    pub async fn count_kind(&self, provider: &str, kind: &str) -> Result<i64, String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM session_messages WHERE provider = ?1 AND kind = ?2",
                params![provider, kind],
            )
            .await
            .map_err(|error| error.to_string())?;
        rows.next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing structured kind count".to_string())?
            .get(0)
            .map_err(|error| error.to_string())
    }

    pub async fn remove_kind_and_reset(&self, provider: &str, kind: &str) -> Result<(), String> {
        let transaction = self
            .database()
            .begin_write_transaction()
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM lcm_raw_messages
                 WHERE provider = ?1
                   AND message_id IN (
                       SELECT message_id FROM session_messages
                       WHERE provider = ?1 AND kind = ?2)",
                params![provider, kind],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_messages WHERE provider = ?1 AND kind = ?2",
                params![provider, kind],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name LIKE 'structured_rows_backfill%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_backfill_meta
                 WHERE key LIKE 'structured_backfill_cursor%'",
                (),
            )
            .await
            .ok();
        transaction
            .commit()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn goal_row(&self) -> Result<(String, Option<String>, Option<String>), String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT text, kind, metadata_json FROM session_messages
                 WHERE provider = 'codex' AND kind = 'goal'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let row = rows
            .next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing Codex goal row".to_string())?;
        Ok((
            row.get(0).map_err(|error| error.to_string())?,
            row.get(1).map_err(|error| error.to_string())?,
            row.get(2).map_err(|error| error.to_string())?,
        ))
    }

    pub async fn marker_version(&self, provider: Option<&str>) -> Result<Option<i64>, String> {
        let name = provider.map_or_else(
            || STRUCTURED_MARKER_NAME.to_string(),
            structured_marker_name,
        );
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT version FROM session_schema_migrations WHERE name = ?1",
                params![name],
            )
            .await
            .map_err(|error| error.to_string())?;
        rows.next()
            .await
            .map_err(|error| error.to_string())
            .map(|row| row.and_then(|row| row.get(0).ok()))
    }

    pub async fn session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<Option<crate::runtime::SessionRecord>, String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT provider, session_id, project_key, project_path, title, started_at,
                        ended_at, transcript_path, metadata_json, parent_session_id,
                        is_subagent, agent_id, parent_tool_use_id
                 FROM sessions WHERE provider = ?1 AND session_id = ?2",
                params![provider, session_id],
            )
            .await
            .map_err(|error| error.to_string())?;
        let Some(row) = rows.next().await.map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        Ok(Some(crate::runtime::SessionRecord {
            provider: row.get(0).map_err(|error| error.to_string())?,
            session_id: row.get(1).map_err(|error| error.to_string())?,
            project_key: row.get(2).map_err(|error| error.to_string())?,
            project_path: row.get(3).map_err(|error| error.to_string())?,
            title: row.get(4).map_err(|error| error.to_string())?,
            started_at: row.get(5).map_err(|error| error.to_string())?,
            ended_at: row.get(6).map_err(|error| error.to_string())?,
            transcript_path: row.get(7).map_err(|error| error.to_string())?,
            metadata_json: row.get(8).map_err(|error| error.to_string())?,
            parent_session_id: row.get(9).map_err(|error| error.to_string())?,
            is_subagent: row.get::<i64>(10).map_err(|error| error.to_string())? != 0,
            agent_id: row.get(11).map_err(|error| error.to_string())?,
            parent_tool_use_id: row.get(12).map_err(|error| error.to_string())?,
        }))
    }

    pub async fn seed_stale_unversioned_cursor(&self) -> Result<(), String> {
        let transaction = self
            .database()
            .begin_write_transaction()
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM lcm_raw_messages
                 WHERE provider = 'codex'
                   AND message_id IN (
                       SELECT message_id FROM session_messages
                       WHERE provider = 'codex' AND kind = 'goal')",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_messages WHERE provider = 'codex' AND kind = 'goal'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name LIKE 'structured_rows_backfill%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = transaction
            .query(
                "SELECT MAX(source_path) FROM session_messages WHERE provider = 'codex'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        let last_path: String = rows
            .next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing Codex source path".to_string())?
            .get(0)
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO session_backfill_meta(key, value)
                 VALUES ('structured_backfill_cursor', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![last_path],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .commit()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn seed_legacy_global_marker(&self, version: i64) -> Result<(), String> {
        let transaction = self
            .database()
            .begin_write_transaction()
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations
                 WHERE name LIKE 'structured_rows_backfill%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO session_schema_migrations(name, version)
                 VALUES (?1, ?2)",
                params![STRUCTURED_MARKER_NAME, version],
            )
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_backfill_meta
                 WHERE key LIKE 'structured_backfill_cursor%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        for key in [
            STRUCTURED_CURSOR_KEY_PREFIX.to_string(),
            format!("{STRUCTURED_CURSOR_KEY_PREFIX}:v{version}"),
        ] {
            transaction
                .execute(
                    "INSERT INTO session_backfill_meta(key, value)
                     VALUES (?1, 'legacy/path.jsonl')",
                    params![key],
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn cursor_count(&self) -> Result<i64, String> {
        let snapshot = self
            .database()
            .read_snapshot()
            .await
            .map_err(|error| error.to_string())?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*) FROM session_backfill_meta
                 WHERE key LIKE 'structured_backfill_cursor%'",
                (),
            )
            .await
            .map_err(|error| error.to_string())?;
        rows.next()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "missing structured cursor count".to_string())?
            .get(0)
            .map_err(|error| error.to_string())
    }

    pub async fn write_cursor(&self, value: &str) -> Option<()> {
        write_structured_backfill_cursor_for_test(self.database(), value).await
    }

    pub async fn read_cursor(&self) -> String {
        read_structured_backfill_cursor_for_test(self.database()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_has_no_legacy_structured_backfill_path() {
        assert_eq!(structured_backfill_target_version("claude"), 0);
        assert!(provider_source("claude").is_none());
    }

    #[test]
    fn codex_structured_backfill_drains_large_files_in_bounded_batches() {
        const MESSAGE_COUNT: usize = 25;
        const MESSAGE_BYTES: usize = 700 * 1024;

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let transcript = dir.path().join("codex-large-backfill.jsonl");
        let mut contents = format!(
            "{}\n",
            serde_json::json!({
                "timestamp": "2026-01-01T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": "codex-large-backfill",
                    "cwd": project,
                    "model": "gpt-5.6"
                }
            })
        );
        let body = "x".repeat(MESSAGE_BYTES);
        for index in 0..MESSAGE_COUNT {
            contents.push_str(
                &serde_json::json!({
                    "timestamp": "2026-01-01T00:00:01.000Z",
                    "type": "event_msg",
                    "payload": {
                        "type": "user_message",
                        "message": format!("{index}:{body}")
                    }
                })
                .to_string(),
            );
            contents.push('\n');
        }
        assert!(contents.len() as u64 > STRUCTURED_BACKFILL_PARSE_BYTES);
        std::fs::write(&transcript, &contents).unwrap();

        let mut cursor = StoredCursor::default();
        let mut batches = 0_usize;
        let mut messages = 0_usize;
        while cursor.position < contents.len() as u64 {
            let parsed = parse_structured_messages(
                "codex",
                transcript.to_str().unwrap(),
                project.to_str().unwrap(),
                cursor,
            )
            .unwrap()
            .expect("bounded Codex backfill batch");
            assert!(parsed.new_cursor.position > cursor.position);
            assert!(
                parsed.new_cursor.position - cursor.position <= STRUCTURED_BACKFILL_PARSE_BYTES
            );
            cursor = parsed.new_cursor;
            messages += parsed.messages.len();
            batches += 1;
        }

        assert!(batches > 1);
        assert_eq!(cursor.position, contents.len() as u64);
        assert_eq!(messages, MESSAGE_COUNT);
    }

    #[tokio::test]
    async fn dogfood_recovery_transcript_fact_failure_rolls_back_entire_batch() {
        let directory = tempfile::tempdir().unwrap();
        let connection =
            crate::db::engine::TestConnection::open(&directory.path().join("sessions.db"));
        let transaction = connection.transaction().await.unwrap();
        transaction
            .execute_batch(
                "CREATE TABLE sessions (
                    provider TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    started_at INTEGER,
                    ended_at INTEGER,
                    PRIMARY KEY(provider, session_id)
                );
                CREATE TABLE session_messages (
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    timestamp INTEGER,
                    metadata_json TEXT,
                    PRIMARY KEY(provider, message_id)
                );
                CREATE TABLE lcm_raw_messages (
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    timestamp INTEGER,
                    metadata_json TEXT,
                    PRIMARY KEY(provider, message_id)
                );
                CREATE TABLE session_schema_migrations (
                    name TEXT PRIMARY KEY,
                    version INTEGER NOT NULL,
                    applied_at INTEGER NOT NULL DEFAULT (unixepoch())
                );
                INSERT INTO sessions(provider, session_id)
                VALUES ('codex', 'session');
                INSERT INTO session_messages(provider, message_id)
                VALUES ('codex', 'message');
                INSERT INTO lcm_raw_messages(provider, message_id, session_id)
                VALUES ('codex', 'message', 'session');
                CREATE TRIGGER fail_transcript_fact_update
                BEFORE UPDATE OF timestamp ON lcm_raw_messages
                WHEN NEW.message_id = 'message'
                BEGIN
                    SELECT RAISE(ABORT, 'forced transcript backfill failure');
                END;",
            )
            .await
            .unwrap();

        let updates = vec![(
            "codex".to_owned(),
            "message".to_owned(),
            LineFacts {
                timestamp: Some(123),
                usage: None,
            },
        )];
        let error = match apply_updates_atomically(&transaction, &updates).await {
            Ok(_) => panic!("forced update failure must abort the backfill batch"),
            Err(error) => error,
        };
        assert!(error.atomicity_preserved());

        // Startup deliberately ignores a failed self-heal. Committing its
        // outer schema transaction must not preserve an earlier batch write.
        transaction.commit().await.unwrap();
        let mut rows = connection
            .query(
                "SELECT timestamp FROM session_messages
                 WHERE provider = 'codex' AND message_id = 'message'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<Option<i64>>(0).unwrap(), None);

        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM session_schema_migrations
                 WHERE name = 'transcript_facts_backfill'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0);
    }
}

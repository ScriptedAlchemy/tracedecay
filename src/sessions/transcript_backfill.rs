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
use std::path::Path;

use libsql::{params, Connection};
use serde_json::Value;

use crate::sessions::cursor::TimestampCarry;
use crate::sessions::source::usage_counters_from;

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
    if marker_version(conn).await >= MARKER_VERSION {
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

async fn marker_version(conn: &Connection) -> i64 {
    let Ok(mut rows) = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![MARKER_NAME],
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
    // For Codex, a `token_count` event reports on the `agent_message` line it
    // follows; remember that line's offset so the usage lands on the message.
    let mut last_assistant_offset: Option<i64> = None;
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
                    if let Some(usage) = crate::sessions::codex::token_count_usage(&value) {
                        if let Some(assistant_offset) = last_assistant_offset.take() {
                            let entry = facts.entry(assistant_offset).or_default();
                            if entry.usage.is_none() {
                                entry.usage = Some(usage);
                            }
                        }
                        continue;
                    }
                    if value.pointer("/payload/type").and_then(Value::as_str)
                        == Some("agent_message")
                    {
                        last_assistant_offset = Some(line_offset);
                    }
                    line_facts.usage = None;
                }
                facts.insert(line_offset, line_facts);
            }
        }
    }
    Some(facts)
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

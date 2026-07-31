//! `store.db` blob-DAG reader (SQL length-gated, reachable-only): metadata,
//! bounded blob fetches, and DAG walk ordering for the per-session Cursor
//! composer chat stores.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde_json::Value;

use crate::runtime::ingest_byte_budget::IngestByteBudget;
use crate::runtime::snapshot_observation::MAX_SNAPSHOT_METADATA_BYTES;

use super::sqlite::{
    BoundedSqliteValue, CursorConn, MAX_COMPOSER_SQLITE_KEY_BYTES, composer_source_charge,
    effective_sqlite_cap, epoch_ms_to_secs, max_composer_record_bytes,
};

fn read_varint(bytes: &[u8], start: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    let mut i = start;
    while i < bytes.len() {
        let byte = bytes[i];
        let payload = u64::from(byte & 0x7f);
        if shift == 63 && payload > 1 {
            return None;
        }
        result |= payload << shift;
        i += 1;
        if byte & 0x80 == 0 {
            return Some((result, i));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Extract length-delimited field-1 entries that are exactly 32 bytes long and
/// hex-encode them — the content-addressed child ids of a DAG node blob. A
/// light protobuf scanner that skips unrelated fields by wire type.
pub(crate) fn protobuf_child_refs(bytes: &[u8]) -> Option<Vec<String>> {
    let mut refs = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if refs.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
            break;
        }
        let (tag, next) = read_varint(bytes, i)?;
        i = next;
        let field = tag >> 3;
        let wire = tag & 0x7;
        if field == 0 {
            return None;
        }
        if field == 1 && wire != 2 {
            return None;
        }
        match wire {
            0 => {
                let (_, next) = read_varint(bytes, i)?;
                i = next;
            }
            1 => {
                i = i.checked_add(8)?;
                if i > bytes.len() {
                    return None;
                }
            }
            5 => {
                i = i.checked_add(4)?;
                if i > bytes.len() {
                    return None;
                }
            }
            2 => {
                let (len, next) = read_varint(bytes, i)?;
                i = next;
                let len = usize::try_from(len).ok()?;
                let end = i.checked_add(len)?;
                if end > bytes.len() {
                    return None;
                }
                if field == 1 && len == 32 {
                    refs.push(encode_hex(&bytes[i..end]));
                }
                i = end;
            }
            _ => return None,
        }
    }
    Some(refs)
}

/// A JSON message leaf is a JSON object carrying a `role` field.
fn store_blob_message(bytes: &[u8]) -> Option<(String, Value)> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    let role = value.get("role").and_then(Value::as_str)?.to_string();
    let content = value.get("content").cloned().unwrap_or(Value::Null);
    Some((role, content))
}

fn fetch_store_blob_bounded_sync(
    conn: &rusqlite::Connection,
    blob_id: &str,
    remaining: Option<u64>,
) -> BoundedSqliteValue<Vec<u8>> {
    let max_bytes = max_composer_record_bytes();
    let effective_cap = effective_sqlite_cap(max_bytes, remaining);
    let Ok(mut stmt) = conn.prepare(
        "SELECT length(CAST(data AS BLOB)) AS nbytes, \
         CASE WHEN length(CAST(data AS BLOB)) <= ?1 THEN data ELSE NULL END AS payload \
         FROM blobs WHERE id = ?2",
    ) else {
        return BoundedSqliteValue::Corrupt;
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![effective_cap as i64, blob_id]) else {
        return BoundedSqliteValue::Corrupt;
    };
    let row = match rows.next() {
        Ok(Some(row)) => row,
        Ok(None) => return BoundedSqliteValue::Missing,
        Err(_) => return BoundedSqliteValue::Corrupt,
    };
    let Ok(nbytes_i) = row.get::<_, i64>(0) else {
        return BoundedSqliteValue::Corrupt;
    };
    if nbytes_i < 0 {
        return BoundedSqliteValue::Malformed { byte_len: 0 };
    }
    let byte_len = nbytes_i as u64;
    let data = row
        .get::<_, Vec<u8>>(1)
        .or_else(|_| row.get::<_, String>(1).map(String::into_bytes));
    match data {
        Ok(value) => BoundedSqliteValue::Ready { byte_len, value },
        Err(_) if byte_len > max_bytes => BoundedSqliteValue::Oversized { byte_len },
        Err(_) if remaining.is_some_and(|cap| byte_len > cap) => {
            BoundedSqliteValue::BudgetExceeded { byte_len }
        }
        Err(_) => BoundedSqliteValue::Malformed { byte_len },
    }
}

async fn fetch_store_blob_bounded(
    conn: &CursorConn,
    blob_id: &str,
    remaining: Option<u64>,
) -> BoundedSqliteValue<Vec<u8>> {
    if blob_id.is_empty() || blob_id.len() as u64 > MAX_COMPOSER_SQLITE_KEY_BYTES {
        return BoundedSqliteValue::Missing;
    }
    let blob_id = blob_id.to_string();
    conn.with(move |conn| fetch_store_blob_bounded_sync(conn, &blob_id, remaining))
        .await
        .unwrap_or(BoundedSqliteValue::Corrupt)
}

pub(crate) async fn read_store_meta_bounded(
    conn: &CursorConn,
    remaining: Option<u64>,
) -> BoundedSqliteValue<StoreMeta> {
    conn.with(move |conn| read_store_meta_bounded_sync(conn, remaining))
        .await
        .unwrap_or(BoundedSqliteValue::Corrupt)
}

fn read_store_meta_bounded_sync(
    conn: &rusqlite::Connection,
    remaining: Option<u64>,
) -> BoundedSqliteValue<StoreMeta> {
    let decoded_cap = effective_sqlite_cap(MAX_COMPOSER_STORE_META_BYTES, remaining);
    let encoded_cap = decoded_cap.saturating_mul(2);
    let Ok(mut stmt) = conn.prepare(
        "SELECT length(CAST(value AS BLOB)) AS nbytes, \
         CASE WHEN length(CAST(value AS BLOB)) <= ?1 THEN value ELSE NULL END AS payload \
         FROM meta WHERE key = '0'",
    ) else {
        return BoundedSqliteValue::Corrupt;
    };
    let Ok(mut rows) = stmt.query(rusqlite::params![encoded_cap as i64]) else {
        return BoundedSqliteValue::Corrupt;
    };
    let row = match rows.next() {
        Ok(Some(row)) => row,
        Ok(None) => return BoundedSqliteValue::Missing,
        Err(_) => return BoundedSqliteValue::Corrupt,
    };
    let Some(encoded_bytes) = row
        .get::<_, i64>(0)
        .ok()
        .filter(|n| *n >= 0)
        .map(|n| n as u64)
    else {
        return BoundedSqliteValue::Malformed { byte_len: 0 };
    };
    let decoded_bytes = encoded_bytes.saturating_add(1) / 2;
    if encoded_bytes > MAX_COMPOSER_STORE_META_HEX_BYTES {
        return BoundedSqliteValue::Oversized {
            byte_len: decoded_bytes,
        };
    }
    if remaining.is_some_and(|cap| decoded_bytes > cap) {
        return BoundedSqliteValue::BudgetExceeded {
            byte_len: decoded_bytes,
        };
    }
    let Ok(hex) = row.get::<_, String>(1) else {
        return BoundedSqliteValue::Malformed {
            byte_len: decoded_bytes,
        };
    };
    let Some(bytes) = decode_hex(&hex) else {
        return BoundedSqliteValue::Malformed {
            byte_len: decoded_bytes,
        };
    };
    if bytes.len() as u64 > MAX_COMPOSER_STORE_META_BYTES {
        return BoundedSqliteValue::Oversized {
            byte_len: bytes.len() as u64,
        };
    }
    let Ok(meta) = serde_json::from_slice::<Value>(&bytes) else {
        return BoundedSqliteValue::Malformed {
            byte_len: bytes.len() as u64,
        };
    };
    let Some(agent_id) = meta
        .get("agentId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
        .map(str::to_string)
    else {
        return BoundedSqliteValue::Malformed {
            byte_len: bytes.len() as u64,
        };
    };
    BoundedSqliteValue::Ready {
        byte_len: bytes.len() as u64,
        value: StoreMeta {
            agent_id,
            latest_root_blob_id: meta
                .get("latestRootBlobId")
                .and_then(Value::as_str)
                .filter(|id| id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
                .map(str::to_string),
            created_at: epoch_ms_to_secs(meta.get("createdAt").and_then(Value::as_i64)),
        },
    }
}

/// Walk the blob DAG from `root` (or id-sorted fallback), fetching each blob by
/// primary key with SQL length/budget gates. Never `SELECT`s the full `blobs`
/// table. Charges reachable blob bytes against `byte_budget` as they materialize.
pub(crate) async fn order_store_messages_bounded(
    conn: &CursorConn,
    root: Option<&str>,
    byte_budget: &mut IngestByteBudget,
) -> StoreWalkOutcome {
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut deferred = false;

    if let Some(root) = root {
        walk_store_blob_bounded(
            conn,
            root,
            byte_budget,
            &mut visited,
            &mut ordered,
            &mut deferred,
        )
        .await;
        if deferred && ordered.is_empty() {
            return StoreWalkOutcome::DeferredEmpty;
        }
        if !ordered.is_empty() {
            return StoreWalkOutcome::Messages(ordered);
        }
    }

    // Fallback: id-sorted leaf scan — ids only first, then length-gated
    // fetches. The scan is already `LIMIT`-bounded, so one buffered page
    // reproduces the original cursor semantics exactly.
    let ids = conn
        .with(|conn| {
            let Ok(mut stmt) = conn.prepare(
                "SELECT id FROM blobs \
                 WHERE length(CAST(id AS BLOB)) <= ?1 \
                 ORDER BY id \
                 LIMIT ?2",
            ) else {
                return Vec::new();
            };
            let rows = stmt.query_map(
                rusqlite::params![
                    MAX_COMPOSER_SQLITE_KEY_BYTES as i64,
                    MAX_COMPOSER_STORE_BLOB_VISITS as i64
                ],
                |row| row.get::<_, String>(0),
            );
            let Ok(rows) = rows else {
                return Vec::new();
            };
            rows.filter_map(std::result::Result::ok).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
    for id in ids {
        if visited.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
            byte_budget.defer();
            break;
        }
        if !visited.insert(id.clone()) {
            continue;
        }
        if byte_budget.exhausted() {
            byte_budget.defer();
            deferred = true;
            break;
        }
        match fetch_store_blob_bounded(conn, &id, byte_budget.remaining()).await {
            BoundedSqliteValue::Ready { byte_len, value } => {
                if !byte_budget.try_consume(byte_len) {
                    deferred = true;
                    break;
                }
                if let Some((role, content)) = store_blob_message(&value) {
                    ordered.push((role, content, byte_len));
                }
            }
            BoundedSqliteValue::BudgetExceeded { .. } => {
                byte_budget.defer();
                deferred = true;
                break;
            }
            BoundedSqliteValue::Oversized { byte_len }
            | BoundedSqliteValue::Malformed { byte_len } => {
                if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                    deferred = true;
                    break;
                }
            }
            BoundedSqliteValue::Corrupt => {
                byte_budget.defer();
                deferred = true;
                break;
            }
            BoundedSqliteValue::Missing => {}
        }
    }
    if deferred && ordered.is_empty() {
        StoreWalkOutcome::DeferredEmpty
    } else {
        StoreWalkOutcome::Messages(ordered)
    }
}

async fn walk_store_blob_bounded(
    conn: &CursorConn,
    id: &str,
    byte_budget: &mut IngestByteBudget,
    visited: &mut HashSet<String>,
    ordered: &mut Vec<(String, Value, u64)>,
    deferred: &mut bool,
) {
    if *deferred || visited.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
        if visited.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
            byte_budget.defer();
            *deferred = true;
        }
        return;
    }
    if !visited.insert(id.to_string()) {
        return;
    }
    if byte_budget.exhausted() {
        byte_budget.defer();
        *deferred = true;
        return;
    }
    match fetch_store_blob_bounded(conn, id, byte_budget.remaining()).await {
        BoundedSqliteValue::Missing => {}
        BoundedSqliteValue::Oversized { byte_len } | BoundedSqliteValue::Malformed { byte_len } => {
            if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                *deferred = true;
            }
        }
        BoundedSqliteValue::BudgetExceeded { .. } => {
            byte_budget.defer();
            *deferred = true;
        }
        BoundedSqliteValue::Corrupt => {
            byte_budget.defer();
            *deferred = true;
        }
        BoundedSqliteValue::Ready { byte_len, value } => {
            if !byte_budget.try_consume(byte_len) {
                *deferred = true;
                return;
            }
            if let Some((role, content)) = store_blob_message(&value) {
                ordered.push((role, content, byte_len));
                return;
            }
            let Some(children) = protobuf_child_refs(&value) else {
                byte_budget.defer();
                *deferred = true;
                return;
            };
            for child in children.into_iter().take(MAX_COMPOSER_STORE_BLOB_VISITS) {
                if *deferred {
                    return;
                }
                Box::pin(walk_store_blob_bounded(
                    conn,
                    &child,
                    byte_budget,
                    visited,
                    ordered,
                    deferred,
                ))
                .await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// store.db blob-DAG reader (SQL length-gated, reachable-only)
// ---------------------------------------------------------------------------

pub(crate) struct StoreMeta {
    pub(crate) agent_id: String,
    pub(crate) latest_root_blob_id: Option<String>,
    pub(crate) created_at: Option<i64>,
}

pub(crate) enum StoreWalkOutcome {
    Messages(Vec<(String, Value, u64)>),
    DeferredEmpty,
}

/// Maximum bytes materializable for `store.db` `meta` hex/JSON.
pub(crate) const MAX_COMPOSER_STORE_META_BYTES: u64 = MAX_SNAPSHOT_METADATA_BYTES;

/// `meta.value` is hexadecimal text, so its encoded byte ceiling is twice the
/// decoded metadata ceiling.
pub(crate) const MAX_COMPOSER_STORE_META_HEX_BYTES: u64 = MAX_COMPOSER_STORE_META_BYTES * 2;

/// Cap on DAG blob visits / child refs per store — aligns with the default
/// ingest discovery unit ceiling (`IngestPassBounds::discovered_units`).
pub(crate) const MAX_COMPOSER_STORE_BLOB_VISITS: usize = 4096;

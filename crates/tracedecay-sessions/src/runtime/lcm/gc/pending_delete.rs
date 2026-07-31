use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::db::engine::{Executor, params};

use super::{LcmGcError, LcmGcPhaseReport, MAX_SAMPLES};
use crate::runtime::lcm::{LcmError, payload, schema};

const PENDING_PAYLOAD_DELETE_PREFIX: &str = "pending_payload_delete:";
pub(super) const PENDING_PAYLOAD_DELETE_ERROR_PREFIX: &str = "pending payload deletion partial:";
const PENDING_PAYLOAD_DELETE_ERROR_COUNT_KEY: &str = "pending_payload_delete_error_count";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingPayloadDelete {
    content_hash: Option<String>,
    byte_count: u64,
    char_count: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PayloadDeleteOutcomes {
    pub(super) removed: LcmGcPhaseReport,
    pub(super) preserved: LcmGcPhaseReport,
    pub(super) missing: LcmGcPhaseReport,
    pub(super) failed: LcmGcPhaseReport,
}

#[derive(Debug, Default)]
pub struct PayloadDeleteDrain {
    pub(super) outcomes: PayloadDeleteOutcomes,
    pub(super) errors: Vec<LcmGcError>,
}

impl PayloadDeleteDrain {
    pub(super) fn removed_bytes(&self, payload_ref: &str) -> Option<u64> {
        (self.outcomes.removed.count == 1
            && self
                .outcomes
                .removed
                .refs
                .first()
                .is_some_and(|value| value == payload_ref))
        .then_some(self.outcomes.removed.bytes)
    }

    fn add_error(&mut self, payload_ref: &str, kind: &str, detail: String) {
        self.outcomes.failed.add(payload_ref, 0);
        if self.errors.len() < MAX_SAMPLES {
            self.errors.push(LcmGcError {
                payload_ref: payload_ref.to_string(),
                kind: kind.to_string(),
                detail,
            });
        }
    }

    pub fn merge(&mut self, other: Self) {
        self.outcomes.removed.merge(other.outcomes.removed);
        self.outcomes.preserved.merge(other.outcomes.preserved);
        self.outcomes.missing.merge(other.outcomes.missing);
        // The second drain observes every tombstone still pending after the
        // first drain and the GC transaction. Its failure set is therefore the
        // authoritative current state, not another batch of attempts to add.
        self.outcomes.failed = other.outcomes.failed;
        self.errors = other.errors;
    }

    pub(super) fn has_failures(&self) -> bool {
        !self.outcomes.failed.is_empty()
    }
}

pub(super) fn pending_payload_delete_key(payload_ref: &str) -> String {
    format!("{PENDING_PAYLOAD_DELETE_PREFIX}{payload_ref}")
}

pub async fn stage_payload_delete(
    conn: &(impl Executor + ?Sized),
    payload_ref: &str,
    content_hash: Option<&str>,
    byte_count: u64,
    char_count: u64,
) -> Result<(), LcmError> {
    payload::validate_payload_ref(payload_ref)?;
    let pending = PendingPayloadDelete {
        content_hash: content_hash.map(str::to_string),
        byte_count,
        char_count: Some(char_count),
    };
    let value = serde_json::to_string(&pending).map_err(|err| LcmError::Db(err.to_string()))?;
    schema::set_gc_meta(conn, &pending_payload_delete_key(payload_ref), &value).await
}

pub async fn drain_pending_payload_deletes_in_transaction(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
) -> Result<PayloadDeleteDrain, LcmError> {
    drain_pending_payload_deletes_matching(conn, storage_root, None).await
}

pub async fn drain_pending_payload_delete_in_transaction(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    payload_ref: &str,
) -> Result<Option<u64>, LcmError> {
    payload::validate_payload_ref(payload_ref)?;
    Ok(
        drain_pending_payload_deletes_matching(conn, storage_root, Some(payload_ref))
            .await?
            .removed_bytes(payload_ref),
    )
}

async fn drain_pending_payload_deletes_matching(
    conn: &(impl Executor + ?Sized),
    storage_root: &Path,
    payload_ref: Option<&str>,
) -> Result<PayloadDeleteDrain, LcmError> {
    let mut rows = match payload_ref {
        Some(payload_ref) => {
            conn.query(
                "SELECT key, value FROM lcm_gc_meta WHERE key = ?1",
                params![pending_payload_delete_key(payload_ref)],
            )
            .await?
        }
        None => {
            conn.query(
                "SELECT key, value FROM lcm_gc_meta WHERE key GLOB 'pending_payload_delete:*' ORDER BY key",
                (),
            )
            .await?
        }
    };
    let mut drain = PayloadDeleteDrain::default();
    let mut pending = Vec::new();
    while let Some(row) = rows.next().await? {
        let key: String = match row.get(0) {
            Ok(key) => key,
            Err(err) => {
                drain.add_error(
                    "<unknown>",
                    "malformed_tombstone",
                    format!("invalid tombstone key: {err}"),
                );
                continue;
            }
        };
        let value: String = match row.get(1) {
            Ok(value) => value,
            Err(err) => {
                drain.add_error(
                    &key,
                    "malformed_tombstone",
                    format!("invalid tombstone value: {err}"),
                );
                continue;
            }
        };
        let Some(payload_ref) = key.strip_prefix(PENDING_PAYLOAD_DELETE_PREFIX) else {
            continue;
        };
        if let Err(err) = payload::validate_payload_ref(payload_ref) {
            drain.add_error(payload_ref, "malformed_tombstone", err.to_string());
            continue;
        }
        let payload_ref = payload_ref.to_string();
        let delete: PendingPayloadDelete = match serde_json::from_str(&value) {
            Ok(delete) => delete,
            Err(err) => {
                drain.add_error(&payload_ref, "malformed_tombstone", err.to_string());
                continue;
            }
        };
        pending.push((key, payload_ref, delete));
    }
    drop(rows);

    for (key, payload_ref, pending) in pending {
        let mut metadata = match conn
            .query(
                "SELECT 1 FROM lcm_external_payloads WHERE payload_ref = ?1 LIMIT 1",
                params![payload_ref.as_str()],
            )
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                drain.add_error(&payload_ref, "metadata_check_failed", err.to_string());
                continue;
            }
        };
        let metadata_exists = match metadata.next().await {
            Ok(row) => row.is_some(),
            Err(err) => {
                drain.add_error(&payload_ref, "metadata_check_failed", err.to_string());
                continue;
            }
        };
        drop(metadata);
        if metadata_exists {
            schema::clear_gc_meta(conn, &key).await?;
            drain
                .outcomes
                .preserved
                .add(&payload_ref, pending.byte_count);
            continue;
        }

        let removal = match payload::remove_committed_payload_file(
            storage_root,
            &payload_ref,
            pending.content_hash.as_deref(),
            pending.byte_count,
            pending.char_count,
        ) {
            Ok(removal) => removal,
            Err(err) => {
                drain.add_error(&payload_ref, "payload_delete_failed", err.to_string());
                continue;
            }
        };
        schema::clear_gc_meta(conn, &key).await?;
        match removal {
            payload::CommittedPayloadRemoval::Missing => {
                drain.outcomes.missing.add(&payload_ref, pending.byte_count);
            }
            payload::CommittedPayloadRemoval::Removed(actual_bytes) => {
                drain.outcomes.removed.add(&payload_ref, actual_bytes);
            }
            payload::CommittedPayloadRemoval::ReplacementPreserved => {
                drain
                    .outcomes
                    .preserved
                    .add(&payload_ref, pending.byte_count);
                tracing::warn!(
                    payload_ref,
                    "preserved payload replacement and cleared stale deletion tombstone"
                );
            }
        }
    }
    record_pending_delete_diagnostics(conn, &drain).await?;
    Ok(drain)
}

async fn record_pending_delete_diagnostics(
    conn: &(impl Executor + ?Sized),
    drain: &PayloadDeleteDrain,
) -> Result<(), LcmError> {
    if drain.outcomes.failed.is_empty() {
        schema::clear_gc_meta(conn, PENDING_PAYLOAD_DELETE_ERROR_COUNT_KEY).await?;
        if schema::get_gc_meta(conn, "last_error")
            .await?
            .is_some_and(|error| error.starts_with(PENDING_PAYLOAD_DELETE_ERROR_PREFIX))
        {
            schema::clear_gc_meta(conn, "last_error").await?;
            if schema::get_gc_meta(conn, "last_gc_status")
                .await?
                .as_deref()
                == Some("partial")
            {
                schema::set_gc_meta(conn, "last_gc_status", "ok").await?;
            }
        }
        return Ok(());
    }

    let sample = drain
        .errors
        .iter()
        .take(3)
        .map(|error| format!("{} [{}]: {}", error.payload_ref, error.kind, error.detail))
        .collect::<Vec<_>>()
        .join("; ");
    let detail = format!(
        "{PENDING_PAYLOAD_DELETE_ERROR_PREFIX} {} failure(s); {}",
        drain.outcomes.failed.count, sample
    );
    let detail = detail.chars().take(1_024).collect::<String>();
    schema::set_gc_meta(
        conn,
        PENDING_PAYLOAD_DELETE_ERROR_COUNT_KEY,
        &drain.outcomes.failed.count.to_string(),
    )
    .await?;
    schema::set_gc_meta(conn, "last_gc_status", "partial").await?;
    schema::set_gc_meta(conn, "last_error", &detail).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_replaces_resolved_failures_with_latest_snapshot() {
        let mut first = PayloadDeleteDrain::default();
        first.add_error(
            "payload_a.payload",
            "payload_delete_failed",
            "first".to_string(),
        );

        let mut second = PayloadDeleteDrain::default();
        second.outcomes.removed.add("payload_a.payload", 12);
        second.add_error(
            "payload_b.payload",
            "payload_delete_failed",
            "second".to_string(),
        );
        first.merge(second);

        assert_eq!(first.outcomes.removed.count, 1);
        assert_eq!(first.outcomes.failed.refs, ["payload_b.payload"]);
        assert_eq!(first.errors.len(), 1);
        assert_eq!(first.errors[0].payload_ref, "payload_b.payload");
    }

    #[test]
    fn pending_delete_serializes_only_digest_and_sizes() {
        let pending = PendingPayloadDelete {
            content_hash: Some("digest".to_string()),
            byte_count: 11,
            char_count: Some(7),
        };
        let serialized = serde_json::to_value(pending).unwrap();
        assert_eq!(
            serialized,
            serde_json::json!({
                "content_hash": "digest",
                "byte_count": 11,
                "char_count": 7,
            })
        );
    }
}

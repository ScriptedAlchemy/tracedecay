//! The one canonical spelling of "advance an observation source cursor".
//!
//! Every executor of the cursor-advance authority — the runtime write
//! command path ([`super::ObservationExecutor`]) and the global-db
//! observation adapter's atomic refusal-marker + coverage transaction —
//! reads, records, verifies, and commits through exactly this statement set,
//! so the authority cannot drift into parallel spellings. The statements are
//! transport-neutral text: one caller binds them on the runtime writer's
//! rusqlite savepoint, the other on the engine's guarded write transaction.

/// Durable cursor for one source: params `(source_json, scope_json)`,
/// column `cursor_json`.
pub const READ_SOURCE_CURSOR_SQL: &str = "SELECT cursor_json FROM source_cursors
     WHERE source_json = ?1 AND scope_json = ?2";

/// Idempotent advance-ledger insert: params `(source_json, scope_json,
/// coverage_json, reason, receipt_id)`. A replay of the same coverage key is
/// a no-op; the read-back verification decides whether the retained row is
/// this advance or a collision.
pub const RECORD_CURSOR_ADVANCE_SQL: &str = "INSERT INTO source_cursor_advances (
        source_json, scope_json, coverage_json, reason, receipt_id
     ) VALUES (?1, ?2, ?3, ?4, ?5)
     ON CONFLICT(source_json, scope_json, coverage_json) DO NOTHING";

/// Advance-ledger read-back for in-transaction verification: params
/// `(source_json, scope_json, coverage_json)`, columns `(reason,
/// receipt_id)`.
pub const READ_CURSOR_ADVANCE_SQL: &str = "SELECT reason, receipt_id FROM source_cursor_advances
     WHERE source_json = ?1 AND scope_json = ?2 AND coverage_json = ?3";

/// Moves the durable cursor to the advance's next position: params
/// `(source_json, scope_json, cursor_json)`.
pub const COMMIT_SOURCE_CURSOR_SQL: &str =
    "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
     VALUES (?1, ?2, ?3)
     ON CONFLICT(source_json, scope_json) DO UPDATE SET
        cursor_json = excluded.cursor_json";

/// Whether one [`READ_CURSOR_ADVANCE_SQL`] row is exactly this advance's
/// row — the same reason and the same (possibly absent) sanitization receipt
/// id. Any other row retained under the coverage key is a cursor-advance
/// collision.
#[must_use]
pub fn cursor_advance_ledger_row_matches(
    stored: Option<&(String, Option<String>)>,
    reason: &str,
    receipt_id: Option<&str>,
) -> bool {
    stored.is_some_and(|(stored_reason, stored_receipt)| {
        stored_reason == reason && stored_receipt.as_deref() == receipt_id
    })
}

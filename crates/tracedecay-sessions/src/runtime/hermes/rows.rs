//! Joined `messages` × `sessions` row model and byte accounting for the
//! bounded `SQLite` page sweep.

#[cfg(test)]
use crate::runtime::shared::StoredCursor;
use tracedecay_runtime_core::privacy::MAX_OBSERVATION_RECORD_BYTES;

use super::MAX_HERMES_VALUE_BYTES;

/// One joined `messages` × `sessions` row read past the cursor.
pub struct HermesRow {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_name: Option<String>,
    pub tool_calls: Option<String>,
    pub timestamp: Option<f64>,
    pub session_model: Option<String>,
    pub parent_session_id: Option<String>,
    pub session_cwd: Option<String>,
    pub session_source: Option<String>,
    pub session_title: Option<String>,
    pub session_started_at: Option<f64>,
    pub session_ended_at: Option<f64>,
    pub session_input_tokens: Option<i64>,
    pub session_output_tokens: Option<i64>,
    pub session_cache_read_tokens: Option<i64>,
    pub session_cache_write_tokens: Option<i64>,
    pub session_reasoning_tokens: Option<i64>,
    /// `messages.active` soft-delete flag (0 = rewound/undone turn). Legacy
    /// stores without the column read as 1.
    pub active: i64,
    /// Set when SQL `typeof`/`length` rejected a column before materialization.
    pub sql_value_oversized: bool,
    /// Sum of SQL `length()` charges for text/blob columns (not Rust `String::len`).
    pub sql_measured_bytes: u64,
}

/// One bounded `SQLite` page: row count, per-value, and cumulative byte caps applied
/// before `String`/`Vec` materialization.
pub(super) struct HermesPageRead {
    pub items: Vec<HermesRow>,
    #[cfg(test)]
    pub new_cursor: StoredCursor,
    /// More rows remain at the authority, but the page byte budget stopped collection.
    pub truncated_by_byte_budget: bool,
}

fn text_bytes<const N: usize>(values: [Option<&str>; N]) -> u64 {
    values.into_iter().flatten().fold(0_u64, |total, value| {
        total.saturating_add(u64::try_from(value.len()).unwrap_or(u64::MAX))
    })
}

pub(super) fn hermes_native_payload_bytes(row: &HermesRow) -> u64 {
    let text_bytes = text_bytes([
        Some(row.session_id.as_str()),
        Some(row.role.as_str()),
        row.content.as_deref(),
        row.reasoning.as_deref(),
        row.tool_name.as_deref(),
        row.tool_calls.as_deref(),
        row.session_model.as_deref(),
        row.parent_session_id.as_deref(),
        row.session_source.as_deref(),
        row.session_title.as_deref(),
    ]);
    let scalar_count = u64::from(row.timestamp.is_some())
        .saturating_add(u64::from(row.session_started_at.is_some()))
        .saturating_add(u64::from(row.session_ended_at.is_some()))
        .saturating_add(u64::from(row.session_input_tokens.is_some()))
        .saturating_add(u64::from(row.session_output_tokens.is_some()))
        .saturating_add(u64::from(row.session_cache_read_tokens.is_some()))
        .saturating_add(u64::from(row.session_cache_write_tokens.is_some()))
        .saturating_add(u64::from(row.session_reasoning_tokens.is_some()));
    text_bytes.saturating_add(scalar_count.saturating_mul(8))
}

fn hermes_row_bytes(row: &HermesRow) -> u64 {
    hermes_native_payload_bytes(row)
        .saturating_add(text_bytes([row.session_cwd.as_deref()]))
        .saturating_add(16)
}

pub(super) fn hermes_budget_bytes(row: &HermesRow) -> u64 {
    let capped = u64::try_from(MAX_OBSERVATION_RECORD_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    // Prefer SQL-measured length (includes rejected oversize/blob sizes) so
    // hostile values charge the pass budget without being materialized.
    row.sql_measured_bytes
        .max(hermes_row_bytes(row))
        .min(capped)
}

pub(super) fn hermes_page_row_charge(sql_measured_bytes: u64) -> u64 {
    let capped = u64::try_from(MAX_HERMES_VALUE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    sql_measured_bytes.min(capped)
}

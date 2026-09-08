//! Diagnostics summary assembly for CLI, MCP admin, and dashboard HTTP.
//!
//! Hook-JSONL import/sync stays in the parent module. This submodule owns the
//! read-side windowed hook-file scan, durable-event row mapping, and the
//! diagnostics payload those surfaces share. `tracedecay-dashboard-api`
//! re-exports these items so HTTP and generated contracts stay on one
//! authority without a usecases → dashboard-api cycle.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_global_db::AnalyticsEventRecord;

pub trait HookReadinessProjectionPort: Send + Sync {
    fn aggregate_hook_completed_readiness(&self, rows: &[Value]) -> Value;
}

static HOOK_READINESS_PROJECTION: OnceLock<Arc<dyn HookReadinessProjectionPort>> = OnceLock::new();

pub fn install_hook_readiness_projection(
    projection: Arc<dyn HookReadinessProjectionPort>,
) -> Result<(), Arc<dyn HookReadinessProjectionPort>> {
    HOOK_READINESS_PROJECTION.set(projection)
}

pub fn aggregate_hook_completed_readiness(rows: &[Value]) -> Value {
    HOOK_READINESS_PROJECTION.get().map_or_else(
        || {
            json!({
                "schema_version": 1,
                "source_event": "hook_completed",
                "collection_status": "unavailable",
                "input_rows_received": rows.len(),
                "input_rows_processed": 0,
                "input_rows_dropped_at_cap": 0,
                "events_considered": 0,
                "events_skipped_non_completed": rows.len(),
                "unavailable_metrics": [{
                    "metric": "hook_readiness",
                    "status": "unavailable",
                    "blocker": "hook readiness projection is not mounted"
                }]
            })
        },
        |projection| projection.aggregate_hook_completed_readiness(rows),
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsDiagnosticsRatiosV1 {
    pub events_per_message: f64,
    pub tool_calls_per_message: f64,
    pub mcp_tool_calls_per_message: f64,
    pub hook_calls_per_message: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsEventKindCountV1 {
    pub event_kind: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsToolCountV1 {
    pub tool_name: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsOutcomeCountV1 {
    pub outcome: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHookWindowV1 {
    pub window_rows: i64,
    pub rows_scanned: i64,
    pub rows_included: i64,
    pub truncated: bool,
    pub total_rows_known: bool,
    pub oldest_ts_unix_ms: Option<i64>,
    pub newest_ts_unix_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsRecentEventV1 {
    pub timestamp: Option<i64>,
    pub event_kind: String,
    pub hook_name: String,
    pub tool_name: String,
    pub outcome: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsToolCategoryCountV1 {
    pub tool_category: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHookNameCountV1 {
    pub hook_name: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsPromptCategoryCountV1 {
    pub prompt_category: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsRecentHookV1 {
    pub ts_unix_ms: Option<i64>,
    pub agent: String,
    pub hook_name: String,
    pub session_id: String,
    pub tool_name: String,
    pub prompt_category: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintEfficacyTotalsV1 {
    pub emitted: i64,
    pub acted: i64,
    pub ignored: i64,
    pub unresolved: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintEfficacyCategoryV1 {
    pub category: String,
    pub emitted: i64,
    pub acted: i64,
    pub ignored: i64,
    pub unresolved: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsHintEfficacyV1 {
    pub available: bool,
    pub source: String,
    pub totals: AnalyticsHintEfficacyTotalsV1,
    pub by_category: Vec<AnalyticsHintEfficacyCategoryV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyticsDiagnosticsPayloadV1 {
    pub available: bool,
    pub source: String,
    pub message_count: i64,
    pub event_count: i64,
    pub tool_call_count: i64,
    pub mcp_tool_call_count: i64,
    pub tracedecay_call_count: i64,
    pub hook_call_count: i64,
    /// Provenance rows for the hook-analytics source files backing the
    /// hook rollups. Free-form provenance, not a versioned sub-contract.
    pub hook_sources: Vec<Value>,
    /// Daemon-owned hook readiness projection. The projection stamps its own
    /// `schema_version`; the diagnostics read carries it verbatim.
    pub hook_readiness: Value,
    #[serde(default)]
    pub events_per_hour: Option<f64>,
    pub ratios: AnalyticsDiagnosticsRatiosV1,
    pub by_event_kind: Vec<AnalyticsEventKindCountV1>,
    pub by_tool: Vec<AnalyticsToolCountV1>,
    pub by_mcp_tool: Vec<AnalyticsToolCountV1>,
    pub by_tool_category: Vec<AnalyticsToolCategoryCountV1>,
    pub by_outcome: Vec<AnalyticsOutcomeCountV1>,
    pub by_hook: Vec<AnalyticsHookNameCountV1>,
    pub by_prompt_category: Vec<AnalyticsPromptCategoryCountV1>,
    pub hint_efficacy: AnalyticsHintEfficacyV1,
    pub hook_window: AnalyticsHookWindowV1,
    pub recent_events: Vec<AnalyticsRecentEventV1>,
    pub recent_hooks: Vec<AnalyticsRecentHookV1>,
}

pub fn durable_analytics_event_row(event: &AnalyticsEventRecord) -> Value {
    json!({
        "provider": &event.provider,
        "timestamp": event.timestamp,
        "event_kind": &event.event_kind,
        "hook_name": &event.hook_name,
        "tool_name": &event.tool_name,
        "tool_category": &event.tool_category,
        "skill_name": &event.skill_name,
        "hint_category": &event.hint_category,
        "outcome": &event.outcome,
        "metadata_json": &event.metadata_json,
    })
}

#[derive(Default)]
struct HintEfficacyCounts {
    emitted: i64,
    acted: i64,
    ignored: i64,
}

/// Per-category hint efficacy from durable `hint_emitted` + `hint_outcome`
/// events: how many hints were emitted, how many the model then acted on, how
/// many it ignored, and how many remain unresolved (emitted with no outcome yet
/// — the correlator's later-pass backlog). `unresolved` is derived so it stays
/// non-negative even if the event sample is truncated mid-pair.
pub fn hint_efficacy_from_events(events: &[AnalyticsEventRecord]) -> AnalyticsHintEfficacyV1 {
    let mut by_category: BTreeMap<String, HintEfficacyCounts> = BTreeMap::new();
    let mut totals = HintEfficacyCounts::default();

    for event in events {
        let category = event.hint_category.as_deref().unwrap_or("");
        if category.is_empty() {
            continue;
        }
        let event_kind = event.event_kind.as_str();
        let outcome = event.outcome.as_deref().unwrap_or("");
        if let Some(counts) = by_category.get_mut(category) {
            apply_hint_efficacy_event(counts, &mut totals, event_kind, outcome);
            continue;
        }
        let mut counts = HintEfficacyCounts::default();
        apply_hint_efficacy_event(&mut counts, &mut totals, event_kind, outcome);
        by_category.insert(category.to_owned(), counts);
    }

    let rows = by_category
        .into_iter()
        .map(|(category, counts)| {
            let unresolved = (counts.emitted - counts.acted - counts.ignored).max(0);
            AnalyticsHintEfficacyCategoryV1 {
                category,
                emitted: counts.emitted,
                acted: counts.acted,
                ignored: counts.ignored,
                unresolved,
            }
        })
        .collect::<Vec<_>>();

    AnalyticsHintEfficacyV1 {
        available: !rows.is_empty(),
        source: "analytics_events".to_owned(),
        totals: AnalyticsHintEfficacyTotalsV1 {
            emitted: totals.emitted,
            acted: totals.acted,
            ignored: totals.ignored,
            unresolved: (totals.emitted - totals.acted - totals.ignored).max(0),
        },
        by_category: rows,
    }
}

fn apply_hint_efficacy_event(
    counts: &mut HintEfficacyCounts,
    totals: &mut HintEfficacyCounts,
    event_kind: &str,
    outcome: &str,
) {
    match event_kind {
        "hint_emitted" => {
            counts.emitted += 1;
            totals.emitted += 1;
        }
        "hint_outcome" => match normalize(outcome).as_str() {
            "acted" => {
                counts.acted += 1;
                totals.acted += 1;
            }
            "ignored" => {
                counts.ignored += 1;
                totals.ignored += 1;
            }
            _ => {}
        },
        _ => {}
    }
}

pub fn diagnostics_summary_from_parts(
    message_count: i64,
    hook_analytics: &HookAnalyticsRows,
    durable_events: Option<&[Value]>,
) -> Value {
    let records = durable_events.map(|events| {
        events
            .iter()
            .map(analytics_event_from_json_row)
            .collect::<Vec<_>>()
    });
    match serde_json::to_value(diagnostics_payload_from_parts(
        message_count,
        hook_analytics,
        records.as_deref(),
    )) {
        Ok(value) => value,
        Err(error) => json!({
            "available": false,
            "source": "analytics_diagnostics_encode_failed",
            "detail": error.to_string(),
        }),
    }
}

fn analytics_event_from_json_row(row: &Value) -> AnalyticsEventRecord {
    AnalyticsEventRecord {
        id: 0,
        provider: str_field(row, "provider").to_owned(),
        project_id: String::new(),
        session_id: optional_text(row, "session_id"),
        timestamp: row.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
        event_kind: str_field(row, "event_kind").to_owned(),
        hook_name: optional_text(row, "hook_name"),
        tool_name: optional_text(row, "tool_name"),
        tool_category: optional_text(row, "tool_category"),
        skill_name: optional_text(row, "skill_name"),
        hint_category: optional_text(row, "hint_category"),
        hint_id: optional_text(row, "hint_id"),
        outcome: optional_text(row, "outcome"),
        metadata_json: optional_text(row, "metadata_json"),
    }
}

pub fn diagnostics_payload_from_parts(
    message_count: i64,
    hook_analytics: &HookAnalyticsRows,
    durable_events: Option<&[AnalyticsEventRecord]>,
) -> AnalyticsDiagnosticsPayloadV1 {
    let hook_rows = &hook_analytics.rows;
    let hook_call_count = hook_invocation_count(hook_rows);
    let hook_readiness = aggregate_hook_completed_readiness(hook_rows);

    let Some(events) = durable_events else {
        return AnalyticsDiagnosticsPayloadV1 {
            available: !hook_rows.is_empty() || message_count > 0,
            source: "session_messages_and_hook_analytics".to_owned(),
            message_count,
            event_count: 0,
            tool_call_count: 0,
            mcp_tool_call_count: 0,
            tracedecay_call_count: 0,
            hook_call_count,
            hook_sources: hook_analytics.sources.clone(),
            hook_readiness,
            events_per_hour: None,
            ratios: diagnostics_ratios(message_count, 0, 0, 0, hook_call_count),
            by_event_kind: Vec::new(),
            by_tool: Vec::new(),
            by_mcp_tool: Vec::new(),
            by_tool_category: Vec::new(),
            by_outcome: Vec::new(),
            by_hook: hook_count_rows(hook_rows),
            by_prompt_category: hook_prompt_category_rows(hook_rows),
            hint_efficacy: AnalyticsHintEfficacyV1 {
                available: false,
                source: "analytics_events_unavailable".to_owned(),
                totals: AnalyticsHintEfficacyTotalsV1 {
                    emitted: 0,
                    acted: 0,
                    ignored: 0,
                    unresolved: 0,
                },
                by_category: Vec::new(),
            },
            hook_window: hook_analytics.window_payload(),
            recent_events: Vec::new(),
            recent_hooks: recent_hook_rows(hook_rows, 20),
        };
    };

    let mut by_event_kind = BTreeMap::new();
    let mut by_tool = BTreeMap::new();
    let mut by_mcp_tool = BTreeMap::new();
    let mut by_tool_category = BTreeMap::new();
    let mut by_outcome = BTreeMap::new();
    let mut tool_call_count = 0;
    let mut mcp_tool_call_count = 0;
    let mut tracedecay_call_count = 0;
    let mut first_ts: Option<i64> = None;
    let mut last_ts: Option<i64> = None;

    for event in events {
        let event_kind = event.event_kind.as_str();
        let tool_name = event.tool_name.as_deref().unwrap_or("");
        increment_string_count(&mut by_event_kind, event_kind);
        increment_string_count(
            &mut by_tool_category,
            event.tool_category.as_deref().unwrap_or(""),
        );
        increment_string_count(&mut by_outcome, event.outcome.as_deref().unwrap_or(""));

        first_ts = Some(first_ts.map_or(event.timestamp, |current| current.min(event.timestamp)));
        last_ts = Some(last_ts.map_or(event.timestamp, |current| current.max(event.timestamp)));

        if !tool_name.is_empty() {
            tool_call_count += 1;
            increment_string_count(&mut by_tool, tool_name);
            if event_kind == "mcp_tool_call" || tool_name.starts_with("mcp__") {
                mcp_tool_call_count += 1;
                increment_string_count(&mut by_mcp_tool, tool_name);
            }
            if tracedecay_automation::analytics::normalize_tool_name(tool_name)
                .starts_with("tracedecay_")
            {
                tracedecay_call_count += 1;
            }
        }
    }

    let span_secs = match (first_ts, last_ts) {
        (Some(first), Some(last)) => last.saturating_sub(first).max(1),
        _ => 0,
    };
    let events_per_hour = if span_secs > 0 {
        (events.len() as f64) * 3600.0 / span_secs as f64
    } else {
        0.0
    };

    AnalyticsDiagnosticsPayloadV1 {
        available: true,
        source: "analytics_events".to_owned(),
        message_count,
        event_count: events.len() as i64,
        tool_call_count,
        mcp_tool_call_count,
        tracedecay_call_count,
        hook_call_count,
        hook_sources: hook_analytics.sources.clone(),
        hook_readiness,
        events_per_hour: Some(events_per_hour),
        ratios: diagnostics_ratios(
            message_count,
            events.len() as i64,
            tool_call_count,
            mcp_tool_call_count,
            hook_call_count,
        ),
        by_event_kind: by_event_kind
            .into_iter()
            .map(|(event_kind, count)| AnalyticsEventKindCountV1 { event_kind, count })
            .collect(),
        by_tool: by_tool
            .into_iter()
            .map(|(tool_name, count)| AnalyticsToolCountV1 { tool_name, count })
            .collect(),
        by_mcp_tool: by_mcp_tool
            .into_iter()
            .map(|(tool_name, count)| AnalyticsToolCountV1 { tool_name, count })
            .collect(),
        by_tool_category: by_tool_category
            .into_iter()
            .map(|(tool_category, count)| AnalyticsToolCategoryCountV1 {
                tool_category,
                count,
            })
            .collect(),
        by_outcome: by_outcome
            .into_iter()
            .map(|(outcome, count)| AnalyticsOutcomeCountV1 { outcome, count })
            .collect(),
        by_hook: hook_count_rows(hook_rows),
        by_prompt_category: hook_prompt_category_rows(hook_rows),
        hint_efficacy: hint_efficacy_from_events(events),
        hook_window: hook_analytics.window_payload(),
        recent_events: recent_event_rows(events, 20),
        recent_hooks: recent_hook_rows(hook_rows, 20),
    }
}

fn diagnostics_ratios(
    message_count: i64,
    event_count: i64,
    tool_call_count: i64,
    mcp_tool_call_count: i64,
    hook_call_count: i64,
) -> AnalyticsDiagnosticsRatiosV1 {
    AnalyticsDiagnosticsRatiosV1 {
        events_per_message: per_message(event_count, message_count),
        tool_calls_per_message: per_message(tool_call_count, message_count),
        mcp_tool_calls_per_message: per_message(mcp_tool_call_count, message_count),
        hook_calls_per_message: per_message(hook_call_count, message_count),
    }
}

fn per_message(count: i64, message_count: i64) -> f64 {
    if message_count <= 0 {
        0.0
    } else {
        count as f64 / message_count as f64
    }
}

fn increment_string_count(counts: &mut BTreeMap<String, i64>, key: &str) {
    if key.is_empty() {
        return;
    }
    if let Some(count) = counts.get_mut(key) {
        *count += 1;
        return;
    }
    counts.insert(key.to_owned(), 1);
}

/// Trailing rows read per `hook_analytics.jsonl` file.
///
/// The hook stream is append-only and unbounded: on an active profile the
/// project-level file reaches hundreds of megabytes and over a million rows,
/// and folding all of it per request cost ~14s. Diagnostics therefore reads a
/// bounded suffix of each file. Every figure derived from hook rows describes
/// that window, not all time, and the payload captions it under `hook_window`.
pub const HOOK_ANALYTICS_WINDOW_ROWS: usize = 10_000;

/// Suffix chunk size used when walking a hook analytics file backwards.
const HOOK_ANALYTICS_TAIL_CHUNK_BYTES: u64 = 1 << 20;

/// Window provenance for the hook rows folded into a diagnostics payload.
#[derive(Default)]
pub struct HookAnalyticsWindow {
    /// Per-file cap on trailing rows scanned.
    pub window_rows: usize,
    /// Rows actually scanned across every file in the window.
    pub rows_scanned: i64,
    /// True when at least one file was larger than its window, so the
    /// aggregates cover a recent suffix rather than the full history.
    pub truncated: bool,
}

pub struct HookAnalyticsRows {
    pub rows: Vec<Value>,
    pub sources: Vec<Value>,
    pub window: HookAnalyticsWindow,
}

impl HookAnalyticsRows {
    pub fn empty() -> Self {
        Self {
            rows: Vec::new(),
            sources: Vec::new(),
            window: HookAnalyticsWindow {
                window_rows: HOOK_ANALYTICS_WINDOW_ROWS,
                rows_scanned: 0,
                truncated: false,
            },
        }
    }

    /// Caption describing exactly which slice of the hook stream the sibling
    /// hook figures (`hook_call_count`, `by_hook`, `by_prompt_category`,
    /// `hook_readiness`, `recent_hooks`) were computed over.
    fn window_payload(&self) -> AnalyticsHookWindowV1 {
        let mut oldest_ts_unix_ms: Option<i64> = None;
        let mut newest_ts_unix_ms: Option<i64> = None;
        for row in &self.rows {
            let Some(timestamp) = row.get("ts_unix_ms").and_then(Value::as_i64) else {
                continue;
            };
            oldest_ts_unix_ms =
                Some(oldest_ts_unix_ms.map_or(timestamp, |current| current.min(timestamp)));
            newest_ts_unix_ms =
                Some(newest_ts_unix_ms.map_or(timestamp, |current| current.max(timestamp)));
        }
        AnalyticsHookWindowV1 {
            window_rows: self.window.window_rows as i64,
            rows_scanned: self.window.rows_scanned,
            rows_included: self.rows.len() as i64,
            truncated: self.window.truncated,
            total_rows_known: !self.window.truncated,
            oldest_ts_unix_ms,
            newest_ts_unix_ms,
        }
    }
}

/// Hooks write `hook_analytics.jsonl` into the project store when they can
/// resolve a project root and into the user-level profile root otherwise, so
/// a project's hook stream is split across both files. Read both, keeping
/// only user-level rows whose attribution places them inside this project.
/// Passing no `project_root` includes every user-level row instead of
/// filtering. Shared with the `tracedecay analytics` CLI.
///
/// Reads only the trailing [`HOOK_ANALYTICS_WINDOW_ROWS`] rows of each file;
/// see [`HookAnalyticsRows::window_payload`] for the caption callers must
/// surface alongside any derived figure.
pub fn read_hook_analytics_rows_at(
    store_root: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
) -> HookAnalyticsRows {
    let mut out = HookAnalyticsRows::empty();
    let store_path = store_root.map(|root| root.join("hook_analytics.jsonl"));
    if let Some(store_path) = &store_path {
        read_hook_analytics_file(store_path, None, &mut out);
    }
    if let Ok(profile_root) = tracedecay_runtime_core::storage::default_profile_root() {
        let global_path = profile_root.join("hook_analytics.jsonl");
        if store_path.as_deref() != Some(global_path.as_path()) {
            read_hook_analytics_file(&global_path, project_root, &mut out);
        }
    }
    sort_hook_analytics_rows(&mut out.rows);
    out
}

pub fn sort_hook_analytics_rows(rows: &mut [Value]) {
    // `sort_by` is stable. Rows sort chronologically, then by durable event fields.
    // Exact-key ties (including rows with all fields missing) retain deterministic
    // ingestion order: project JSONL line order, followed by profile JSONL line order.
    rows.sort_by(|left, right| {
        hook_analytics_row_order_key(left).cmp(&hook_analytics_row_order_key(right))
    });
}

fn hook_analytics_row_order_key(row: &Value) -> (i64, &str, &str, &str) {
    (
        row.get("ts_unix_ms")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        row.get("session_id").and_then(Value::as_str).unwrap_or(""),
        row.get("hook_name").and_then(Value::as_str).unwrap_or(""),
        row.get("agent").and_then(Value::as_str).unwrap_or(""),
    )
}

/// Read the last `window_rows` newline-delimited records of `path`.
///
/// Walks the file backwards in [`HOOK_ANALYTICS_TAIL_CHUNK_BYTES`] chunks so
/// cost tracks the window, not the file. Returns the lines oldest-first
/// alongside `reached_file_start`, which is false when the file held more rows
/// than the window and the result is therefore a suffix.
fn read_hook_analytics_tail(
    path: &std::path::Path,
    window_rows: usize,
) -> std::io::Result<(Vec<String>, bool)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let mut end = file.metadata()?.len();
    let mut buffer: Vec<u8> = Vec::new();
    let mut reached_file_start = true;
    let mut starts_at_line_boundary = true;
    let mut retained_newlines = 0_usize;

    while end > 0 {
        let chunk_len = HOOK_ANALYTICS_TAIL_CHUNK_BYTES.min(end);
        let start = end - chunk_len;
        let mut chunk = vec![0u8; usize::try_from(chunk_len).unwrap_or(usize::MAX)];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk)?;
        // Count newlines in the newly read chunk only; the running total
        // covers the whole retained buffer without rescanning it per chunk.
        retained_newlines += chunk.iter().filter(|byte| **byte == b'\n').count();
        chunk.extend_from_slice(&buffer);
        buffer = chunk;
        end = start;

        // Every newline in the retained bytes terminates a record we hold in
        // full, so it is a lower bound on complete records available.
        if end > 0 && retained_newlines >= window_rows {
            reached_file_start = false;
            file.seek(SeekFrom::Start(end.saturating_sub(1)))?;
            let mut preceding = [0_u8; 1];
            file.read_exact(&mut preceding)?;
            starts_at_line_boundary = preceding[0] == b'\n';
            break;
        }
    }

    let text = String::from_utf8_lossy(&buffer);
    let mut lines: Vec<&str> = text.lines().collect();
    if !reached_file_start && !starts_at_line_boundary && !lines.is_empty() {
        // The first retained line began before the chunk boundary and is
        // truncated; drop it rather than reporting it as malformed.
        lines.remove(0);
    }
    if lines.len() > window_rows {
        reached_file_start = false;
        lines.drain(..lines.len() - window_rows);
    }

    Ok((
        lines.into_iter().map(str::to_string).collect(),
        reached_file_start,
    ))
}

pub fn read_hook_analytics_file(
    path: &std::path::Path,
    project_filter: Option<&std::path::Path>,
    out: &mut HookAnalyticsRows,
) {
    let window_rows = out.window.window_rows;
    let Ok((lines, reached_file_start)) = read_hook_analytics_tail(path, window_rows) else {
        return;
    };
    // `rows_scanned` counts every line in the window; `rows_total` keeps its
    // original meaning of well-formed rows, so malformed lines stay visible as
    // the difference between the two rather than inflating the parsed count.
    let rows_scanned = lines.len() as i64;
    let mut rows_total = 0i64;
    let mut rows_included = 0i64;
    let mut rows_malformed = 0i64;
    let mut first_malformed_offset = None;
    let mut first_malformed_error = None;
    for (index, line) in lines.iter().enumerate() {
        let row = match serde_json::from_str::<Value>(line) {
            Ok(row) => row,
            Err(err) => {
                rows_malformed += 1;
                if first_malformed_offset.is_none() {
                    first_malformed_offset = Some(index + 1);
                    first_malformed_error = Some(err.to_string());
                }
                tracing::warn!(
                    hook_analytics_path = %path.display(),
                    window_line_number = index + 1,
                    error = %err,
                    "skipping malformed hook analytics jsonl row"
                );
                continue;
            }
        };
        rows_total += 1;
        let included = match project_filter {
            None => true,
            Some(root) => hook_row_matches_project(&row, root),
        };
        if included {
            rows_included += 1;
            out.rows.push(row);
        }
    }
    out.window.rows_scanned += rows_scanned;
    out.window.truncated |= !reached_file_start;
    out.sources.push(json!({
        "path": path.display().to_string(),
        // Counts describe the trailing window only. `window_truncated` is true
        // when the file extends past it, so `rows_total` is not the file total.
        "rows_scanned": rows_scanned,
        "rows_total": rows_total,
        "rows_included": rows_included,
        "rows_malformed": rows_malformed,
        "window_rows": window_rows as i64,
        "window_truncated": !reached_file_start,
        // Line numbers are relative to the window, not the file, when truncated.
        "first_malformed_line": first_malformed_offset,
        "first_malformed_error": first_malformed_error,
    }));
}

/// Rows written since project attribution landed carry `project_root` and/or
/// `event_cwd`; earlier user-level rows carry neither and stay unattributed.
fn hook_row_matches_project(row: &Value, project_root: &std::path::Path) -> bool {
    ["project_root", "event_cwd"].iter().any(|key| {
        row.get(*key)
            .and_then(Value::as_str)
            .is_some_and(|value| std::path::Path::new(value).starts_with(project_root))
    })
}

fn hook_invocation_count(rows: &[Value]) -> i64 {
    rows.iter()
        .filter(|row| str_field(row, "event") == "hook_invoked")
        .count() as i64
}

fn hook_count_rows(rows: &[Value]) -> Vec<AnalyticsHookNameCountV1> {
    let mut counts = BTreeMap::new();
    for row in rows {
        if str_field(row, "event") == "hook_invoked" {
            increment_string_count(&mut counts, str_field(row, "hook_name"));
        }
    }
    counts
        .into_iter()
        .map(|(hook_name, count)| AnalyticsHookNameCountV1 { hook_name, count })
        .collect()
}

fn hook_prompt_category_rows(rows: &[Value]) -> Vec<AnalyticsPromptCategoryCountV1> {
    let mut counts = BTreeMap::new();
    for row in rows {
        if str_field(row, "event") == "hook_invoked" {
            increment_string_count(&mut counts, str_field(row, "prompt_category"));
        }
    }
    counts
        .into_iter()
        .map(|(prompt_category, count)| AnalyticsPromptCategoryCountV1 {
            prompt_category,
            count,
        })
        .collect()
}

fn recent_event_rows(events: &[AnalyticsEventRecord], limit: usize) -> Vec<AnalyticsRecentEventV1> {
    events
        .iter()
        .rev()
        .take(limit)
        .map(|event| AnalyticsRecentEventV1 {
            timestamp: Some(event.timestamp),
            event_kind: event.event_kind.clone(),
            hook_name: event.hook_name.clone().unwrap_or_default(),
            tool_name: event.tool_name.clone().unwrap_or_default(),
            outcome: event.outcome.clone().unwrap_or_default(),
        })
        .collect()
}

pub fn recent_hook_rows(rows: &[Value], limit: usize) -> Vec<AnalyticsRecentHookV1> {
    rows.iter()
        .rev()
        .filter(|row| str_field(row, "event") == "hook_invoked")
        .take(limit)
        .map(|row| AnalyticsRecentHookV1 {
            ts_unix_ms: row.get("ts_unix_ms").and_then(Value::as_i64),
            agent: str_field(row, "agent").to_owned(),
            hook_name: str_field(row, "hook_name").to_owned(),
            session_id: str_field(row, "session_id").to_owned(),
            tool_name: str_field(row, "tool_name").to_owned(),
            prompt_category: str_field(row, "prompt_category").to_owned(),
        })
        .collect()
}

fn str_field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn optional_text(row: &Value, key: &str) -> Option<String> {
    let value = str_field(row, key).trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

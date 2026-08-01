//! Savings & Cost dashboard API (`/api/plugins/savings/*`).
//!
//! Two data stores feed this tab:
//!
//! - **Global accounting DB** (`~/.tracedecay/global.db`, the store behind
//!   `tracedecay gain` / `tracedecay cost` / `tracedecay monitor`): the
//!   `savings_ledger` event log, the legacy per-project `projects.tokens_saved`
//!   lifetime counters, and the `turns` cost table (Claude Code transcripts,
//!   cost computed from real usage data at ingest — labeled `actual`).
//!   Ledger aggregation reuses [`RegisteredGlobalDb::sum_savings`] /
//!   [`RegisteredGlobalDb::savings_history`], the same queries `tracedecay gain` runs.
//! - **Session store** (the resolved LCM store the dashboard already serves):
//!   `sessions` +
//!   `session_messages`, whose `model` and `metadata_json` columns drive
//!   per-session cost accounting.
//!
//! Token counts carry an explicit provenance label everywhere, with three
//! quality tiers (best available wins per message):
//!
//! - `cost_basis: "actual"` — the transcript recorded usage data
//!   (`metadata_json.usage.*`).
//! - `"tokenized"` — no usage data, but the stored text was counted with a
//!   real BPE tokenizer (see `token_count`): exact for OpenAI-family
//!   models, a labeled approximation for vendors without a public
//!   tokenizer.
//! - `"estimated"` — the chars/4 heuristic the LCM views use
//!   (`(LENGTH(text)+3)/4`), the fallback when the `token-counting`
//!   feature is compiled out.
//! - `"mixed"` keeps its meaning: usage-backed and non-usage messages in
//!   one aggregate.
//!
//! Dollar costs are computed client-side from the `/pricing` table (see
//! `savings_pricing`); unknown models keep their token counts but get no
//! invented price.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_application::CostsReadModelV1;
use tracedecay_domain::CoverageStateV1;

use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, scope_from_state};
use super::token_count::{
    MESSAGE_TOKENS_CTE, MessageTokens, counting_available, encoder_for_model,
};
use super::util::{
    JsonQuery, coerce_limit, i64_field, query_i64, query_i64_result, query_rows, str_field,
};
use super::{DashboardState, savings_pricing, token_count};
use crate::accounting::metrics::parse_range;
use crate::global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{Value as DbValue, params, params_from_iter};

/// Aggregate SELECT list shared by the per-session and per-model rollups.
/// "Actual" sums only count usage-bearing messages; estimated sums only count
/// the rest, attributing non-assistant text to input and assistant text to
/// output (a deliberate lower bound — resent context is not modeled).
const TOKEN_AGG_COLUMNS: &str = "
    COUNT(*) AS messages,
    SUM(CASE WHEN usage_in IS NOT NULL OR usage_out IS NOT NULL THEN 1 ELSE 0 END) AS usage_messages,
    SUM(CASE WHEN usage_in IS NOT NULL OR usage_out IS NOT NULL THEN COALESCE(usage_in, 0) ELSE 0 END) AS actual_input_tokens,
    SUM(CASE WHEN usage_in IS NOT NULL OR usage_out IS NOT NULL THEN COALESCE(usage_out, 0) ELSE 0 END) AS actual_output_tokens,
    SUM(COALESCE(usage_cache_read, 0)) AS cache_read_tokens,
    SUM(COALESCE(usage_cache_write, 0)) AS cache_write_tokens,
    SUM(CASE WHEN usage_in IS NULL AND usage_out IS NULL AND role <> 'assistant' THEN est_tokens ELSE 0 END) AS estimated_input_tokens,
    SUM(CASE WHEN usage_in IS NULL AND usage_out IS NULL AND role = 'assistant' THEN est_tokens ELSE 0 END) AS estimated_output_tokens";

#[derive(Deserialize)]
pub struct RangeParams {
    range: Option<String>,
}

#[derive(Deserialize)]
pub struct SessionsParams {
    range: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsSumV1 {
    saved_tokens: i64,
    calls: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsLedgerSummaryV1 {
    today: SavingsSumV1,
    last_7d: SavingsSumV1,
    last_30d: SavingsSumV1,
    all_time: SavingsSumV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsLifetimeProjectV1 {
    path: Option<String>,
    tokens_saved: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsLifetimeCountersV1 {
    total_tokens_saved: i64,
    project_total: i64,
    projects_limit: i64,
    projects_truncated: bool,
    projects: Vec<SavingsLifetimeProjectV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsAccountingSummaryV1 {
    available: bool,
    db: String,
    recording: Value,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    ledger: Option<SavingsLedgerSummaryV1>,
    #[serde(default)]
    lifetime_counters: Option<SavingsLifetimeCountersV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct TokenActualV1 {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct TokenPairV1 {
    input_tokens: i64,
    output_tokens: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsSessionSummaryV1 {
    available: bool,
    db: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    messages: Option<i64>,
    #[serde(default)]
    usage_messages: Option<i64>,
    #[serde(default)]
    tokenized_messages: Option<i64>,
    #[serde(default)]
    estimated_messages: Option<i64>,
    #[serde(default)]
    cost_basis: Option<String>,
    #[serde(default)]
    actual: Option<TokenActualV1>,
    #[serde(default)]
    tokenized: Option<TokenPairV1>,
    #[serde(default)]
    estimated: Option<TokenPairV1>,
    #[serde(default)]
    session_count: Option<i64>,
    #[serde(default)]
    model_count: Option<i64>,
    #[serde(default)]
    unknown_model_messages: Option<i64>,
    #[serde(default)]
    token_counting: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct TurnsSummaryV1 {
    available: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    turn_count: Option<i64>,
    #[serde(default)]
    total_cost_usd: Option<f64>,
    #[serde(default)]
    total_tokens: Option<i64>,
    #[serde(default)]
    cost_basis: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsPricingSummaryV1 {
    source: Value,
    fetched_at: Value,
    offline: Value,
    model_count: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct SavingsOverviewPayloadV1 {
    savings: SavingsAccountingSummaryV1,
    sessions: SavingsSessionSummaryV1,
    turns: TurnsSummaryV1,
    pricing: SavingsPricingSummaryV1,
    costs: CostsReadModelV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsSessionModelV1 {
    model: Option<String>,
    tokenizer: Option<Value>,
    messages: i64,
    usage_messages: i64,
    tokenized_messages: i64,
    estimated_messages: i64,
    cost_basis: String,
    actual: TokenActualV1,
    tokenized: TokenPairV1,
    estimated: TokenPairV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsSessionRowV1 {
    provider: String,
    session_id: String,
    title: Option<String>,
    started_at: Option<i64>,
    last_message_at: Option<i64>,
    is_subagent: bool,
    messages: i64,
    usage_messages: i64,
    tokenized_messages: i64,
    estimated_messages: i64,
    cost_basis: String,
    models: Vec<SavingsSessionModelV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct SavingsSessionsPayloadV1 {
    available: bool,
    db: String,
    #[serde(default)]
    scope: Option<String>,
    range: String,
    #[serde(default)]
    since: Option<i64>,
    total: i64,
    sessions: Vec<SavingsSessionRowV1>,
}

fn decode_contract<T: DeserializeOwned>(payload: Value, label: &str) -> Result<T, String> {
    serde_json::from_value(payload)
        .map_err(|error| format!("{label} did not match its response contract: {error}"))
}

fn range_since(range: Option<&str>) -> (String, i64) {
    let range = range.unwrap_or("all").to_string();
    let since = parse_range(&range) as i64;
    (range, since)
}

/// `""` (no model recorded) → JSON null so the UI can render an explicit
/// "unknown model" row instead of an empty label.
fn model_value(model: &str) -> Value {
    if model.is_empty() {
        Value::Null
    } else {
        Value::String(model.to_string())
    }
}

/// Provenance label for an aggregate. `tokenized` only applies when every
/// non-usage message in the aggregate was BPE-counted; partial coverage
/// stays `estimated` (conservative), and `mixed` keeps its legacy meaning
/// of usage-backed plus non-usage messages.
fn basis_label(usage_messages: i64, tokenized_messages: i64, messages: i64) -> &'static str {
    if messages > 0 && usage_messages >= messages {
        "actual"
    } else if usage_messages > 0 {
        "mixed"
    } else if messages > 0 && tokenized_messages >= messages {
        "tokenized"
    } else {
        "estimated"
    }
}

/// Tier sums for the non-usage messages of one aggregate, folded from the
/// `token_count` overlay. `estimated_*` is strictly the chars/4 remainder —
/// the three tiers (actual / tokenized / estimated) never overlap.
#[derive(Debug, Clone, Copy, Default)]
struct TierSums {
    tokenized_messages: i64,
    tokenized_input: i64,
    tokenized_output: i64,
    estimated_messages: i64,
    estimated_input: i64,
    estimated_output: i64,
}

impl TierSums {
    /// Same role attribution as the SQL aggregates: non-assistant text
    /// counts as input, assistant text as output.
    fn add(&mut self, msg: &MessageTokens) {
        let is_output = msg.role == "assistant";
        if msg.tokenized {
            self.tokenized_messages += 1;
            if is_output {
                self.tokenized_output += msg.tokens;
            } else {
                self.tokenized_input += msg.tokens;
            }
        } else {
            self.estimated_messages += 1;
            if is_output {
                self.estimated_output += msg.tokens;
            } else {
                self.estimated_input += msg.tokens;
            }
        }
    }
}

fn fold_overlay<K, F>(overlay: &[MessageTokens], mut key: F) -> HashMap<K, TierSums>
where
    K: std::hash::Hash + Eq,
    F: FnMut(&MessageTokens) -> Option<K>,
{
    let mut out: HashMap<K, TierSums> = HashMap::new();
    for msg in overlay {
        if let Some(k) = key(msg) {
            out.entry(k).or_default().add(msg);
        }
    }
    out
}

/// Token-aggregate JSON shared by session-model and model rows. `tiers` is
/// the overlay fold for the same group; when `None` (overlay unavailable)
/// the SQL chars/4 sums serve, which is exactly the legacy two-tier shape.
fn token_block(row: &Value, tiers: Option<&TierSums>) -> Value {
    let messages = i64_field(row, "messages");
    let usage_messages = i64_field(row, "usage_messages");
    let fallback = TierSums {
        estimated_messages: messages - usage_messages,
        estimated_input: i64_field(row, "estimated_input_tokens"),
        estimated_output: i64_field(row, "estimated_output_tokens"),
        ..TierSums::default()
    };
    let tiers = tiers.copied().unwrap_or(fallback);
    json!({
        "messages": messages,
        "usage_messages": usage_messages,
        "tokenized_messages": tiers.tokenized_messages,
        "estimated_messages": tiers.estimated_messages,
        "cost_basis": basis_label(usage_messages, tiers.tokenized_messages, messages),
        "actual": {
            "input_tokens": i64_field(row, "actual_input_tokens"),
            "output_tokens": i64_field(row, "actual_output_tokens"),
            "cache_read_tokens": i64_field(row, "cache_read_tokens"),
            "cache_write_tokens": i64_field(row, "cache_write_tokens"),
        },
        "tokenized": {
            "input_tokens": tiers.tokenized_input,
            "output_tokens": tiers.tokenized_output,
        },
        "estimated": {
            "input_tokens": tiers.estimated_input,
            "output_tokens": tiers.estimated_output,
        },
    })
}

/// Tokenizer provenance for a model-keyed row (`model` is `""` for
/// unknown-model rows, which still get the approximate o200k count).
fn tokenizer_block(model: &str) -> Value {
    if !counting_available() {
        return Value::Null;
    }
    let encoder = encoder_for_model(model);
    json!({ "encoder": encoder.name, "exact": encoder.exact })
}

/// Ledger-recording gate state, evaluated in the dashboard's own
/// environment. MCP servers evaluate the same gate at startup, so this is
/// the best honest signal the dashboard has: when recording is disabled (or
/// a long-running MCP server predates ledger recording), the UI can explain
/// an empty ledger instead of just saying "no events yet".
fn recording_block() -> Value {
    let mode = crate::global_db::global_accounting_mode();
    json!({
        "enabled": mode.enabled(),
        "mode": mode.as_str(),
    })
}

fn merge(base: Value, extra: Value) -> Value {
    let (Value::Object(mut base_map), Value::Object(extra_map)) = (base, extra) else {
        return Value::Null;
    };
    base_map.extend(extra_map);
    Value::Object(base_map)
}

/// GET `/api/plugins/savings/overview`
pub async fn overview(State(state): State<DashboardState>) -> Response {
    savings_pricing::ensure_background_refresh();

    let savings = match state.savings_db.as_deref() {
        Some(gdb) => savings_overview(gdb, &state.savings_db_path).await,
        None => json!({
            "available": false,
            "db": state.savings_db_path,
            "recording": recording_block(),
        }),
    };
    let sessions = match state.lcm_db.as_deref() {
        Some(db) => sessions_overview(db, &state).await.unwrap_or_else(|error| {
            // The session block's contract requires `db`, which the shared
            // failure block cannot know. Without it a failed session read
            // would fail to decode and collapse the whole route to a 500 —
            // turning one unavailable block into a total outage, and hiding
            // which read actually failed.
            merge(
                json!({ "db": state.lcm_db_path.clone() }),
                read_failed_block(error),
            )
        }),
        None => json!({ "available": false, "db": state.lcm_db_path }),
    };
    let turns = match state.savings_db.as_deref() {
        Some(gdb) => turns_overview(gdb).await.unwrap_or_else(read_failed_block),
        None => json!({ "available": false }),
    };
    let pricing_full = savings_pricing::pricing_payload();
    let pricing = json!({
        "source": pricing_full.get("source"),
        "fetched_at": pricing_full.get("fetched_at"),
        "offline": pricing_full.get("offline"),
        "model_count": pricing_full.get("model_count"),
    });
    let costs = match state.savings_db.as_deref() {
        Some(db) => crate::application::observability::costs_read_model(db, None, 0).await,
        None => crate::application::observability::costs_unavailable_read_model(
            None,
            0,
            "accounting_store_unavailable",
        ),
    };
    let payload: Result<SavingsOverviewPayloadV1, String> = (|| {
        Ok(SavingsOverviewPayloadV1 {
            savings: decode_contract(savings, "savings summary")?,
            sessions: decode_contract(sessions, "session savings summary")?,
            turns: decode_contract(turns, "turns summary")?,
            pricing: decode_contract(pricing, "pricing summary")?,
            costs,
        })
    })();
    match payload {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "contract_invalid", "error": error})),
        )
            .into_response(),
    }
}

/// Canonical Plan 26 Costs projection; all dollar values were priced at ingest.
pub async fn costs(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<CostsReadModelV1>> {
    let model = costs_model(&state).await;
    let metrics = model.usage.iter().chain(&model.estimated_cost);
    let eligible = metrics.clone().count() as u64;
    let known = metrics
        .filter(|metric| metric.coverage.state == CoverageStateV1::Known)
        .count() as u64;
    let envelope = if model.current && known == eligible {
        DashboardEnvelopeV1::ready(
            scope_from_state(&state),
            DashboardCoverageV1::complete(eligible, "metrics"),
            model,
        )
    } else {
        DashboardEnvelopeV1::partial(
            scope_from_state(&state),
            eligible,
            known,
            "metrics",
            vec!["incomplete_metric_coverage".to_owned()],
            model,
        )
    };
    Json(envelope)
}

pub async fn costs_http(State(state): State<DashboardState>) -> Response {
    let model = costs_model(&state).await;
    match crate::application::observability::costs_http_value(&model) {
        Ok(value) => Json(value).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn costs_export(State(state): State<DashboardState>) -> Response {
    let model = costs_model(&state).await;
    match crate::application::observability::costs_export_bytes(&model) {
        Ok(bytes) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                ),
                (
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_static("attachment; filename=\"tracedecay-costs-v1.json\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn costs_model(state: &DashboardState) -> CostsReadModelV1 {
    match state.savings_db.as_deref() {
        Some(db) => crate::application::observability::costs_read_model(db, None, 0).await,
        None => crate::application::observability::costs_unavailable_read_model(
            None,
            0,
            "accounting_store_unavailable",
        ),
    }
}

async fn savings_overview(gdb: &RegisteredGlobalDb, db_path: &str) -> Value {
    const PROJECT_LIMIT: i64 = 25;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let today = gdb.sum_savings(None, now - (now % 86_400)).await;
    let week = gdb.sum_savings(None, now - 7 * 86_400).await;
    let month = gdb.sum_savings(None, now - 30 * 86_400).await;
    let all_time = gdb.sum_savings(None, 0).await;

    // Legacy lifetime counters (`projects.tokens_saved`) predate the ledger
    // and often carry history the event log does not — surface both.
    let conn = gdb.read_connection();
    let lifetime_projects = match query_rows(
        conn,
        "SELECT path, tokens_saved FROM projects
         WHERE tokens_saved > 0 ORDER BY tokens_saved DESC LIMIT ?1",
        params![PROJECT_LIMIT],
    )
    .await
    {
        Ok(projects) => projects,
        Err(error) => {
            return json!({
                "available": false,
                "db": db_path,
                "recording": recording_block(),
                "error": format!("failed to read lifetime project savings: {error}"),
            });
        }
    };
    let lifetime_total = match query_i64_result(
        conn,
        "SELECT COALESCE(SUM(tokens_saved), 0) FROM projects",
        (),
    )
    .await
    {
        Ok(total) => total,
        Err(error) => {
            return json!({
                "available": false,
                "db": db_path,
                "recording": recording_block(),
                "error": format!("failed to read lifetime savings total: {error}"),
            });
        }
    };
    let project_total = match query_i64_result(
        conn,
        "SELECT COUNT(*) FROM projects WHERE tokens_saved > 0",
        (),
    )
    .await
    {
        Ok(total) => total,
        Err(error) => {
            return json!({
                "available": false,
                "db": db_path,
                "recording": recording_block(),
                "error": format!("failed to read lifetime project count: {error}"),
            });
        }
    };

    let sum_json = |total: &crate::global_db::SavingsTotal| json!({ "saved_tokens": total.saved_tokens, "calls": total.calls });
    json!({
        "available": true,
        "db": db_path,
        "recording": recording_block(),
        "ledger": {
            "today": sum_json(&today),
            "last_7d": sum_json(&week),
            "last_30d": sum_json(&month),
            "all_time": sum_json(&all_time),
        },
        "lifetime_counters": {
            "total_tokens_saved": lifetime_total,
            "project_total": project_total,
            "projects_limit": PROJECT_LIMIT,
            "projects_truncated": project_total > lifetime_projects.len() as i64,
            "projects": lifetime_projects.iter().map(|row| json!({
                "path": str_field(row, "path"),
                "tokens_saved": i64_field(row, "tokens_saved"),
            })).collect::<Vec<_>>(),
        },
    })
}

fn read_failed_block(error: String) -> Value {
    json!({
        "available": false,
        "status": "read_failed",
        "error": error,
    })
}

async fn sessions_overview(
    db: &RegisteredGlobalDb,
    state: &DashboardState,
) -> Result<Value, String> {
    let conn = db.read_connection();
    let sql = format!(
        "SELECT {TOKEN_AGG_COLUMNS},
                COUNT(DISTINCT session_id) AS session_count,
                COUNT(DISTINCT CASE WHEN model <> '' THEN model END) AS model_count,
                SUM(CASE WHEN model = '' THEN 1 ELSE 0 END) AS unknown_model_messages
         FROM ({MESSAGE_TOKENS_CTE})"
    );
    let rows = query_rows(conn, &sql, ()).await?;
    let agg = rows
        .first()
        .cloned()
        .ok_or_else(|| "session overview query returned no row".to_string())?;
    let session_count = query_i64_result(conn, "SELECT COUNT(*) FROM sessions", ()).await?;

    let overlay = token_count::non_usage_message_tokens(state).await;
    let total_tiers = overlay.as_deref().map(|messages| {
        let mut sums = TierSums::default();
        for msg in messages {
            sums.add(msg);
        }
        sums
    });

    Ok(merge(
        token_block(&agg, total_tiers.as_ref()),
        json!({
            "available": true,
            "db": state.lcm_db_path,
            "scope": state.lcm_scope,
            "session_count": session_count,
            "model_count": i64_field(&agg, "model_count"),
            "unknown_model_messages": i64_field(&agg, "unknown_model_messages"),
            "token_counting": counting_available(),
        }),
    ))
}

async fn turns_overview(gdb: &RegisteredGlobalDb) -> Result<Value, String> {
    let turn_count =
        query_i64_result(gdb.read_connection(), "SELECT COUNT(*) FROM turns", ()).await?;
    let total_cost = gdb.try_total_cost_since(0).await?;
    let total_tokens = gdb.try_total_tokens_since(0).await?;
    Ok(json!({
        "available": true,
        "turn_count": turn_count,
        "total_cost_usd": total_cost,
        "total_tokens": total_tokens,
        "cost_basis": "actual",
    }))
}

/// GET `/api/plugins/savings/ledger?range=today|7d|30d|all`
pub async fn ledger(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<RangeParams>,
) -> Json<Value> {
    let (range, since) = range_since(params.range.as_deref());
    let Some(gdb) = state.savings_db.as_deref() else {
        return Json(json!({
            "available": false,
            "db": state.savings_db_path,
            "range": range,
        }));
    };

    let total = gdb.sum_savings(None, since).await;
    let history = gdb.savings_history(None, since).await;
    let conn = gdb.read_connection();
    let by_tool = query_rows(
        conn,
        "SELECT tool_name,
                COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0) AS saved_tokens,
                COUNT(*) AS calls
         FROM savings_ledger WHERE ts >= ?1
         GROUP BY tool_name ORDER BY saved_tokens DESC LIMIT 50",
        params![since],
    )
    .await
    .unwrap_or_default();
    let by_project = query_rows(
        conn,
        "SELECT project_path,
                COALESCE(SUM(CASE WHEN before_tokens > after_tokens THEN before_tokens - after_tokens ELSE 0 END), 0) AS saved_tokens,
                COUNT(*) AS calls
         FROM savings_ledger WHERE ts >= ?1
         GROUP BY project_path ORDER BY saved_tokens DESC LIMIT 50",
        params![since],
    )
    .await
    .unwrap_or_default();

    Json(json!({
        "available": true,
        "db": state.savings_db_path,
        "range": range,
        "since": since,
        "total": { "saved_tokens": total.saved_tokens, "calls": total.calls },
        "by_day": history.iter().map(|day| json!({
            "day": day.day,
            "saved_tokens": day.saved_tokens,
            "calls": day.calls,
        })).collect::<Vec<_>>(),
        "by_tool": by_tool.iter().map(|row| json!({
            "tool": str_field(row, "tool_name"),
            "saved_tokens": i64_field(row, "saved_tokens"),
            "calls": i64_field(row, "calls"),
        })).collect::<Vec<_>>(),
        "by_project": by_project.iter().map(|row| json!({
            "project": str_field(row, "project_path"),
            "saved_tokens": i64_field(row, "saved_tokens"),
            "calls": i64_field(row, "calls"),
        })).collect::<Vec<_>>(),
    }))
}

/// GET `/api/plugins/savings/sessions?range=&limit=&offset=`
///
/// Sessions without any timestamp (neither `started_at` nor message
/// timestamps — true for Cursor hook ingests today) are only included in the
/// default `all` range, since they cannot be placed on a timeline.
pub async fn sessions(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SessionsParams>,
) -> Response {
    let (range, since) = range_since(params.range.as_deref());
    let limit = coerce_limit(params.limit, 25, 100);
    let offset = params.offset.unwrap_or(0).max(0);
    let Some(db) = state.lcm_db.as_deref() else {
        return match decode_contract::<SavingsSessionsPayloadV1>(
            json!({
            "available": false,
            "db": state.lcm_db_path,
            "range": range,
            "sessions": [],
            "total": 0,
            }),
            "savings sessions",
        ) {
            Ok(payload) => Json(payload).into_response(),
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"status": "contract_invalid", "error": error})),
            )
                .into_response(),
        };
    };
    let conn = db.read_connection();

    let page_sql = "
        SELECT s.provider, s.session_id, s.title, s.started_at, s.ended_at,
               s.is_subagent,
               (SELECT MAX(m.timestamp) FROM session_messages m
                 WHERE m.provider = s.provider AND m.session_id = s.session_id) AS last_message_at
        FROM sessions s
        WHERE ?1 = 0 OR COALESCE(s.started_at,
              (SELECT MAX(m.timestamp) FROM session_messages m
                WHERE m.provider = s.provider AND m.session_id = s.session_id), 0) >= ?1
        ORDER BY (s.started_at IS NULL), s.started_at DESC, s.rowid DESC
        LIMIT ?2 OFFSET ?3";
    let page = query_rows(conn, page_sql, params![since, limit, offset])
        .await
        .unwrap_or_default();
    let total = query_i64(
        conn,
        "SELECT COUNT(*) FROM sessions s
         WHERE ?1 = 0 OR COALESCE(s.started_at,
               (SELECT MAX(m.timestamp) FROM session_messages m
                 WHERE m.provider = s.provider AND m.session_id = s.session_id), 0) >= ?1",
        params![since],
    )
    .await;

    let overlay = token_count::non_usage_message_tokens(&state).await;
    let session_model_tiers = overlay.as_deref().map(|messages| {
        fold_overlay(messages, |msg| {
            Some((
                msg.provider.clone(),
                msg.session_id.clone(),
                msg.model.clone(),
            ))
        })
    });

    // One grouped aggregate over the page's (provider, session_id) pairs —
    // previously each page row ran its own aggregate query (N+1, up to 100
    // round-trips re-running the json_extract CTE per page render). The
    // VALUES list joins as the outer loop so each pair stays an indexed
    // probe of session_messages (a row-value `IN (VALUES …)` predicate does
    // not get pushed into the index and full-scans instead). The global
    // `messages DESC` order keeps each session's model rows descending after
    // bucketing, matching the old per-session ORDER BY.
    let mut model_rows_by_session: HashMap<(String, String), Vec<Value>> = HashMap::new();
    if !page.is_empty() {
        let tuples = vec!["(?, ?)"; page.len()].join(", ");
        let agg_sql = format!(
            "SELECT provider, session_id, model, {TOKEN_AGG_COLUMNS}
             FROM (VALUES {tuples}) pairs
             JOIN ({MESSAGE_TOKENS_CTE}) ON provider = pairs.column1
                                        AND session_id = pairs.column2
             GROUP BY provider, session_id, model
             ORDER BY messages DESC"
        );
        let mut agg_params: Vec<DbValue> = Vec::with_capacity(page.len() * 2);
        for row in &page {
            agg_params.push(DbValue::Text(str_field(row, "provider").to_string()));
            agg_params.push(DbValue::Text(str_field(row, "session_id").to_string()));
        }
        let rows = query_rows(conn, &agg_sql, params_from_iter(agg_params))
            .await
            .unwrap_or_default();
        for row in rows {
            let key = (
                str_field(&row, "provider").to_string(),
                str_field(&row, "session_id").to_string(),
            );
            model_rows_by_session.entry(key).or_default().push(row);
        }
    }

    let mut sessions_json = Vec::with_capacity(page.len());
    for row in &page {
        let provider = str_field(row, "provider");
        let session_id = str_field(row, "session_id");
        let model_rows = model_rows_by_session
            .remove(&(provider.to_string(), session_id.to_string()))
            .unwrap_or_default();

        let mut messages = 0;
        let mut usage_messages = 0;
        let mut tokenized_messages = 0;
        let mut estimated_messages = 0;
        let models: Vec<Value> = model_rows
            .iter()
            .map(|model_row| {
                let model = str_field(model_row, "model");
                let tiers = session_model_tiers.as_ref().and_then(|map| {
                    map.get(&(
                        provider.to_string(),
                        session_id.to_string(),
                        model.to_string(),
                    ))
                });
                let block = token_block(model_row, tiers);
                messages += i64_field(&block, "messages");
                usage_messages += i64_field(&block, "usage_messages");
                tokenized_messages += i64_field(&block, "tokenized_messages");
                estimated_messages += i64_field(&block, "estimated_messages");
                merge(
                    block,
                    json!({
                        "model": model_value(model),
                        "tokenizer": tokenizer_block(model),
                    }),
                )
            })
            .collect();

        sessions_json.push(json!({
            "provider": provider,
            "session_id": session_id,
            "title": row.get("title").cloned().unwrap_or(Value::Null),
            "started_at": row.get("started_at").cloned().unwrap_or(Value::Null),
            "last_message_at": row.get("last_message_at").cloned().unwrap_or(Value::Null),
            "is_subagent": i64_field(row, "is_subagent") != 0,
            "messages": messages,
            "usage_messages": usage_messages,
            "tokenized_messages": tokenized_messages,
            "estimated_messages": estimated_messages,
            "cost_basis": basis_label(usage_messages, tokenized_messages, messages),
            "models": models,
        }));
    }

    match decode_contract::<SavingsSessionsPayloadV1>(
        json!({
            "available": true,
            "db": state.lcm_db_path,
            "scope": state.lcm_scope,
            "range": range,
            "since": since,
            "total": total,
            "sessions": sessions_json,
        }),
        "savings sessions",
    ) {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"status": "contract_invalid", "error": error})),
        )
            .into_response(),
    }
}

/// GET `/api/plugins/savings/models?range=`
///
/// Per-model token aggregates from the session store, per-day series for
/// timestamped messages, plus the `turns` accounting (per-model cost and
/// per-day cost — `actual`, computed from transcript usage at ingest by
/// `tracedecay cost`, reusing [`RegisteredGlobalDb::cost_by_model_since`]).
pub async fn models(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<RangeParams>,
) -> Json<Value> {
    let (range, since) = range_since(params.range.as_deref());

    let mut payload = json!({
        "available": state.lcm_db.is_some(),
        "range": range,
        "since": since,
        "models": [],
        "daily": [],
        "turns": { "available": state.savings_db.is_some(), "by_model": [], "by_day": [] },
    });

    if let Some(db) = state.lcm_db.as_deref() {
        let conn = db.read_connection();
        let overlay = token_count::non_usage_message_tokens(&state).await;
        // Folds replicate the SQL range predicates exactly: per-model rows
        // use COALESCE(timestamp, 0), the daily series requires a positive
        // timestamp.
        let model_tiers = overlay.as_deref().map(|messages| {
            fold_overlay(messages, |msg| {
                (since == 0 || msg.timestamp.unwrap_or(0) >= since).then(|| msg.model.clone())
            })
        });
        let day_tiers = overlay.as_deref().map(|messages| {
            fold_overlay(messages, |msg| {
                let ts = msg.timestamp.unwrap_or(0);
                (ts > 0 && (since == 0 || ts >= since))
                    .then(|| ((ts / 86_400) * 86_400, msg.model.clone()))
            })
        });

        let model_sql = format!(
            "SELECT model, COUNT(DISTINCT session_id) AS session_count, {TOKEN_AGG_COLUMNS}
             FROM ({MESSAGE_TOKENS_CTE})
             WHERE ?1 = 0 OR COALESCE(timestamp, 0) >= ?1
             GROUP BY model ORDER BY messages DESC LIMIT 100"
        );
        let model_rows = query_rows(conn, &model_sql, params![since])
            .await
            .unwrap_or_default();
        payload["models"] = Value::Array(
            model_rows
                .iter()
                .map(|row| {
                    let model = str_field(row, "model");
                    let tiers = model_tiers
                        .as_ref()
                        .and_then(|map| map.get(&model.to_string()));
                    merge(
                        token_block(row, tiers),
                        json!({
                            "model": model_value(model),
                            "sessions": i64_field(row, "session_count"),
                            "tokenizer": tokenizer_block(model),
                        }),
                    )
                })
                .collect(),
        );

        let daily_sql = format!(
            "WITH daily AS (
                SELECT (timestamp / 86400) * 86400 AS day, model, {TOKEN_AGG_COLUMNS}
                FROM ({MESSAGE_TOKENS_CTE})
                WHERE timestamp IS NOT NULL AND timestamp > 0 AND (?1 = 0 OR timestamp >= ?1)
                GROUP BY day, model
             ),
             latest_days AS (
                SELECT day FROM daily GROUP BY day ORDER BY day DESC LIMIT 366
             )
             SELECT daily.*
             FROM daily JOIN latest_days ON latest_days.day = daily.day
             ORDER BY daily.day ASC, daily.messages DESC"
        );
        let daily_rows = query_rows(conn, &daily_sql, params![since])
            .await
            .unwrap_or_default();
        payload["daily"] = Value::Array(
            daily_rows
                .iter()
                .map(|row| {
                    let day = i64_field(row, "day");
                    let model = str_field(row, "model");
                    let tiers = day_tiers
                        .as_ref()
                        .and_then(|map| map.get(&(day, model.to_string())));
                    merge(
                        token_block(row, tiers),
                        json!({ "day": day, "model": model_value(model) }),
                    )
                })
                .collect(),
        );
    }

    if let Some(gdb) = state.savings_db.as_deref() {
        let by_model = gdb.cost_by_model_since(since.max(0) as u64).await;
        payload["turns"]["by_model"] = Value::Array(
            by_model
                .iter()
                .map(|(model, cost, tokens)| {
                    json!({
                        "model": model,
                        "cost_usd": cost,
                        "total_tokens": tokens,
                        "cost_basis": "actual",
                    })
                })
                .collect(),
        );
        let conn = gdb.read_connection();
        let by_day = query_rows(
            conn,
            "WITH daily AS (
                SELECT (timestamp / 86400) * 86400 AS day,
                       SUM(cost_usd) AS cost_usd,
                       SUM(input_tokens + output_tokens) AS total_tokens
                FROM turns WHERE timestamp >= ?1
                GROUP BY day
             ),
             latest_days AS (
                SELECT day FROM daily ORDER BY day DESC LIMIT 366
             )
             SELECT daily.*
             FROM daily JOIN latest_days ON latest_days.day = daily.day
             ORDER BY daily.day ASC",
            params![since],
        )
        .await
        .unwrap_or_default();
        payload["turns"]["by_day"] = Value::Array(
            by_day
                .iter()
                .map(|row| {
                    json!({
                        "day": i64_field(row, "day"),
                        "cost_usd": row.get("cost_usd").cloned().unwrap_or(Value::Null),
                        "total_tokens": i64_field(row, "total_tokens"),
                    })
                })
                .collect(),
        );
    }

    Json(payload)
}

/// GET `/api/plugins/savings/pricing` — the merged model price table with
/// provenance (`live` data is always served from its disk cache, so `source`
/// is `"cache"` or `"fallback"`).
pub async fn pricing() -> Json<Value> {
    savings_pricing::ensure_background_refresh();
    Json(savings_pricing::pricing_payload())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_labels() {
        assert_eq!(basis_label(0, 0, 0), "estimated");
        assert_eq!(basis_label(0, 0, 4), "estimated");
        assert_eq!(basis_label(2, 0, 4), "mixed");
        assert_eq!(basis_label(4, 0, 4), "actual");
        // Fully BPE-counted aggregates get the new tier…
        assert_eq!(basis_label(0, 4, 4), "tokenized");
        // …partial coverage stays conservative…
        assert_eq!(basis_label(0, 2, 4), "estimated");
        // …and any usage data keeps the legacy mixed/actual labels.
        assert_eq!(basis_label(2, 2, 4), "mixed");
        assert_eq!(basis_label(4, 4, 4), "actual");
    }

    #[test]
    fn tier_sums_attribute_roles_like_sql() {
        let mut sums = TierSums::default();
        let msg = |role: &str, tokens: i64, tokenized: bool| MessageTokens {
            provider: "cursor".into(),
            session_id: "s".into(),
            model: "gpt-5".into(),
            role: role.into(),
            timestamp: None,
            tokens,
            tokenized,
        };
        sums.add(&msg("user", 10, true));
        sums.add(&msg("assistant", 20, true));
        sums.add(&msg("system", 5, false));
        sums.add(&msg("assistant", 7, false));
        assert_eq!(sums.tokenized_messages, 2);
        assert_eq!(sums.tokenized_input, 10);
        assert_eq!(sums.tokenized_output, 20);
        assert_eq!(sums.estimated_messages, 2);
        assert_eq!(sums.estimated_input, 5);
        assert_eq!(sums.estimated_output, 7);
    }

    #[test]
    fn token_block_falls_back_to_sql_estimates_without_overlay() {
        let row = json!({
            "messages": 3,
            "usage_messages": 1,
            "actual_input_tokens": 100,
            "actual_output_tokens": 50,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "estimated_input_tokens": 40,
            "estimated_output_tokens": 60,
        });
        let block = token_block(&row, None);
        assert_eq!(block["cost_basis"], "mixed");
        assert_eq!(block["tokenized_messages"], 0);
        assert_eq!(block["estimated_messages"], 2);
        assert_eq!(block["estimated"]["input_tokens"], 40);
        assert_eq!(block["estimated"]["output_tokens"], 60);
        assert_eq!(block["tokenized"]["input_tokens"], 0);
    }

    #[test]
    fn token_block_prefers_overlay_tiers() {
        let row = json!({
            "messages": 2,
            "usage_messages": 0,
            "actual_input_tokens": 0,
            "actual_output_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "estimated_input_tokens": 40,
            "estimated_output_tokens": 60,
        });
        let tiers = TierSums {
            tokenized_messages: 2,
            tokenized_input: 33,
            tokenized_output: 44,
            ..TierSums::default()
        };
        let block = token_block(&row, Some(&tiers));
        assert_eq!(block["cost_basis"], "tokenized");
        assert_eq!(block["tokenized_messages"], 2);
        assert_eq!(block["estimated_messages"], 0);
        assert_eq!(block["tokenized"]["input_tokens"], 33);
        assert_eq!(block["tokenized"]["output_tokens"], 44);
        assert_eq!(block["estimated"]["input_tokens"], 0);
    }

    #[test]
    fn unknown_model_serializes_as_null() {
        assert_eq!(model_value(""), Value::Null);
        assert_eq!(model_value("gpt-5.5"), Value::String("gpt-5.5".into()));
    }
}

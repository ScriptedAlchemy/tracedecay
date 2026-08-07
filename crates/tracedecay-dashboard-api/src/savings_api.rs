//! Savings & Cost dashboard API (`/api/plugins/savings/*`).
//!
//! Two data stores feed this tab:
//!
//! - **Global accounting DB** (the registered profile store behind
//!   `tracedecay gain` / `tracedecay cost` / `tracedecay monitor`): the
//!   `savings_ledger` and legacy lifetime savings counters.
//!   Ledger aggregation reuses [`RegisteredGlobalDb::sum_savings`] /
//!   [`RegisteredGlobalDb::savings_history`], the same queries `tracedecay gain` runs.
//! - **Session store** (the resolved LCM store the dashboard already serves):
//!   canonical provider-usage observations plus `sessions` +
//!   `session_messages`, whose content and model fields provide a separate
//!   non-billing token-count overlay.
//!
//! Content token counts carry an explicit provenance label:
//!
//! - `"tokenized"` — stored text counted with a
//!   real BPE tokenizer (see `token_count`): exact for OpenAI-family
//!   models, a labeled approximation for vendors without a public
//!   tokenizer.
//! - `"estimated"` — the chars/4 heuristic the LCM views use
//!   (`(LENGTH(text)+3)/4`), the fallback when the `token-counting`
//!   feature is compiled out.
//!
//! Provider billing counters are exposed separately as provider-usage events;
//! they are never treated as message counts.
//!
//! Dollar costs and `/pricing` use one bundled, deterministic all-provider
//! authority. Unknown models keep their token counts but get no invented
//! price.

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_application::CostsReadModelV1;
use tracedecay_domain::{CoverageStateV1, ObservationScopeV1};
use tracedecay_usecases::provider_usage::{
    AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
    ProviderUsageDeltaV1, price_provider_usage, provider_usage_aggregate,
    provider_usage_range_start,
};

use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, scope_from_state};
use super::token_count::{
    MESSAGE_TOKENS_CTE, MessageTokens, counting_available, encoder_for_model,
};
use super::util::{
    JsonQuery, coerce_limit, i64_field, query_i64, query_i64_result, query_rows, str_field,
};
use super::{DashboardState, savings_pricing, token_count};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{Value as DbValue, params, params_from_iter};

/// Content-size aggregate shared by the per-session and per-model rollups.
/// Provider billing usage is joined from the canonical observation projection,
/// never inferred from message rows.
const TOKEN_AGG_COLUMNS: &str = "
    COUNT(*) AS messages,
    SUM(CASE WHEN role <> 'assistant' THEN est_tokens ELSE 0 END) AS estimated_input_tokens,
    SUM(CASE WHEN role = 'assistant' THEN est_tokens ELSE 0 END) AS estimated_output_tokens";

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
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
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
    provider_usage_events: Option<i64>,
    #[serde(default)]
    tokenized_messages: Option<i64>,
    #[serde(default)]
    estimated_messages: Option<i64>,
    #[serde(default)]
    cost_basis: Option<String>,
    #[serde(default)]
    provider_actual: Option<TokenActualV1>,
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
struct ProviderUsageSummaryV1 {
    available: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    usage_event_count: Option<i64>,
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
    revision: Value,
    fetched_at: Value,
    offline: Value,
    model_count: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SavingsOverviewPayloadV1 {
    savings: SavingsAccountingSummaryV1,
    sessions: SavingsSessionSummaryV1,
    provider_usage: ProviderUsageSummaryV1,
    pricing: SavingsPricingSummaryV1,
    costs: CostsReadModelV1,
}

fn provider_usage_scope(state: &DashboardState) -> Option<ObservationScopeV1> {
    state
        .resolved_scope
        .as_ref()
        .map(|scope| ObservationScopeV1::Project {
            project_id: scope.project_id.clone(),
        })
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsSessionModelV1 {
    model: Option<String>,
    tokenizer: Option<Value>,
    messages: i64,
    provider_usage_events: i64,
    tokenized_messages: i64,
    estimated_messages: i64,
    cost_basis: String,
    provider_actual: Option<TokenActualV1>,
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
    provider_usage_events: i64,
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

fn range_since(range: Option<&str>) -> Result<(String, i64), String> {
    let range = range.unwrap_or("all").to_string();
    let since = provider_usage_range_start(&range)?;
    let since = i64::try_from(since)
        .map_err(|_| "provider usage range exceeds the timestamp domain".to_owned())?;
    Ok((range, since))
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

/// Provenance label for content sizing only.
fn basis_label(tokenized_messages: i64, messages: i64) -> &'static str {
    if messages > 0 && tokenized_messages >= messages {
        "tokenized"
    } else {
        "estimated"
    }
}

fn actual_tokens(aggregate: &ProviderUsageAggregateV1) -> Option<TokenActualV1> {
    if aggregate.coverage != ProviderUsageCoverageV1::Complete {
        return None;
    }
    Some(TokenActualV1 {
        input_tokens: aggregate
            .totals
            .input_tokens
            .and_then(|value| i64::try_from(value).ok()),
        output_tokens: aggregate
            .totals
            .output_tokens
            .and_then(|value| i64::try_from(value).ok()),
        cache_read_tokens: aggregate
            .totals
            .cache_read_tokens
            .and_then(|value| i64::try_from(value).ok()),
        cache_write_tokens: aggregate
            .totals
            .cache_write_tokens
            .and_then(|value| i64::try_from(value).ok()),
    })
}

fn actual_for_deltas<'a>(
    deltas: impl Iterator<Item = &'a ProviderUsageDeltaV1>,
) -> (usize, Option<TokenActualV1>) {
    let deltas = deltas.collect::<Vec<_>>();
    if deltas.is_empty() {
        return (0, None);
    }
    let sum = |field: fn(&AggregatedProviderUsageCountersV1) -> Option<u64>| {
        deltas.iter().try_fold(0_u64, |total, delta| {
            total.checked_add(field(&delta.counters)?)
        })
    };
    (
        deltas.len(),
        Some(TokenActualV1 {
            input_tokens: sum(|counters| counters.input_tokens)
                .and_then(|value| i64::try_from(value).ok()),
            output_tokens: sum(|counters| counters.output_tokens)
                .and_then(|value| i64::try_from(value).ok()),
            cache_read_tokens: sum(|counters| counters.cache_read_tokens)
                .and_then(|value| i64::try_from(value).ok()),
            cache_write_tokens: sum(|counters| counters.cache_write_tokens)
                .and_then(|value| i64::try_from(value).ok()),
        }),
    )
}

fn price_deltas<'a>(
    deltas: impl Iterator<Item = &'a ProviderUsageDeltaV1>,
    prices: &tracedecay_usecases::provider_pricing::PriceTable,
) -> tracedecay_usecases::provider_usage::ProviderUsageCostSummaryV1 {
    let deltas = deltas.cloned().collect::<Vec<_>>();
    let observations_seen = deltas.len() as u64;
    let aggregate = ProviderUsageAggregateV1 {
        coverage: if deltas.is_empty() {
            ProviderUsageCoverageV1::Unavailable
        } else {
            ProviderUsageCoverageV1::Complete
        },
        observations_seen,
        totals: AggregatedProviderUsageCountersV1::unknown(),
        upper_observation_sequence: deltas.last().map(|delta| delta.observation_sequence),
        deltas,
        issues: Vec::new(),
    };
    price_provider_usage(&aggregate, prices, 0)
}

fn apply_provider_actual(block: &mut Value, event_count: usize, actual: Option<TokenActualV1>) {
    let Value::Object(values) = block else {
        return;
    };
    values.insert(
        "provider_usage_events".to_owned(),
        i64::try_from(event_count).map_or(Value::Null, Value::from),
    );
    values.insert(
        "provider_actual".to_owned(),
        actual.map_or(Value::Null, |tokens| {
            json!({
                "input_tokens": tokens.input_tokens,
                "output_tokens": tokens.output_tokens,
                "cache_read_tokens": tokens.cache_read_tokens,
                "cache_write_tokens": tokens.cache_write_tokens,
            })
        }),
    );
}

/// Tier sums for the content messages of one aggregate.
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
    let fallback = TierSums {
        estimated_messages: messages,
        estimated_input: i64_field(row, "estimated_input_tokens"),
        estimated_output: i64_field(row, "estimated_output_tokens"),
        ..TierSums::default()
    };
    let tiers = tiers.copied().unwrap_or(fallback);
    json!({
        "messages": messages,
        "provider_usage_events": 0,
        "tokenized_messages": tiers.tokenized_messages,
        "estimated_messages": tiers.estimated_messages,
        "cost_basis": basis_label(tiers.tokenized_messages, messages),
        "provider_actual": Value::Null,
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
    let mode = tracedecay_global_db::global_accounting_mode();
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
pub async fn overview(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<Option<SavingsOverviewPayloadV1>>> {
    let provider_scope = provider_usage_scope(&state);
    let usage_aggregate = match (state.lcm_db.as_deref(), provider_scope.as_ref()) {
        (Some(db), Some(scope)) => Some(provider_usage_aggregate(db, scope, None, None).await),
        _ => None,
    };
    let savings = match state.savings_db.as_deref() {
        Some(gdb) => savings_overview(gdb, &state.savings_db_path).await,
        None => json!({
            "available": false,
            "db": state.savings_db_path,
            "recording": recording_block(),
        }),
    };
    let sessions = match state.lcm_db.as_deref() {
        Some(db) => sessions_overview(db, &state, usage_aggregate.as_ref())
            .await
            .unwrap_or_else(|error| {
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
    let provider_usage = match usage_aggregate.as_ref() {
        Some(aggregate) => provider_usage_overview(aggregate),
        None => json!({ "available": false }),
    };
    let pricing_full = savings_pricing::pricing_payload();
    let pricing = json!({
        "source": pricing_full.get("source"),
        "revision": pricing_full.get("revision"),
        "fetched_at": pricing_full.get("fetched_at"),
        "offline": pricing_full.get("offline"),
        "model_count": pricing_full.get("model_count"),
    });
    let costs = match (state.savings_db.as_deref(), usage_aggregate.as_ref()) {
        (Some(db), Some(aggregate)) => {
            crate::application::observability::costs_read_model_with_provider_usage(
                db, None, 0, aggregate,
            )
            .await
        }
        _ => crate::application::observability::costs_unavailable_read_model(
            None,
            0,
            "accounting_store_unavailable",
        ),
    };
    let payload: Result<SavingsOverviewPayloadV1, String> = (|| {
        Ok(SavingsOverviewPayloadV1 {
            savings: decode_contract(savings, "savings summary")?,
            sessions: decode_contract(sessions, "session savings summary")?,
            provider_usage: decode_contract(provider_usage, "provider usage summary")?,
            pricing: decode_contract(pricing, "pricing summary")?,
            costs,
        })
    })();
    match payload {
        Ok(payload) => {
            let available = [
                payload.savings.available,
                payload.sessions.available,
                payload.provider_usage.available,
            ]
            .into_iter()
            .filter(|available| *available)
            .count() as u64;
            if available == 0 {
                Json(DashboardEnvelopeV1::unavailable(
                    scope_from_state(&state),
                    Some(payload),
                    "savings_sources_unavailable",
                ))
            } else if available < 3 {
                Json(DashboardEnvelopeV1::partial(
                    scope_from_state(&state),
                    3,
                    available,
                    "savings_sources",
                    vec!["one_or_more_savings_sources_unavailable".to_owned()],
                    Some(payload),
                ))
            } else {
                Json(DashboardEnvelopeV1::ready(
                    scope_from_state(&state),
                    DashboardCoverageV1::complete(3, "savings_sources"),
                    Some(payload),
                ))
            }
        }
        Err(error) => Json(DashboardEnvelopeV1::error(
            scope_from_state(&state),
            None,
            error,
        )),
    }
}

/// Canonical costs projection over exact provider usage and bundled pricing.
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

// `costs_http` / `costs_export` are deleted with their last caller, for the
// same reason as their Observatory twins above `observatory_model`. They
// mounted `/api/plugins/savings/costs{,/export}` over the identical
// `costs_model` that `/api/costs` — the route `CanonicalCosts.tsx` reads —
// already serves. The savings family's OTHER routes (`overview`, `ledger`,
// `sessions`, `models`, `pricing`) are not aliases: each is the sole mount of
// its handler and has live consumers, so they stay.

async fn costs_model(state: &DashboardState) -> CostsReadModelV1 {
    let provider_scope = provider_usage_scope(state);
    match (
        state.savings_db.as_deref(),
        state.lcm_db.as_deref(),
        provider_scope.as_ref(),
    ) {
        (Some(savings_db), Some(usage_db), Some(scope)) => {
            let usage = provider_usage_aggregate(usage_db, scope, None, None).await;
            crate::application::observability::costs_read_model_with_provider_usage(
                savings_db, None, 0, &usage,
            )
            .await
        }
        _ => crate::application::observability::costs_unavailable_read_model(
            None,
            0,
            "provider_usage_scope_or_store_unavailable",
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

    let sum_json = |total: &tracedecay_global_db::SavingsTotal| json!({ "saved_tokens": total.saved_tokens, "calls": total.calls });
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
    provider_usage: Option<&ProviderUsageAggregateV1>,
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
    let mut content = token_block(&agg, total_tiers.as_ref());
    if let Value::Object(block) = &mut content {
        let actual = provider_usage.and_then(actual_tokens);
        let usage_events = provider_usage
            .as_ref()
            .filter(|usage| usage.coverage == ProviderUsageCoverageV1::Complete)
            .and_then(|usage| i64::try_from(usage.deltas.len()).ok());
        block.insert(
            "provider_usage_events".to_owned(),
            usage_events.map_or(Value::Null, Value::from),
        );
        block.insert(
            "provider_actual".to_owned(),
            actual
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| format!("failed to encode provider usage: {error}"))?
                .unwrap_or(Value::Null),
        );
    }
    Ok(merge(
        content,
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

fn provider_usage_overview(aggregate: &ProviderUsageAggregateV1) -> Value {
    let priced = price_provider_usage(aggregate, savings_pricing::load_table(), 0);
    let complete = priced.coverage == ProviderUsageCoverageV1::Complete;
    let total_tokens = priced
        .total_input_tokens
        .zip(priced.total_output_tokens)
        .and_then(|(input, output)| input.checked_add(output));
    json!({
        "available": priced.coverage != ProviderUsageCoverageV1::Unavailable,
        "status": match priced.coverage {
            ProviderUsageCoverageV1::Complete => "complete",
            ProviderUsageCoverageV1::Partial => "partial",
            ProviderUsageCoverageV1::Unavailable => "unavailable",
        },
        "error": (!complete).then_some("provider_usage_incomplete"),
        "usage_event_count": i64::try_from(priced.usage_events).ok(),
        "total_cost_usd": priced.total_cost_usd,
        "total_tokens": total_tokens,
        "cost_basis": if priced.total_cost_usd.is_some() {
            "provider_reported_priced"
        } else {
            "provider_reported_unpriced"
        },
    })
}

/// GET `/api/plugins/savings/ledger?range=today|7d|30d|all`
pub async fn ledger(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<RangeParams>,
) -> Json<Value> {
    let (range, since) = match range_since(params.range.as_deref()) {
        Ok(range) => range,
        Err(error) => return Json(read_failed_block(error)),
    };
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
    let (range, since) = match range_since(params.range.as_deref()) {
        Ok(range) => range,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(read_failed_block(error)),
            )
                .into_response();
        }
    };
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
    let provider_scope = provider_usage_scope(&state);
    let provider_usage = match (state.lcm_db.as_deref(), provider_scope.as_ref()) {
        (Some(usage_db), Some(scope)) => {
            Some(provider_usage_aggregate(usage_db, scope, None, None).await)
        }
        _ => None,
    };
    let usage_deltas = provider_usage
        .as_ref()
        .filter(|usage| usage.coverage == ProviderUsageCoverageV1::Complete)
        .map(|usage| usage.deltas.as_slice());
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
        let mut provider_usage_events = 0;
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
                let mut block = token_block(model_row, tiers);
                let (event_count, actual) = usage_deltas.map_or((0, None), |deltas| {
                    actual_for_deltas(deltas.iter().filter(|delta| {
                        delta.provider == provider
                            && delta.session_id == session_id
                            && delta.model.as_deref().unwrap_or_default() == model
                            && (since == 0
                                || delta
                                    .native_timestamp
                                    .is_some_and(|timestamp| timestamp >= since))
                    }))
                });
                apply_provider_actual(&mut block, event_count, actual);
                messages += i64_field(&block, "messages");
                provider_usage_events += i64_field(&block, "provider_usage_events");
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
            "provider_usage_events": provider_usage_events,
            "tokenized_messages": tokenized_messages,
            "estimated_messages": estimated_messages,
            "cost_basis": basis_label(tokenized_messages, messages),
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
/// Per-model token aggregates from the session store plus canonical
/// provider-usage cost grouped by exact provider/model and day.
pub async fn models(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<RangeParams>,
) -> Json<Value> {
    let (range, since) = match range_since(params.range.as_deref()) {
        Ok(range) => range,
        Err(error) => return Json(read_failed_block(error)),
    };
    let provider_scope = provider_usage_scope(&state);
    let provider_usage = match (state.lcm_db.as_deref(), provider_scope.as_ref()) {
        (Some(usage_db), Some(scope)) => {
            Some(provider_usage_aggregate(usage_db, scope, None, None).await)
        }
        _ => None,
    };
    let usage_deltas = provider_usage
        .as_ref()
        .filter(|usage| usage.coverage == ProviderUsageCoverageV1::Complete)
        .map(|usage| usage.deltas.as_slice());
    let prices = savings_pricing::load_table();

    let mut payload = json!({
        "available": state.lcm_db.is_some(),
        "range": range,
        "since": since,
        "models": [],
        "daily": [],
        "provider_usage_coverage": provider_usage.as_ref().map(|usage| match usage.coverage {
            ProviderUsageCoverageV1::Complete => "complete",
            ProviderUsageCoverageV1::Partial => "partial",
            ProviderUsageCoverageV1::Unavailable => "unavailable",
        }),
        "provider_usage": { "available": usage_deltas.is_some(), "by_model": [], "by_day": [] },
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
                    let mut block = token_block(row, tiers);
                    let (event_count, actual) = usage_deltas.map_or((0, None), |deltas| {
                        actual_for_deltas(deltas.iter().filter(|delta| {
                            delta.model.as_deref().unwrap_or_default() == model
                                && (since == 0
                                    || delta
                                        .native_timestamp
                                        .is_some_and(|timestamp| timestamp >= since))
                        }))
                    });
                    apply_provider_actual(&mut block, event_count, actual);
                    merge(
                        block,
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
                    let mut block = token_block(row, tiers);
                    let (event_count, actual) = usage_deltas.map_or((0, None), |deltas| {
                        actual_for_deltas(deltas.iter().filter(|delta| {
                            delta.model.as_deref().unwrap_or_default() == model
                                && delta
                                    .native_timestamp
                                    .is_some_and(|timestamp| (timestamp / 86_400) * 86_400 == day)
                        }))
                    });
                    apply_provider_actual(&mut block, event_count, actual);
                    merge(block, json!({ "day": day, "model": model_value(model) }))
                })
                .collect(),
        );
    }

    if let Some(deltas) = usage_deltas {
        let mut by_model: BTreeMap<(String, String), Vec<&ProviderUsageDeltaV1>> = BTreeMap::new();
        let mut by_day: BTreeMap<i64, Vec<&ProviderUsageDeltaV1>> = BTreeMap::new();
        for delta in deltas.iter().filter(|delta| {
            since == 0
                || delta
                    .native_timestamp
                    .is_some_and(|timestamp| timestamp >= since)
        }) {
            by_model
                .entry((
                    delta.provider.clone(),
                    delta.model.clone().unwrap_or_default(),
                ))
                .or_default()
                .push(delta);
            if let Some(timestamp) = delta.native_timestamp {
                by_day
                    .entry((timestamp / 86_400) * 86_400)
                    .or_default()
                    .push(delta);
            }
        }
        payload["provider_usage"]["by_model"] = Value::Array(
            by_model
                .into_iter()
                .map(|((provider, model), deltas)| {
                    let priced = price_deltas(deltas.iter().copied(), prices);
                    let (_, actual) = actual_for_deltas(deltas.into_iter());
                    let total_tokens = actual
                        .as_ref()
                        .and_then(|tokens| tokens.input_tokens?.checked_add(tokens.output_tokens?));
                    json!({
                        "provider": provider,
                        "model": model_value(&model),
                        "cost_usd": priced.total_cost_usd,
                        "total_tokens": total_tokens,
                        "cost_basis": if priced.total_cost_usd.is_some() {
                            "provider_reported_priced"
                        } else {
                            "provider_reported_unpriced"
                        },
                        "provider_actual": actual,
                    })
                })
                .collect(),
        );
        payload["provider_usage"]["by_day"] = Value::Array(
            by_day
                .into_iter()
                .rev()
                .take(366)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|(day, deltas)| {
                    let priced = price_deltas(deltas.iter().copied(), prices);
                    let (_, actual) = actual_for_deltas(deltas.into_iter());
                    let total_tokens = actual
                        .as_ref()
                        .and_then(|tokens| tokens.input_tokens?.checked_add(tokens.output_tokens?));
                    json!({
                        "day": day,
                        "cost_usd": priced.total_cost_usd,
                        "total_tokens": total_tokens,
                        "provider_actual": actual,
                    })
                })
                .collect(),
        );
    }

    Json(payload)
}

/// GET `/api/plugins/savings/pricing` — deterministic bundled all-provider
/// prices with content-addressed provenance.
pub async fn pricing() -> Json<Value> {
    Json(savings_pricing::pricing_payload())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_usecases::provider_usage::{
        AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
    };

    #[test]
    fn partial_provider_usage_never_becomes_an_actual_zero_token_block() {
        let aggregate = ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Partial,
            observations_seen: 1,
            totals: AggregatedProviderUsageCountersV1 {
                input_tokens: Some(10),
                output_tokens: Some(2),
                ..AggregatedProviderUsageCountersV1::unknown()
            },
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: Some(1),
        };

        assert!(actual_tokens(&aggregate).is_none());
    }

    #[test]
    fn basis_labels() {
        assert_eq!(basis_label(0, 0), "estimated");
        assert_eq!(basis_label(0, 4), "estimated");
        assert_eq!(basis_label(4, 4), "tokenized");
        assert_eq!(basis_label(2, 4), "estimated");
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
            "estimated_input_tokens": 40,
            "estimated_output_tokens": 60,
        });
        let block = token_block(&row, None);
        assert_eq!(block["cost_basis"], "estimated");
        assert_eq!(block["tokenized_messages"], 0);
        assert_eq!(block["estimated_messages"], 3);
        assert_eq!(block["estimated"]["input_tokens"], 40);
        assert_eq!(block["estimated"]["output_tokens"], 60);
        assert_eq!(block["tokenized"]["input_tokens"], 0);
    }

    #[test]
    fn token_block_prefers_overlay_tiers() {
        let row = json!({
            "messages": 2,
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

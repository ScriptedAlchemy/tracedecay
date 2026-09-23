//! Savings & Cost dashboard API (`/api/plugins/savings/*`).
//!
//! Two data stores feed this tab:
//!
//! - **Global accounting DB** (the registered profile store behind
//!   `tracedecay gain` / `tracedecay cost` / `tracedecay monitor`): the
//!   `savings_ledger`. Ledger aggregation reuses [`RegisteredGlobalDb::sum_savings`] /
//!   [`RegisteredGlobalDb::savings_history`], the same queries `tracedecay gain` runs.
//! - **Session store** (the resolved LCM store the dashboard already serves):
//!   canonical provider-usage observations plus `sessions` +
//!   `session_messages`, whose content and model fields provide a separate
//!   non-billing token-count overlay.
//!
//! Content token counts carry an explicit provenance label:
//!
//! - `"tokenized"`, stored text counted with a
//!   real BPE tokenizer (see `token_count`): exact for OpenAI-family
//!   models, a labeled approximation for vendors without a public
//!   tokenizer.
//! - `"estimated"`, the chars/4 heuristic the LCM views use
//!   (`(LENGTH(text)+3)/4`), the fallback when the `token-counting`
//!   feature is compiled out.
//!
//! Provider billing counters are exposed separately as provider-usage events;
//! they are never treated as message counts.
//!
//! Dollar costs use one bundled, deterministic all-provider authority. Unknown models keep their token counts but get no invented
//! price.

use std::collections::{BTreeMap, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_contracts::CostsReadModelV1;
use tracedecay_domain::{CoverageStateV1, ObservationScopeV1};
use tracedecay_session_memory::provider_usage::{
    AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
    ProviderUsageDeltaV1, price_provider_usage, provider_usage_aggregate,
    provider_usage_range_start,
};

use super::read_model::{DashboardCoverageV1, DashboardEnvelopeV1, scope_from_state};
use super::token_count::{
    MESSAGE_TOKENS_CTE, MessageTokens, counting_available, encoder_for_model,
};
use super::util::{
    JsonQuery, i64_field, query_i64_result, query_rows, str_field,
};
use super::{DashboardState, savings_pricing, token_count};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::params;

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
struct SavingsAccountingSummaryV1 {
    available: bool,
    db: String,
    recording: Value,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    ledger: Option<SavingsLedgerSummaryV1>,
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

/// One model-keyed content aggregate from the session store, joined to the
/// exact provider usage recorded for that model. `model` is `None` for
/// messages whose model was never recorded; that row keeps its token counts
/// and is priced by nothing.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsModelRowV1 {
    model: Option<String>,
    sessions: i64,
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

/// One UTC-day bucket of the per-model content aggregate.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsModelDayRowV1 {
    day: i64,
    model: Option<String>,
    messages: i64,
    provider_usage_events: i64,
    tokenized_messages: i64,
    estimated_messages: i64,
    cost_basis: String,
    provider_actual: Option<TokenActualV1>,
    tokenized: TokenPairV1,
    estimated: TokenPairV1,
}

/// Canonical priced usage for one exact provider/model pair. `cost_usd` is
/// `None` whenever any usage event in the pair could not be priced: the
/// projector never emits a partial dollar figure for a model.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsProviderModelSpendV1 {
    provider: String,
    model: Option<String>,
    usage_events: i64,
    cost_usd: Option<f64>,
    total_tokens: Option<i64>,
    cost_basis: String,
    provider_actual: Option<TokenActualV1>,
}

/// Canonical priced usage for one UTC day across every provider.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsProviderDaySpendV1 {
    day: i64,
    usage_events: i64,
    cost_usd: Option<f64>,
    total_tokens: Option<i64>,
    provider_actual: Option<TokenActualV1>,
}

/// How much of one provider's observed usage the pricing authority could
/// price. `priced` means every usage event priced; `partial` means some did
/// and the dollar figure covers only those; `unpriced` means none did.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SavingsPricingClassV1 {
    Priced,
    Partial,
    Unpriced,
}

/// Provider-level spend attribution over exact provider usage observations.
///
/// `priced_cost_usd` sums only the model groups the canonical projector
/// priced completely, and the event/model counts beside it say how much of
/// the provider's usage that figure covers. `total_cost_usd` is the
/// projector's own complete total and is `None` unless every event priced.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsProviderSpendV1 {
    provider: String,
    pricing: SavingsPricingClassV1,
    usage_events: i64,
    priced_events: i64,
    unpriced_events: i64,
    /// Usage events whose observation carried no model identity. They are
    /// counted in `unpriced_events` as well: no model means no rate.
    unknown_model_events: i64,
    /// Usage events with no native timestamp. They are attributed here but
    /// cannot be placed on the dated series.
    undated_events: i64,
    models: i64,
    priced_models: i64,
    unpriced_models: i64,
    sessions: i64,
    priced_cost_usd: Option<f64>,
    total_cost_usd: Option<f64>,
    total_tokens: Option<i64>,
    provider_actual: Option<TokenActualV1>,
}

/// One provider's canonical priced usage on one UTC day.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsProviderDayPointV1 {
    day: i64,
    provider: String,
    usage_events: i64,
    priced_events: i64,
    unpriced_events: i64,
    priced_cost_usd: Option<f64>,
    total_cost_usd: Option<f64>,
    total_tokens: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct SavingsProviderUsageAttributionV1 {
    available: bool,
    #[serde(default)]
    pricing_revision: Option<String>,
    #[serde(default)]
    undated_events: Option<i64>,
    by_model: Vec<SavingsProviderModelSpendV1>,
    by_day: Vec<SavingsProviderDaySpendV1>,
    by_provider: Vec<SavingsProviderSpendV1>,
    by_provider_day: Vec<SavingsProviderDayPointV1>,
}

/// GET `/api/plugins/savings/models` response contract.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub(super) struct SavingsModelsPayloadV1 {
    available: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<String>,
    range: String,
    #[serde(default)]
    since: Option<i64>,
    models: Vec<SavingsModelRowV1>,
    daily: Vec<SavingsModelDayRowV1>,
    #[serde(default)]
    provider_usage_coverage: Option<String>,
    provider_usage: SavingsProviderUsageAttributionV1,
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
    prices: &tracedecay_session_memory::provider_pricing::PriceTable,
) -> tracedecay_session_memory::provider_usage::ProviderUsageCostSummaryV1 {
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

fn count_i64(value: impl TryInto<i64>) -> i64 {
    value.try_into().unwrap_or(i64::MAX)
}

fn cost_basis_label(cost_usd: Option<f64>) -> &'static str {
    if cost_usd.is_some() {
        "provider_reported_priced"
    } else {
        "provider_reported_unpriced"
    }
}

fn total_tokens_of(actual: Option<&TokenActualV1>) -> Option<i64> {
    actual.and_then(|tokens| tokens.input_tokens?.checked_add(tokens.output_tokens?))
}

/// The dollar figure the canonical projector priced completely, and the
/// population it covers. Sums only model groups whose every usage event
/// priced; a group with one unpriced event contributes nothing, and the
/// counts beside the sum say so.
struct PricedSubtotal {
    priced_cost_usd: Option<f64>,
    priced_events: u64,
    priced_models: usize,
    unpriced_models: usize,
}

fn priced_subtotal(
    summary: &tracedecay_session_memory::provider_usage::ProviderUsageCostSummaryV1,
) -> PricedSubtotal {
    let mut priced_cost = 0.0_f64;
    let mut priced_events = 0_u64;
    let mut priced_models = 0_usize;
    let mut unpriced_models = 0_usize;
    for model in &summary.by_model {
        match model.cost_usd {
            Some(cost) => {
                priced_cost += cost;
                priced_events = priced_events.saturating_add(model.usage_events);
                priced_models += 1;
            }
            None => unpriced_models += 1,
        }
    }
    PricedSubtotal {
        priced_cost_usd: (priced_events > 0 && priced_cost.is_finite()).then_some(priced_cost),
        priced_events,
        priced_models,
        unpriced_models,
    }
}

fn pricing_class(
    usage_events: u64,
    unpriced_events: u64,
    priced_events: u64,
) -> SavingsPricingClassV1 {
    if usage_events > 0 && unpriced_events == 0 {
        SavingsPricingClassV1::Priced
    } else if priced_events > 0 {
        SavingsPricingClassV1::Partial
    } else {
        SavingsPricingClassV1::Unpriced
    }
}

/// Provider-level attribution over one provider's exact usage deltas, priced
/// by the canonical projection so every dollar here agrees with `/api/costs`.
fn provider_spend(
    provider: &str,
    deltas: &[&ProviderUsageDeltaV1],
    prices: &tracedecay_session_memory::provider_pricing::PriceTable,
) -> SavingsProviderSpendV1 {
    let summary = price_deltas(deltas.iter().copied(), prices);
    let subtotal = priced_subtotal(&summary);
    let (_, actual) = actual_for_deltas(deltas.iter().copied());
    let sessions = deltas
        .iter()
        .map(|delta| delta.session_id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    SavingsProviderSpendV1 {
        provider: provider.to_owned(),
        pricing: pricing_class(
            summary.usage_events,
            summary.unpriced_events,
            subtotal.priced_events,
        ),
        usage_events: count_i64(summary.usage_events),
        priced_events: count_i64(subtotal.priced_events),
        unpriced_events: count_i64(summary.unpriced_events),
        unknown_model_events: count_i64(
            deltas.iter().filter(|delta| delta.model.is_none()).count(),
        ),
        undated_events: count_i64(
            deltas
                .iter()
                .filter(|delta| delta.native_timestamp.is_none())
                .count(),
        ),
        models: count_i64(summary.by_model.len()),
        priced_models: count_i64(subtotal.priced_models),
        unpriced_models: count_i64(subtotal.unpriced_models),
        sessions: count_i64(sessions),
        priced_cost_usd: subtotal.priced_cost_usd,
        total_cost_usd: summary.total_cost_usd,
        total_tokens: total_tokens_of(actual.as_ref()),
        provider_actual: actual,
    }
}

fn provider_day_point(
    day: i64,
    provider: &str,
    deltas: &[&ProviderUsageDeltaV1],
    prices: &tracedecay_session_memory::provider_pricing::PriceTable,
) -> SavingsProviderDayPointV1 {
    let summary = price_deltas(deltas.iter().copied(), prices);
    let subtotal = priced_subtotal(&summary);
    let (_, actual) = actual_for_deltas(deltas.iter().copied());
    SavingsProviderDayPointV1 {
        day,
        provider: provider.to_owned(),
        usage_events: count_i64(summary.usage_events),
        priced_events: count_i64(subtotal.priced_events),
        unpriced_events: count_i64(summary.unpriced_events),
        priced_cost_usd: subtotal.priced_cost_usd,
        total_cost_usd: summary.total_cost_usd,
        total_tokens: total_tokens_of(actual.as_ref()),
    }
}

fn day_bucket(timestamp: i64) -> i64 {
    (timestamp / 86_400) * 86_400
}

/// The provider-usage attribution block of `/models`: exact deltas grouped by
/// provider/model, day, provider, and provider/day, each priced by the same
/// canonical projection. `since == 0` admits undated deltas to every
/// non-dated grouping; a positive range excludes them, as the SQL folds do.
fn provider_usage_attribution(
    deltas: &[ProviderUsageDeltaV1],
    since: i64,
    prices: &tracedecay_session_memory::provider_pricing::PriceTable,
) -> SavingsProviderUsageAttributionV1 {
    const DAY_LIMIT: usize = 366;
    let in_range = deltas
        .iter()
        .filter(|delta| {
            since == 0
                || delta
                    .native_timestamp
                    .is_some_and(|timestamp| timestamp >= since)
        })
        .collect::<Vec<_>>();

    let mut by_model: BTreeMap<(String, String), Vec<&ProviderUsageDeltaV1>> = BTreeMap::new();
    let mut by_day: BTreeMap<i64, Vec<&ProviderUsageDeltaV1>> = BTreeMap::new();
    let mut by_provider: BTreeMap<String, Vec<&ProviderUsageDeltaV1>> = BTreeMap::new();
    let mut by_provider_day: BTreeMap<(i64, String), Vec<&ProviderUsageDeltaV1>> = BTreeMap::new();
    let mut undated_events = 0_usize;
    for delta in in_range {
        by_model
            .entry((
                delta.provider.clone(),
                delta.model.clone().unwrap_or_default(),
            ))
            .or_default()
            .push(delta);
        by_provider
            .entry(delta.provider.clone())
            .or_default()
            .push(delta);
        match delta.native_timestamp {
            Some(timestamp) => {
                let day = day_bucket(timestamp);
                by_day.entry(day).or_default().push(delta);
                by_provider_day
                    .entry((day, delta.provider.clone()))
                    .or_default()
                    .push(delta);
            }
            None => undated_events += 1,
        }
    }

    // The dated series keep the newest 366 days, as the content series does.
    let day_floor = by_day
        .keys()
        .rev()
        .nth(DAY_LIMIT - 1)
        .copied()
        .unwrap_or(i64::MIN);

    SavingsProviderUsageAttributionV1 {
        available: true,
        pricing_revision: Some(prices.revision.clone()),
        undated_events: Some(count_i64(undated_events)),
        by_model: by_model
            .into_iter()
            .map(|((provider, model), deltas)| {
                let priced = price_deltas(deltas.iter().copied(), prices);
                let (_, actual) = actual_for_deltas(deltas.into_iter());
                SavingsProviderModelSpendV1 {
                    provider,
                    model: (!model.is_empty()).then_some(model),
                    usage_events: count_i64(priced.usage_events),
                    cost_usd: priced.total_cost_usd,
                    total_tokens: total_tokens_of(actual.as_ref()),
                    cost_basis: cost_basis_label(priced.total_cost_usd).to_owned(),
                    provider_actual: actual,
                }
            })
            .collect(),
        by_day: by_day
            .into_iter()
            .filter(|(day, _)| *day >= day_floor)
            .map(|(day, deltas)| {
                let priced = price_deltas(deltas.iter().copied(), prices);
                let (_, actual) = actual_for_deltas(deltas.into_iter());
                SavingsProviderDaySpendV1 {
                    day,
                    usage_events: count_i64(priced.usage_events),
                    cost_usd: priced.total_cost_usd,
                    total_tokens: total_tokens_of(actual.as_ref()),
                    provider_actual: actual,
                }
            })
            .collect(),
        by_provider: by_provider
            .iter()
            .map(|(provider, deltas)| provider_spend(provider, deltas, prices))
            .collect(),
        by_provider_day: by_provider_day
            .iter()
            .filter(|((day, _), _)| *day >= day_floor)
            .map(|((day, provider), deltas)| provider_day_point(*day, provider, deltas, prices))
            .collect(),
    }
}

fn unavailable_provider_usage_attribution() -> SavingsProviderUsageAttributionV1 {
    SavingsProviderUsageAttributionV1 {
        available: false,
        pricing_revision: None,
        undated_events: None,
        by_model: Vec::new(),
        by_day: Vec::new(),
        by_provider: Vec::new(),
        by_provider_day: Vec::new(),
    }
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
#[hotpath::measure(label = "dashboard_api.savings.overview", future = true)]
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
                // would fail to decode and collapse the whole route to a 500,
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
    hotpath::future!(
        async move {
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
        },
        label = "dashboard_api.savings.costs"
    )
    .await
}

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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // An unreadable ledger renders as an unavailable block naming the failed
    // read, the same honest degrade the sibling blocks below already use,
    // never as a page of zero totals.
    let windows = async {
        Ok::<_, String>((
            gdb.sum_savings(None, now - (now % 86_400)).await?,
            gdb.sum_savings(None, now - 7 * 86_400).await?,
            gdb.sum_savings(None, now - 30 * 86_400).await?,
            gdb.sum_savings(None, 0).await?,
        ))
    };
    let (today, week, month, all_time) = match windows.await {
        Ok(windows) => windows,
        Err(error) => {
            return merge(
                json!({ "db": db_path, "recording": recording_block() }),
                read_failed_block(error),
            );
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
    let rows = query_rows(&conn, &sql, ()).await?;
    let agg = rows
        .first()
        .cloned()
        .ok_or_else(|| "session overview query returned no row".to_string())?;
    let session_count = query_i64_result(&conn, "SELECT COUNT(*) FROM sessions", ()).await?;

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

/// The typed `/models` failure body: the request could not be served and no
/// row of it is invented. `range` echoes what was asked for.
fn models_read_failed(range: Option<&str>, error: String) -> SavingsModelsPayloadV1 {
    SavingsModelsPayloadV1 {
        available: false,
        status: Some("read_failed".to_owned()),
        error: Some(error),
        range: range.unwrap_or("all").to_owned(),
        since: None,
        models: Vec::new(),
        daily: Vec::new(),
        provider_usage_coverage: None,
        provider_usage: unavailable_provider_usage_attribution(),
    }
}

/// GET `/api/plugins/savings/models?range=`
///
/// Per-model token aggregates from the session store plus canonical
/// provider-usage cost grouped by exact provider/model, day, provider, and
/// provider/day. Every dollar figure comes from the same pricing projection
/// `/api/costs` serves; unpriced usage keeps its counts and gets no price.
#[hotpath::measure(label = "dashboard_api.savings.models", future = true)]
pub async fn models(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<RangeParams>,
) -> Response {
    let (range, since) = match range_since(params.range.as_deref()) {
        Ok(range) => range,
        // The caller named a window this route does not serve: a request
        // fault, answered as one, with the typed empty body rather than a
        // ledger of zeros.
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(models_read_failed(params.range.as_deref(), error)),
            )
                .into_response();
        }
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
        let model_rows = query_rows(&conn, &model_sql, params![since])
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
        let daily_rows = query_rows(&conn, &daily_sql, params![since])
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

    let attribution = match usage_deltas {
        Some(deltas) => provider_usage_attribution(deltas, since, prices),
        None => unavailable_provider_usage_attribution(),
    };
    let provider_usage = match serde_json::to_value(attribution) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(models_read_failed(
                    Some(&range),
                    format!("failed to encode provider usage attribution: {error}"),
                )),
            )
                .into_response();
        }
    };
    payload["provider_usage"] = provider_usage;

    match decode_contract::<SavingsModelsPayloadV1>(payload, "savings models") {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(models_read_failed(Some(&range), error)),
        )
            .into_response(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_session_memory::provider_usage::{
        AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
    };

    fn delta(
        sequence: u64,
        provider: &str,
        model: Option<&str>,
        session: &str,
        timestamp: Option<i64>,
    ) -> ProviderUsageDeltaV1 {
        ProviderUsageDeltaV1 {
            observation_id: format!("obs-{sequence}"),
            receipt_id: format!("receipt-{sequence}"),
            observation_sequence: sequence,
            usage_ordinal: 0,
            scope: ObservationScopeV1::Profile,
            provider: provider.to_owned(),
            model: model.map(str::to_owned),
            session_id: session.to_owned(),
            turn_id: None,
            message_id: None,
            request_id: None,
            native_kind: "usage".to_owned(),
            native_field: "usage".to_owned(),
            native_timestamp: timestamp,
            derivation:
                tracedecay_session_memory::provider_usage::ProviderUsageDeltaDerivationV1::NativeDelta,
            derived_from_sequence: None,
            counters: AggregatedProviderUsageCountersV1 {
                input_tokens: Some(1_000),
                output_tokens: Some(100),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                reasoning_tokens: None,
                total_tokens: Some(1_100),
            },
        }
    }

    #[test]
    fn provider_attribution_keeps_priced_partial_and_unpriced_providers_distinct() {
        let day = 1_760_000_000_i64 - (1_760_000_000_i64 % 86_400);
        let deltas = vec![
            // Fully priced: an exact bundled Anthropic slug on two sessions.
            delta(1, "claude", Some("claude-opus-4"), "s-1", Some(day + 60)),
            delta(2, "claude", Some("claude-opus-4"), "s-2", Some(day + 120)),
            // Partial: one priced OpenAI model and one model nobody prices.
            delta(3, "codex", Some("gpt-4.1"), "s-3", Some(day + 86_400)),
            delta(
                4,
                "codex",
                Some("gpt-nonexistent-fixture"),
                "s-3",
                Some(day + 86_400),
            ),
            // Unpriced: a provider outside every vendor namespace, one event
            // with no model identity, one with no timestamp.
            delta(5, "mystery", None, "s-4", Some(day)),
            delta(6, "mystery", Some("mystery-1"), "s-4", None),
        ];

        let attribution = provider_usage_attribution(&deltas, 0, savings_pricing::load_table());

        assert!(attribution.available);
        assert_eq!(attribution.undated_events, Some(1));
        let by_provider: HashMap<_, _> = attribution
            .by_provider
            .iter()
            .map(|row| (row.provider.as_str(), row))
            .collect();

        let claude = by_provider["claude"];
        assert_eq!(claude.pricing, SavingsPricingClassV1::Priced);
        assert_eq!(claude.usage_events, 2);
        assert_eq!(claude.priced_events, 2);
        assert_eq!(claude.unpriced_events, 0);
        assert_eq!(claude.sessions, 2);
        assert_eq!(claude.models, 1);
        assert!(claude.priced_cost_usd.is_some_and(|cost| cost > 0.0));
        assert_eq!(claude.total_cost_usd, claude.priced_cost_usd);
        assert_eq!(claude.total_tokens, Some(2_200));

        let codex = by_provider["codex"];
        assert_eq!(codex.pricing, SavingsPricingClassV1::Partial);
        assert_eq!(codex.usage_events, 2);
        assert_eq!(codex.priced_events, 1);
        assert_eq!(codex.unpriced_events, 1);
        assert_eq!(codex.priced_models, 1);
        assert_eq!(codex.unpriced_models, 1);
        assert_eq!(codex.sessions, 1);
        assert!(codex.priced_cost_usd.is_some_and(|cost| cost > 0.0));
        assert_eq!(
            codex.total_cost_usd, None,
            "a provider with one unpriced event has no complete total"
        );

        let mystery = by_provider["mystery"];
        assert_eq!(mystery.pricing, SavingsPricingClassV1::Unpriced);
        assert_eq!(mystery.usage_events, 2);
        assert_eq!(mystery.unpriced_events, 2);
        assert_eq!(mystery.unknown_model_events, 1);
        assert_eq!(mystery.undated_events, 1);
        assert_eq!(mystery.priced_cost_usd, None);
        assert_eq!(mystery.total_cost_usd, None);
        assert_eq!(
            mystery.total_tokens,
            Some(2_200),
            "tokens are facts independent of price"
        );

        // The dated series carries only timestamped events, per provider.
        assert_eq!(attribution.by_provider_day.len(), 3);
        let claude_day = attribution
            .by_provider_day
            .iter()
            .find(|point| point.provider == "claude" && point.day == day)
            .expect("claude day point");
        assert_eq!(claude_day.usage_events, 2);
        assert_eq!(claude_day.priced_cost_usd, claude.priced_cost_usd);
        let codex_day = attribution
            .by_provider_day
            .iter()
            .find(|point| point.provider == "codex")
            .expect("codex day point");
        assert_eq!(codex_day.day, day + 86_400);
        assert_eq!(codex_day.unpriced_events, 1);
        assert_eq!(codex_day.total_cost_usd, None);

        // The unknown-model row keeps its identity absence explicit.
        assert!(
            attribution
                .by_model
                .iter()
                .any(|row| row.provider == "mystery"
                    && row.model.is_none()
                    && row.cost_usd.is_none())
        );
    }

    #[test]
    fn provider_attribution_range_excludes_undated_deltas() {
        let deltas = vec![
            delta(
                1,
                "claude",
                Some("claude-opus-4"),
                "s-1",
                Some(2_000_000_000),
            ),
            delta(2, "claude", Some("claude-opus-4"), "s-1", None),
        ];
        let attribution =
            provider_usage_attribution(&deltas, 1_000_000_000, savings_pricing::load_table());
        assert_eq!(attribution.undated_events, Some(0));
        assert_eq!(attribution.by_provider.len(), 1);
        assert_eq!(attribution.by_provider[0].usage_events, 1);
        assert_eq!(attribution.by_provider[0].undated_events, 0);
    }

    #[test]
    fn models_read_failed_body_is_typed_and_empty() {
        let body = models_read_failed(Some("tomorrow"), "unsupported range".to_owned());
        assert!(!body.available);
        assert_eq!(body.status.as_deref(), Some("read_failed"));
        assert_eq!(body.range, "tomorrow");
        assert!(body.models.is_empty());
        assert!(!body.provider_usage.available);
        let value = serde_json::to_value(&body).expect("encode");
        decode_contract::<SavingsModelsPayloadV1>(value, "savings models")
            .expect("failure body satisfies its own contract");
    }

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
    fn tier_sums_attribute_roles_like_sql() {
        let mut sums = TierSums::default();
        let msg = |role: &str, tokens: i64, tokenized: bool| MessageTokens {
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
}

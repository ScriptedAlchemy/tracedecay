use std::collections::{BTreeMap, HashMap};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalUnknownStateV1, ObservationScopeV1, ProviderUsageCounterSemanticsV1,
    ProviderUsageCountersV1, ProviderUsageCursorV1, ProviderUsageModelV1,
    ProviderUsageObservationV1, ProviderUsageReadV1,
};
use tracedecay_global_db::RegisteredGlobalDb;

use crate::provider_pricing::{PriceTable, cost_of_usage, load_table};

const PROVIDER_USAGE_PAGE_SIZE: usize = 1_000;

pub fn provider_usage_range_start(range: &str) -> Result<u64, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
        .as_secs();
    Ok(match range {
        "today" => now - (now % 86_400),
        "7d" => now.saturating_sub(7 * 86_400),
        "30d" | "month" => now.saturating_sub(30 * 86_400),
        "all" => 0,
        _ => return Err(format!("unsupported provider usage range: {range}")),
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageCoverageV1 {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageIssueKindV1 {
    InitialCumulativeCheckpoint,
    DuplicateCumulativeCheckpoint,
    CumulativeReset,
    MultipleDeltaRows,
    MultipleCumulativeCheckpoints,
    PairedCheckpointMismatch,
    MalformedCounters,
    UnavailableCounters,
    UnknownCounterSemantics,
    UnavailableCounterSemantics,
    UnknownModel,
    UnavailableModel,
    ReadUnknown,
    ReadUnavailable,
    PaginationInterrupted,
    PaginationWatermarkChanged,
    PaginationCursorDidNotAdvance,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProviderUsageIssueV1 {
    pub kind: ProviderUsageIssueKindV1,
    pub observation_sequence: Option<u64>,
    pub provider: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct AggregatedProviderUsageCountersV1 {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

impl AggregatedProviderUsageCountersV1 {
    pub const fn unknown() -> Self {
        Self {
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageDeltaDerivationV1 {
    NativeDelta,
    CumulativeDifference,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProviderUsageDeltaV1 {
    pub observation_id: String,
    pub receipt_id: String,
    pub observation_sequence: u64,
    pub usage_ordinal: u32,
    pub scope: ObservationScopeV1,
    pub provider: String,
    pub model: Option<String>,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub message_id: Option<String>,
    pub request_id: Option<String>,
    pub native_kind: String,
    pub native_field: String,
    pub native_timestamp: Option<i64>,
    pub derivation: ProviderUsageDeltaDerivationV1,
    pub derived_from_sequence: Option<u64>,
    pub counters: AggregatedProviderUsageCountersV1,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ProviderUsageAggregateV1 {
    pub coverage: ProviderUsageCoverageV1,
    pub observations_seen: u64,
    pub totals: AggregatedProviderUsageCountersV1,
    pub deltas: Vec<ProviderUsageDeltaV1>,
    pub issues: Vec<ProviderUsageIssueV1>,
    pub upper_observation_sequence: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ProviderUsageModelCostV1 {
    pub provider: String,
    pub model: String,
    pub usage_events: u64,
    pub total_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ProviderUsageCostSummaryV1 {
    pub coverage: ProviderUsageCoverageV1,
    pub pricing_revision: String,
    pub usage_events: u64,
    pub unpriced_events: u64,
    pub total_cost_usd: Option<f64>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub total_cache_read_tokens: Option<u64>,
    pub total_cache_write_tokens: Option<u64>,
    pub by_model: Vec<ProviderUsageModelCostV1>,
}

#[derive(Debug, Eq, PartialEq)]
enum ScanStep {
    Continue(ProviderUsageCursorV1),
    Complete(ProviderUsageAggregateV1),
}

struct ProviderUsageScanV1 {
    observations: Vec<ProviderUsageObservationV1>,
    upper_observation_sequence: Option<u64>,
    last_cursor: Option<ProviderUsageCursorV1>,
}

impl ProviderUsageScanV1 {
    fn new() -> Self {
        Self {
            observations: Vec::new(),
            upper_observation_sequence: None,
            last_cursor: None,
        }
    }

    fn accept(&mut self, read: ProviderUsageReadV1) -> ScanStep {
        match read {
            ProviderUsageReadV1::Known {
                observations,
                upper_observation_sequence,
                next_cursor,
            } => {
                if self
                    .upper_observation_sequence
                    .is_some_and(|upper| upper != upper_observation_sequence)
                {
                    return ScanStep::Complete(
                        self.finish_with_issue(
                            ProviderUsageIssueKindV1::PaginationWatermarkChanged,
                        ),
                    );
                }
                self.upper_observation_sequence = Some(upper_observation_sequence);
                self.observations.extend(observations);
                match next_cursor {
                    Some(cursor)
                        if self.last_cursor.as_ref().is_some_and(|previous| {
                            (cursor.observation_sequence, cursor.usage_ordinal)
                                <= (previous.observation_sequence, previous.usage_ordinal)
                                || cursor.upper_observation_sequence
                                    != previous.upper_observation_sequence
                        }) =>
                    {
                        ScanStep::Complete(self.finish_with_issue(
                            ProviderUsageIssueKindV1::PaginationCursorDidNotAdvance,
                        ))
                    }
                    Some(cursor)
                        if cursor.upper_observation_sequence != upper_observation_sequence =>
                    {
                        ScanStep::Complete(self.finish_with_issue(
                            ProviderUsageIssueKindV1::PaginationWatermarkChanged,
                        ))
                    }
                    Some(cursor) => {
                        self.last_cursor = Some(cursor.clone());
                        ScanStep::Continue(cursor)
                    }
                    None => ScanStep::Complete(self.finish()),
                }
            }
            ProviderUsageReadV1::Unknown {
                upper_observation_sequence,
                ..
            } => {
                self.upper_observation_sequence = Some(upper_observation_sequence);
                ScanStep::Complete(self.finish_with_issue(ProviderUsageIssueKindV1::ReadUnknown))
            }
            ProviderUsageReadV1::Unavailable {
                upper_observation_sequence,
                ..
            } => {
                self.upper_observation_sequence = Some(upper_observation_sequence);
                ScanStep::Complete(
                    self.finish_with_issue(ProviderUsageIssueKindV1::ReadUnavailable),
                )
            }
        }
    }

    fn fail(&mut self) -> ProviderUsageAggregateV1 {
        self.finish_with_issue(ProviderUsageIssueKindV1::PaginationInterrupted)
    }

    fn finish(&self) -> ProviderUsageAggregateV1 {
        let mut aggregate = reduce_provider_usage(&self.observations);
        if self.observations.is_empty() && self.upper_observation_sequence.is_some() {
            aggregate.coverage = ProviderUsageCoverageV1::Complete;
        }
        aggregate.upper_observation_sequence = self.upper_observation_sequence;
        aggregate
    }

    fn finish_with_issue(&self, kind: ProviderUsageIssueKindV1) -> ProviderUsageAggregateV1 {
        let mut aggregate = self.finish();
        aggregate.issues.push(ProviderUsageIssueV1 {
            kind,
            observation_sequence: self
                .last_cursor
                .as_ref()
                .map(|cursor| cursor.observation_sequence),
            provider: None,
            session_id: None,
        });
        aggregate.coverage = if self.observations.is_empty() {
            ProviderUsageCoverageV1::Unavailable
        } else {
            ProviderUsageCoverageV1::Partial
        };
        aggregate
    }
}

/// Walks one pinned provider-usage snapshot to exhaustion. Pagination and
/// truncation semantics live here so HTTP, MCP, hooks, and CLI cannot each
/// implement a subtly different bounded scan.
pub async fn provider_usage_aggregate(
    db: &RegisteredGlobalDb,
    scope: &ObservationScopeV1,
    provider: Option<&str>,
    session_id: Option<&str>,
) -> ProviderUsageAggregateV1 {
    let mut scan = ProviderUsageScanV1::new();
    let mut cursor = None;
    loop {
        let read = db
            .provider_usage_observations_after(
                scope,
                provider,
                session_id,
                cursor.as_ref(),
                PROVIDER_USAGE_PAGE_SIZE,
            )
            .await;
        let Ok(read) = read else {
            return scan.fail();
        };
        match scan.accept(read) {
            ScanStep::Continue(next) => cursor = Some(next),
            ScanStep::Complete(aggregate) => return aggregate,
        }
    }
}

/// Reads, reduces, and prices one pinned provider-usage snapshot. Pricing is
/// deterministic and side-effect-free; incomplete observations, unknown
/// models, missing range timestamps, and unpriceable counters keep the overall
/// dollar total unavailable.
pub async fn provider_usage_cost_summary(
    db: &RegisteredGlobalDb,
    scope: &ObservationScopeV1,
    provider: Option<&str>,
    session_id: Option<&str>,
    since_seconds: i64,
) -> ProviderUsageCostSummaryV1 {
    let aggregate = provider_usage_aggregate(db, scope, provider, session_id).await;
    price_provider_usage(&aggregate, load_table(), since_seconds)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Counters {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    reasoning: Option<u64>,
    total: Option<u64>,
}

impl Counters {
    fn from_observation(
        observation: &ProviderUsageObservationV1,
    ) -> Result<Self, ProviderUsageIssueKindV1> {
        match &observation.counters {
            ProviderUsageCountersV1::Known {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                total_tokens,
            } => {
                if [
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                    total_tokens,
                ]
                .iter()
                .all(|value| value.is_none())
                {
                    return Err(ProviderUsageIssueKindV1::MalformedCounters);
                }
                Ok(Self {
                    input: *input_tokens,
                    output: *output_tokens,
                    cache_read: *cache_read_tokens,
                    cache_write: *cache_write_tokens,
                    reasoning: *reasoning_tokens,
                    total: *total_tokens,
                })
            }
            ProviderUsageCountersV1::Unknown { reason } => Err(match reason {
                CanonicalUnknownStateV1::Malformed => ProviderUsageIssueKindV1::MalformedCounters,
                _ => ProviderUsageIssueKindV1::UnavailableCounters,
            }),
            ProviderUsageCountersV1::Unavailable { .. } => {
                Err(ProviderUsageIssueKindV1::UnavailableCounters)
            }
        }
    }

    fn difference(&self, previous: &Self) -> Result<Self, ProviderUsageIssueKindV1> {
        if self == previous {
            return Err(ProviderUsageIssueKindV1::DuplicateCumulativeCheckpoint);
        }
        let difference = |current: Option<u64>, prior: Option<u64>| match (current, prior) {
            (Some(current), Some(prior)) => current
                .checked_sub(prior)
                .ok_or(ProviderUsageIssueKindV1::CumulativeReset)
                .map(Some),
            (None, None) => Ok(None),
            _ => Err(ProviderUsageIssueKindV1::MalformedCounters),
        };
        Ok(Self {
            input: difference(self.input, previous.input)?,
            output: difference(self.output, previous.output)?,
            cache_read: difference(self.cache_read, previous.cache_read)?,
            cache_write: difference(self.cache_write, previous.cache_write)?,
            reasoning: difference(self.reasoning, previous.reasoning)?,
            total: difference(self.total, previous.total)?,
        })
    }

    fn as_public(&self) -> AggregatedProviderUsageCountersV1 {
        AggregatedProviderUsageCountersV1 {
            input_tokens: self.input,
            output_tokens: self.output,
            cache_read_tokens: self.cache_read,
            cache_write_tokens: self.cache_write,
            reasoning_tokens: self.reasoning,
            total_tokens: self.total,
        }
    }
}

#[derive(Default)]
struct CounterSum {
    input: FieldSum,
    output: FieldSum,
    cache_read: FieldSum,
    cache_write: FieldSum,
    reasoning: FieldSum,
    total: FieldSum,
}

impl CounterSum {
    fn add(&mut self, counters: &Counters) {
        self.input.add(counters.input);
        self.output.add(counters.output);
        self.cache_read.add(counters.cache_read);
        self.cache_write.add(counters.cache_write);
        self.reasoning.add(counters.reasoning);
        self.total.add(counters.total);
    }

    fn finish(self) -> AggregatedProviderUsageCountersV1 {
        AggregatedProviderUsageCountersV1 {
            input_tokens: self.input.finish(),
            output_tokens: self.output.finish(),
            cache_read_tokens: self.cache_read.finish(),
            cache_write_tokens: self.cache_write.finish(),
            reasoning_tokens: self.reasoning.finish(),
            total_tokens: self.total.finish(),
        }
    }
}

#[derive(Default)]
struct FieldSum {
    value: u64,
    observed: bool,
    complete: bool,
}

impl FieldSum {
    fn add(&mut self, value: Option<u64>) {
        if !self.observed {
            self.complete = true;
        }
        self.observed = true;
        match value {
            Some(value) => {
                if let Some(sum) = self.value.checked_add(value) {
                    self.value = sum;
                } else {
                    self.complete = false;
                }
            }
            None => self.complete = false,
        }
    }

    fn finish(self) -> Option<u64> {
        (self.observed && self.complete).then_some(self.value)
    }
}

#[derive(Default)]
struct ModelCostAccumulator {
    usage_events: u64,
    tokens: FieldSum,
    cost_usd: f64,
    cost_complete: bool,
}

impl ModelCostAccumulator {
    fn add(&mut self, counters: &Counters, cost_usd: Option<f64>) {
        self.usage_events = self.usage_events.saturating_add(1);
        self.tokens.add(match (counters.input, counters.output) {
            (Some(input), Some(output)) => input.checked_add(output),
            _ => None,
        });
        if self.usage_events == 1 {
            self.cost_complete = true;
        }
        match cost_usd {
            Some(cost) => {
                self.cost_usd += cost;
                if !self.cost_usd.is_finite() {
                    self.cost_complete = false;
                }
            }
            None => self.cost_complete = false,
        }
    }

    fn finish(self, provider: String, model: String) -> ProviderUsageModelCostV1 {
        ProviderUsageModelCostV1 {
            provider,
            model,
            usage_events: self.usage_events,
            total_tokens: self.tokens.finish(),
            cost_usd: self.cost_complete.then_some(self.cost_usd),
        }
    }
}

#[derive(Clone)]
struct Checkpoint {
    sequence: u64,
    counters: Counters,
}

fn issue(
    kind: ProviderUsageIssueKindV1,
    observation: &ProviderUsageObservationV1,
) -> ProviderUsageIssueV1 {
    ProviderUsageIssueV1 {
        kind,
        observation_sequence: Some(observation.observation_sequence),
        provider: Some(observation.provider.as_str().to_owned()),
        session_id: Some(observation.session_id.as_str().to_owned()),
    }
}

fn model_and_issue(
    observation: &ProviderUsageObservationV1,
) -> (Option<String>, Option<ProviderUsageIssueV1>) {
    match &observation.model {
        ProviderUsageModelV1::Known { model } => (Some(model.clone()), None),
        ProviderUsageModelV1::Unknown { .. } => (
            None,
            Some(issue(ProviderUsageIssueKindV1::UnknownModel, observation)),
        ),
        ProviderUsageModelV1::Unavailable { .. } => (
            None,
            Some(issue(
                ProviderUsageIssueKindV1::UnavailableModel,
                observation,
            )),
        ),
    }
}

fn delta(
    observation: &ProviderUsageObservationV1,
    counters: &Counters,
    derivation: ProviderUsageDeltaDerivationV1,
    derived_from_sequence: Option<u64>,
) -> (ProviderUsageDeltaV1, Option<ProviderUsageIssueV1>) {
    let (model, model_issue) = model_and_issue(observation);
    (
        ProviderUsageDeltaV1 {
            observation_id: observation.observation_id.as_str().to_owned(),
            receipt_id: observation.receipt_id.clone(),
            observation_sequence: observation.observation_sequence,
            usage_ordinal: observation.usage_ordinal,
            scope: observation.scope.clone(),
            provider: observation.provider.as_str().to_owned(),
            model,
            session_id: observation.session_id.as_str().to_owned(),
            turn_id: observation
                .turn_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            message_id: observation
                .message_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            request_id: observation
                .request_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            native_kind: observation.native_kind.clone(),
            native_field: observation.native_field.clone(),
            native_timestamp: observation.native_timestamp,
            derivation,
            derived_from_sequence,
            counters: counters.as_public(),
        },
        model_issue,
    )
}

type CheckpointKey = (
    ObservationScopeV1,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn checkpoint_key(
    observation: &ProviderUsageObservationV1,
) -> (CheckpointKey, Option<ProviderUsageIssueV1>) {
    let (model, model_issue) = match &observation.model {
        ProviderUsageModelV1::Known { model } => (model.clone(), None),
        ProviderUsageModelV1::Unknown { .. } => (
            format!("unknown:{}", observation.observation_sequence),
            Some(issue(ProviderUsageIssueKindV1::UnknownModel, observation)),
        ),
        ProviderUsageModelV1::Unavailable { .. } => (
            format!("unavailable:{}", observation.observation_sequence),
            Some(issue(
                ProviderUsageIssueKindV1::UnavailableModel,
                observation,
            )),
        ),
    };
    (
        (
            observation.scope.clone(),
            observation.provider.as_str().to_owned(),
            observation.session_id.as_str().to_owned(),
            model,
            observation.native_scope.as_str().to_owned(),
            observation.native_kind.clone(),
            observation.native_field.clone(),
        ),
        model_issue,
    )
}

/// Reduces a stable, ordered provider-usage snapshot. Native deltas are the
/// billing evidence; cumulative rows are checkpoints and are never summed in
/// addition to a paired delta.
pub fn reduce_provider_usage(
    observations: &[ProviderUsageObservationV1],
) -> ProviderUsageAggregateV1 {
    if observations.is_empty() {
        return ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Unavailable,
            observations_seen: 0,
            totals: AggregatedProviderUsageCountersV1::unknown(),
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: None,
        };
    }

    let mut ordered = observations.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|row| (row.observation_sequence, row.usage_ordinal));
    let mut checkpoints: HashMap<CheckpointKey, Checkpoint> = HashMap::new();
    let mut deltas = Vec::new();
    let mut issues = Vec::new();
    let mut totals = CounterSum::default();
    let mut index = 0;

    while index < ordered.len() {
        let first = ordered[index];
        let mut end = index + 1;
        while end < ordered.len()
            && ordered[end].observation_sequence == first.observation_sequence
            && ordered[end].observation_id == first.observation_id
        {
            end += 1;
        }
        let group = &ordered[index..end];
        let mut native_deltas = Vec::new();
        let mut cumulative = Vec::new();
        for observation in group {
            match observation.counter_semantics {
                ProviderUsageCounterSemanticsV1::Delta => native_deltas.push(*observation),
                ProviderUsageCounterSemanticsV1::Cumulative => cumulative.push(*observation),
                ProviderUsageCounterSemanticsV1::Unknown => issues.push(issue(
                    ProviderUsageIssueKindV1::UnknownCounterSemantics,
                    observation,
                )),
                ProviderUsageCounterSemanticsV1::Unavailable => issues.push(issue(
                    ProviderUsageIssueKindV1::UnavailableCounterSemantics,
                    observation,
                )),
            }
        }

        if native_deltas.len() > 1 {
            issues.push(issue(
                ProviderUsageIssueKindV1::MultipleDeltaRows,
                native_deltas[1],
            ));
        }
        if cumulative.len() > 1 {
            issues.push(issue(
                ProviderUsageIssueKindV1::MultipleCumulativeCheckpoints,
                cumulative[1],
            ));
        }

        let native = (native_deltas.len() == 1)
            .then(|| {
                let observation = native_deltas[0];
                Counters::from_observation(observation).map(|counters| (observation, counters))
            })
            .transpose();
        let checkpoint = (cumulative.len() == 1)
            .then(|| {
                let observation = cumulative[0];
                Counters::from_observation(observation).map(|counters| (observation, counters))
            })
            .transpose();

        let native = match native {
            Ok(value) => value,
            Err(kind) => {
                issues.push(issue(kind, native_deltas[0]));
                None
            }
        };
        let checkpoint = match checkpoint {
            Ok(value) => value,
            Err(kind) => {
                issues.push(issue(kind, cumulative[0]));
                None
            }
        };

        match (native, checkpoint) {
            (Some((native_row, native_counters)), Some((checkpoint_row, current))) => {
                let (key, checkpoint_model_issue) = checkpoint_key(checkpoint_row);
                let checkpoint_has_model_issue = checkpoint_model_issue.is_some();
                issues.extend(checkpoint_model_issue);
                let accept = match checkpoints.get(&key) {
                    None => true,
                    Some(previous) => match current.difference(&previous.counters) {
                        Ok(difference) if difference == native_counters => true,
                        Ok(_) => {
                            issues.push(issue(
                                ProviderUsageIssueKindV1::PairedCheckpointMismatch,
                                checkpoint_row,
                            ));
                            false
                        }
                        Err(kind) => {
                            issues.push(issue(kind, checkpoint_row));
                            false
                        }
                    },
                };
                checkpoints.insert(
                    key,
                    Checkpoint {
                        sequence: checkpoint_row.observation_sequence,
                        counters: current,
                    },
                );
                if accept {
                    let (event, model_issue) = delta(
                        native_row,
                        &native_counters,
                        ProviderUsageDeltaDerivationV1::NativeDelta,
                        None,
                    );
                    totals.add(&native_counters);
                    deltas.push(event);
                    if !checkpoint_has_model_issue {
                        issues.extend(model_issue);
                    }
                }
            }
            (Some((native_row, native_counters)), None) => {
                let (event, model_issue) = delta(
                    native_row,
                    &native_counters,
                    ProviderUsageDeltaDerivationV1::NativeDelta,
                    None,
                );
                totals.add(&native_counters);
                deltas.push(event);
                issues.extend(model_issue);
            }
            (None, Some((checkpoint_row, current))) => {
                let (key, checkpoint_model_issue) = checkpoint_key(checkpoint_row);
                issues.extend(checkpoint_model_issue);
                if let Some(previous) = checkpoints.get(&key) {
                    match current.difference(&previous.counters) {
                        Ok(difference) => {
                            let (event, model_issue) = delta(
                                checkpoint_row,
                                &difference,
                                ProviderUsageDeltaDerivationV1::CumulativeDifference,
                                Some(previous.sequence),
                            );
                            totals.add(&difference);
                            deltas.push(event);
                            issues.extend(model_issue);
                        }
                        Err(kind) => issues.push(issue(kind, checkpoint_row)),
                    }
                } else {
                    issues.push(issue(
                        ProviderUsageIssueKindV1::InitialCumulativeCheckpoint,
                        checkpoint_row,
                    ));
                }
                checkpoints.insert(
                    key,
                    Checkpoint {
                        sequence: checkpoint_row.observation_sequence,
                        counters: current,
                    },
                );
            }
            (None, None) => {}
        }
        index = end;
    }

    ProviderUsageAggregateV1 {
        coverage: if issues.is_empty() {
            ProviderUsageCoverageV1::Complete
        } else {
            ProviderUsageCoverageV1::Partial
        },
        observations_seen: observations.len() as u64,
        totals: totals.finish(),
        deltas,
        issues,
        upper_observation_sequence: ordered.last().map(|row| row.observation_sequence),
    }
}

/// Pure pricing projection shared by every transport.
pub fn price_provider_usage(
    aggregate: &ProviderUsageAggregateV1,
    prices: &PriceTable,
    since_seconds: i64,
) -> ProviderUsageCostSummaryV1 {
    let mut totals = CounterSum::default();
    let mut total_cost_usd = 0.0;
    let mut usage_events = 0_u64;
    let mut unpriced_events = 0_u64;
    let mut complete = aggregate.coverage == ProviderUsageCoverageV1::Complete;
    let mut by_model: BTreeMap<(String, String), ModelCostAccumulator> = BTreeMap::new();

    for delta in &aggregate.deltas {
        if since_seconds > 0 {
            match delta.native_timestamp {
                Some(timestamp) if timestamp < since_seconds => continue,
                Some(_) => {}
                None => {
                    complete = false;
                    unpriced_events = unpriced_events.saturating_add(1);
                    continue;
                }
            }
        }
        usage_events = usage_events.saturating_add(1);
        let counters = Counters {
            input: delta.counters.input_tokens,
            output: delta.counters.output_tokens,
            cache_read: delta.counters.cache_read_tokens,
            cache_write: delta.counters.cache_write_tokens,
            reasoning: delta.counters.reasoning_tokens,
            total: delta.counters.total_tokens,
        };
        totals.add(&counters);
        let cost = delta.model.as_deref().and_then(|model| {
            cost_of_usage(
                prices,
                &delta.provider,
                model,
                counters.input?,
                counters.output?,
                counters.cache_read,
                counters.cache_write,
            )
        });
        match cost {
            Some(cost) => {
                total_cost_usd += cost;
                if !total_cost_usd.is_finite() {
                    complete = false;
                }
            }
            None => {
                complete = false;
                unpriced_events = unpriced_events.saturating_add(1);
            }
        }
        if let Some(model) = &delta.model {
            by_model
                .entry((delta.provider.clone(), model.clone()))
                .or_default()
                .add(&counters, cost);
        }
    }

    let totals = totals.finish();
    ProviderUsageCostSummaryV1 {
        coverage: if complete {
            ProviderUsageCoverageV1::Complete
        } else if aggregate.coverage == ProviderUsageCoverageV1::Unavailable && usage_events == 0 {
            ProviderUsageCoverageV1::Unavailable
        } else {
            ProviderUsageCoverageV1::Partial
        },
        pricing_revision: prices.revision.clone(),
        usage_events,
        unpriced_events,
        total_cost_usd: complete.then_some(total_cost_usd),
        total_input_tokens: totals.input_tokens,
        total_output_tokens: totals.output_tokens,
        total_cache_read_tokens: totals.cache_read_tokens,
        total_cache_write_tokens: totals.cache_write_tokens,
        by_model: by_model
            .into_iter()
            .map(|((provider, model), summary)| summary.finish(provider, model))
            .collect(),
    }
}

#[cfg(test)]
mod tests;

//! The Plan 26 execution-topology metrics read model.
//!
//! Plan 26 owns schemas, joins, descriptors, and read models for the
//! execution-topology event family; Plans 24, 32, 36, and 37 own emission and
//! push their source facts through the one observability application
//! boundary. This module is therefore a pure projection: it reads recorded
//! `ObservabilityEnvelopeV1` events through [`ObservabilityQueryPort`] and
//! derives every descriptor named in Plan 26 from those events alone.
//!
//! Two invariants shape every type here. First, nothing is estimated: a
//! quantity that no recorded event carries is a typed absence
//! ([`ExecutionMetricUnavailableV1`]) with its eligible, observed, censored,
//! and unknown counts intact, never a zero or a hundred percent. Second,
//! nothing identifies: the projection reads only bounded classes and counts
//! out of the payloads and never copies an anchor, trace, scope, actor, path,
//! ref, or commit into a metric label.
//!
//! This is deliberately *not* an extension of
//! [`crate::work_topology_view::ExecutionTopologyViewV1`]. That view is the
//! current structural shape of Work in a scope, read from the attempt page,
//! the durable placement relation, and the resolved topology policy. These
//! metrics are a time-horizon aggregate over recorded observations with their
//! own denominators, coverage floors, and retention. They share a name family
//! and nothing else: joining them would let a policy-carried dimension stand
//! in for measured evidence.

mod projection;
mod support;

pub use projection::execution_topology_metrics;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    BlockedCauseV1, ConflictKindV1, ConflictOutcomeV1, DeliverySurfaceFamilyV1,
    DuplicateEffectOutcomeV1, DuplicateEffortKindV1, DurationBucketV1, IntegrationOperationKindV1,
    IntegrationResultV1, IntervalStateV1, RerunCauseV1, RerunSourceV1, StackDriftKindV1,
    WorkExecutionLeakKindV1, WorkExecutionLeakRecoveryV1,
};

use crate::observability::{MetricCoverageV1, MetricValueV1, ObservabilityHorizonV1};

/// Descriptor revision every measurement in this read model is pinned to.
pub const EXECUTION_TOPOLOGY_DESCRIPTOR_REVISION_V1: &str = "execution-topology-metrics.v1";

/// Projector revision recorded in every measurement's provenance. A change in
/// any formula below must change this string so a stored value can never be
/// compared against a differently derived one.
pub const EXECUTION_TOPOLOGY_PROJECTOR_REVISION_V1: &str = "execution-topology-projector.v1";

/// The persisted execution-topology event family, in the exact event-kind
/// spelling the domain contract stamps. The projection reads these kinds and
/// no others: a metric can never be fed by an event outside this family.
pub const EXECUTION_TOPOLOGY_EVENT_KINDS_V1: [&str; 11] = [
    "work.execution_topology.sampled.v1",
    "work.conflict_prediction.observed.v1",
    "work.conflict_outcome.linked.v1",
    "work.integration.transition.observed.v1",
    "work.stack_drift.observed.v1",
    "work.github_stack_capability.observed.v1",
    "work.duplicate_effort.observed.v1",
    "work.blocked_interval.observed.v1",
    "work.rerun.observed.v1",
    "work.execution_leak.observed.v1",
    "work.delivery_fanout.observed.v1",
];

/// Upper bound on events one read may draw. A horizon that holds more events
/// than this returns a `Capped` page, and every derived metric becomes
/// unavailable rather than reporting a partial denominator as a total.
pub const MAX_EXECUTION_TOPOLOGY_EVENTS_V1: u32 = 20_000;

/// Independently adjudicated eligible cases a conflict kind needs before
/// precision or recall is rendered at all.
pub const CONFLICT_MIN_ADJUDICATED_CASES_V1: u64 = 50;

/// Eligible cases merge-success, rerun rate, and ready-to-integrated latency
/// need before a rate or distribution is rendered.
pub const RATE_MIN_ELIGIBLE_CASES_V1: u64 = 20;

/// Minimum observed-over-eligible ratio any rate or distribution requires.
pub const MIN_COVERAGE_RATIO_V1: f64 = 0.9;

/// Maximum censored-over-eligible ratio conflict precision and recall admit.
pub const MAX_CENSORING_RATIO_V1: f64 = 0.1;

/// Maximum grouping dimensions any single measurement may carry.
pub const MAX_METRIC_DIMENSIONS_V1: usize = 8;

macro_rules! mirrored_enum {
    (
        $(#[$outer:meta])*
        $name:ident from $domain:ident { $($variant:ident),+ $(,)? }
    ) => {
        $(#[$outer])*
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl From<$domain> for $name {
            fn from(value: $domain) -> Self {
                match value {
                    $($domain::$variant => Self::$variant),+
                }
            }
        }
    };
}

macro_rules! projection_enum {
    (
        $(#[$outer:meta])*
        $name:ident { $($variant:ident),+ $(,)? }
    ) => {
        $(#[$outer])*
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
    };
}

projection_enum!(
    /// Concurrency-width phase. `Useful` counts only distinct admitted
    /// attempts that advanced a committed progress frontier; heartbeats,
    /// queued work, child processes, and transport fanout never reach it.
    ExecutionConcurrencyPhaseV1 {
        Requested,
        Accepted,
        Admitted,
        Active,
        Useful,
    }
);

projection_enum!(
    /// Fan-out width phase. `PeakActive` is the sampled active width; the
    /// fan-out distribution is unweighted so serialized and blocked samples,
    /// which carry no interval, are preserved rather than dropped.
    ExecutionFanoutPhaseV1 {
        Requested,
        Accepted,
        Admitted,
        PeakActive,
        Useful,
    }
);

projection_enum!(
    /// Fixed width buckets. Raw widths stay authorized local detail; only
    /// bucket counts leave the projection.
    ExecutionWidthBucketV1 {
        Zero,
        One,
        Two,
        From3To4,
        From5To8,
        From9To16,
        From17To32,
        From33To64,
        Over64,
    }
);

mirrored_enum!(
    /// Fixed duration buckets shared by ready-to-integrated latency, stale
    /// stack age, blocked time, and rerun latency. Raw timestamps and exact
    /// durations stay authorized local detail.
    ExecutionDurationBucketV1 from DurationBucketV1 {
        Under1m,
        From1mTo5m,
        From5mTo15m,
        From15mTo1h,
        From1hTo4h,
        From4hTo24h,
        From1dTo7d,
        Over7d,
    }
);

projection_enum!(
    /// Quantity unit for duplicate-effort accounting. Each unit is reported
    /// separately: wall time, tokens, cost, tests, and effects are never
    /// summed into one number.
    ExecutionQuantityUnitV1 {
        WallMicros,
        Tokens,
        CostMicros,
        Tests,
        Effects,
    }
);

projection_enum!(
    /// Per-surface delivery outcome. A multi-surface delivery is never a
    /// duplicate of product work; only `Deduplicated` is.
    ExecutionDeliveryOutcomeV1 {
        Delivered,
        Deduplicated,
        Dropped,
        Unknown,
    }
);

mirrored_enum!(
    /// Adjudicated duplicate-work relation. Similarity, proximity, shared
    /// paths, and concurrency never produce one of these.
    ExecutionDuplicateKindV1 from DuplicateEffortKindV1 {
        ExactDuplicate,
        SupersededOverlap,
        RepeatedInvestigation,
        DuplicateEffect,
        NotDuplicate,
        Censored,
        Unknown,
    }
);

mirrored_enum!(
    /// Whether a duplicate effect was prevented or actually committed. The
    /// two never collapse into one count.
    ExecutionDuplicateOutcomeV1 from DuplicateEffectOutcomeV1 {
        Prevented,
        Committed,
        Unknown,
        NotApplicable,
    }
);

mirrored_enum!(
    /// Conflict prediction kind. Mechanical and semantic keep separate
    /// denominators because their adjudicators are not interchangeable.
    ExecutionConflictKindV1 from ConflictKindV1 {
        Mechanical,
        Semantic,
        Combined,
    }
);

mirrored_enum!(
    /// Independently observed conflict outcome. `Censored` and `Unknown`
    /// never enter a confusion-matrix denominator.
    ExecutionConflictOutcomeV1 from ConflictOutcomeV1 {
        Conflict,
        NoConflict,
        Censored,
        Unknown,
    }
);

mirrored_enum!(
    /// Integration operation kind. Rebase remains external observation only.
    ExecutionIntegrationKindV1 from IntegrationOperationKindV1 {
        FastForward,
        MergeCommit,
        Rebase,
        CherryPick,
        StackRetarget,
        GraphOnly,
        ExternalObserved,
        Unknown,
    }
);

mirrored_enum!(
    /// Terminal result of an observed native integration.
    ExecutionIntegrationOutcomeV1 from IntegrationResultV1 {
        Succeeded,
        Conflicted,
        Rejected,
        Denied,
        Stale,
        Locked,
        Cancelled,
        TimedOut,
        Failed,
        Partial,
        EffectUnknown,
        Unsupported,
        Unknown,
    }
);

mirrored_enum!(
    /// Proved stack-invalidating observation.
    ExecutionDriftKindV1 from StackDriftKindV1 {
        HeadAdvanced,
        BaseAdvanced,
        MergeBaseChanged,
        DependencyMissing,
        RefDeleted,
        WorktreeGenerationChanged,
        Retargeted,
        Superseded,
        Unknown,
    }
);

mirrored_enum!(
    /// Whether a drift interval is still open or has a proved terminal.
    ExecutionIntervalStateV1 from IntervalStateV1 {
        Open,
        Closed,
    }
);

mirrored_enum!(
    /// Cause a work item was blocked for.
    ExecutionBlockedCauseV1 from BlockedCauseV1 {
        Dependency,
        NeedsInput,
        Capability,
        Policy,
        Scope,
        Conflict,
        Lease,
        Backpressure,
        Test,
        Ci,
        Review,
        EffectUnknown,
        Other,
        Unknown,
    }
);

mirrored_enum!(
    /// Which independent system observed the rerun.
    ExecutionRerunSourceV1 from RerunSourceV1 {
        Runtime,
        Test,
        Ci,
    }
);

mirrored_enum!(
    /// Typed rerun cause. Repeated logs and transport redelivery are not
    /// reruns and never carry one of these.
    ExecutionRerunCauseV1 from RerunCauseV1 {
        RuntimeRetry,
        RuntimeFallback,
        TestRerun,
        CiRerun,
        Recovery,
        HumanRequested,
        Unknown,
    }
);

mirrored_enum!(
    /// Independently proved execution-leak class.
    ExecutionLeakKindV1 from WorkExecutionLeakKindV1 {
        LeaseAfterTerminal,
        AttemptWithoutLiveOwner,
        EffectUnknownPastDeadline,
        MissingWorktreeBinding,
        UnboundedDelivery,
        None,
        Unknown,
    }
);

mirrored_enum!(
    /// Recovery state a proved leak reached.
    ExecutionLeakOutcomeV1 from WorkExecutionLeakRecoveryV1 {
        NotRequired,
        Pending,
        Recovered,
        Failed,
        Unknown,
    }
);

mirrored_enum!(
    /// Delivery surface family. Addresses, payloads, principals, and
    /// recipients are never observed, so the family is the whole label.
    ExecutionSurfaceFamilyV1 from DeliverySurfaceFamilyV1 {
        Hook,
        Mcp,
        Lsp,
        Dashboard,
        Cli,
        Other,
    }
);

/// One allowed local grouping dimension. The set is closed by construction:
/// every value is produced by an exhaustive match over a bounded domain class,
/// so no person, agent, task, project, repository, worktree, branch, ref,
/// commit, model version, or route can ever appear as a label.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "dimension", content = "value", rename_all = "snake_case")]
pub enum ExecutionTopologyDimensionV1 {
    ConcurrencyPhase(ExecutionConcurrencyPhaseV1),
    FanoutPhase(ExecutionFanoutPhaseV1),
    WidthBucket(ExecutionWidthBucketV1),
    DurationBucket(ExecutionDurationBucketV1),
    DuplicateKind(ExecutionDuplicateKindV1),
    Unit(ExecutionQuantityUnitV1),
    DuplicateOutcome(ExecutionDuplicateOutcomeV1),
    ConflictKind(ExecutionConflictKindV1),
    ConflictOutcome(ExecutionConflictOutcomeV1),
    IntegrationKind(ExecutionIntegrationKindV1),
    IntegrationOutcome(ExecutionIntegrationOutcomeV1),
    DriftKind(ExecutionDriftKindV1),
    IntervalState(ExecutionIntervalStateV1),
    BlockedCause(ExecutionBlockedCauseV1),
    RerunSource(ExecutionRerunSourceV1),
    RerunCause(ExecutionRerunCauseV1),
    LeakKind(ExecutionLeakKindV1),
    LeakOutcome(ExecutionLeakOutcomeV1),
    Surface(ExecutionSurfaceFamilyV1),
    DeliveryOutcome(ExecutionDeliveryOutcomeV1),
}

/// Why a measurement carries no value. Absence is always one of these typed
/// reasons; it is never an empty string, a zero, or a silently dropped row.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMetricUnavailableV1 {
    /// The observation store could not be read at all for this horizon.
    StoreUnavailable,
    /// The horizon holds more events than one read may draw, so every
    /// denominator here would be a partial count presented as a total.
    EventBudgetExceeded,
    /// No recorded event in the family supplies this metric's numerator or
    /// denominator.
    NoEligibleEvidence,
    /// Eligible cases exist but fall below the metric's support floor.
    SupportFloorUnmet,
    /// Observed cases cover less of the eligible population than the metric's
    /// coverage floor allows.
    CoverageFloorUnmet,
    /// More of the eligible population is censored than the metric admits.
    CensoringCeilingExceeded,
    /// The interval this metric integrates over has no proved upper bound, so
    /// its duration cannot be measured without inventing a terminal.
    UnboundedInterval,
}

impl ExecutionMetricUnavailableV1 {
    /// Canonical wire spelling, reused verbatim as the landed
    /// [`MetricValueV1::unavailable_reason`] so a transport that only reads
    /// the generic metric envelope sees the same typed reason.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StoreUnavailable => "store_unavailable",
            Self::EventBudgetExceeded => "event_budget_exceeded",
            Self::NoEligibleEvidence => "no_eligible_evidence",
            Self::SupportFloorUnmet => "support_floor_unmet",
            Self::CoverageFloorUnmet => "coverage_floor_unmet",
            Self::CensoringCeilingExceeded => "censoring_ceiling_exceeded",
            Self::UnboundedInterval => "unbounded_interval",
        }
    }
}

/// One descriptor cell: the Plan 26 descriptor name, its grouping dimensions,
/// and the landed metric envelope carrying value, denominator, coverage, and
/// provenance.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(title = "ExecutionTopologyMeasurementV1")]
pub struct ExecutionTopologyMeasurementV1 {
    /// Allowed local grouping dimensions, at most
    /// [`MAX_METRIC_DIMENSIONS_V1`], in a fixed order per descriptor.
    pub dimensions: Vec<ExecutionTopologyDimensionV1>,
    /// Typed absence reason. It is `Some` exactly when `value.value` is
    /// `None`, so a reader can never mistake a refused metric for a zero.
    pub unavailable: Option<ExecutionMetricUnavailableV1>,
    pub value: MetricValueV1,
}

/// The canonical execution-topology read model. Observatory and Costs render
/// this without local formulas; CLI, MCP, and HTTP return the same bytes.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[schemars(title = "ExecutionTopologyMetricsV1")]
pub struct ExecutionTopologyMetricsV1 {
    /// Local authorization anchor the read was admitted under. It is derived
    /// from the resolved scope, never from a caller-supplied string.
    pub authorized_scope_ref: String,
    pub horizon: ObservabilityHorizonV1,
    /// Watermark of the last event this projection consumed. A rebuild at the
    /// same watermark yields the same values.
    pub watermark: String,
    pub observed_at_micros: i64,
    /// True only when the whole family was read with `Known` coverage. A
    /// false value means at least one descriptor below is a typed absence.
    pub current: bool,
    /// Family-level coverage over the event population, independent of any
    /// single descriptor's denominator.
    pub coverage: MetricCoverageV1,
    pub measurements: Vec<ExecutionTopologyMeasurementV1>,
}

/// One horizon-bounded execution-topology metrics read. The authorized scope
/// is taken from the request context, not from the request, so a caller can
/// never widen the population it reads.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "ExecutionTopologyMetricsRequestV1")]
pub struct ExecutionTopologyMetricsRequestV1 {
    pub horizon: ObservabilityHorizonV1,
    /// Event budget for this read, at most
    /// [`MAX_EXECUTION_TOPOLOGY_EVENTS_V1`].
    pub max_events: u32,
}

impl ExecutionQuantityUnitV1 {
    const fn wire_unit(self) -> &'static str {
        match self {
            Self::WallMicros => "microseconds",
            Self::Tokens => "tokens",
            Self::CostMicros => "cost_micros",
            Self::Tests => "tests",
            Self::Effects => "effects",
        }
    }
}

const ALL_WIDTH_BUCKETS_V1: [ExecutionWidthBucketV1; 9] = [
    ExecutionWidthBucketV1::Zero,
    ExecutionWidthBucketV1::One,
    ExecutionWidthBucketV1::Two,
    ExecutionWidthBucketV1::From3To4,
    ExecutionWidthBucketV1::From5To8,
    ExecutionWidthBucketV1::From9To16,
    ExecutionWidthBucketV1::From17To32,
    ExecutionWidthBucketV1::From33To64,
    ExecutionWidthBucketV1::Over64,
];

const ALL_DURATION_BUCKETS_V1: [ExecutionDurationBucketV1; 8] = [
    ExecutionDurationBucketV1::Under1m,
    ExecutionDurationBucketV1::From1mTo5m,
    ExecutionDurationBucketV1::From5mTo15m,
    ExecutionDurationBucketV1::From15mTo1h,
    ExecutionDurationBucketV1::From1hTo4h,
    ExecutionDurationBucketV1::From4hTo24h,
    ExecutionDurationBucketV1::From1dTo7d,
    ExecutionDurationBucketV1::Over7d,
];

const ALL_QUANTITY_UNITS_V1: [ExecutionQuantityUnitV1; 5] = [
    ExecutionQuantityUnitV1::WallMicros,
    ExecutionQuantityUnitV1::Tokens,
    ExecutionQuantityUnitV1::CostMicros,
    ExecutionQuantityUnitV1::Tests,
    ExecutionQuantityUnitV1::Effects,
];

/// Fixed width buckets. The boundaries are contract, not tuning: a changed
/// boundary changes the projector revision.
#[must_use]
pub const fn width_bucket(width: u16) -> ExecutionWidthBucketV1 {
    match width {
        0 => ExecutionWidthBucketV1::Zero,
        1 => ExecutionWidthBucketV1::One,
        2 => ExecutionWidthBucketV1::Two,
        3..=4 => ExecutionWidthBucketV1::From3To4,
        5..=8 => ExecutionWidthBucketV1::From5To8,
        9..=16 => ExecutionWidthBucketV1::From9To16,
        17..=32 => ExecutionWidthBucketV1::From17To32,
        33..=64 => ExecutionWidthBucketV1::From33To64,
        _ => ExecutionWidthBucketV1::Over64,
    }
}

/// Fixed duration buckets over an exact measured microsecond span.
#[must_use]
pub const fn duration_bucket(micros: u64) -> ExecutionDurationBucketV1 {
    const MINUTE: u64 = 60_000_000;
    if micros < MINUTE {
        ExecutionDurationBucketV1::Under1m
    } else if micros < 5 * MINUTE {
        ExecutionDurationBucketV1::From1mTo5m
    } else if micros < 15 * MINUTE {
        ExecutionDurationBucketV1::From5mTo15m
    } else if micros < 60 * MINUTE {
        ExecutionDurationBucketV1::From15mTo1h
    } else if micros < 240 * MINUTE {
        ExecutionDurationBucketV1::From1hTo4h
    } else if micros < 1_440 * MINUTE {
        ExecutionDurationBucketV1::From4hTo24h
    } else if micros < 10_080 * MINUTE {
        ExecutionDurationBucketV1::From1dTo7d
    } else {
        ExecutionDurationBucketV1::Over7d
    }
}

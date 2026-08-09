//! Provider-routing facts declared by the configuration authority.
//!
//! These contracts describe a configured route; the policy crate only ranks
//! the facts supplied here and never discovers a provider or a model itself.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Ordinal band. Never a probability, never a scalar score.
///
/// Bands are ordered `Lowest` .. `Highest`. Comparison is the only operation
/// consumers perform over them, so no weighted sum can be reconstructed from
/// a recorded decision.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkOrdinalBandV1 {
    Lowest,
    Low,
    Moderate,
    High,
    Highest,
}

impl WorkOrdinalBandV1 {
    /// Widen one band toward `Highest`, saturating.
    pub const fn widened(self) -> Self {
        match self {
            Self::Lowest => Self::Low,
            Self::Low => Self::Moderate,
            Self::Moderate => Self::High,
            Self::High | Self::Highest => Self::Highest,
        }
    }

    /// Mirror a coverage band onto the uncertainty scale.
    pub const fn inverted(self) -> Self {
        match self {
            Self::Lowest => Self::Highest,
            Self::Low => Self::High,
            Self::Moderate => Self::Moderate,
            Self::High => Self::Low,
            Self::Highest => Self::Lowest,
        }
    }
}

/// Where a configured route places task content.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkContentLocationClassV1 {
    Local,
    Tenant,
    External,
}

/// Declared effort class of a configured route.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkEffortClassV1 {
    Minimal,
    Standard,
    Extended,
}

/// One eligible route supplied by the authorized configuration snapshot.
///
/// The application filters these candidates by the current request grant and
/// verifies the exact pinned executable before policy evaluates them.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRouteCandidateV1 {
    pub route_id: String,
    pub provider_capability_id: String,
    pub model_id: String,
    pub effort: WorkEffortClassV1,
    pub declared_budget_ceiling: u64,
    pub content_location: WorkContentLocationClassV1,
    pub correctness: WorkOrdinalBandV1,
    pub sensitive_data_fitness: WorkOrdinalBandV1,
    pub latency: WorkOrdinalBandV1,
    pub cost: WorkOrdinalBandV1,
    pub autonomy: WorkOrdinalBandV1,
    pub evidence_quality: WorkOrdinalBandV1,
}

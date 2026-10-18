use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Durable classification of one GitHub stack transition.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum StackSignalKindV1 {
    DependencyReady,
    ActualConflict,
    StackTipDrift,
    PullRequestDrift,
    CiEvaluatedCommitDrift,
    IntegrationCommitted,
    IntegrationNeedsInspection,
}

impl StackSignalKindV1 {
    pub const fn debounce_micros(self) -> i64 {
        match self {
            Self::DependencyReady => 250_000,
            Self::StackTipDrift | Self::PullRequestDrift | Self::CiEvaluatedCommitDrift => {
                1_000_000
            }
            _ => 0,
        }
    }

    pub const fn is_material(self) -> bool {
        matches!(
            self,
            Self::ActualConflict | Self::IntegrationCommitted | Self::IntegrationNeedsInspection
        )
    }
}

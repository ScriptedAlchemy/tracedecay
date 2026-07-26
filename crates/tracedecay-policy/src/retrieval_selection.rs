//! Pure retrieval selection for the production semantic query journey.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalAvailabilityV1 {
    Ready,
    Unavailable,
    Indexing,
    Degraded,
    Failed,
    Stale,
    Incompatible,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalRequirementV1 {
    FallbackAllowed,
    StrictSemantic,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSelectionV1 {
    Semantic,
    FrozenFallback,
    Unavailable,
}

/// Chooses between the atomically-current semantic lane and the already
/// authorized frozen fallback. It never constructs either lane.
pub const fn select_retrieval(
    availability: RetrievalAvailabilityV1,
    requirement: RetrievalRequirementV1,
) -> RetrievalSelectionV1 {
    if matches!(availability, RetrievalAvailabilityV1::Ready) {
        RetrievalSelectionV1::Semantic
    } else if matches!(requirement, RetrievalRequirementV1::FallbackAllowed) {
        RetrievalSelectionV1::FrozenFallback
    } else {
        RetrievalSelectionV1::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_retrieval_fails_closed() {
        assert_eq!(
            select_retrieval(
                RetrievalAvailabilityV1::Indexing,
                RetrievalRequirementV1::StrictSemantic,
            ),
            RetrievalSelectionV1::Unavailable
        );
    }
}

//! Exact resource-scale and projection-case requirements for native evidence.

use super::SemanticProjectionCaseV1;

pub(super) const REQUIRED_RESOURCE_SCALES: [&str; 2] = ["current", "10x"];
pub(super) const REQUIRED_PROJECTION_CASES: [SemanticProjectionCaseV1; 7] = [
    SemanticProjectionCaseV1::Clean,
    SemanticProjectionCaseV1::OneSymbol,
    SemanticProjectionCaseV1::Deletion,
    SemanticProjectionCaseV1::NoOp,
    SemanticProjectionCaseV1::IdempotencyReplay,
    SemanticProjectionCaseV1::Cancellation,
    SemanticProjectionCaseV1::IncompatibleState,
];

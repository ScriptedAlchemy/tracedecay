//! Transport-neutral operation subscription and cancellation contracts.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{DisclosureClass, RequestId, StreamFrontier};

/// Idempotent outcome of an explicit operation cancellation request.
#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationCancelOutcome {
    Requested,
    AlreadyRequested,
    AlreadyTerminal,
}

/// Stable operation identity derived from the originating authorized request.
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct OperationId(RequestId);

impl OperationId {
    pub fn from_request(request_id: RequestId) -> Self {
        Self(request_id)
    }

    pub fn request_id(&self) -> &RequestId {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

/// Closed operation names prevent lifecycle metadata from becoming an
/// arbitrary payload side channel.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    GitPreview,
    GitApply,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    TestRun,
}

/// The only item payload published by the lifecycle stream. Progress, gaps,
/// and terminal receipts use the canonical stream event variants.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum OperationEventItem {
    Accepted {
        operation_id: OperationId,
        originating_request_id: RequestId,
        operation: OperationKind,
        content_class: DisclosureClass,
    },
    TestRunResult {
        test: String,
        passed: bool,
    },
}

/// Stable subscription identity, initial replay frontier, and owner stream.
///
/// The stream remains generic so application contracts do not erase canonical
/// event items into transport payloads or depend on an async runtime.
pub struct OperationEventSubscription<S> {
    correlation_id: RequestId,
    frontier: StreamFrontier,
    stream: S,
}

impl<S> OperationEventSubscription<S> {
    pub fn new(correlation_id: RequestId, frontier: StreamFrontier, stream: S) -> Self {
        Self {
            correlation_id,
            frontier,
            stream,
        }
    }

    pub fn correlation_id(&self) -> &RequestId {
        &self.correlation_id
    }

    pub fn frontier(&self) -> &StreamFrontier {
        &self.frontier
    }

    pub fn into_parts(self) -> (RequestId, StreamFrontier, S) {
        (self.correlation_id, self.frontier, self.stream)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{OperationCancelOutcome, OperationEventSubscription};
    use crate::{RequestId, ResumeToken, StreamFrontier};

    #[test]
    fn subscription_parts_preserve_the_canonical_correlation_and_frontier() {
        let correlation_id = RequestId::new("request.operation.correlation").expect("request");
        let frontier = StreamFrontier {
            next_sequence: 9,
            retained_from_sequence: 4,
            resume_token: Some(ResumeToken::new("resume.operation").expect("resume token")),
        };
        let subscription =
            OperationEventSubscription::new(correlation_id.clone(), frontier.clone(), "stream");

        assert_eq!(subscription.correlation_id(), &correlation_id);
        assert_eq!(subscription.frontier(), &frontier);
        assert_eq!(
            subscription.into_parts(),
            (correlation_id, frontier, "stream")
        );
    }

    #[test]
    fn cancellation_outcomes_have_one_canonical_wire_vocabulary() {
        for (outcome, expected) in [
            (OperationCancelOutcome::Requested, json!("requested")),
            (
                OperationCancelOutcome::AlreadyRequested,
                json!("already_requested"),
            ),
            (
                OperationCancelOutcome::AlreadyTerminal,
                json!("already_terminal"),
            ),
        ] {
            assert_eq!(serde_json::to_value(outcome).expect("outcome"), expected);
        }
    }
}

use serde::Serialize;

/// Outcome of admitting one hook envelope into the registered orchestrator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOrchestrationAdmissionV1 {
    Enqueued,
    Backpressured,
    UnsupportedTrigger,
    Unavailable,
}

use serde::{Deserialize, Serialize};

use super::ObservabilityTerminalResultV1;

/// Whether the one dispatch deadline expired before the terminal response.
///
/// This is a closed state rather than a deadline timestamp so observability
/// never retains request-specific deadline values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDispatchDeadlineV1 {
    Enforced,
    Expired,
}

/// Origin of terminal dispatch cancellation, when one occurred.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDispatchCancellationV1 {
    NotRequested,
    CallerRequested,
    DeadlineTriggered,
    ShutdownTriggered,
}

/// Terminal classification for one MCP tool dispatch.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpDispatchTerminalV1 {
    Completed,
    Denied,
    Unavailable,
    Failed,
    TimedOut,
    Cancelled,
    Shutdown,
}

/// Fixed-shape, content-free timing and terminal receipt for one MCP tool
/// dispatch. Tool names, arguments, routes, request ids, and stage names are
/// intentionally absent: their cardinality or content is not telemetry-safe.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpDispatchObservedV1 {
    pub route_admission_micros: u64,
    pub handler_micros: u64,
    pub result_materialization_micros: u64,
    pub total_micros: u64,
    pub deadline: McpDispatchDeadlineV1,
    pub cancellation: McpDispatchCancellationV1,
    pub terminal: McpDispatchTerminalV1,
}

impl McpDispatchObservedV1 {
    pub const fn terminal_result(&self) -> ObservabilityTerminalResultV1 {
        match self.terminal {
            McpDispatchTerminalV1::Completed => ObservabilityTerminalResultV1::Succeeded,
            McpDispatchTerminalV1::Denied => ObservabilityTerminalResultV1::Denied,
            // The fixed terminal payload preserves that this was unavailable;
            // the envelope's existing abstention state means no work result
            // was claimed.
            McpDispatchTerminalV1::Unavailable => ObservabilityTerminalResultV1::Abstained,
            McpDispatchTerminalV1::Failed => ObservabilityTerminalResultV1::Failed,
            McpDispatchTerminalV1::TimedOut => ObservabilityTerminalResultV1::TimedOut,
            McpDispatchTerminalV1::Cancelled => ObservabilityTerminalResultV1::Cancelled,
            McpDispatchTerminalV1::Shutdown => ObservabilityTerminalResultV1::Cancelled,
        }
    }

    pub fn validate(
        &self,
        envelope_terminal: Option<ObservabilityTerminalResultV1>,
    ) -> Result<(), &'static str> {
        if self.total_micros
            < self
                .route_admission_micros
                .saturating_add(self.handler_micros)
                .saturating_add(self.result_materialization_micros)
        {
            return Err("mcp_dispatch_timings");
        }
        if envelope_terminal != Some(self.terminal_result()) {
            return Err("mcp_dispatch_terminal");
        }
        match (self.deadline, self.cancellation, self.terminal) {
            (
                McpDispatchDeadlineV1::Enforced,
                McpDispatchCancellationV1::NotRequested,
                McpDispatchTerminalV1::Completed
                | McpDispatchTerminalV1::Denied
                | McpDispatchTerminalV1::Unavailable
                | McpDispatchTerminalV1::Failed,
            )
            | (
                McpDispatchDeadlineV1::Enforced,
                McpDispatchCancellationV1::CallerRequested,
                McpDispatchTerminalV1::Cancelled,
            )
            | (
                McpDispatchDeadlineV1::Enforced,
                McpDispatchCancellationV1::ShutdownTriggered,
                McpDispatchTerminalV1::Shutdown,
            )
            | (
                McpDispatchDeadlineV1::Expired,
                McpDispatchCancellationV1::DeadlineTriggered,
                McpDispatchTerminalV1::TimedOut,
            ) => Ok(()),
            _ => Err("mcp_dispatch_control"),
        }
    }
}

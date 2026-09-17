//! The universal per-call dispatch ceiling every MCP tool runs under.
//!
//! The composition root wraps each dispatched tool call in this bound; the
//! binding table consults the same ceiling when it projects a catalog
//! contract without an explicit deadline.

use tracedecay_domain::errors::TraceDecayError;

/// The hard ceiling every MCP tool call is bounded by, regardless of dispatch
/// group, when admission carried no client deadline.
///
/// Principle 6 of `docs/SERVING-PATH-PERFORMANCE.md`: deadlines bound failure,
/// not work. Before this existed only the git and memory groups were wrapped,
/// so `dispatch_deadline_horizon_micros` returning `None` for a graph tool meant
/// `tracedecay_context` dispatched with no bound at all — a live Codex call once
/// hung for 900 seconds against a daemon grinding a failing publish loop, and
/// only the client's own timeout ended it. A firing ceiling is always a bug
/// somewhere above it; the fix is that bug, never a larger ceiling.
pub const TOOL_DISPATCH_CEILING: std::time::Duration = std::time::Duration::from_mins(2);

/// The ceiling for the few tools whose *requested work* is itself a long job —
/// running a test suite, an admin index/sync — rather than an interactive read.
///
/// These are still bounded: nothing may run unbounded, and nothing may reach the
/// 900 seconds that motivated this wrap. They simply cannot share the
/// interactive ceiling without failing correct, user-requested work.
pub const LONG_RUNNING_TOOL_DISPATCH_CEILING: std::time::Duration =
    std::time::Duration::from_mins(10);

/// Tools whose ceiling is [`LONG_RUNNING_TOOL_DISPATCH_CEILING`].
///
/// Deliberately tiny and explicit: membership is a statement that the tool's
/// duration is the caller's own job, not a serving-path stall. Everything not
/// listed here — every graph, info, analysis, health, session, and memory read —
/// inherits [`TOOL_DISPATCH_CEILING`] automatically, so a tool added tomorrow is
/// bounded without touching this file.
const LONG_RUNNING_DISPATCH_TOOLS: &[&str] = &[
    "tracedecay_run_affected_tests",
    "tracedecay_fact_store_curate",
    "tracedecay_admin_cli",
    "tracedecay_admin_project",
    "tracedecay_admin_sync",
    "tracedecay_admin_branch_add",
];

/// The ceiling that applies to `tool_name` in the absence of a shorter carried
/// deadline.
pub fn tool_dispatch_ceiling(tool_name: &str) -> std::time::Duration {
    if LONG_RUNNING_DISPATCH_TOOLS.contains(&tool_name) {
        LONG_RUNNING_TOOL_DISPATCH_CEILING
    } else {
        TOOL_DISPATCH_CEILING
    }
}

/// The bound one tool call dispatches under: the admission-carried client
/// deadline when it is present and shorter, otherwise the tool's own ceiling.
///
/// `None` means the carried deadline has already elapsed, which must be
/// rejected rather than dispatched — the same rule the git and memory wraps
/// apply to a non-positive budget.
pub fn tool_dispatch_budget(
    tool_name: &str,
    deadline: Option<&tracedecay_contracts::Deadline>,
) -> Option<std::time::Duration> {
    let ceiling = tool_dispatch_ceiling(tool_name);
    match deadline {
        // A carried deadline is preferred whenever it is shorter; the ceiling
        // still clamps a pathologically distant one so it can never be a way
        // out of the bound.
        Some(deadline) => tracedecay_daemon_protocol::deadline_remaining(deadline)
            .map(|remaining| remaining.min(ceiling)),
        None => Some(ceiling),
    }
}

/// The typed, retryable problem a tool call reports when it exhausts the
/// universal dispatch ceiling.
///
/// Its stable `reason_code`, retryability bit, and human detail let the MCP
/// boundary surface a structured error instead of holding the transport open.
/// Retry is safe: the ceiling is a
/// backstop over work that was already admitted, never a commit signal.
pub fn tool_dispatch_deadline_error(
    tool_name: &str,
    budget: std::time::Duration,
) -> TraceDecayError {
    // A firing ceiling is a defect signal upstream; count every occurrence so
    // profiling sees the refusals, not only the successful dispatches.
    hotpath::gauge!("mcp.tool_call.dispatch_deadline_total").inc(1_u64);
    TraceDecayError::project_route(
        "tool_dispatch_deadline_exceeded",
        true,
        format!(
            "tool '{tool_name}' exceeded its {}s dispatch ceiling and was cancelled",
            budget.as_secs()
        ),
    )
}

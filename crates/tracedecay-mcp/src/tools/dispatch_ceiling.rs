//! The universal per-call dispatch ceiling every MCP tool runs under.
//!
//! The composition root wraps each dispatched tool call in this bound; the
//! binding table consults the same ceiling when it projects a catalog
//! contract without an explicit deadline.

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

/// The hard ceiling every MCP tool call is bounded by, regardless of dispatch
/// group, when admission carried no client deadline.
///
/// Principle 6 of `docs/SERVING-PATH-PERFORMANCE.md`: deadlines bound failure,
/// not work. Before this existed only the git and memory groups were wrapped,
/// so `dispatch_deadline_horizon_micros` returning `None` for a graph tool meant
/// `tracedecay_context` dispatched with no bound at all, a live Codex call once
/// hung for 900 seconds against a daemon grinding a failing publish loop, and
/// only the client's own timeout ended it. A firing ceiling is always a bug
/// somewhere above it; the fix is that bug, never a larger ceiling.
pub const TOOL_DISPATCH_CEILING: std::time::Duration = std::time::Duration::from_mins(2);

/// The ceiling for the few tools whose *requested work* is itself a long job,
/// running a test suite, an admin index/sync, rather than an interactive read.
///
/// These are still bounded: nothing may run unbounded, and nothing may reach the
/// 900 seconds that motivated this wrap. They simply cannot share the
/// interactive ceiling without failing correct, user-requested work.
pub const LONG_RUNNING_TOOL_DISPATCH_CEILING: std::time::Duration =
    std::time::Duration::from_millis(tracedecay_tool_catalog::LONG_RUNNING_CEILING_MILLIS);

/// Tools whose ceiling is [`LONG_RUNNING_TOOL_DISPATCH_CEILING`].
///
/// Deliberately tiny and explicit: membership is a statement that the tool's
/// duration is the caller's own job, not a serving-path stall. Everything not
/// listed here, every graph, info, analysis, health, session, and memory read,
/// inherits [`TOOL_DISPATCH_CEILING`] automatically, so a tool added tomorrow is
/// bounded without touching this file.
const LONG_RUNNING_DISPATCH_TOOLS: &[&str] = &[
    "tracedecay_fact_store_curate",
    "tracedecay_admin_branch_add",
];

/// The ceiling that applies to `tool_name` in the absence of a shorter carried
/// deadline.
///
/// An owner-served side effect's tool-catalog entry names its own ceiling.
pub fn tool_dispatch_ceiling(tool_name: &str) -> std::time::Duration {
    if let Some(entry) = ApplicationSurfaceOperation::from_tool_name(tool_name)
        .and_then(ApplicationSurfaceOperation::owner_side_effect)
    {
        return std::time::Duration::from_millis(entry.ceiling_millis);
    }
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
/// rejected rather than dispatched, the same rule the git and memory wraps
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
    TraceDecayError::project_route(
        "tool_dispatch_deadline_exceeded",
        true,
        format!(
            "tool '{tool_name}' exceeded its {}s dispatch ceiling and was cancelled",
            budget.as_secs()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        LONG_RUNNING_TOOL_DISPATCH_CEILING, TOOL_DISPATCH_CEILING, tool_dispatch_budget,
        tool_dispatch_ceiling, tool_dispatch_deadline_error,
    };

    /// One tool from every dispatch group. `tracedecay_context` is listed first
    /// because it is the one that actually hung: it is neither an
    /// application-surface operation nor a controlled read, so
    /// `dispatch_deadline_horizon_micros` returned `None` for it and it reached its
    /// handler with no bound at all.
    const DISPATCH_GROUP_SPOT_CHECKS: &[&str] = &[
        "tracedecay_context",
        "tracedecay_search",
        "tracedecay_callers",
        "tracedecay_status",
        "tracedecay_files",
        "tracedecay_dead_code",
        "tracedecay_complexity",
        "tracedecay_health",
        "tracedecay_test_map",
        "tracedecay_pr_context",
        "tracedecay_affected",
        "tracedecay_analytics",
        "tracedecay_skill_list",
        "tracedecay_dashboard",
        "tracedecay_diagnose",
        "tracedecay_hook_runtime",
        "tracedecay_str_replace",
        "tracedecay_fact_store_search",
    ];

    /// Absent a carried deadline every tool still dispatches under a bound. Before
    /// the universal ceiling only the git and memory groups were wrapped, so most
    /// of these names had no ceiling of any kind.
    #[test]
    fn every_dispatch_group_has_a_ceiling_without_a_carried_deadline() {
        for tool_name in DISPATCH_GROUP_SPOT_CHECKS {
            let budget = tool_dispatch_budget(tool_name, None)
                .expect("a tool with no carried deadline still dispatches under its ceiling");
            assert_eq!(
                budget,
                tool_dispatch_ceiling(tool_name),
                "{tool_name} must inherit the universal ceiling",
            );
            assert!(
                budget <= LONG_RUNNING_TOOL_DISPATCH_CEILING,
                "{tool_name} must never dispatch near the 900s client hang this replaced",
            );
        }
    }

    /// The interactive ceiling is the default; only the explicitly listed
    /// long-running jobs get the larger one, and even those stay bounded.
    #[test]
    fn long_running_tools_are_bounded_above_the_interactive_ceiling() {
        assert_eq!(
            tool_dispatch_ceiling("tracedecay_context"),
            TOOL_DISPATCH_CEILING,
        );
        assert_eq!(
            tool_dispatch_ceiling("tracedecay_run_affected_tests"),
            LONG_RUNNING_TOOL_DISPATCH_CEILING,
        );
        assert!(
            TOOL_DISPATCH_CEILING < LONG_RUNNING_TOOL_DISPATCH_CEILING,
            "the interactive ceiling must be the tighter of the two",
        );
        assert!(
            LONG_RUNNING_TOOL_DISPATCH_CEILING < std::time::Duration::from_mins(15),
            "nothing may ever run 900 seconds",
        );
    }

    /// An already-elapsed carried deadline is rejected rather than dispatched, for
    /// every group, the same rule the git and memory wraps already applied.
    #[test]
    fn an_elapsed_carried_deadline_is_rejected_for_every_group() {
        let elapsed =
            tracedecay_contracts::Deadline::new(tracedecay_domain::UtcMicros(1)).expect("deadline");
        for tool_name in DISPATCH_GROUP_SPOT_CHECKS {
            assert!(
                tool_dispatch_budget(tool_name, Some(&elapsed)).is_none(),
                "{tool_name} must refuse to dispatch under an elapsed deadline",
            );
        }
    }

    /// The ceiling reports the same typed, retryable problem shape the memory wrap
    /// established, so the MCP boundary surfaces structure instead of a hang.
    #[test]
    fn the_ceiling_reports_a_typed_retryable_problem() {
        let error = tool_dispatch_deadline_error("tracedecay_context", TOOL_DISPATCH_CEILING);
        let (reason_code, retryable, detail) = error
            .project_route_context()
            .expect("the ceiling must surface a typed project-route problem, not an opaque error");
        assert_eq!(
            reason_code, "tool_dispatch_deadline_exceeded",
            "the ceiling must keep its own reason code so callers can branch on it",
        );
        assert!(
            retryable,
            "a dispatch-ceiling cancellation is retryable: nothing was committed",
        );
        // The typed fields carry the shape; only the detail names the subject, so
        // these two checks assert information the typed fields cannot express.
        assert!(
            detail.contains("tracedecay_context"),
            "the problem must name the tool it cancelled, got {detail:?}",
        );
        assert!(
            detail.contains(&format!("{}s", TOOL_DISPATCH_CEILING.as_secs())),
            "the problem must name the budget it enforced, got {detail:?}",
        );
    }
}

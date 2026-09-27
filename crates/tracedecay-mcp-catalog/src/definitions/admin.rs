//! Project registry, runtime, and automation admin tool definitions.

use serde_json::Value;

use super::{def, def_always_load, def_rw};
use crate::ToolDefinition;

pub(super) fn def_status(input_schema: Value) -> ToolDefinition {
    def_always_load(
        "tracedecay_status",
        "Graph Status",
        "Return a compact summary of the code graph (counts and freshness). Full branch diagnostics are opt-in.",
        input_schema,
    )
}

pub(super) fn def_active_project(input_schema: Value) -> ToolDefinition {
    def_always_load(
        "tracedecay_active_project",
        "Active Project",
        "Return the resolved active project context for this MCP session, including project ID, project root, scope prefix, branch identity, and the active project store paths. Use this instead of guessing from repo-local marker files or hardcoded DB paths.",
        input_schema,
    )
}

pub(super) fn def_project_list(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_project_list",
        "Project List",
        "List projects from the profile/global registry without opening or mutating their stores. Results are bounded and include only registry metadata. Output is grouped into a `project_tree` by repository alongside a `summary`, and the calling project is marked with `is_active` when it is registered.",
        input_schema,
    )
}

pub(super) fn def_project_search(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_project_search",
        "Project Search",
        "Search registered projects by project id, root path, aliases, or default branch. This is read-only and bounded; output omits credential-bearing remotes. Output is grouped into a `project_tree` by repository alongside a `summary`, and the calling project is marked with `is_active` when it is registered.",
        input_schema,
    )
}

pub(super) fn def_project_context(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_project_context",
        "Project Context",
        "Return registry context for one project: project metadata, aliases, store instances, graph scopes, and artifacts. Defaults to the active project alias when neither project_selector nor path is provided.",
        input_schema,
    )
}

pub(super) fn def_runtime(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_runtime",
        "Runtime Snapshot",
        "Capture a process + database telemetry snapshot for the running tracedecay MCP server: PID, resident memory, virtual size, sustained CPU% (sampled over ~200ms), thread count, system memory, DB / WAL / SHM file sizes, journal mode, and the DB-to-source byte ratio. Use this when triaging unexpected CPU or RAM consumption (issue #80). Set authority_audit=true only for exhaustive Doctor-style observation-authority validation. Single call, output is a JSON object.",
        input_schema,
    )
}

pub(super) fn def_dashboard(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_dashboard",
        "Dashboard",
        "Start (or manage) the tracedecay dashboard server for the current project as a background task inside the MCP server. Returns the listening URL. Idempotent: if already running, returns the existing URL. Pass action:\"stop\" to shut down a running instance. MCP dashboard binds are loopback-only: optional host must be 127.0.0.1, localhost, or ::1. Port is optional.",
        input_schema,
    )
}

pub(super) fn def_analytics(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_analytics",
        "Usage Analytics",
        "Read-only adoption/telemetry rollup over the durable analytics_events table, the memory-fact funnel, and the automation run ledger. Answers 'what did the agent actually do' without querying .tracedecay databases directly: per-tool call/error counts grouped into tiers (navigation, analysis, session, memory, edit, admin), top-N tools by call volume, zero-call defined tools, hint emitted/followed/ignored/suppressed counts by category, the fact-store funnel (facts, retrievals, rated, helpful/unhelpful), and automation run outcomes (succeeded/failed/skipped) per job from the run ledger. Defaults to the active project over the last 14 days; pass scope:\"all\" for every registered project's analytics_events, or a project selector to inspect another registered project (fact/automation sections always report the resolved single project even in scope:\"all\").",
        input_schema,
    )
}

pub(super) fn def_automation_run_artifact_view(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_automation_run_artifact_view",
        "Automation Run Artifact View",
        "Read and hash-verify one durable automation run artifact payload from the active project's dashboard sidecar. Returns the run id, artifact metadata, and JSON payload without mutating automation state. Human/operator equivalents: `tracedecay automation runs artifact <run_id> <kind> --json` and `GET /api/automation/runs/{run_id}/artifacts/{kind}`.",
        input_schema,
    )
}

pub(super) fn def_automation_run_list(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_automation_run_list",
        "Automation Run List",
        "List the newest durable automation run ledger records for the active project without triggering or mutating automation. Results are newest-first, deduplicated by run id, bounded to 200 records, and report whether the returned page is complete or contains malformed ledger rows.",
        input_schema,
    )
}

pub(super) fn def_automation_run_view(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_automation_run_view",
        "Automation Run View",
        "Read one exact durable automation run ledger record from the active project by run id without triggering or mutating automation. A missing run returns a typed not-found error and does not enumerate other run ids.",
        input_schema,
    )
}

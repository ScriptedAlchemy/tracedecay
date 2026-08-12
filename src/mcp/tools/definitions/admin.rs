//! Project registry, runtime, and automation admin tool definitions.

use serde_json::json;
use tracedecay_agent_hosts::automation::runner::{
    CURATION_DEFAULT_FACT_REVIEW_LIMIT, CURATION_DEFAULT_MIN_CONFIDENCE,
};

use super::{def, def_always_load, def_rw, project_selector_object};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_status() -> ToolDefinition {
    def_always_load(
        "tracedecay_status",
        "Graph Status",
        "Return aggregate statistics about the code graph (node/edge/file counts, DB size, etc.).",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "include_branch_diagnostics": {
                    "type": "boolean",
                    "description": "Include the full tracked-branch diagnostic list (default: true). Disable for compact status consumers."
                }
            }
        }),
    )
}

pub(super) fn def_active_project() -> ToolDefinition {
    def_always_load(
        "tracedecay_active_project",
        "Active Project",
        "Return the resolved active project context for this MCP session, including project root, scope prefix, branch identity, and the active project store paths. Use this instead of guessing from repo-local marker files or hardcoded DB paths.",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

pub(super) fn def_project_list() -> ToolDefinition {
    def(
        "tracedecay_project_list",
        "Project List",
        "List projects from the profile/global registry without opening or mutating their stores. Results are bounded and include only registry metadata. Output is grouped into a `project_tree` by repository alongside a `summary`, and the calling project is marked with `is_active` when it is registered.",
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum projects to return (default: 25, max: 100)"
                }
            }
        }),
    )
}

pub(super) fn def_project_search() -> ToolDefinition {
    def(
        "tracedecay_project_search",
        "Project Search",
        "Search registered projects by project id, root path, aliases, or default branch. This is read-only and bounded; output omits credential-bearing remotes. Output is grouped into a `project_tree` by repository alongside a `summary`, and the calling project is marked with `is_active` when it is registered.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Case-insensitive substring query over registry project metadata"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum projects to return (default: 10, max: 50)"
                }
            },
            "required": ["query"]
        }),
    )
}

pub(super) fn def_project_context() -> ToolDefinition {
    def(
        "tracedecay_project_context",
        "Project Context",
        "Return registry context for one project: project metadata, aliases, store instances, graph scopes, and artifacts. Defaults to the active project alias when neither project_selector nor path is provided.",
        json!({
            "type": "object",
            "properties": {
                "project_selector": project_selector_object(
                    "Optional registered project id to inspect. Omit to use path or the active project."
                ),
                "path": {
                    "type": "string",
                    "description": "Project path or registered alias to resolve"
                }
            }
        }),
    )
}

pub(super) fn def_runtime() -> ToolDefinition {
    def(
        "tracedecay_runtime",
        "Runtime Snapshot",
        "Capture a process + database telemetry snapshot for the running tracedecay MCP server: PID, resident memory, virtual size, sustained CPU% (sampled over ~200ms), thread count, system memory, DB / WAL / SHM file sizes, journal mode, and the DB-to-source byte ratio. Use this when triaging unexpected CPU or RAM consumption (issue #80). Set authority_audit=true only for exhaustive Doctor-style observation-authority validation. Single call — output is a JSON object.",
        json!({
            "type": "object",
            "properties": {
                "authority_audit": {
                    "type": "boolean",
                    "description": "Run the exhaustive observation-authority audit and include authority_audit_ok (true = audit ran and passed, false = audit ran and failed, null = audit did not run), authority_audit_reason (typed: authority_invariant_failed, authority_store_unavailable, authority_store_missing, authority_audit_not_run), and authority_audit_error (observed detail) in database telemetry (default: false)"
                },
                "doctor_report": {
                    "type": "boolean",
                    "description": "Include the daemon-owned canonical Doctor report and typed per-table growth evidence (default: false)"
                },
                "session_ingest_health": {
                    "type": "boolean",
                    "description": "Include Cursor transcript-ingest health from the daemon-retained project session authority (default: false)"
                },
                "startup_health": {
                    "type": "boolean",
                    "description": "Return only daemon-mounted database integrity telemetry for post-update startup validation (default: false)"
                }
            }
        }),
    )
}

pub(super) fn def_dashboard() -> ToolDefinition {
    def_rw(
        "tracedecay_dashboard",
        "Dashboard",
        "Start (or manage) the tracedecay dashboard server for the current project as a background task inside the MCP server. Returns the listening URL. Idempotent: if already running, returns the existing URL. Pass action:\"stop\" to shut down a running instance. MCP dashboard binds are loopback-only: optional host must be 127.0.0.1, localhost, or ::1. Port is optional.",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "stop"],
                    "description": "Action to perform (default: \"start\"). \"stop\" shuts down a previously started dashboard if any."
                },
                "host": {
                    "type": "string",
                    "description": "Loopback host address to bind: 127.0.0.1, localhost, or ::1 (default: \"127.0.0.1\"). Wildcard, LAN, public IPs, and other hostnames are rejected."
                },
                "port": {
                    "type": "number",
                    "description": "Port to listen on; 0 picks an ephemeral port (default: 7341)"
                }
            }
        }),
    )
}

pub(super) fn def_analytics() -> ToolDefinition {
    def(
        "tracedecay_analytics",
        "Usage Analytics",
        "Read-only adoption/telemetry rollup over the durable analytics_events table, the memory-fact funnel, and the automation run ledger. Answers 'what did the agent actually do' without querying .tracedecay databases directly: per-tool call/error counts grouped into tiers (navigation, analysis, session, memory, edit, admin), top-N tools by call volume, zero-call defined tools, hint emitted/followed/ignored/suppressed counts by category, the fact-store funnel (facts, retrievals, rated, helpful/unhelpful), and automation run outcomes (succeeded/failed/skipped) per job from the run ledger. Defaults to the active project over the last 14 days; pass scope:\"all\" for every registered project's analytics_events, or a project selector to inspect another registered project (fact/automation sections always report the resolved single project even in scope:\"all\").",
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["project", "all"],
                    "description": "\"project\" (default) scopes analytics_events to the resolved project; \"all\" reports across every registered project."
                },
                "window_days": {
                    "type": "number",
                    "description": "Lookback window in days for events and automation runs (default: 14, clamped 1-365)."
                },
                "section": {
                    "type": "string",
                    "enum": ["tools", "hints", "facts", "automation"],
                    "description": "Optional filter to a single section. Omit to return all sections."
                },
                "project_selector": project_selector_object(
                    "Advanced optional registered project selector. Omit to use the active project."
                )
            }
        }),
    )
}

pub(super) fn def_automation_run_artifact_view() -> ToolDefinition {
    def(
        "tracedecay_automation_run_artifact_view",
        "Automation Run Artifact View",
        "Read and hash-verify one durable automation run artifact payload from the active project's dashboard sidecar. Returns the run id, artifact metadata, and JSON payload without mutating automation state. Human/operator equivalents: `tracedecay automation runs artifact <run_id> <kind> --json` and `GET /api/automation/runs/{run_id}/artifacts/{kind}`.",
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Automation run id to inspect."
                },
                "kind": {
                    "type": "string",
                    "description": "Artifact kind to read, such as traces, feedback, generated_evals, validation_gate, optimizer_diagnosis, or codex_handoff."
                }
            },
            "required": ["run_id", "kind"]
        }),
    )
}

pub(super) fn def_memory_automation_run() -> ToolDefinition {
    def_rw(
        "tracedecay_memory_automation_run",
        "Run Memory Curator",
        "Run the daemon-owned automatic Memory Curator for the active project. The daemon derives the run identity, task, operations, validation, and apply authority; callers may only bound the fact review and confidence floor. The terminal is recorded through the canonical durable MemoryAutomationRun authority and can be inspected with automation run list, view, and artifact view.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "fact_review_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1000,
                    "default": CURATION_DEFAULT_FACT_REVIEW_LIMIT,
                    "description": "Maximum canonical facts included in the automatic review."
                },
                "min_confidence": {
                    "type": "number",
                    "minimum": 0,
                    "maximum": 1,
                    "default": CURATION_DEFAULT_MIN_CONFIDENCE,
                    "description": "Confidence floor below which proposed curator operations are rejected."
                }
            }
        }),
    )
}

pub(super) fn def_automation_run_list() -> ToolDefinition {
    def(
        "tracedecay_automation_run_list",
        "Automation Run List",
        "List the newest durable automation run ledger records for the active project without triggering or mutating automation. Results are newest-first, deduplicated by run id, bounded to 200 records, and report whether the returned page is complete or contains malformed ledger rows.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Maximum run records to return (default: 50, max: 200)."
                }
            }
        }),
    )
}

pub(super) fn def_automation_run_view() -> ToolDefinition {
    def(
        "tracedecay_automation_run_view",
        "Automation Run View",
        "Read one exact durable automation run ledger record from the active project by run id without triggering or mutating automation. A missing run returns a typed not-found error and does not enumerate other run ids.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "run_id": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Exact automation run id to inspect."
                }
            },
            "required": ["run_id"]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_memory_automation_schema_exposes_no_manual_effect_authority() {
        let definition = def_memory_automation_run();
        assert_eq!(definition.annotations.unwrap()["readOnlyHint"], false);
        assert_eq!(definition.input_schema["additionalProperties"], false);
        let properties = definition.input_schema["properties"]
            .as_object()
            .expect("closed object properties");
        assert_eq!(
            properties.keys().map(String::as_str).collect::<Vec<_>>(),
            ["fact_review_limit", "min_confidence"]
        );
        for forbidden in [
            "run_id",
            "task",
            "operations",
            "proposal_id",
            "approve",
            "reject",
            "apply",
        ] {
            assert!(!properties.contains_key(forbidden));
        }
    }
}

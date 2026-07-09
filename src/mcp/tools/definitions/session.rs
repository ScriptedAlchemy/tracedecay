use serde_json::{Value, json};

use super::{def, git_scope, project_selector_object};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_message_search() -> ToolDefinition {
    def(
        "tracedecay_message_search",
        "Message Search",
        "Search ingested transcript messages across all supported providers by default. Searches catch up the selected provider scope unless catch_up is false; pass provider only when intentionally scoping results to one provider. Set goals=true to instead list each session's latest Codex thread goal (objective + current status) — a quick 'what was this session working on / is it still active' view; goals mode makes query optional.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Full-text query to search in ingested transcript messages. Required unless goals=true."
                },
                "goals": {
                    "type": "boolean",
                    "description": "When true, list each session's latest Codex goal (kind='goal') — objective as text plus lifecycle status (e.g. active/paused) from metadata — newest first, instead of running a full-text search. query becomes optional; limit and project_key still apply. Scoped to the selected project store."
                },
                "provider": {
                    "type": "string",
                    "description": "Optional explicit result scope. Omit or use 'all' for unified cross-provider recall; scoped searches catch up only that provider. Use 'hermes' for Hermes agent conversation history ingested from per-profile state.db stores.",
                    "enum": crate::sessions::providers::MESSAGE_SEARCH_PROVIDER_IDS
                },
                "project_key": {
                    "type": "string",
                    "description": "Optional provider-level project key/path filter within the selected session-message store. This is not a registered-project selector; use project_id or project_path to search another registered project's store."
                },
                "include_subagents": {
                    "type": "boolean",
                    "description": "Whether to include child subagent sessions in results (default: true)."
                },
                "catch_up": {
                    "type": "boolean",
                    "description": "Whether to ingest/catch up local provider transcripts before searching (default: true). Set false for strictly read-only audits of already-ingested messages."
                },
                "parent_session_id": {
                    "type": "string",
                    "description": "Optional parent session id filter. Primarily useful with scope=subagents_only."
                },
                "since": time_filter_schema("Optional inclusive minimum message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like 'last hour'."),
                "until": time_filter_schema("Optional inclusive maximum message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like 'last hour'."),
                "time_from": time_filter_schema("Alias for since."),
                "time_to": time_filter_schema("Alias for until."),
                "scope": {
                    "type": "string",
                    "description": "Relationship scope for search results (default: all).",
                    "enum": ["all", "parents_only", "subagents_only"]
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of messages to return (default: 10, max: 50)."
                },
                "project_selector": project_selector_object(
                    "Advanced optional registered project selector. Omit to use the active project.",
                    "search",
                ),
                "project_id": {
                    "type": "string",
                    "description": "Optional registered project id to search instead of the active project."
                },
                "project_path": {
                    "type": "string",
                    "description": "Optional registered project root path or alias to search instead of the active project."
                },
                "project_scope": {
                    "type": "string",
                    "description": "Set to all_registered to search transcript messages across every registered project's session store. Cannot be combined with project_id, project_path, project_root, or project_selector.",
                    "enum": ["all_registered"]
                },
                "branch": git_scope::branch_schema("Optional git branch filter: only messages from sessions active on this branch (via the session-git correlation index)."),
                "worktree": git_scope::worktree_schema("Optional git worktree root path filter: only messages from sessions active in this worktree (via the session-git correlation index)."),
                "commit": git_scope::commit_schema("Optional commit sha filter (full or >=6-char hex prefix): only messages from sessions attributed to this commit (via the session-git correlation index)."),
                "workflow_run": workflow_run_scope_schema(),
                "workflow_agent": workflow_agent_scope_schema()
            },
            "required": []
        }),
    )
}

pub(super) fn def_sessions_for() -> ToolDefinition {
    def(
        "tracedecay_sessions_for",
        "Sessions For Git Ref",
        "Find agent sessions correlated with a git artifact in the active project: sessions active on a branch or worktree, or sessions with evidence that they produced or observed a commit. Commit queries default to direct producer evidence; use relation=observed/all for weaker historical overlap. Supports time-scoped queries via since/until.",
        json!({
            "type": "object",
            "properties": {
                "git_ref": {
                    "type": "string",
                    "enum": ["branch", "worktree", "commit"],
                    "description": "Kind of git reference to correlate against."
                },
                "value": {
                    "type": "string",
                    "description": "The branch name, worktree root path, or commit sha (full or >=6-char hex prefix) to look up."
                },
                "since": time_filter_schema("Optional inclusive minimum activity/commit timestamp. Integer strings and timezone-aware ISO/RFC3339 strings are accepted."),
                "until": time_filter_schema("Optional inclusive maximum activity/commit timestamp. Integer strings and timezone-aware ISO/RFC3339 strings are accepted."),
                "relation": {
                    "type": "string",
                    "enum": ["produced", "observed", "all"],
                    "description": "Commit relationship to return (default: produced). produced requires direct creation evidence; observed is weaker HEAD/reflog/time-overlap evidence. Ignored for branch/worktree queries."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum sessions to return (default: 20)."
                }
            },
            "required": ["git_ref", "value"]
        }),
    )
}

pub(super) fn def_workflows() -> ToolDefinition {
    def(
        "tracedecay_workflows",
        "Workflow Runs",
        "Recover Claude Code workflow runs (multi-agent `wf_*` orchestrations) and their per-phase agents from the active project. Three modes, chosen by which argument is set: (1) list runs for a parent thread via session_id, or every run on a branch/worktree/commit via branch/worktree/commit (a run inherits its parent session's git spans); (2) show one run's result summary, phases, and agent roster via run_id; (3) drill into one agent's transcript via run_id + agent_label. Read-only; runs that never ran leave no rows.",
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Parent thread/session id: list the workflow runs it spawned (newest first). Mutually exclusive with run_id and the git filters."
                },
                "run_id": {
                    "type": "string",
                    "description": "A `wf_*` run id: show that run's summary, phases, and agents. Combine with agent_label to drill into one agent."
                },
                "agent_label": {
                    "type": "string",
                    "description": "With run_id, drill into a single agent of that run by its label (e.g. 'mine:claude-transcripts')."
                },
                "branch": git_scope::branch_schema("List workflow runs whose parent session was active on this git branch (via the session-git correlation index)."),
                "worktree": git_scope::worktree_schema("List workflow runs whose parent session was active in this git worktree root path."),
                "commit": git_scope::commit_schema("List workflow runs whose parent session was attributed to this commit sha (full or >=6-char hex prefix)."),
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum runs or agents to return (default: 20)."
                }
            }
        }),
    )
}

fn time_filter_schema(description: &str) -> Value {
    json!({
        "oneOf": [
            { "type": "integer", "minimum": 0 },
            { "type": "string" }
        ],
        "description": description
    })
}

fn workflow_run_scope_schema() -> Value {
    json!({
        "type": "string",
        "description": "Optional workflow run id (`wf_*`) filter: only messages from sessions that spawned this workflow run (via the workflow-run index). Pair with agent_label to scope to one agent."
    })
}

fn workflow_agent_scope_schema() -> Value {
    json!({
        "type": "string",
        "description": "Optional workflow agent label filter, used with workflow_run to scope to a single agent of that run."
    })
}

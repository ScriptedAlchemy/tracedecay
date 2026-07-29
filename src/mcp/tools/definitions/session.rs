use serde_json::{Value, json};

use super::{def, def_rw, git_scope, project_selector_object};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_session_refresh() -> ToolDefinition {
    def_rw(
        "tracedecay_session_refresh",
        "Session Refresh",
        "Start, join, resume, inspect, or cancel one daemon-owned durable session-temporal refresh. start, join, resume, and the compatibility action begin all invoke the same idempotent durable begin-or-join operation and return an opaque handle; the typed started or joined outcome reports what occurred. status is read-only and returns progress or a terminal receipt using a handle returned by start, join, resume, or begin. cancel is the only action that requests durable cancellation, and success is receipt-backed. Request abort or deadline outcomes never imply durable cancellation. Every call is bound to explicit profile, session, source, target, and project-or-profile scope selectors. The handler delegates only to the injected daemon service; unavailable authority fails closed without opening stores or ingesting transcripts.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "status", "join", "resume", "cancel", "begin"],
                    "description": "Operation to perform. start, join, resume, and begin all idempotently begin or join the exact durable target, return a typed started or joined outcome, and provide an opaque handle. begin is retained for compatibility. status is read-only and returns progress or the terminal receipt. cancel requests durable cancellation and succeeds only with a terminal receipt."
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "profile"],
                    "description": "Authoritative session-store scope. project requires project; profile forbids it."
                },
                "project": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Authoritative typed project id."
                        },
                        "repository_id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Resolved repository identity bound to the project session root."
                        },
                        "worktree_id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Resolved worktree identity bound to the project session root."
                        },
                        "branch_id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Resolved branch identity bound to the project session root."
                        }
                    },
                    "required": ["id", "repository_id", "worktree_id", "branch_id"]
                },
                "profile": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Authoritative typed profile id."
                        }
                    },
                    "required": ["id"]
                },
                "session": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Canonical session id to refresh."
                        },
                        "store_id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Resolved authoritative session-store id."
                        },
                        "root_id": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Resolved authoritative session-root id."
                        }
                    },
                    "required": ["id", "store_id", "root_id"]
                },
                "source": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "scope": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Canonical provider/source scope admitted for this refresh."
                        }
                    },
                    "required": ["scope"]
                },
                "target": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "temporal_mode": {
                            "description": "Typed temporal interpretation for the refreshed projection.",
                            "oneOf": [
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": { "kind": { "const": "current" } },
                                    "required": ["kind"]
                                },
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "kind": { "const": "as_of" },
                                        "cutoff": { "type": "integer" }
                                    },
                                    "required": ["kind", "cutoff"]
                                },
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": { "kind": { "const": "evolution" } },
                                    "required": ["kind"]
                                },
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": { "kind": { "const": "forensic" } },
                                    "required": ["kind"]
                                }
                            ]
                        },
                        "grain": {
                            "type": "string",
                            "enum": [
                                "occurrence",
                                "logical_message",
                                "turn",
                                "session",
                                "thread",
                                "agent",
                                "summary"
                            ],
                            "description": "Typed retrieval grain produced by the refresh."
                        },
                        "frontier": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "observed_through": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Frozen source frontier to refresh through."
                                },
                                "committed_through": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Already committed source frontier; cannot exceed observed_through."
                                }
                            },
                            "required": ["observed_through", "committed_through"]
                        }
                    },
                    "required": ["temporal_mode", "grain", "frontier"]
                },
                "handle": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Opaque daemon-local handle returned by successful start, join, resume, or begin. Required for read-only status and durable cancel; forbidden for start, join, resume, and begin."
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "json"],
                    "description": "Optional output format. Defaults to markdown."
                }
            },
            "required": ["action", "scope", "profile", "session", "source", "target"],
            "allOf": [
                {
                    "if": {
                        "properties": { "scope": { "const": "project" } },
                        "required": ["scope"]
                    },
                    "then": { "required": ["project"] }
                },
                {
                    "if": {
                        "properties": { "scope": { "const": "profile" } },
                        "required": ["scope"]
                    },
                    "then": { "not": { "required": ["project"] } }
                },
                {
                    "if": {
                        "properties": {
                            "action": {
                                "enum": ["start", "join", "resume", "begin"]
                            }
                        },
                        "required": ["action"]
                    },
                    "then": { "not": { "required": ["handle"] } },
                    "else": { "required": ["handle"] }
                }
            ]
        }),
    )
}

pub(super) fn def_message_search() -> ToolDefinition {
    def(
        "tracedecay_message_search",
        "Message Search",
        "Read session-temporal message evidence from one authorized project or profile root. This tool never ingests or refreshes provider history. Omitted catch_up is false; explicit catch_up=true requires fresh data and returns typed refresh guidance when the selected root is stale or partial. Set goals=true to list each session's latest thread goal; goals mode makes query optional. project_scope=all_registered is accepted but returns a typed deferred result: multi-root retrieval is not implemented.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Full-text query to search in ingested transcript messages. Required unless goals=true."
                },
                "goals": {
                    "type": "boolean",
                    "default": false,
                    "description": "When true, list each session's latest Codex goal (kind='goal') — objective as text plus lifecycle status (e.g. active/paused) from metadata — newest first, instead of running a full-text search. query becomes optional; limit and project_key still apply. Scoped to the selected project store."
                },
                "provider": {
                    "type": "string",
                    "default": "all",
                    "description": "Optional explicit result scope. Omit or use 'all' for unified cross-provider recall. A scoped freshness precondition applies only to that provider, but this read never catches it up. Use 'hermes' for Hermes agent conversation history already present in the authorized profile store.",
                    "enum": crate::sessions::providers::MESSAGE_SEARCH_PROVIDER_IDS
                },
                "project_key": {
                    "type": "string",
                    "description": "Optional provider-level project key/path filter within the selected session-message store. This is not a registered-project selector; use project_id or project_path to search another registered project's store."
                },
                "include_subagents": {
                    "type": "boolean",
                    "default": true,
                    "description": "Whether to include child subagent sessions in results (default: true)."
                },
                "catch_up": {
                    "type": "boolean",
                    "default": false,
                    "description": "Deprecated compatibility flag. Omitted/false allows stored data. Explicit true is a freshness precondition only: the read executes when fresh, while stale or partial coverage returns refresh_required and a typed tracedecay_session_refresh next action. This tool never performs catch-up, refresh, or ingest."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Opaque session-temporal continuation cursor returned by a prior compatible request."
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
                    "default": "all",
                    "description": "Relationship scope for search results (default: all).",
                    "enum": ["all", "parents_only", "subagents_only"]
                },
                "message_type": {
                    "type": "string",
                    "default": "all",
                    "description": "Semantic message filter. direct_user excludes provider-mislabeled tool results; tool_result includes role-, kind-, and metadata-identified tool output. Default: all.",
                    "enum": ["all", "direct_user", "tool_result"]
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "default": 10,
                    "description": "Maximum number of messages to return (default: 10, max: 50)."
                },
                "project_selector": closed_project_selector_object(
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
                    "description": "Accepted compatibility selector. all_registered returns a typed deferred result without opening the registry or any project store. Cannot be combined with project_id, project_path, or project_selector.",
                    "enum": ["all_registered"]
                },
                "branch": git_scope::branch_schema("Optional git branch filter: only messages from sessions active on this branch (via the session-git correlation index)."),
                "worktree": git_scope::worktree_schema("Optional git worktree root path filter: only messages from sessions active in this worktree (via the session-git correlation index)."),
                "commit": git_scope::commit_schema("Optional commit sha filter (full or >=6-char hex prefix): only messages from sessions attributed to this commit (via the session-git correlation index)."),
                "workflow_run": workflow_run_scope_schema(),
                "workflow_agent": workflow_agent_scope_schema(),
                "format": {
                    "type": "string",
                    "enum": ["markdown", "json"],
                    "description": "Optional output format. MCP defaults to compact Markdown; use json for the full compatibility and temporal envelopes."
                }
            },
            "required": [],
            "anyOf": [
                { "required": ["query"] },
                {
                    "properties": { "goals": { "const": true } },
                    "required": ["goals"]
                }
            ]
        }),
    )
}

fn closed_project_selector_object(description: &str, verb: &str) -> Value {
    let mut schema = project_selector_object(description, verb);
    // project_selector_object always builds a schema object.
    #[allow(clippy::expect_used)]
    schema
        .as_object_mut()
        .expect("project selector schema must be an object")
        .insert("additionalProperties".to_string(), Value::Bool(false));
    schema
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

#[cfg(test)]
mod message_search_definition_tests {
    use serde_json::{Value, json};

    use super::def_message_search;

    fn assert_closed_objects(schema: &Value) {
        if schema.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "object schema is not closed: {schema}"
            );
        }
        if let Some(object) = schema.as_object() {
            for value in object.values() {
                assert_closed_objects(value);
            }
        } else if let Some(array) = schema.as_array() {
            for value in array {
                assert_closed_objects(value);
            }
        }
    }

    #[test]
    fn message_search_definition_is_read_only_closed_and_freshness_explicit() {
        let definition = def_message_search();

        assert_eq!(
            definition.annotations.as_ref().unwrap()["readOnlyHint"],
            true
        );
        assert!(
            definition
                .description
                .contains("never ingests or refreshes")
        );
        assert_eq!(
            definition.input_schema["properties"]["catch_up"]["default"],
            false
        );
        assert!(
            definition.input_schema["properties"]["catch_up"]["description"]
                .as_str()
                .unwrap()
                .contains("freshness precondition")
        );
        assert_eq!(
            definition.input_schema["properties"]["project_scope"]["enum"],
            json!(["all_registered"])
        );
        assert_closed_objects(&definition.input_schema);
    }

    #[test]
    fn message_search_schema_keeps_query_conditional_and_cursor_additive() {
        let definition = def_message_search();
        let schema = &definition.input_schema;

        assert!(schema["required"].as_array().unwrap().is_empty());
        assert_eq!(schema["anyOf"][0]["required"], json!(["query"]));
        assert_eq!(schema["anyOf"][1]["required"], json!(["goals"]));
        assert_eq!(schema["anyOf"][1]["properties"]["goals"]["const"], true);
        assert_eq!(schema["properties"]["goals"]["default"], false);
        assert_eq!(schema["properties"]["include_subagents"]["default"], true);
        assert_eq!(schema["properties"]["scope"]["default"], "all");
        assert_eq!(schema["properties"]["message_type"]["default"], "all");
        assert_eq!(schema["properties"]["limit"]["default"], 10);
        assert_eq!(schema["properties"]["limit"]["minimum"], 1);
        assert_eq!(schema["properties"]["limit"]["maximum"], 50);
        assert_eq!(schema["properties"]["cursor"]["type"], "string");
    }
}

#[cfg(test)]
mod lcm_definition_compatibility_tests {
    use serde_json::Value;

    use super::super::{def_lcm_describe, def_lcm_expand};

    fn assert_closed_objects(schema: &Value) {
        if schema.get("type").and_then(Value::as_str) == Some("object") {
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "object schema is not closed: {schema}"
            );
        }
        if let Some(object) = schema.as_object() {
            for value in object.values() {
                assert_closed_objects(value);
            }
        } else if let Some(array) = schema.as_array() {
            for value in array {
                assert_closed_objects(value);
            }
        }
    }

    #[test]
    fn describe_and_expand_keep_closed_string_id_compatibility() {
        let describe = def_lcm_describe();
        let expand = def_lcm_expand();

        assert_closed_objects(&describe.input_schema);
        assert_closed_objects(&expand.input_schema);
        assert_eq!(
            describe.input_schema["properties"]["target"]["oneOf"][1]["properties"]["node_id"]["type"],
            "string"
        );
        assert_eq!(
            expand.input_schema["properties"]["target"]["oneOf"][1]["properties"]["node_id"]["type"],
            "string"
        );
        assert_eq!(
            expand.input_schema["properties"]["source_offset"]["minimum"],
            0
        );
        assert_eq!(
            expand.input_schema["properties"]["source_limit"]["maximum"],
            100
        );
        assert_eq!(
            expand.input_schema["properties"]["cursor"]["type"],
            "string"
        );
    }
}

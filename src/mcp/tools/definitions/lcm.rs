//! LCM session-store and session health-baseline tool definitions.

use serde_json::json;

use super::{def, git_scope};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_lcm_status() -> ToolDefinition {
    def(
        "tracedecay_lcm_status",
        "LCM Status",
        "Return LCM schema, raw-message, summary, payload, and maintenance counts plus store token estimates, stored summary-depth distribution with compression ratio, payload byte totals, and payload GC status from the active project session store. Summaries are present only after authenticated host publication.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Optional provider id. Omit or use 'all' to inspect all providers."
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional provider-local session id filter."
                },
                "deep": {
                    "type": "boolean",
                    "description": "When true, include an on-disk payload integrity sweep and populate integrity_mismatch_count. Defaults to false."
                }
            }
        }),
    )
}

pub(super) fn def_lcm_doctor() -> ToolDefinition {
    def(
        "tracedecay_lcm_doctor",
        "LCM Doctor",
        "Read a bounded, redacted temporal-store health report through the daemon-owned LCM authority without payload body exposure. This surface never repairs, cleans, or garbage-collects; daemon-owned maintenance acts on findings outside this tool.",
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    )
}

pub(super) fn def_lcm_load_session() -> ToolDefinition {
    def(
        "tracedecay_lcm_load_session",
        "LCM Load Session",
        "Load ordered lossless raw session messages with stable pagination and bounded content slices from the active project LCM store.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Optional provider id. Omit or use 'all' to load messages for this session id across all providers."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Authenticated opaque continuation cursor returned as next_cursor."
                },
                "temporal_mode": {
                    "type": "string",
                    "enum": ["current", "as_of", "evolution", "forensic"],
                    "description": "Canonical temporal retrieval mode. Defaults to forensic for exact-session loading."
                },
                "as_of_micros": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Required cutoff in UTC microseconds when temporal_mode=as_of."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum rows."
                },
                "role": {
                    "type": "string",
                    "description": "Optional single role filter. Prefer roles for native Hermes parity."
                },
                "roles": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional role filters. Matches any listed role."
                },
                "start_time": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional inclusive minimum message timestamp."
                },
                "end_time": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional inclusive maximum message timestamp."
                },
                "content_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Character offset for each returned content slice."
                },
                "content_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20000,
                    "description": "Maximum characters returned per message. Values above 20000 are clamped and reported in content_limit_clamped_from."
                }
            },
            "required": ["session_id"]
        }),
    )
}

pub(super) fn def_lcm_grep() -> ToolDefinition {
    def(
        "tracedecay_lcm_grep",
        "LCM Grep",
        "Search bounded LCM raw-message snippets and optional summary text across all providers in the active project session store. Pass provider to constrain one provider.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Optional provider id. Omit or use 'all' to search all providers."
                },
                "query": {
                    "type": "string",
                    "description": "Full-text query for LCM snippets."
                },
                "scope": {
                    "type": "string",
                    "enum": ["current", "session", "all"],
                    "description": "Search scope. current/session require session_id; all is the default."
                },
                "relationship_scope": {
                    "type": "string",
                    "enum": ["all", "parents_only", "subagents_only"],
                    "description": "Optional parent/subagent relationship filter across sessions. Default: all."
                },
                "message_type": {
                    "type": "string",
                    "enum": ["all", "direct_user", "tool_result"],
                    "description": "Semantic raw-message filter. direct_user excludes provider-mislabeled tool results; tool_result recognizes role, kind, and tool-event metadata. Default: all."
                },
                "session_id": {
                    "type": "string",
                    "description": "Session id used when scope is current or session."
                },
                "include_summaries": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include eligible canonical summary nodes alongside raw occurrences. Current/as-of/evolution supersession and source-horizon rules are applied before ranking."
                },
                "sort": {
                    "type": "string",
                    "enum": ["recency", "relevance", "hybrid"],
                    "default": "relevance",
                    "description": "Canonical temporal ranking supports relevance. recency and hybrid return a typed unsupported_filter response."
                },
                "source": {
                    "type": "string",
                    "description": "Optional canonical observation source id or provider filter. Summary nodes match when an eligible retained source matches."
                },
                "role": {
                    "type": "string",
                    "enum": ["system", "user", "assistant", "tool", "unknown"],
                    "description": "Optional raw-message role filter. When supplied, summary results are omitted."
                },
                "start_time": {
                    "oneOf": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "string" }
                    ],
                    "description": "Optional inclusive minimum raw-message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like 'last hour'."
                },
                "end_time": {
                    "oneOf": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "string" }
                    ],
                    "description": "Optional inclusive maximum raw-message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like 'last hour'."
                },
                "since": {
                    "oneOf": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "string" }
                    ],
                    "description": "Alias for start_time."
                },
                "until": {
                    "oneOf": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "string" }
                    ],
                    "description": "Alias for end_time."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum hits."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Authenticated opaque continuation cursor returned as next_cursor."
                },
                "temporal_mode": {
                    "type": "string",
                    "enum": ["current", "as_of", "evolution", "forensic"],
                    "description": "Canonical temporal retrieval mode. Defaults to current."
                },
                "as_of_micros": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Required cutoff in UTC microseconds when temporal_mode=as_of."
                },
                "branch": git_scope::branch_schema("Optional git branch filter: only LCM snippets from sessions active on this branch (via the session-git correlation index)."),
                "worktree": git_scope::worktree_schema("Optional git worktree root path filter: only LCM snippets from sessions active in this worktree (via the session-git correlation index)."),
                "commit": git_scope::commit_schema("Optional commit sha filter (full or >=6-char hex prefix): only LCM snippets from sessions attributed to this commit (via the session-git correlation index).")
            },
            "required": ["query"]
        }),
    )
}

pub(super) fn def_lcm_describe() -> ToolDefinition {
    def(
        "tracedecay_lcm_describe",
        "LCM Describe",
        "Describe one session's LCM raw-message and summary-DAG shape from the active project store without exposing full payload bodies.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Specific provider id. Required because describe targets are provider-local."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "target": {
                    "description": "Optional describe target. Omit for session overview.",
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "session"}
                            },
                            "required": ["kind"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "summary_node"},
                                "node_id": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "Summary node id."
                                }
                            },
                            "required": ["kind", "node_id"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "external_payload"},
                                "payload_ref": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "External payload ref."
                                }
                            },
                            "required": ["kind", "payload_ref"]
                        }
                    ]
                }
            },
            "required": ["provider", "session_id"]
        }),
    )
}

pub(super) fn def_lcm_expand() -> ToolDefinition {
    def(
        "tracedecay_lcm_expand",
        "LCM Expand",
        "Expand one raw message, summary node, or external payload through the bounded LCM query API from the active project store.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Specific provider id. Required because expansion targets are provider-local."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "target": {
                    "description": "Expansion target.",
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "raw_message"},
                                "store_id": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Owner-bound legacy raw-message store alias."
                                }
                            },
                            "required": ["kind", "store_id"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "summary_node"},
                                "node_id": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "Summary node id."
                                }
                            },
                            "required": ["kind", "node_id"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "kind": {"const": "external_payload"},
                                "payload_ref": {
                                    "type": "string",
                                    "minLength": 1,
                                    "description": "Owner-bound external payload ref."
                                }
                            },
                            "required": ["kind", "payload_ref"]
                        }
                    ]
                },
                "content_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Character offset for returned content."
                },
                "content_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 8192,
                    "description": "Maximum characters returned."
                },
                "source_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 50,
                    "description": "Maximum immediate sources returned for a summary_node target; resume only with the response's authenticated next_cursor. If a returned source has content_truncated=true, continue via target.kind=raw_message for that source's store_id and content_offset."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Opaque continuation cursor returned by the same authorized root and target."
                }
            },
            "required": ["provider", "session_id", "target"]
        }),
    )
}

pub(super) fn def_lcm_expand_query() -> ToolDefinition {
    def(
        "tracedecay_lcm_expand_query",
        "LCM Expand Query",
        "Assemble bounded LCM retrieval context for a prompt from the active project store and, when an automation backend is configured and available, synthesize the answer directly (returned first as `answer`, with `needs_synthesis:false`). Pure-noise blocks (base64 signature blobs, directory listings) are filtered from the context. When no backend is available it falls back to returning the raw context with `needs_synthesis:true` for the host to synthesize.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Specific provider id. Required because retrieval context is provider-local."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "query": {
                    "type": "string",
                    "description": "Optional search query to select candidate LCM context."
                },
                "prompt": {
                    "type": "string",
                    "description": "Question or instruction to answer from LCM context."
                },
                "node_ids": {
                    "type": "array",
                    "items": {
                        "oneOf": [
                            { "type": "string" },
                            { "type": "integer", "minimum": 0 }
                        ]
                    },
                    "description": "Optional summary node ids to expand."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum candidate results."
                },
                "max_tokens": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 8192,
                    "description": "Desired synthesized answer token budget passed through to the LCM engine. Does not affect the retrieval context size; use context_max_tokens for that."
                },
                "context_max_tokens": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 65536,
                    "description": "Maximum retrieval context budget (tokens of LCM material assembled before synthesis). Defaults to 32000. Independent of max_tokens, which governs the synthesis output size."
                },
                "cursor": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Opaque continuation cursor returned by the same expand-query request. The original provider, session, query or single node id, limits, and context budget must remain unchanged."
                }
            },
            "required": ["provider", "session_id", "prompt"]
        }),
    )
}

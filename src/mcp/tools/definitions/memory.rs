//! Memory fact-store tool definitions.

use serde_json::{Value, json};

use super::{def, def_rw, project_selector_object, project_selector_properties};
use crate::mcp::tools::ToolDefinition;

fn memory_fact_properties() -> Value {
    json!({
        "memory_scope": {
            "type": "string",
            "enum": ["project", "user"],
            "description": "Fact scope. project (default) uses the active project shard; user uses the profile-level store for durable preferences and projectless conversations."
        },
        "action": {
            "type": "string",
            "enum": ["add", "search", "probe", "related", "reason", "contradict", "get", "update", "remove", "list"],
            "description": "Fact-store action to perform."
        },
        "content": {
            "type": "string",
            "description": "Fact content for add/update actions."
        },
        "query": {
            "type": "string",
            "description": "Search query for search actions."
        },
        "entity": {
            "type": "string",
            "description": "Single entity name for probe/related actions, or extra add entity."
        },
        "entities": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Entity names for add/update/reason actions."
        },
        "fact_id": {
            "oneOf": [{ "type": "number" }, { "type": "string" }],
            "description": "Fact id for update/remove/feedback; numeric strings are accepted."
        },
        "category": {
            "type": "string",
            "enum": ["general", "user_pref", "project", "tool", "decision", "code_area"],
            "description": "Optional fact category."
        },
        "tags": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Free-form tags stored with fact metadata."
        },
        "min_trust": {
            "type": "number",
            "description": "Minimum trust score for search/list actions."
        },
        "trust": {
            "type": "number",
            "minimum": 0,
            "maximum": 1,
            "description": "Initial or replacement trust score for add/update actions."
        },
        "trust_delta": {
            "type": "number",
            "description": "Hermes-compatible trust delta field. Current feedback actions apply the built-in helpful/unhelpful deltas."
        },
        "threshold": {
            "type": "number",
            "description": "Threshold for contradiction scans."
        },
        "limit": {
            "type": "number",
            "description": "Maximum number of facts to return (default: 20, max: 200)."
        },
        "source": {
            "type": "string",
            "description": "Source label for facts or feedback."
        },
        "metadata": {
            "type": "object",
            "description": "Arbitrary structured metadata stored with the fact."
        },
        "note": {
            "type": "string",
            "description": "Human-readable feedback note or action context."
        },
        "project_selector": project_selector_object(
            "Advanced optional registered project selector. Omit to use the active project.",
            "query",
        ),
        "project_id": {
            "type": "string",
            "description": "Optional registered project id to query instead of the active project."
        },
        "project_path": {
            "type": "string",
            "description": "Optional registered project root path or alias to query instead of the active project."
        }
    })
}

fn fact_store_action_requirements() -> Value {
    json!([
        {
            "if": {
                "properties": { "action": { "const": "add" } },
                "required": ["action"]
            },
            "then": { "required": ["content"] }
        },
        {
            "if": {
                "properties": { "action": { "const": "search" } },
                "required": ["action"]
            },
            "then": { "required": ["query"] }
        },
        {
            "if": {
                "properties": { "action": { "const": "probe" } },
                "required": ["action"]
            },
            "then": { "required": ["entity"] }
        },
        {
            "if": {
                "properties": { "action": { "const": "related" } },
                "required": ["action"]
            },
            "then": { "required": ["entity"] }
        },
        {
            "if": {
                "properties": { "action": { "const": "get" } },
                "required": ["action"]
            },
            "then": { "required": ["fact_id"] }
        },
        {
            "if": {
                "properties": { "action": { "const": "update" } },
                "required": ["action"]
            },
            "then": { "required": ["fact_id"] }
        },
        {
            "if": {
                "properties": { "action": { "const": "remove" } },
                "required": ["action"]
            },
            "then": { "required": ["fact_id"] }
        }
    ])
}

pub(super) fn def_fact_store() -> ToolDefinition {
    def_rw(
        "tracedecay_fact_store",
        "Fact Store",
        "Add, search, probe, relate, reason over, get, update, remove, or list holographic memory facts. The action field selects the operation. \
         Defaults to the active project; project_id/project_path selectors are supported for read-only retrieval actions only (search/probe/related/reason/contradict/get/list). \
         The add result carries a write-time diff report (diff/closest_fact_id/similarity/reason): near_duplicate = a very similar fact exists \
         (prefer updating it), possible_conflict = a negation/state-change cue suggests supersession (confirm which fact is current), \
         rejected_secret_like = credential-like content was NOT stored. The get action returns the full fact plus trust_history so operators can answer \
         why a trust score changed. Calibrate trust on add instead of defaulting high \
         (>=0.85 verified/durable, ~0.7 ordinary, ~0.5 unsure — aim for a spread), and search memory before external lookups. \
         Use it proactively, without waiting to be asked: when the user states a durable preference, decision, or correction, add or update a fact for it; \
         and before answering a question about this project or the user, search or probe memory first rather than guessing.",
        json!({
            "type": "object",
            "properties": memory_fact_properties(),
            "allOf": fact_store_action_requirements(),
            "required": ["action"]
        }),
    )
}

pub(super) fn def_fact_feedback() -> ToolDefinition {
    def_rw(
        "tracedecay_fact_feedback",
        "Fact Feedback",
        "Record helpful/unhelpful feedback for an active-project memory fact and adjust its trust score. Call this on the fact_id values surfaced in tracedecay_context's Memory Matches (or from fact_store search) whenever a recalled fact materially helped or misled you -- feedback is how trust is earned, and recalled facts are almost never rated.",
        json!({
            "type": "object",
            "properties": {
                "fact_id": {
                    "oneOf": [{ "type": "number" }, { "type": "string" }],
                    "description": "Fact id; numeric strings are accepted."
                },
                "action": {
                    "type": "string",
                    "enum": ["helpful", "unhelpful"],
                    "description": "Feedback action. One of action, helpful, or unhelpful must be provided."
                },
                "helpful": {
                    "type": "boolean",
                    "description": "Hermes-compatible shorthand for action=helpful."
                },
                "unhelpful": {
                    "type": "boolean",
                    "description": "Hermes-compatible shorthand for action=unhelpful."
                },
                "trust_delta": {
                    "type": "number",
                    "description": "Hermes-compatible trust delta field. Built-in action deltas are applied."
                },
                "source": {
                    "type": "string",
                    "description": "Feedback source label."
                },
                "metadata": {
                    "type": "object",
                    "description": "Additional feedback metadata reserved for compatibility."
                },
                "note": {
                    "type": "string",
                    "description": "Optional feedback note."
                },
                "memory_scope": {
                    "type": "string",
                    "enum": ["project", "user"],
                    "description": "Scope containing the fact id (default: project)."
                }
            },
            // No root-level anyOf: some providers (e.g. Moonshot) reject `anyOf`
            // alongside a parent `type`; feedback_action() enforces the
            // action/helpful/unhelpful requirement at runtime.
            "required": ["fact_id"]
        }),
    )
}

pub(super) fn def_memory_status() -> ToolDefinition {
    def(
        "tracedecay_memory_status",
        "Memory Status",
        "Inspect derived holographic memory state without advancing repair: return fact/entity counts, trust distribution, below-threshold and missing-vector signals, capacity-per-bank, and the current repair backlog. Defaults to the active project; pass project_id or project_path only when intentionally checking another registered project. Human/operator equivalents: `tracedecay memory status` and `GET /api/plugins/holographic/status`.",
        json!({
            "type": "object",
            "properties": memory_status_properties()
        }),
    )
}

fn memory_status_properties() -> Value {
    let mut properties = project_selector_properties();
    if let Some(properties) = properties.as_object_mut() {
        properties.insert(
            "memory_scope".to_string(),
            json!({
                "type": "string",
                "enum": ["project", "user"],
                "description": "Memory scope to inspect (default: project)."
            }),
        );
    }
    properties
}

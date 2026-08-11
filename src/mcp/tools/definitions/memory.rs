//! Memory fact-store tool definitions.

use serde_json::{Value, json};

use super::{def, def_rw, project_selector_properties};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_fact_store_add(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_fact_store_add",
        "Fact Store Add",
        "Add one holographic memory fact. The result includes a write-time diff report for near duplicates, possible conflicts, and rejected secret-like content. Calibrate trust to the evidence instead of defaulting high.",
        input_schema,
    )
}

pub(super) fn def_fact_store_search(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_fact_store_search",
        "Fact Store Search",
        "Search holographic memory facts by text and trust. Search durable project or user memory before guessing or repeating external research.",
        input_schema,
    )
}

pub(super) fn def_fact_store_probe(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_fact_store_probe",
        "Fact Store Probe",
        "Find holographic memory facts connected to one entity.",
        input_schema,
    )
}

pub(super) fn def_fact_store_related(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_fact_store_related",
        "Fact Store Related",
        "List entities related to one entity through holographic memory facts.",
        input_schema,
    )
}

pub(super) fn def_fact_store_reason(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_fact_store_reason",
        "Fact Store Reason",
        "Reason over holographic memory facts connecting multiple entities.",
        input_schema,
    )
}

pub(super) fn def_fact_store_contradict(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_fact_store_contradict",
        "Fact Store Contradict",
        "Find potentially contradictory holographic memory facts above an optional threshold.",
        input_schema,
    )
}

pub(super) fn def_fact_store_get(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_fact_store_get",
        "Fact Store Get",
        "Get one holographic memory fact, including trust history explaining score changes.",
        input_schema,
    )
}

pub(super) fn def_fact_store_update(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_fact_store_update",
        "Fact Store Update",
        "Update one existing holographic memory fact without changing its identity.",
        input_schema,
    )
}

pub(super) fn def_fact_store_remove(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_fact_store_remove",
        "Fact Store Remove",
        "Remove one holographic memory fact by exact fact id.",
        input_schema,
    )
}

pub(super) fn def_fact_store_list(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_fact_store_list",
        "Fact Store List",
        "List holographic memory facts with optional category, trust, and project selectors.",
        input_schema,
    )
}

pub(super) fn def_fact_store_curate(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_fact_store_curate",
        "Fact Store Curate",
        "Apply one canonical batch of tag normalizations and fact links to retained project or user memory. Every operation requires explicit evidence fact ids and bounded confidence; the result carries durable commit and replay identity.",
        input_schema,
    )
}

pub(super) fn def_fact_feedback() -> ToolDefinition {
    def_rw(
        "tracedecay_fact_feedback",
        "Fact Feedback",
        "Record helpful/unhelpful feedback for an active-project memory fact and adjust its trust score. Call this on fact_id values surfaced in tracedecay_context's Memory Matches or tracedecay_fact_store_search whenever a recalled fact materially helped or misled you -- feedback is how trust is earned, and recalled facts are almost never rated.",
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

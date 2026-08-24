//! Memory fact-store tool definitions.

use serde_json::Value;

use super::{def, def_rw};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_fact_store_add(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_fact_store_add",
        "Fact Store Add",
        "Add one holographic memory fact. The result includes a write-time diff report for near duplicates, possible conflicts, and rejected secret-like content. Calibrate trust to the evidence instead of defaulting high.",
        input_schema,
    )
}

pub(super) fn def_fact_store_curate(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_fact_store_curate",
        "Fact Store Curate",
        "Run the daemon-owned automatic Memory Curator. Callers may bound review size and confidence only; TraceDecay derives the run, operations, validation, policy, and apply authority. Inspect the durable terminal with the read-only automation run tools.",
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

pub(super) fn def_fact_feedback(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_fact_feedback",
        "Fact Feedback",
        "Record helpful/unhelpful feedback for an active-project memory fact and adjust its trust score. Call this on fact_id values surfaced in tracedecay_context's Memory Matches or tracedecay_fact_store_search whenever a recalled fact materially helped or misled you -- feedback is how trust is earned, and recalled facts are almost never rated.",
        input_schema,
    )
}

pub(super) fn def_memory_status(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_memory_status",
        "Memory Status",
        "Inspect canonical memory state: return the owner, fact/entity counts, algebra identity and capacity, trust distribution, below-threshold facts, feedback totals, and retrieval funnel. Defaults to the active project; pass an exact project_selector only when intentionally checking another registered project. Human/operator equivalent: `tracedecay memory status`.",
        input_schema,
    )
}

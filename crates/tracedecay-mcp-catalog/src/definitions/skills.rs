//! Managed-skill tool definitions.

use serde_json::Value;

use super::def;
use crate::ToolDefinition;

pub(super) fn def_skill_list(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_skill_list",
        "Skill List",
        "List agent-managed skills from the active TraceDecay profile. Returns metadata, lifecycle state, support-file paths, usage summary, stale/archive and improvement recommendation evidence, and optional body text without mutating the skill store.",
        input_schema,
    )
}

pub(super) fn def_skill_view(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_skill_view",
        "Skill View",
        "Read one agent-managed skill package from the active TraceDecay profile. Returns metadata, body text, usage summary, and support-file path summaries. Support-file bytes are omitted unless include_support_files is true.",
        input_schema,
    )
}

pub(super) fn def_hermes_skill_bridge(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_hermes_skill_bridge",
        "Hermes Skill Inventory",
        "Read skills, pending approvals, usage telemetry, and archive counts owned by the standard ~/.hermes user install. Read-only; alternate Hermes roots and TraceDecay storage selectors are not supported.",
        input_schema,
    )
}

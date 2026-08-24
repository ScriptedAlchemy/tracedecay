//! Managed-skill tool definitions.

use serde_json::{Value, json};

use super::def;
use crate::mcp::tools::ToolDefinition;

fn skill_state_property() -> Value {
    json!({
        "type": "string",
        "enum": ["active", "disabled", "archived"],
        "description": "Optional managed-skill lifecycle state filter."
    })
}

pub(super) fn def_skill_list() -> ToolDefinition {
    def(
        "tracedecay_skill_list",
        "Skill List",
        "List agent-managed skills from the active TraceDecay profile. Returns metadata, lifecycle state, support-file paths, usage summary, stale/archive and improvement recommendation evidence, and optional body text without mutating the skill store.",
        json!({
            "type": "object",
            "properties": {
                "state": skill_state_property(),
                "include_body": {
                    "type": "boolean",
                    "description": "If true, include each skill's body_markdown in the list response (default: false)."
                }
            }
        }),
    )
}

pub(super) fn def_skill_view() -> ToolDefinition {
    def(
        "tracedecay_skill_view",
        "Skill View",
        "Read one agent-managed skill package from the active TraceDecay profile. Returns full metadata, body text, usage summary, stale/archive and improvement recommendation evidence, and support files by default.",
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Managed skill id to read."
                },
                "include_support_files": {
                    "type": "boolean",
                    "description": "If false, omit support file byte payloads from the response (default: true)."
                }
            },
            "required": ["id"]
        }),
    )
}

pub(super) fn def_hermes_skill_bridge() -> ToolDefinition {
    def(
        "tracedecay_hermes_skill_bridge",
        "Hermes Skill Inventory",
        "Read skills, pending approvals, usage telemetry, and archive counts owned by the standard ~/.hermes user install. Read-only; alternate Hermes roots and TraceDecay storage selectors are not supported.",
        json!({
            "type": "object",
            "properties": {
                "include_skill_bodies": {
                    "type": "boolean",
                    "description": "Include bounded SKILL.md contents (default: false)."
                },
                "include_pending_payloads": {
                    "type": "boolean",
                    "description": "Include staged Hermes skill-write payloads (default: false)."
                }
            }
        }),
    )
}

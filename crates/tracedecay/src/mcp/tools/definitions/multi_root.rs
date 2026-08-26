//! MCP discovery definitions for the daemon-owned multi-root operations.

use serde_json::json;

use super::{def, def_rw, required_object_schema};
use crate::mcp::tools::ToolDefinition;

fn selector_schema() -> serde_json::Value {
    required_object_schema(
        json!({
            "project_id": {
                "type": "string",
                "description": "Exact registered project identity."
            },
            "root": {
                "type": "string",
                "description": "Exact canonical registered root for that project."
            }
        }),
        &["project_id", "root"],
    )
}

pub(super) fn def_multi_root_scope_set_read() -> ToolDefinition {
    def(
        "tracedecay_multi_root_scope_set_read",
        "Read a saved multi-root scope set",
        "Read one authorized multi-root scope set through the daemon-owned route. The daemon conceals absent and unauthorized records identically.",
        required_object_schema(
            json!({
                "scope_set_id": {
                    "type": "string",
                    "description": "Exact opaque scope-set identity."
                }
            }),
            &["scope_set_id"],
        ),
    )
}

pub(super) fn def_multi_root_scope_set_compare_and_swap() -> ToolDefinition {
    def_rw(
        "tracedecay_multi_root_scope_set_compare_and_swap",
        "Save a multi-root scope set",
        "Create or replace one exact registered-root scope set through the daemon's coordinated compare-and-swap and recovery authority.",
        required_object_schema(
            json!({
                "scope_set_id": {
                    "type": "string",
                    "description": "Exact opaque scope-set identity."
                },
                "expected_revision": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Expected saved revision, or null when creating the set."
                },
                "roots": {
                    "type": "array",
                    "minItems": 1,
                    "items": selector_schema(),
                    "description": "Every exact registered root that belongs in this frozen scope set."
                }
            }),
            &["scope_set_id", "roots"],
        ),
    )
}

pub(super) fn def_multi_root_execute() -> ToolDefinition {
    def(
        "tracedecay_multi_root_execute",
        "Execute a frozen multi-root query",
        "Execute one closed query family against an exact saved scope-set revision. Continuations are daemon-authenticated and retain frozen root generations.",
        required_object_schema(
            json!({
                "scope_set_id": {"type": "string"},
                "scope_set_revision": {"type": "integer", "minimum": 1},
                "scope_set_digest": {"type": "string"},
                "operation": {
                    "oneOf": [
                        {"type": "object", "properties": {"kind": {"const": "work"}, "request": {}}, "required": ["kind", "request"]},
                        {"type": "object", "properties": {"kind": {"const": "git"}, "request": {}}, "required": ["kind", "request"]},
                        {"type": "object", "properties": {"kind": {"const": "feedback"}, "request": {}}, "required": ["kind", "request"]},
                        {"type": "object", "properties": {"kind": {"const": "impact"}, "request": {}}, "required": ["kind", "request"]},
                        {"type": "object", "properties": {"kind": {"const": "query"}, "request": {}}, "required": ["kind", "request"]}
                    ]
                },
                "page": {"type": "integer", "minimum": 0},
                "continuation": {
                    "type": ["string", "null"],
                    "description": "Daemon-authenticated continuation from the previous multi-root page."
                }
            }),
            &[
                "scope_set_id",
                "scope_set_revision",
                "scope_set_digest",
                "operation",
                "page",
            ],
        ),
    )
}

use serde_json::json;

use super::{def, def_always_load, def_rw, required_object_schema, string_property};
use crate::mcp::tools::ToolDefinition;

fn feedback_surface_definition(
    name: &str,
    title: &str,
    description: &str,
    writes: bool,
) -> ToolDefinition {
    let schema = required_object_schema(
        json!({
            "request_handle": string_property(
                "Daemon-minted opaque request handle. Clients cannot reconstruct application requests from this value."
            )
        }),
        &["request_handle"],
    );
    if writes {
        def_rw(name, title, description, schema)
    } else {
        def(name, title, description, schema)
    }
}

pub(super) fn def_git_preview() -> ToolDefinition {
    def(
        "tracedecay_git_preview",
        "Preview Git index changes",
        "Preview one typed stage_hunks, unstage_hunks, or commit_index request through the daemon-owned Git transaction service. The daemon mints the preview identity; no generic Git arguments are accepted.",
        required_object_schema(
            json!({
                "operation": {
                    "type": "string",
                    "enum": ["stage_hunks", "unstage_hunks", "commit_index"],
                    "description": "Closed internal operation selected by the public preview facade."
                },
                "repository_snapshot": {
                    "type": "object",
                    "description": "Exact PR9 repository state snapshot used for compare-and-swap validation."
                },
                "selected_hunks": {
                    "type": "array",
                    "items": {"type": "object"},
                    "description": "Exact HunkRef objects; required for stage/unstage and empty for commit."
                },
                "commit_intent": {
                    "type": ["object", "null"],
                    "description": "Structured commit intent; required only for commit_index."
                }
            }),
            &["operation", "repository_snapshot"],
        ),
    )
}

pub(super) fn def_git_apply() -> ToolDefinition {
    def_rw(
        "tracedecay_git_apply",
        "Apply Git index preview",
        "Apply one exact immutable Git preview through the daemon-owned transaction service with CAS, policy recheck, idempotency, and a durable receipt.",
        required_object_schema(
            json!({
                "preview": {
                    "type": "object",
                    "description": "Exact git_preview result payload, including preview identity, digest, and CAS evidence."
                },
                "idempotency_key": string_property("Stable key for safe apply retry and terminal receipt replay.")
            }),
            &["preview", "idempotency_key"],
        ),
    )
}

pub(super) fn def_feedback_diagnostics() -> ToolDefinition {
    feedback_surface_definition(
        "tracedecay_feedback_diagnostics",
        "Run feedback diagnostics",
        "Resolve the catalog feedback diagnostics binding and return its canonical application result.",
        false,
    )
}

pub(super) fn def_feedback_get() -> ToolDefinition {
    feedback_surface_definition(
        "tracedecay_feedback_get",
        "Get feedback finding",
        "Resolve the catalog feedback get binding and return its canonical application result.",
        false,
    )
}

pub(super) fn def_feedback_expand() -> ToolDefinition {
    feedback_surface_definition(
        "tracedecay_feedback_expand",
        "Expand feedback evidence",
        "Resolve the catalog feedback expansion binding and return its canonical application result.",
        false,
    )
}

pub(super) fn def_feedback_list() -> ToolDefinition {
    feedback_surface_definition(
        "tracedecay_feedback_list",
        "List feedback findings",
        "Resolve the catalog feedback list binding and return its canonical application result with opaque continuation semantics.",
        false,
    )
}

pub(super) fn def_feedback_impact() -> ToolDefinition {
    def(
        "tracedecay_feedback_impact",
        "Read feedback impact",
        "Resolve impact for one symbol through the daemon-retained PR12 primitive owner, preserving canonical coverage, partial state, and continuation.",
        required_object_schema(
            json!({
                "node_id": string_property("Exact graph node identity."),
                "maximum_depth": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 64,
                    "default": 3
                },
                "path_prefix": {
                    "type": ["string", "null"],
                    "description": "Optional admitted-root-relative scope prefix."
                }
            }),
            &["node_id"],
        ),
    )
}

pub(super) fn def_affected_tests() -> ToolDefinition {
    def(
        "tracedecay_affected_tests",
        "Read affected tests",
        "Resolve changed files through the daemon-retained PR12 test primitive with canonical ranking, coverage, partial state, and opaque continuation.",
        required_object_schema(
            json!({
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 256,
                    "items": {"type": "string"}
                },
                "maximum_depth": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 10,
                    "default": 5
                },
                "filter": {
                    "type": ["string", "null"],
                    "description": "Optional bounded test-file filter."
                }
            }),
            &["files"],
        ),
    )
}

pub(super) fn def_test_results() -> ToolDefinition {
    def(
        "tracedecay_test_results",
        "Read recent test results",
        "Read the latest daemon-retained managed test result for the admitted project root.",
        required_object_schema(json!({}), &[]),
    )
}

fn primitive_read_definition(
    operation: &str,
    title: &str,
    properties: serde_json::Value,
    required: &[&str],
) -> ToolDefinition {
    def(
        &format!("tracedecay_{operation}"),
        title,
        "Invoke the daemon-retained typed primitive owner and preserve its canonical evidence envelope.",
        required_object_schema(properties, required),
    )
}

pub(super) fn def_session_lookup() -> ToolDefinition {
    primitive_read_definition(
        "session_lookup",
        "Look up a session",
        json!({
            "session_id": string_property("Exact session identity."),
            "meta": {"type": "object", "description": "Canonical retrieval metadata."}
        }),
        &["session_id", "meta"],
    )
}

pub(super) fn def_qualified_name_read() -> ToolDefinition {
    primitive_read_definition(
        "qualified_name",
        "Read qualified symbols",
        json!({
            "qualified_name": string_property("Exact qualified symbol name."),
            "page": {"type": "object", "description": "Canonical page request."}
        }),
        &["qualified_name", "page"],
    )
}

pub(super) fn def_call_chain_read() -> ToolDefinition {
    primitive_read_definition(
        "call_chain",
        "Read call chain",
        json!({
            "from_node_id": string_property("Exact caller-side graph node identity."),
            "to_node_id": string_property("Exact callee-side graph node identity."),
            "maximum_depth": {
                "type": "integer",
                "minimum": 0,
                "default": 8,
                "description": "Maximum directed traversal depth."
            }
        }),
        &["from_node_id", "to_node_id"],
    )
}

pub(super) fn def_file_dependents_read() -> ToolDefinition {
    primitive_read_definition(
        "file_dependents",
        "Read file dependents",
        json!({"file": string_property("Project-relative file path.")}),
        &["file"],
    )
}

pub(super) fn def_source_lines_read() -> ToolDefinition {
    primitive_read_definition(
        "source_lines",
        "Read source lines",
        json!({
            "file": string_property("Exact file occurrence identity."),
            "span": {"type": "object", "description": "Canonical source span."},
            "meta": {"type": "object", "description": "Canonical retrieval metadata."}
        }),
        &["file", "span", "meta"],
    )
}

pub(super) fn def_source_body_read() -> ToolDefinition {
    primitive_read_definition(
        "source_body",
        "Read symbol body",
        json!({"node_id": string_property("Exact graph node identity.")}),
        &["node_id"],
    )
}

pub(super) fn def_source_outline_read() -> ToolDefinition {
    primitive_read_definition(
        "source_outline",
        "Read source outline",
        json!({"file": string_property("Project-relative file path.")}),
        &["file"],
    )
}

pub(super) fn def_module_api_read() -> ToolDefinition {
    primitive_read_definition(
        "module_api",
        "Read module API",
        json!({"path": string_property("File path or directory prefix to inspect.")}),
        &["path"],
    )
}

pub(super) fn def_file_metadata_read() -> ToolDefinition {
    primitive_read_definition(
        "file_metadata",
        "Read file metadata",
        json!({
            "files": {
                "type": "array",
                "minItems": 1,
                "maxItems": 256,
                "items": {"type": "string"}
            }
        }),
        &["files"],
    )
}

pub(super) fn def_health_read() -> ToolDefinition {
    primitive_read_definition(
        "health_read",
        "Read project health",
        json!({"meta": {"type": "object", "description": "Canonical retrieval metadata."}}),
        &["meta"],
    )
}

pub(super) fn def_storage_status_read() -> ToolDefinition {
    def_always_load(
        "tracedecay_storage_status",
        "Read storage status",
        "Invoke the daemon-retained typed primitive owner and preserve its canonical evidence envelope.",
        required_object_schema(
            json!({
                "include_details": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include bounded storage details when true."
                }
            }),
            &[],
        ),
    )
}

pub(super) fn def_diagnostics_read() -> ToolDefinition {
    primitive_read_definition(
        "diagnostics_read",
        "Read canonical diagnostics",
        json!({
            "scope": {"description": "Workspace, package, or file diagnostic scope."},
            "maximum_diagnostics": {"type": "integer", "minimum": 1, "maximum": 10000}
        }),
        &["scope", "maximum_diagnostics"],
    )
}

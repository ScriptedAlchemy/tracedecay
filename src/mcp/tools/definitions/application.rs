use serde_json::json;

use super::{def, def_always_load, def_rw, required_object_schema, string_property};
use crate::mcp::tools::ToolDefinition;

fn git_read_bounds() -> serde_json::Value {
    json!({
        "max_entries": {
            "type": "integer",
            "minimum": 1,
            "maximum": 1000,
            "default": 1000,
            "description": "Maximum retained status paths, files, commits, blame lines, or hunk references."
        },
        "max_bytes": {
            "type": "integer",
            "minimum": 1,
            "maximum": 4194304,
            "default": 4194304,
            "description": "Maximum serialized typed result bytes."
        }
    })
}

fn git_read_definition(
    operation: &str,
    title: &str,
    description: &str,
    mut properties: serde_json::Value,
    required: &[&str],
) -> ToolDefinition {
    if let (Some(properties), Some(bounds)) =
        (properties.as_object_mut(), git_read_bounds().as_object())
    {
        properties.extend(bounds.clone());
    }
    def(
        &format!("tracedecay_git_{operation}"),
        title,
        description,
        required_object_schema(properties, required),
    )
}

pub(super) fn def_git_status() -> ToolDefinition {
    git_read_definition(
        "status",
        "Read typed Git status",
        "Read the PR9 typed status summary through the exact registered project/worktree authority.",
        json!({}),
        &[],
    )
}

pub(super) fn def_git_diff() -> ToolDefinition {
    git_read_definition(
        "diff",
        "Read typed Git diff",
        "Read a bounded PR9 structured diff through the exact registered project/worktree authority.",
        json!({
            "scope": {
                "type": "string",
                "enum": ["working_tree", "staged", "commit_range"],
                "default": "working_tree"
            },
            "base": {
                "type": "string",
                "description": "Exact base commit object id; required for commit_range."
            },
            "head": {
                "type": "string",
                "description": "Exact head commit object id; required for commit_range."
            }
        }),
        &[],
    )
}

pub(super) fn def_git_history() -> ToolDefinition {
    git_read_definition(
        "history",
        "Read typed Git history",
        "Read bounded PR9 commit history through the exact registered project/worktree authority.",
        json!({
            "count": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1000,
                "default": 100
            },
            "path": {
                "type": "string",
                "description": "Optional admitted-root-relative path filter."
            },
            "follow": {"type": "boolean", "default": false},
            "first_parent": {"type": "boolean", "default": false}
        }),
        &[],
    )
}

pub(super) fn def_git_blame() -> ToolDefinition {
    git_read_definition(
        "blame",
        "Read typed Git blame",
        "Read bounded PR9 line provenance through the exact registered project/worktree authority.",
        json!({
            "path": string_property("Admitted-root-relative file path."),
            "follow_renames": {"type": "boolean", "default": false}
        }),
        &["path"],
    )
}

pub(super) fn def_git_hunks() -> ToolDefinition {
    git_read_definition(
        "hunks",
        "Read typed Git hunks",
        "Mint bounded PR9 HunkRef evidence for a working-tree or staged diff; commit-range hunks are not applicable.",
        json!({
            "scope": {
                "type": "string",
                "enum": ["working_tree", "staged"],
                "default": "working_tree"
            },
            "preview_id": string_property("Opaque preview identity bound into every HunkRef."),
            "snapshot_digest": string_property("Exact sha256 repository snapshot digest.")
        }),
        &["preview_id", "snapshot_digest"],
    )
}

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

fn configuration_definition(
    operation: &str,
    title: &str,
    description: &str,
    properties: serde_json::Value,
    required: &[&str],
    writes: bool,
) -> ToolDefinition {
    let name = format!("tracedecay_configuration_{operation}");
    let schema = required_object_schema(properties, required);
    if writes {
        def_rw(&name, title, description, schema)
    } else {
        def(&name, title, description, schema)
    }
}

pub(super) fn configuration_definitions() -> Vec<ToolDefinition> {
    let key = || string_property("Canonical typed configuration setting key.");
    let revision = || string_property("Exact expected configuration revision for CAS.");
    vec![
        configuration_definition(
            "list",
            "List configuration settings",
            "Invoke the daemon-retained configuration authority.",
            json!({}),
            &[],
            false,
        ),
        configuration_definition(
            "explain",
            "Explain configuration setting",
            "Resolve one effective value and its provenance through the configuration authority.",
            json!({"key": key()}),
            &["key"],
            false,
        ),
        configuration_definition(
            "get",
            "Get configuration setting",
            "Read one effective typed value through the configuration authority.",
            json!({"key": key()}),
            &["key"],
            false,
        ),
        configuration_definition(
            "set",
            "Set configuration value",
            "Apply one authorized typed value with exact revision CAS.",
            json!({
                "layer": {"description": "Canonical configuration layer identity."},
                "key": key(),
                "value": {"description": "Typed configuration value."},
                "expected_revision": revision()
            }),
            &["layer", "key", "value", "expected_revision"],
            true,
        ),
        configuration_definition(
            "unset",
            "Unset configuration value",
            "Remove one authorized typed value with exact revision CAS.",
            json!({
                "layer": {"description": "Canonical configuration layer identity."},
                "key": key(),
                "expected_revision": revision()
            }),
            &["layer", "key", "expected_revision"],
            true,
        ),
        configuration_definition(
            "batch",
            "Apply configuration batch",
            "Apply one authorized atomic batch with exact revision CAS.",
            json!({
                "mutations": {"type": "array", "minItems": 1, "items": {"type": "object"}},
                "expected_revision": revision()
            }),
            &["mutations", "expected_revision"],
            true,
        ),
        configuration_definition(
            "write_credential",
            "Write configuration credential",
            "Resolve one opaque write handle into redacted credential-reference metadata.",
            json!({
                "expected_reference_id": {"type": ["string", "null"]},
                "kind": {"description": "Typed credential kind."},
                "write_handle": string_property("Opaque credential write handle; never plaintext credential material."),
                "expected_revision": revision()
            }),
            &["kind", "write_handle", "expected_revision"],
            true,
        ),
        configuration_definition(
            "observed_state",
            "Read configuration activation state",
            "Read desired-versus-observed component activation through the configuration authority.",
            json!({}),
            &[],
            false,
        ),
        configuration_definition(
            "protected_preview",
            "Preview protected configuration change",
            "Create a revalidated redacted protected-change preview.",
            json!({
                "change": {"type": "object"},
                "expected_revision": revision()
            }),
            &["change", "expected_revision"],
            false,
        ),
        configuration_definition(
            "protected_apply",
            "Apply protected configuration change",
            "Apply an actor-bound protected preview with exact CAS evidence.",
            json!({
                "plan_id": {"type": "string"},
                "expected_base_revision_id": revision(),
                "operation_digest": {"type": "string"},
                "idempotency_key": {"type": "string"}
            }),
            &[
                "plan_id",
                "expected_base_revision_id",
                "operation_digest",
                "idempotency_key",
            ],
            true,
        ),
        configuration_definition(
            "rollback_preview",
            "Preview configuration rollback",
            "Create a forward rollback preview against one historical revision.",
            json!({
                "target_revision_id": {"type": "string"},
                "mode": {"description": "Typed forward rollback mode."}
            }),
            &["target_revision_id", "mode"],
            false,
        ),
        configuration_definition(
            "rollback_apply",
            "Apply configuration rollback",
            "Apply an actor-bound forward rollback preview with exact CAS evidence.",
            json!({
                "plan_id": {"type": "string"},
                "expected_base_revision_id": revision(),
                "operation_digest": {"type": "string"},
                "idempotency_key": {"type": "string"}
            }),
            &[
                "plan_id",
                "expected_base_revision_id",
                "operation_digest",
                "idempotency_key",
            ],
            true,
        ),
        configuration_definition(
            "audit",
            "Read configuration audit",
            "Read reauthorized append-only redacted configuration audit events.",
            json!({
                "after_event_id": {"type": ["string", "null"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000}
            }),
            &["limit"],
            false,
        ),
    ]
}

use serde_json::json;

use super::{def, def_always_load, def_rw, required_object_schema, string_property};
use crate::mcp::tools::ToolDefinition;

fn closed_object_schema(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    let mut schema = required_object_schema(properties, required);
    schema["additionalProperties"] = json!(false);
    schema
}

fn page_request_schema() -> serde_json::Value {
    closed_object_schema(
        json!({
            "page_size": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1000
            },
            "cursor": {
                "type": ["string", "null"],
                "minLength": 1,
                "maxLength": 4096
            }
        }),
        &["page_size"],
    )
}

fn temporal_mode_schema() -> serde_json::Value {
    json!({
        "oneOf": [
            closed_object_schema(json!({"kind": {"const": "current"}}), &["kind"]),
            closed_object_schema(
                json!({
                    "kind": {"const": "as_of"},
                    "cutoff": {"type": "integer"}
                }),
                &["kind", "cutoff"]
            ),
            closed_object_schema(json!({"kind": {"const": "evolution"}}), &["kind"]),
            closed_object_schema(json!({"kind": {"const": "forensic"}}), &["kind"])
        ]
    })
}

fn retrieval_meta_schema() -> serde_json::Value {
    closed_object_schema(
        json!({
            "temporal": temporal_mode_schema(),
            "page": page_request_schema(),
            "projection": {
                "type": "string",
                "enum": ["summary", "evidence", "references_only"]
            },
            "order": {
                "type": "string",
                "enum": [
                    "relevance",
                    "source_position",
                    "temporal_descending",
                    "stable_identity"
                ]
            }
        }),
        &["temporal", "page", "projection", "order"],
    )
}

fn source_span_schema() -> serde_json::Value {
    closed_object_schema(
        json!({
            "start_byte": {"type": "integer", "minimum": 0},
            "end_byte": {"type": "integer", "minimum": 0}
        }),
        &["start_byte", "end_byte"],
    )
}

fn diagnostics_scope_schema() -> serde_json::Value {
    json!({
        "oneOf": [
            {"const": "workspace"},
            closed_object_schema(
                json!({"file": string_property("Exact file diagnostic scope.")}),
                &["file"]
            )
        ],
        "description": "Workspace or exact file diagnostic scope."
    })
}

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
            "maximum": 4_194_304,
            "default": 4_194_304,
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
        "Read the typed status summary through the exact registered project/worktree authority.",
        json!({}),
        &[],
    )
}

pub(super) fn def_git_diff() -> ToolDefinition {
    git_read_definition(
        "diff",
        "Read typed Git diff",
        "Read a bounded structured diff through the exact registered project/worktree authority.",
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
        "Read bounded commit history through the exact registered project/worktree authority.",
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
        "Read bounded line provenance through the exact registered project/worktree authority.",
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
        "Mint bounded HunkRef evidence for a working-tree or staged diff; commit-range hunks are not applicable.",
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
    let schema = closed_object_schema(
        json!({
            "request_handle": {
                "type": "string",
                "minLength": 1,
                "maxLength": 256,
                "description": "Daemon-minted opaque request handle. Clients cannot reconstruct application requests from this value."
            }
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
                    "description": "Exact repository state snapshot used for compare-and-swap validation."
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

pub(super) fn def_feedback_advisory_cycle() -> ToolDefinition {
    def(
        "tracedecay_feedback_advisory_cycle",
        "Run advisory feedback cycle",
        "Run one authorized four-pillar feedback cycle for a saved document. The daemon resolves project scope and providers, then returns a canonical diagnostics result with a daemon-minted read handle.",
        closed_object_schema(
            json!({
                "document_uri": string_property("Canonical file URI for the saved document in the admitted project.")
            }),
            &["document_uri"],
        ),
    )
}

fn context_scout_address_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "Opaque exact Context Scout address returned by the daemon.",
        "additionalProperties": false,
        "required": [
            "profile_id", "provider_id", "protected_session_id", "thread_id",
            "turn_id", "agent_id", "logical_message_id", "project_id"
        ],
        "properties": {
            "profile_id": {"type": "array", "minItems": 16, "maxItems": 16, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "provider_id": {"type": "array", "minItems": 16, "maxItems": 16, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "protected_session_id": {"type": "array", "minItems": 32, "maxItems": 32, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "thread_id": {"type": "array", "minItems": 16, "maxItems": 16, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "turn_id": {"type": "array", "minItems": 16, "maxItems": 16, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "agent_id": {"type": "array", "minItems": 16, "maxItems": 16, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "logical_message_id": {"type": "array", "minItems": 16, "maxItems": 16, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "project_id": {"type": "array", "minItems": 16, "maxItems": 16, "items": {"type": "integer", "minimum": 0, "maximum": 255}}
        }
    })
}

fn context_scout_read_definition(name: &str, title: &str, description: &str) -> ToolDefinition {
    let mut properties = json!({"address": context_scout_address_schema()});
    if matches!(
        name,
        "tracedecay_context_scout_recent" | "tracedecay_context_scout_explain"
    ) {
        properties["limit"] = json!({
            "type": "integer",
            "minimum": 1,
            "maximum": 32,
            "default": 8
        });
    }
    def(
        name,
        title,
        description,
        required_object_schema(properties, &["address"]),
    )
}

pub(super) fn def_context_scout_status() -> ToolDefinition {
    context_scout_read_definition(
        "tracedecay_context_scout_status",
        "Read Context Scout status",
        "Read authenticated Context Scout status for the exact owning session.",
    )
}

pub(super) fn def_context_scout_recent() -> ToolDefinition {
    context_scout_read_definition(
        "tracedecay_context_scout_recent",
        "Read recent Context Scout state",
        "Read authenticated pending and delivered Scout state for the exact owning session.",
    )
}

pub(super) fn def_context_scout_explain() -> ToolDefinition {
    context_scout_read_definition(
        "tracedecay_context_scout_explain",
        "Explain Context Scout state",
        "Explain authenticated Scout routing, suppression, delivery, and explicit feedback state.",
    )
}

pub(super) fn def_context_scout_capability() -> ToolDefinition {
    context_scout_read_definition(
        "tracedecay_context_scout_capability",
        "Read Context Scout capability",
        "Read authenticated deterministic and configured-model Scout capability state.",
    )
}

pub(super) fn def_context_scout_budget() -> ToolDefinition {
    context_scout_read_definition(
        "tracedecay_context_scout_budget",
        "Read Context Scout budget",
        "Read authenticated Scout limits and the latest model usage receipt.",
    )
}

fn context_scout_control_definition(
    operation: &str,
    title: &str,
    description: &str,
    mut properties: serde_json::Value,
    required: &[&str],
) -> ToolDefinition {
    properties["address"] = context_scout_address_schema();
    let name = format!("tracedecay_context_scout_{operation}");
    def_rw(
        &name,
        title,
        description,
        required_object_schema(properties, required),
    )
}

pub(super) fn context_scout_control_definitions() -> Vec<ToolDefinition> {
    vec![
        context_scout_control_definition(
            "pause",
            "Pause Context Scout",
            "Persist a paused Scout state through the canonical configuration authority.",
            json!({"expected_revision": string_property("Exact configuration revision for CAS.")}),
            &["address", "expected_revision"],
        ),
        context_scout_control_definition(
            "resume",
            "Resume Context Scout",
            "Persist an active Scout state through the canonical configuration authority.",
            json!({"expected_revision": string_property("Exact configuration revision for CAS.")}),
            &["address", "expected_revision"],
        ),
        context_scout_control_definition(
            "cancel",
            "Cancel Context Scout work",
            "Cancel one exact-address Scout work generation.",
            json!({"work": {"type": "object"}}),
            &["address", "work"],
        ),
        context_scout_control_definition(
            "claim",
            "Claim Context Scout delivery",
            "Claim one exact idle-window or explicit-request suggestion.",
            json!({"window": {"type": "string", "enum": ["idle_window", "on_request"]}}),
            &["address", "window"],
        ),
        context_scout_control_definition(
            "delivery",
            "Record Context Scout delivery",
            "Complete one exact-address delivery claim with its typed receipt.",
            json!({"claim": {"type": "object"}, "receipt": {"type": "object"}}),
            &["address", "claim", "receipt"],
        ),
        context_scout_control_definition(
            "feedback",
            "Record Context Scout feedback",
            "Record explicit feedback against one exact-address delivery receipt.",
            json!({"receipt": {"type": "object"}, "feedback": {"type": "object"}}),
            &["address", "receipt", "feedback"],
        ),
    ]
}

pub(super) fn def_feedback_impact() -> ToolDefinition {
    feedback_surface_definition(
        "tracedecay_feedback_impact",
        "Read feedback impact",
        "Project impact from the authorized completed feedback cycle identified by its daemon-minted opaque handle.",
        false,
    )
}

pub(super) fn def_affected_tests() -> ToolDefinition {
    feedback_surface_definition(
        "tracedecay_affected_tests",
        "Read affected tests",
        "Project affected tests from the authorized completed feedback cycle identified by its daemon-minted opaque handle.",
        false,
    )
}

pub(super) fn def_test_results() -> ToolDefinition {
    def(
        "tracedecay_test_results",
        "Read recent test results",
        "Read the latest daemon-retained managed test result for the admitted project root.",
        closed_object_schema(json!({}), &[]),
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
        closed_object_schema(properties, required),
    )
}

pub(super) fn def_session_lookup() -> ToolDefinition {
    primitive_read_definition(
        "session_lookup",
        "Look up a session",
        json!({
            "session_id": string_property("Exact session identity."),
            "meta": retrieval_meta_schema()
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
            "page": page_request_schema()
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
            "span": source_span_schema(),
            "meta": retrieval_meta_schema()
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
        json!({"meta": retrieval_meta_schema()}),
        &["meta"],
    )
}

pub(super) fn def_health_delta() -> ToolDefinition {
    primitive_read_definition(
        "health_delta",
        "Compare pinned project health",
        json!({
            "before_cursor": {
                "type": "string",
                "maxLength": 96,
                "description": "Stable cursor returned by an earlier health_delta call. Omit to pin the current state."
            },
            "path_prefix": {
                "type": "string",
                "maxLength": 4096,
                "description": "Optional project-relative scope prefix."
            },
            "meta": retrieval_meta_schema()
        }),
        &["meta"],
    )
}

pub(super) fn def_storage_status_read() -> ToolDefinition {
    def_always_load(
        "tracedecay_storage_status",
        "Read storage status",
        "Invoke the daemon-retained typed primitive owner and preserve its canonical evidence envelope.",
        closed_object_schema(
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
            "scope": diagnostics_scope_schema(),
            "maximum_diagnostics": {"type": "integer", "minimum": 1, "maximum": 1000},
            "cursor": {
                "type": ["string", "null"],
                "minLength": 1,
                "description": "Opaque cursor returned by the prior diagnostic page."
            }
        }),
        &["scope", "maximum_diagnostics"],
    )
}

fn callable_code_scope_schema() -> serde_json::Value {
    closed_object_schema(
        json!({
            "generation": {
                "type": "string",
                "minLength": 1,
                "maxLength": 512,
                "default": tracedecay_application::UNPINNED_LATEST_GENERATION_SENTINEL,
                "description": format!(
                    "Exact immutable code-index generation identity. Pass '{}' to bind the latest complete generation when you are not pinning a specific one.",
                    tracedecay_application::UNPINNED_LATEST_GENERATION_SENTINEL
                )
            },
            "path_prefix": {
                "type": ["string", "null"],
                "minLength": 1,
                "maxLength": 4096,
                "description": "Optional admitted-root-relative path prefix."
            }
        }),
        &["generation"],
    )
}

fn callable_code_meta_schema() -> serde_json::Value {
    closed_object_schema(
        json!({
            "projection": {
                "type": "string",
                "enum": ["summary", "evidence", "references_only"]
            },
            "order": {
                "type": "string",
                "enum": [
                    "relevance",
                    "source_position",
                    "temporal_descending",
                    "stable_identity"
                ]
            },
            "cursor": {
                "type": ["string", "null"],
                "minLength": 1,
                "description": "Authenticated opaque continuation returned as next_cursor by the prior page of an otherwise identical request."
            }
        }),
        &["projection", "order"],
    )
}

fn callable_symbol_graph_scope_schema() -> serde_json::Value {
    closed_object_schema(
        json!({
            "path_prefix": {
                "type": ["string", "null"],
                "minLength": 1,
                "maxLength": 4096,
                "description": "Optional admitted-root-relative path prefix."
            }
        }),
        &[],
    )
}

fn callable_symbol_graph_definition(
    operation: &str,
    title: &str,
    mut properties: serde_json::Value,
    required: &[&str],
) -> ToolDefinition {
    if let Some(properties) = properties.as_object_mut() {
        properties.insert("scope".to_owned(), callable_symbol_graph_scope_schema());
        properties.insert("meta".to_owned(), callable_code_meta_schema());
    }
    primitive_read_definition(operation, title, properties, required)
}

fn callable_code_definition(
    operation: &str,
    title: &str,
    description: &str,
    mut properties: serde_json::Value,
    required: &[&str],
) -> ToolDefinition {
    if let Some(properties) = properties.as_object_mut() {
        properties.insert("scope".to_owned(), callable_code_scope_schema());
        properties.insert("meta".to_owned(), callable_code_meta_schema());
    }
    def(
        &format!("tracedecay_{operation}"),
        title,
        description,
        closed_object_schema(properties, required),
    )
}

pub(super) fn def_code_exact_occurrence() -> ToolDefinition {
    callable_code_definition(
        "code_exact_occurrence",
        "Find exact code occurrences",
        "Invoke the callable code_exact_occurrence application surface and preserve its generation-bound canonical evidence envelope.",
        json!({
            "literal": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "Exact technical literal to match."
            },
            "kind": {
                "type": ["string", "null"],
                "enum": [
                    null,
                    "whole_symbol",
                    "qualified_name",
                    "path",
                    "compiler_error_code",
                    "compiler_error_text",
                    "runtime_error_code",
                    "runtime_error_text",
                    "cli_flag",
                    "tool_name",
                    "configuration_key",
                    "commit_identifier"
                ],
                "description": "Optional exact technical-term classification."
            }
        }),
        &["literal", "scope", "meta"],
    )
}

pub(super) fn def_code_phrase_search() -> ToolDefinition {
    callable_code_definition(
        "code_phrase_search",
        "Search code phrases",
        "Invoke the callable code_phrase_search application surface and preserve its generation-bound canonical evidence envelope.",
        json!({
            "query": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "Sanitized lexical query text."
            },
            "phrases": {
                "type": "array",
                "minItems": 1,
                "maxItems": 32,
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096
                },
                "description": "Required bounded phrases that constrain lexical matching."
            },
            "field_filters": {
                "type": "array",
                "maxItems": 32,
                "items": {
                    "type": "object",
                    "properties": {
                        "field": {
                            "type": "string",
                            "enum": [
                                "symbol_name",
                                "qualified_name",
                                "path",
                                "body_text",
                                "preamble_text",
                                "exact_term",
                                "subtoken"
                            ]
                        },
                        "include": {"type": "boolean"}
                    },
                    "required": ["field", "include"],
                    "additionalProperties": false
                },
                "description": "Typed lexical fields to include or exclude."
            },
            "fuzzy_budget": {
                "type": "integer",
                "minimum": 0,
                "maximum": 64,
                "description": "Maximum bounded fuzzy term expansions."
            }
        }),
        &[
            "query",
            "phrases",
            "field_filters",
            "fuzzy_budget",
            "scope",
            "meta",
        ],
    )
}

pub(super) fn def_code_symbol_search() -> ToolDefinition {
    callable_symbol_graph_definition(
        "code_symbol_search",
        "Search callable code symbols",
        json!({
            "query": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096
            },
            "lazy_index_ignored_dependencies": {
                "type": "boolean"
            }
        }),
        &["query", "lazy_index_ignored_dependencies", "scope", "meta"],
    )
}

pub(super) fn def_code_signature_search() -> ToolDefinition {
    callable_symbol_graph_definition(
        "code_signature_search",
        "Search callable code signatures",
        json!({
            "returns": {
                "type": ["string", "null"],
                "minLength": 1,
                "maxLength": 4096
            },
            "params": {
                "type": "array",
                "maxItems": 32,
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096
                }
            },
            "is_async": {
                "type": ["boolean", "null"]
            }
        }),
        &["params", "scope", "meta"],
    )
}

pub(super) fn def_code_implementations() -> ToolDefinition {
    callable_symbol_graph_definition(
        "code_implementations",
        "Read callable code implementations",
        json!({
            "selector": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "selector": {"const": "trait"},
                            "name": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": 4096
                            }
                        },
                        "required": ["selector", "name"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "selector": {"const": "method"},
                            "name": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": 4096
                            }
                        },
                        "required": ["selector", "name"],
                        "additionalProperties": false
                    }
                ]
            }
        }),
        &["selector", "scope", "meta"],
    )
}

pub(super) fn def_code_type_hierarchy() -> ToolDefinition {
    callable_symbol_graph_definition(
        "code_type_hierarchy",
        "Read callable code type hierarchy",
        json!({
            "node_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096
            },
            "maximum_depth": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10
            }
        }),
        &["node_id", "maximum_depth", "scope", "meta"],
    )
}

pub(super) fn def_code_callers() -> ToolDefinition {
    callable_symbol_graph_definition(
        "code_callers",
        "Read callable code callers",
        json!({
            "node_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096
            },
            "maximum_depth": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10
            },
            "resolve_trait_dispatch": {
                "type": "boolean"
            }
        }),
        &[
            "node_id",
            "maximum_depth",
            "resolve_trait_dispatch",
            "scope",
            "meta",
        ],
    )
}

pub(super) fn def_code_callees() -> ToolDefinition {
    callable_code_definition(
        "code_callees",
        "Read callable code callees",
        "Invoke the callable code_callees application surface and preserve its generation-bound canonical evidence envelope.",
        json!({
            "node_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "Exact graph node identity."
            },
            "maximum_depth": {
                "type": "integer",
                "minimum": 1,
                "maximum": 10
            },
            "resolve_trait_dispatch": {
                "type": "boolean",
                "description": "Whether to include concrete implementations reached through trait dispatch."
            }
        }),
        &[
            "node_id",
            "maximum_depth",
            "resolve_trait_dispatch",
            "scope",
            "meta",
        ],
    )
}

pub(super) fn def_code_facets() -> ToolDefinition {
    callable_code_definition(
        "code_facets",
        "Read callable code facets",
        "Aggregate one typed facet over the selected immutable code generation.",
        json!({
            "dimension": {
                "type": "string",
                "enum": ["kind", "language", "path"]
            }
        }),
        &["dimension", "scope", "meta"],
    )
}

pub(super) fn def_code_timeline() -> ToolDefinition {
    callable_code_definition(
        "code_timeline",
        "Read callable code timeline",
        "Read the selected immutable code generation's bounded timeline record.",
        json!({}),
        &["scope", "meta"],
    )
}

fn callable_code_navigation_definition(operation: &str, title: &str) -> ToolDefinition {
    callable_code_definition(
        operation,
        title,
        "Navigate generation-bound code evidence from one exact symbol occurrence.",
        json!({
            "node_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096
            }
        }),
        &["node_id", "scope", "meta"],
    )
}

pub(super) fn def_code_declaration() -> ToolDefinition {
    callable_code_navigation_definition("code_declaration", "Read callable code declaration")
}

pub(super) fn def_code_definition() -> ToolDefinition {
    callable_code_navigation_definition("code_definition", "Read callable code definition")
}

pub(super) fn def_code_type_definition() -> ToolDefinition {
    callable_code_navigation_definition(
        "code_type_definition",
        "Read callable code type definition",
    )
}

pub(super) fn def_code_references() -> ToolDefinition {
    callable_code_navigation_definition("code_references", "Read callable code references")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr12_primitive_definitions_expose_closed_typed_request_schemas() {
        let definitions = [
            def_feedback_diagnostics(),
            def_feedback_get(),
            def_feedback_expand(),
            def_feedback_list(),
            def_feedback_advisory_cycle(),
            def_feedback_impact(),
            def_affected_tests(),
            def_test_results(),
            def_session_lookup(),
            def_qualified_name_read(),
            def_call_chain_read(),
            def_file_dependents_read(),
            def_source_lines_read(),
            def_source_body_read(),
            def_source_outline_read(),
            def_module_api_read(),
            def_file_metadata_read(),
            def_health_read(),
            def_health_delta(),
            def_storage_status_read(),
            def_diagnostics_read(),
            def_code_exact_occurrence(),
            def_code_phrase_search(),
            def_code_symbol_search(),
            def_code_signature_search(),
            def_code_implementations(),
            def_code_type_hierarchy(),
            def_code_callers(),
            def_code_callees(),
            def_code_facets(),
            def_code_timeline(),
            def_code_declaration(),
            def_code_definition(),
            def_code_type_definition(),
            def_code_references(),
        ];

        for definition in definitions {
            assert_eq!(
                definition.input_schema["additionalProperties"],
                json!(false),
                "{} must reject fields denied by its typed DTO",
                definition.name
            );
        }

        let session = def_session_lookup();
        let meta = &session.input_schema["properties"]["meta"];
        assert_eq!(
            meta["required"],
            json!(["temporal", "page", "projection", "order"])
        );
        assert_eq!(meta["additionalProperties"], json!(false));
        assert_eq!(meta["properties"]["page"]["required"], json!(["page_size"]));
        assert_eq!(
            meta["properties"]["page"]["additionalProperties"],
            json!(false)
        );

        let qualified_name = def_qualified_name_read();
        assert_eq!(
            qualified_name.input_schema["properties"]["page"]["required"],
            json!(["page_size"])
        );

        let source_lines = def_source_lines_read();
        assert_eq!(
            source_lines.input_schema["properties"]["span"]["required"],
            json!(["start_byte", "end_byte"])
        );
        assert_eq!(
            source_lines.input_schema["properties"]["span"]["additionalProperties"],
            json!(false)
        );

        let diagnostics = def_diagnostics_read();
        let scope = &diagnostics.input_schema["properties"]["scope"];
        assert_eq!(scope["oneOf"].as_array().map(Vec::len), Some(2));
        assert_eq!(scope["oneOf"][0]["const"], "workspace");
        assert_eq!(scope["oneOf"][1]["required"], json!(["file"]));
    }

    #[test]
    fn callable_code_definitions_use_distinct_surface_operation_names() {
        let definitions = [
            def_code_exact_occurrence(),
            def_code_phrase_search(),
            def_code_symbol_search(),
            def_code_signature_search(),
            def_code_implementations(),
            def_code_type_hierarchy(),
            def_code_callers(),
            def_code_callees(),
            def_code_facets(),
            def_code_timeline(),
            def_code_declaration(),
            def_code_definition(),
            def_code_type_definition(),
            def_code_references(),
        ];

        assert_eq!(
            definitions.map(|definition| definition.name),
            [
                "tracedecay_code_exact_occurrence",
                "tracedecay_code_phrase_search",
                "tracedecay_code_symbol_search",
                "tracedecay_code_signature_search",
                "tracedecay_code_implementations",
                "tracedecay_code_type_hierarchy",
                "tracedecay_code_callers",
                "tracedecay_code_callees",
                "tracedecay_code_facets",
                "tracedecay_code_timeline",
                "tracedecay_code_declaration",
                "tracedecay_code_definition",
                "tracedecay_code_type_definition",
                "tracedecay_code_references",
            ]
        );
    }

    #[test]
    fn callable_code_definitions_expose_typed_request_schemas() {
        let exact = def_code_exact_occurrence();
        assert_eq!(exact.input_schema["additionalProperties"], json!(false));
        assert_eq!(
            exact.input_schema["required"],
            json!(["literal", "scope", "meta"])
        );
        assert_eq!(
            exact.input_schema["properties"]["kind"]["enum"],
            json!([
                null,
                "whole_symbol",
                "qualified_name",
                "path",
                "compiler_error_code",
                "compiler_error_text",
                "runtime_error_code",
                "runtime_error_text",
                "cli_flag",
                "tool_name",
                "configuration_key",
                "commit_identifier"
            ])
        );
        assert_eq!(
            exact.input_schema["properties"]["meta"]["required"],
            json!(["projection", "order"])
        );
        assert_eq!(
            exact.input_schema["properties"]["meta"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            exact.input_schema["properties"]["scope"]["additionalProperties"],
            json!(false)
        );
        assert!(
            exact.input_schema["properties"]["meta"]["properties"]
                .get("page")
                .is_none()
        );

        let phrase = def_code_phrase_search();
        assert_eq!(
            phrase.input_schema["required"],
            json!([
                "query",
                "phrases",
                "field_filters",
                "fuzzy_budget",
                "scope",
                "meta"
            ])
        );
        assert_eq!(phrase.input_schema["properties"]["phrases"]["maxItems"], 32);

        let symbol_search = def_code_symbol_search();
        assert_eq!(
            symbol_search.input_schema["required"],
            json!(["query", "lazy_index_ignored_dependencies", "scope", "meta"])
        );
        assert_eq!(
            symbol_search.input_schema["properties"]["meta"]["required"],
            json!(["projection", "order"])
        );
        assert!(
            symbol_search.input_schema["properties"]["meta"]["properties"]
                .get("page")
                .is_none()
        );

        let signature_search = def_code_signature_search();
        assert_eq!(
            signature_search.input_schema["required"],
            json!(["params", "scope", "meta"])
        );
        assert_eq!(
            signature_search.input_schema["properties"]["params"]["maxItems"],
            32
        );

        let implementations = def_code_implementations();
        assert_eq!(
            implementations.input_schema["properties"]["selector"]["oneOf"][0]["properties"]["selector"]
                ["const"],
            "trait"
        );

        let type_hierarchy = def_code_type_hierarchy();
        assert_eq!(
            type_hierarchy.input_schema["properties"]["maximum_depth"]["maximum"],
            10
        );

        let callers = def_code_callers();
        assert_eq!(
            callers.input_schema["required"],
            json!([
                "node_id",
                "maximum_depth",
                "resolve_trait_dispatch",
                "scope",
                "meta"
            ])
        );

        let callees = def_code_callees();
        assert_eq!(
            callees.input_schema["required"],
            json!([
                "node_id",
                "maximum_depth",
                "resolve_trait_dispatch",
                "scope",
                "meta"
            ])
        );
        assert_eq!(
            callees.input_schema["properties"]["maximum_depth"]["maximum"],
            10
        );

        let phrase = def_code_phrase_search();
        assert_eq!(
            phrase.input_schema["properties"]["fuzzy_budget"]["maximum"],
            64
        );
        assert_eq!(
            phrase.input_schema["properties"]["field_filters"]["items"]["additionalProperties"],
            json!(false)
        );

        let facets = def_code_facets();
        assert_eq!(
            facets.input_schema["properties"]["dimension"]["enum"],
            json!(["kind", "language", "path"])
        );

        let references = def_code_references();
        assert_eq!(
            references.input_schema["required"],
            json!(["node_id", "scope", "meta"])
        );
    }
}

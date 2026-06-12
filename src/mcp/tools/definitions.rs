//! MCP tool definitions (JSON Schema descriptors).
//!
//! Each `def_*` function returns a `ToolDefinition` with the tool name,
//! description, JSON Schema for its input parameters, MCP annotations
//! (readOnlyHint, title), and optional `_meta` (anthropic/alwaysLoad).

use serde_json::{json, Value};

use super::ToolDefinition;

/// Read-only annotations shared by every tool.
fn read_only(title: &str) -> Value {
    json!({
        "readOnlyHint": true,
        "title": title
    })
}

/// Build a `ToolDefinition` with `readOnlyHint` annotation and no `_meta`.
fn def(name: &str, title: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        annotations: Some(read_only(title)),
        meta: None,
    }
}

/// Write/exec annotations: tools that mutate files or run subprocesses.
fn read_write(title: &str) -> Value {
    json!({
        "readOnlyHint": false,
        "title": title
    })
}

/// Build a `ToolDefinition` for a tool that writes files or executes
/// subprocesses (`readOnlyHint: false`, no `_meta`).
fn def_rw(name: &str, title: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        annotations: Some(read_write(title)),
        meta: None,
    }
}

/// Build a `ToolDefinition` with `readOnlyHint` AND `anthropic/alwaysLoad`.
fn def_always_load(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        annotations: Some(read_only(title)),
        meta: Some(json!({ "anthropic/alwaysLoad": true })),
    }
}

/// Computes the call budget based on project size.
pub fn explore_call_budget(total_nodes: u64) -> u8 {
    match total_nodes {
        0..=5_000 => 3,
        5_001..=20_000 => 4,
        20_001..=80_000 => 5,
        80_001..=250_000 => 7,
        _ => 10,
    }
}

/// Generates the `tokensave_context` description with a dynamic call budget.
pub fn context_description(node_count: u64, budget: u8) -> String {
    format!(
        "Build an AI-ready context for a task description. Returns relevant symbols, \
         relationships, and optionally code snippets.\n\n\
         CALL BUDGET: {budget} calls maximum for this project ({node_count} nodes). \
         Stop after {budget} calls. If the question is not fully answered, synthesise \
         from what you have — do not exceed the budget."
    )
}

/// Returns tool definitions with a dynamic call budget for `tokensave_context`.
pub fn get_tool_definitions_with_budget(node_count: u64, budget: u8) -> Vec<ToolDefinition> {
    let mut defs = get_tool_definitions();
    // Replace the context tool's description with the budgeted version
    for def in &mut defs {
        if def.name == "tokensave_context" {
            def.description = context_description(node_count, budget);
        }
    }
    defs
}

/// Returns the list of all tool definitions exposed by this MCP server.
///
/// Tools whose backing dependency is missing on the current host are
/// filtered out so the model never sees a tool that will immediately
/// fail when called. Currently this only affects `tokensave_ast_grep_rewrite`,
/// which shells out to the `ast-grep` binary.
pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = vec![
        def_search(),
        def_context(),
        def_callers(),
        def_callees(),
        def_impact(),
        def_node(),
        def_status(),
        def_files(),
        def_affected(),
        def_dead_code(),
        def_diff_context(),
        def_module_api(),
        def_circular(),
        def_hotspots(),
        def_similar(),
        def_rename_preview(),
        def_unused_imports(),
        def_rank(),
        def_largest(),
        def_coupling(),
        def_inheritance_depth(),
        def_distribution(),
        def_recursion(),
        def_complexity(),
        def_doc_coverage(),
        def_god_class(),
        def_changelog(),
        def_port_status(),
        def_port_order(),
        def_commit_context(),
        def_pr_context(),
        def_simplify_scan(),
        def_test_map(),
        def_type_hierarchy(),
        def_branch_search(),
        def_branch_diff(),
        def_branch_list(),
        def_str_replace(),
        def_multi_str_replace(),
        def_insert_at(),
        def_ast_grep_rewrite(),
        def_gini(),
        def_dependency_depth(),
        def_health(),
        def_redundancy(),
        def_runtime(),
        def_dsm(),
        def_test_risk(),
        def_session_start(),
        def_session_end(),
        def_body(),
        def_todos(),
        def_callers_for(),
        def_by_qualified_name(),
        def_signature(),
        def_impls(),
        def_diagnose(),
        def_derives(),
        def_run_affected_tests(),
        def_fact_store(),
        def_fact_feedback(),
        def_memory_status(),
        def_dashboard(),
        def_message_search(),
        def_lcm_status(),
        def_lcm_doctor(),
        def_lcm_load_session(),
        def_lcm_grep(),
        def_lcm_describe(),
        def_lcm_expand(),
        def_lcm_expand_query(),
        def_lcm_preflight(),
        def_lcm_compress(),
        def_lcm_session_boundary(),
        def_read(),
        def_outline(),
        def_implementations(),
        def_unsafe_patterns(),
        def_diagnostics(),
        def_config(),
        def_signature_search(),
        def_constructors(),
        def_field_sites(),
        def_call_chain(),
        def_file_dependents(),
        def_replace_symbol(),
        def_insert_at_symbol(),
        def_find_exact_symbol(),
    ];
    if !ast_grep_available() {
        definitions.retain(|d| d.name != "tokensave_ast_grep_rewrite");
    }
    debug_assert!(
        !definitions.is_empty(),
        "get_tool_definitions returned empty list"
    );
    debug_assert!(
        definitions.iter().all(|d| d.name.starts_with("tokensave_")),
        "all tool definitions must have 'tokensave_' prefix"
    );
    definitions
}

/// Returns true when the external `ast-grep` binary is on PATH. Result is
/// cached after the first check so we don't fork a subprocess on every
/// `tools/list` request.
pub fn ast_grep_available() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("ast-grep")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

// ── alwaysLoad tools (loaded into the model prompt immediately) ─────────

fn def_search() -> ToolDefinition {
    def_always_load(
        "tokensave_search",
        "Search Symbols",
        "Search for symbols (functions, structs, traits, etc.) in the code graph by name or keyword.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query string to match against symbol names"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            },
            "required": ["query"]
        }),
    )
}

fn def_context() -> ToolDefinition {
    def_always_load(
        "tokensave_context",
        "Task Context",
        &context_description(0, 3),
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Natural language description of the task or question"
                },
                "max_nodes": {
                    "type": "number",
                    "description": "Maximum number of symbols to include (default: 20)"
                },
                "include_code": {
                    "type": "boolean",
                    "description": "If true, include source code snippets for key symbols (default: false)"
                },
                "max_code_blocks": {
                    "type": "number",
                    "description": "Maximum number of code snippets when include_code is true (default: 5)"
                },
                "mode": {
                    "type": "string",
                    "enum": ["explore", "plan"],
                    "description": "Context mode: 'explore' (default) for general exploration, 'plan' for implementation planning (adds extension points, dependency order, test coverage)"
                },
                "keywords": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Extra search keywords for synonym expansion. Use this when the task uses conceptual terms that may not match symbol names — e.g. for 'authentication', pass [\"login\", \"session\", \"credential\", \"token\", \"auth\"]. The graph is searched for each keyword independently."
                },
                "exclude_node_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Node IDs to exclude from results (pass seen_node_ids from previous call for session deduplication)"
                },
                "merge_adjacent": {
                    "type": "boolean",
                    "description": "When true, merge code blocks from the same file whose line ranges are adjacent or overlapping (default: false)"
                },
                "max_per_file": {
                    "type": "number",
                    "description": "Maximum symbols from a single file in results. Prevents one large file from dominating (default: max_nodes/3, minimum 3)"
                }
            },
            "required": ["task"]
        }),
    )
}

fn def_status() -> ToolDefinition {
    def_always_load(
        "tokensave_status",
        "Graph Status",
        "Return aggregate statistics about the code graph (node/edge/file counts, DB size, etc.).",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

fn def_callers_for() -> ToolDefinition {
    def(
        "tokensave_callers_for",
        "Bulk callers",
        "Returns the caller set of every supplied node ID in one round-trip. \
         Useful for clustering or similarity queries that need many caller \
         sets at once. Returns a map of {node_id: [caller_id, …]}. Defaults \
         to `calls` edges; pass `kind` to filter by `uses`, `type_of`, etc.",
        json!({
            "type": "object",
            "properties": {
                "node_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Node IDs to look up callers for."
                },
                "kind": {
                    "type": "string",
                    "description": "Edge kind to filter by (default: \"calls\"). Pass an empty string to match all kinds."
                },
                "max_per_item": {
                    "type": "number",
                    "description": "Cap callers per item (default: 1000)."
                }
            },
            "required": ["node_ids"]
        }),
    )
}

fn def_by_qualified_name() -> ToolDefinition {
    def(
        "tokensave_by_qualified_name",
        "Lookup by qualified name",
        "Look up nodes by their qualified name. Multiple rows can share a \
         qualified name (overloads, generics, separate impl blocks). Useful \
         for cross-run lookups where the content-hash node ID has changed.",
        json!({
            "type": "object",
            "properties": {
                "qualified_name": {
                    "type": "string",
                    "description": "The exact qualified name to look up."
                }
            },
            "required": ["qualified_name"]
        }),
    )
}

fn def_impls() -> ToolDefinition {
    def(
        "tokensave_impls",
        "Trait Implementations",
        "List `impl` blocks matching a trait, a type, or both. With no filter \
         returns every impl in the graph (use sparingly). Both arguments \
         accept short names (e.g. `Display`) or qualified names. Surfaces \
         information that is otherwise hard to query: trait-method dispatch \
         targets, which types satisfy a given trait, and which traits a type \
         implements.",
        json!({
            "type": "object",
            "properties": {
                "trait": {
                    "type": "string",
                    "description": "Trait name to filter by (short or qualified). Omit to include all traits."
                },
                "type": {
                    "type": "string",
                    "description": "Implementing type to filter by (short or qualified). Omit to include all types."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 100)."
                }
            }
        }),
    )
}

fn def_signature() -> ToolDefinition {
    def(
        "tokensave_signature",
        "Signature",
        "Return the signature-level metadata for symbols matching a qualified \
         name — visibility, signature string (generics, params, return type, \
         where clauses), docstring, async flag, and kind. No bodies. Use this \
         instead of reading source files when you only need the public-API \
         surface of a function, method, or type. Multiple rows can be \
         returned (overloads, separate impls).",
        json!({
            "type": "object",
            "properties": {
                "qualified_name": {
                    "type": "string",
                    "description": "The exact qualified name to look up."
                },
                "node_id": {
                    "type": "string",
                    "description": "Optional: look up a single node by its ID instead of qualified_name."
                }
            }
        }),
    )
}

// ── Deferred tools (discovered via ToolSearch on demand) ────────────────

fn def_callers() -> ToolDefinition {
    def(
        "tokensave_callers",
        "Callers",
        "Find all callers of a given node (function, method, etc.) up to a specified depth.",
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The unique node ID to find callers for"
                },
                "max_depth": {
                    "type": "number",
                    "description": "Maximum traversal depth (default: 3)"
                }
            },
            "required": ["node_id"]
        }),
    )
}

fn def_callees() -> ToolDefinition {
    def(
        "tokensave_callees",
        "Callees",
        "Find all callees of a given node (function, method, etc.) up to a \
         specified depth. When a callee resolves to a trait method, the \
         concrete impl methods reachable through that trait are also \
         returned, tagged with `dispatch_via_trait: true` and a `dispatch_from` \
         pointing at the trait method. Pass `resolve_dispatch: false` to \
         disable this behaviour and get only direct call edges.",
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The unique node ID to find callees for"
                },
                "max_depth": {
                    "type": "number",
                    "description": "Maximum traversal depth (default: 3)"
                },
                "resolve_dispatch": {
                    "type": "boolean",
                    "description": "If true (default), append concrete impl methods for any trait-method callee."
                }
            },
            "required": ["node_id"]
        }),
    )
}

fn def_impact() -> ToolDefinition {
    def(
        "tokensave_impact",
        "Impact Radius",
        "Compute the impact radius of a node: all symbols that directly or indirectly depend on it.",
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The unique node ID to compute impact for"
                },
                "max_depth": {
                    "type": "number",
                    "description": "Maximum traversal depth (default: 3)"
                }
            },
            "required": ["node_id"]
        }),
    )
}

fn def_node() -> ToolDefinition {
    def(
        "tokensave_node",
        "Node Details",
        "Retrieve detailed information about a single node by its ID.",
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The unique node ID to retrieve"
                }
            },
            "required": ["node_id"]
        }),
    )
}

fn def_files() -> ToolDefinition {
    def(
        "tokensave_files",
        "File List",
        "List indexed project files. Use to explore file structure without reading file contents.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "pattern": {
                    "type": "string",
                    "description": "Filter files matching this glob pattern (e.g. '**/*.rs')"
                },
                "format": {
                    "type": "string",
                    "enum": ["flat", "grouped"],
                    "description": "Output format: flat (one per line) or grouped by directory (default: grouped)"
                }
            }
        }),
    )
}

fn def_affected() -> ToolDefinition {
    def(
        "tokensave_affected",
        "Affected Tests",
        "Find test files affected by changed source files via dependency graph traversal.",
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of changed file paths to analyze"
                },
                "depth": {
                    "type": "number",
                    "description": "Maximum dependency traversal depth (default: 5)"
                },
                "filter": {
                    "type": "string",
                    "description": "Custom glob pattern for test files (default: common test patterns)"
                }
            },
            "required": ["files"]
        }),
    )
}

fn def_dead_code() -> ToolDefinition {
    def(
        "tokensave_dead_code",
        "Dead Code",
        "Find symbols with no incoming edges (potentially unreachable code). \
         Always excludes `main` and `test*` functions. By default also excludes \
         `pub` items (they may be referenced outside the indexed scope) — pass \
         `include_public: true` to audit pub items with zero indexed callers, \
         which is what you want for workspace-internal cleanup.",
        json!({
            "type": "object",
            "properties": {
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Node kinds to check (default: [\"function\", \"method\"])"
                },
                "include_public": {
                    "type": "boolean",
                    "description": "When true, do NOT exclude pub items. Default false."
                }
            }
        }),
    )
}

fn def_diff_context() -> ToolDefinition {
    def(
        "tokensave_diff_context",
        "Diff Context",
        "Given changed file paths, return semantic context: which symbols were modified, what depends on them, and affected tests.",
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of changed file paths"
                },
                "depth": {
                    "type": "number",
                    "description": "Maximum impact traversal depth (default: 2)"
                }
            },
            "required": ["files"]
        }),
    )
}

fn def_module_api() -> ToolDefinition {
    def(
        "tokensave_module_api",
        "Module API",
        "Show the public API surface of a file or directory: all pub symbols sorted by file and line.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path or directory prefix to inspect"
                }
            },
            "required": ["path"]
        }),
    )
}

fn def_circular() -> ToolDefinition {
    def(
        "tokensave_circular",
        "Circular Deps",
        "Detect circular dependencies between files in the code graph.",
        json!({
            "type": "object",
            "properties": {
                "max_depth": {
                    "type": "number",
                    "description": "Maximum cycle detection depth (default: 10)"
                }
            }
        }),
    )
}

fn def_hotspots() -> ToolDefinition {
    def(
        "tokensave_hotspots",
        "Hotspots",
        "Find symbols with the highest connectivity (most incoming + outgoing edges).",
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum number of hotspots to return (default: 10)"
                }
            }
        }),
    )
}

fn def_similar() -> ToolDefinition {
    def(
        "tokensave_similar",
        "Similar Symbols",
        "Find symbols with similar names using full-text search and substring matching.",
        json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to find similar matches for"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results (default: 10)"
                }
            },
            "required": ["symbol"]
        }),
    )
}

fn def_rename_preview() -> ToolDefinition {
    def(
        "tokensave_rename_preview",
        "References",
        "Show all references to a symbol -- all edges where the node appears as source or target.",
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The unique node ID to find references for"
                }
            },
            "required": ["node_id"]
        }),
    )
}

fn def_unused_imports() -> ToolDefinition {
    def(
        "tokensave_unused_imports",
        "Unused Imports",
        "Find import/use nodes that are never referenced by any other node.",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

fn def_rank() -> ToolDefinition {
    def(
        "tokensave_rank",
        "Rank",
        "Rank nodes by edge count for a given relationship type (calls, implements, extends, etc.).",
        json!({
            "type": "object",
            "properties": {
                "edge_kind": {
                    "type": "string",
                    "enum": ["implements", "extends", "calls", "uses", "contains", "annotates", "derives_macro"],
                    "description": "The relationship type to rank by (e.g. 'implements' to find most-implemented interfaces)"
                },
                "direction": {
                    "type": "string",
                    "enum": ["incoming", "outgoing"],
                    "description": "Edge direction: 'incoming' ranks targets (default, e.g. most-implemented interface), 'outgoing' ranks sources (e.g. class that implements the most interfaces)"
                },
                "node_kind": {
                    "type": "string",
                    "description": "Optional filter for node kind (e.g. 'interface', 'class', 'trait', 'function', 'method')"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (e.g. 'src/main/java')"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            },
            "required": ["edge_kind"]
        }),
    )
}

fn def_largest() -> ToolDefinition {
    def(
        "tokensave_largest",
        "Largest Symbols",
        "Rank nodes by size (line count). Find the largest classes, longest methods, biggest enums, etc.",
        json!({
            "type": "object",
            "properties": {
                "node_kind": {
                    "type": "string",
                    "description": "Filter by node kind (e.g. 'class', 'method', 'function', 'interface', 'enum', 'struct')"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (e.g. 'src/main/java')"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            }
        }),
    )
}

fn def_coupling() -> ToolDefinition {
    def(
        "tokensave_coupling",
        "Coupling",
        "Rank files by coupling: fan_in (most depended on) or fan_out (most dependencies).",
        json!({
            "type": "object",
            "properties": {
                "direction": {
                    "type": "string",
                    "enum": ["fan_in", "fan_out"],
                    "description": "fan_in: files depended on by the most others. fan_out: files that depend on the most others (default: fan_in)"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (e.g. 'src/main/java')"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            }
        }),
    )
}

fn def_inheritance_depth() -> ToolDefinition {
    def(
        "tokensave_inheritance_depth",
        "Inheritance Depth",
        "Find the deepest class/interface inheritance hierarchies by walking extends chains.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (e.g. 'src/main/java')"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            }
        }),
    )
}

fn def_distribution() -> ToolDefinition {
    def(
        "tokensave_distribution",
        "Distribution",
        "Show node kind distribution (classes, methods, fields, etc.) per file or directory.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory or file path prefix to filter (e.g. 'src/main/java/com/example'). Omit for entire codebase."
                },
                "summary": {
                    "type": "boolean",
                    "description": "If true, aggregate counts across all matching files instead of per-file breakdown (default: false)"
                }
            }
        }),
    )
}

fn def_recursion() -> ToolDefinition {
    def(
        "tokensave_recursion",
        "Recursion",
        "Detect recursive and mutually-recursive call cycles in the call graph.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (e.g. 'src/main/java')"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of cycles to return (default: 10)"
                }
            }
        }),
    )
}

fn def_complexity() -> ToolDefinition {
    def(
        "tokensave_complexity",
        "Complexity",
        "Rank functions/methods by composite complexity score (lines + fan-out + fan-in).",
        json!({
            "type": "object",
            "properties": {
                "node_kind": {
                    "type": "string",
                    "description": "Filter by node kind (default: function and method)"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (e.g. 'src/main/java')"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            }
        }),
    )
}

fn def_doc_coverage() -> ToolDefinition {
    def(
        "tokensave_doc_coverage",
        "Doc Coverage",
        "Find public symbols missing documentation (docstrings).",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory or file path prefix to filter (e.g. 'src/main'). Omit for entire codebase."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 50)"
                }
            }
        }),
    )
}

fn def_god_class() -> ToolDefinition {
    def(
        "tokensave_god_class",
        "God Classes",
        "Find classes with the most members (methods + fields).",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (e.g. 'src/main/java')"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            }
        }),
    )
}

fn def_changelog() -> ToolDefinition {
    def(
        "tokensave_changelog",
        "Changelog",
        "Generate a semantic diff/changelog between two git refs, categorizing symbols as added, removed, or modified.",
        json!({
            "type": "object",
            "properties": {
                "from_ref": {
                    "type": "string",
                    "description": "Starting git ref (commit, branch, tag)"
                },
                "to_ref": {
                    "type": "string",
                    "description": "Ending git ref (commit, branch, tag)"
                }
            },
            "required": ["from_ref", "to_ref"]
        }),
    )
}

fn def_port_status() -> ToolDefinition {
    def(
        "tokensave_port_status",
        "Port Status",
        "Compare symbols between source and target directories to track porting progress.",
        json!({
            "type": "object",
            "properties": {
                "source_dir": {
                    "type": "string",
                    "description": "Path prefix for source code (e.g. 'src/python/')"
                },
                "target_dir": {
                    "type": "string",
                    "description": "Path prefix for target code (e.g. 'src/rust/')"
                },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Node kinds to compare (default: [\"function\", \"method\", \"class\", \"struct\", \"interface\", \"trait\", \"enum\", \"module\"])"
                }
            },
            "required": ["source_dir", "target_dir"]
        }),
    )
}

fn def_port_order() -> ToolDefinition {
    def(
        "tokensave_port_order",
        "Port Order",
        "Topological sort of symbols in a directory -- port leaves first, dependents after.",
        json!({
            "type": "object",
            "properties": {
                "source_dir": {
                    "type": "string",
                    "description": "Path prefix for source code (e.g. 'src/python/')"
                },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Node kinds to include (default: [\"function\", \"method\", \"class\", \"struct\", \"interface\", \"trait\", \"enum\", \"module\"])"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of symbols to return (default: 50)"
                }
            },
            "required": ["source_dir"]
        }),
    )
}

fn def_commit_context() -> ToolDefinition {
    def(
        "tokensave_commit_context",
        "Commit Context",
        "Semantic summary of uncommitted changes for drafting a commit message. Returns changed symbols, file roles, and recent commit style.",
        json!({
            "type": "object",
            "properties": {
                "staged_only": {
                    "type": "boolean",
                    "description": "If true, only analyze staged changes (default: false = all uncommitted changes)"
                }
            }
        }),
    )
}

fn def_pr_context() -> ToolDefinition {
    def(
        "tokensave_pr_context",
        "PR Context",
        "Semantic summary of changes between two git refs for drafting a pull request description.",
        json!({
            "type": "object",
            "properties": {
                "base_ref": {
                    "type": "string",
                    "description": "Base branch or ref to compare against (default: 'main')"
                },
                "head_ref": {
                    "type": "string",
                    "description": "Head branch or ref (default: 'HEAD')"
                }
            }
        }),
    )
}

fn def_simplify_scan() -> ToolDefinition {
    def(
        "tokensave_simplify_scan",
        "Simplify Scan",
        "Quality analysis of changed files: duplications, dead code, coupling, and complexity hotspots.",
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Changed file paths to analyze"
                }
            },
            "required": ["files"]
        }),
    )
}

fn def_test_map() -> ToolDefinition {
    def(
        "tokensave_test_map",
        "Test Map",
        "Map source symbols to their test functions. Shows which tests cover which source code.",
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Source file path to find test coverage for"
                },
                "node_id": {
                    "type": "string",
                    "description": "Specific node ID to find test coverage for (alternative to file)"
                }
            }
        }),
    )
}

fn def_type_hierarchy() -> ToolDefinition {
    def(
        "tokensave_type_hierarchy",
        "Type Hierarchy",
        "Show the full type hierarchy for a trait/interface/class: all implementors and extenders, recursively.",
        json!({
            "type": "object",
            "properties": {
                "node_id": {
                    "type": "string",
                    "description": "The type node ID to build the hierarchy for"
                },
                "max_depth": {
                    "type": "number",
                    "description": "Maximum inheritance depth to traverse (default: 5)"
                }
            },
            "required": ["node_id"]
        }),
    )
}

fn def_branch_search() -> ToolDefinition {
    def(
        "tokensave_branch_search",
        "Cross-Branch Search",
        "Search for symbols in another branch's code graph. Opens the target branch's DB and runs a search query against it.",
        json!({
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Branch name to search in (must be tracked via `tokensave branch add`)"
                },
                "query": {
                    "type": "string",
                    "description": "Search query string to match against symbol names"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 10)"
                }
            },
            "required": ["branch", "query"]
        }),
    )
}

fn def_branch_diff() -> ToolDefinition {
    def(
        "tokensave_branch_diff",
        "Branch Diff",
        "Compare the code graphs of two branches. Shows symbols added, removed, and changed (signature differs) between base and head.",
        json!({
            "type": "object",
            "properties": {
                "base": {
                    "type": "string",
                    "description": "Base branch name (e.g. 'main'). Defaults to the project's default branch."
                },
                "head": {
                    "type": "string",
                    "description": "Head branch name (e.g. 'feature/foo'). Defaults to the current branch."
                },
                "file": {
                    "type": "string",
                    "description": "Optional file path filter — only show diffs for symbols in this file"
                },
                "kind": {
                    "type": "string",
                    "description": "Optional kind filter — only show diffs for this symbol kind (e.g. 'function', 'struct')"
                }
            }
        }),
    )
}

fn def_branch_list() -> ToolDefinition {
    def(
        "tokensave_branch_list",
        "List Tracked Branches",
        "List all tracked branches with their DB sizes, parent branch, and last sync time. Returns an empty list if multi-branch is not active.",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

fn def_str_replace() -> ToolDefinition {
    ToolDefinition {
        name: "tokensave_str_replace".to_string(),
        description: "Replace a unique string in a file with new content. Fails if the old string is not found or matches more than once. This is the safest edit primitive — use this instead of sed/awk.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or project-relative file path"
                },
                "old_str": {
                    "type": "string",
                    "description": "Exact string to find and replace. Must match exactly once in the file."
                },
                "new_str": {
                    "type": "string",
                    "description": "Replacement string"
                }
            },
            "required": ["path", "old_str", "new_str"]
        }),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Edit File"
        })),
        meta: None,
    }
}

fn def_multi_str_replace() -> ToolDefinition {
    ToolDefinition {
        name: "tokensave_multi_str_replace".to_string(),
        description: "Apply multiple string replacements atomically in a single file. All replacements must match exactly once. If any replacement fails (0 or >1 matches), the entire operation is aborted and no changes are made.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or project-relative file path"
                },
                "replacements": {
                    "type": "array",
                    "description": "Array of [old_str, new_str] pairs to replace",
                    "items": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 2,
                        "maxItems": 2
                    }
                }
            },
            "required": ["path", "replacements"]
        }),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Multi-Edit File"
        })),
        meta: None,
    }
}

fn def_insert_at() -> ToolDefinition {
    ToolDefinition {
        name: "tokensave_insert_at".to_string(),
        description: "Insert content before or after a unique anchor in a file. The anchor can be a unique string or a 1-indexed line number. Fails if the anchor matches more than one line.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or project-relative file path"
                },
                "anchor": {
                    "type": "string",
                    "description": "Unique string or line number (1-indexed) to insert at"
                },
                "content": {
                    "type": "string",
                    "description": "Content to insert"
                },
                "before": {
                    "type": "boolean",
                    "description": "If true, insert before the anchor line; if false, insert after (default: false)"
                }
            },
            "required": ["path", "anchor", "content"]
        }),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Insert Into File"
        })),
        meta: None,
    }
}

fn def_gini() -> ToolDefinition {
    def(
        "tokensave_gini",
        "Gini Inequality",
        "Compute inequality (Gini coefficient) for any metric across files or symbols. Detects god files and uneven complexity distribution.",
        json!({
            "type": "object",
            "properties": {
                "metric": {
                    "type": "string",
                    "enum": ["complexity", "lines", "fan_in", "fan_out", "members"],
                    "description": "Metric to measure inequality for (default: complexity)"
                },
                "scope": {
                    "type": "string",
                    "enum": ["file", "symbol"],
                    "description": "Aggregate per file or per symbol (default: file)"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "limit": {
                    "type": "number",
                    "description": "Number of top outliers to return (default: 10)"
                }
            }
        }),
    )
}

fn def_dependency_depth() -> ToolDefinition {
    def(
        "tokensave_dependency_depth",
        "Dependency Depth",
        "Show the longest file-level dependency chains. Files at the end of long chains are fragile to upstream changes.",
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum number of chains to return (default: 10)"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                }
            }
        }),
    )
}

fn def_health() -> ToolDefinition {
    def(
        "tokensave_health",
        "Health Score",
        "Get quality signal (0-10000) with root cause breakdown (acyclicity, depth, equality, redundancy, modularity). Quality signal = geometric mean of 5 dimensions — maximize this ONE number.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "details": {
                    "type": "boolean",
                    "description": "If true, include full dimension breakdown (default: false)"
                }
            }
        }),
    )
}

fn def_runtime() -> ToolDefinition {
    def(
        "tokensave_runtime",
        "Runtime Snapshot",
        "Capture a process + database telemetry snapshot for the running tokensave MCP server: PID, resident memory, virtual size, sustained CPU% (sampled over ~200ms), thread count, system memory, DB / WAL / SHM file sizes, journal mode, and the DB-to-source byte ratio. Use this when triaging unexpected CPU or RAM consumption (issue #80). Single call — output is a JSON object.",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

fn def_dashboard() -> ToolDefinition {
    def(
        "tokensave_dashboard",
        "Dashboard",
        "Start (or manage) the tokensave dashboard server for the current project as a background task inside the MCP server. Returns the listening URL. Idempotent: if already running, returns the existing URL. Pass action:\"stop\" to shut down a running instance. Optional host/port (defaults match `tokensave dashboard`).",
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "stop"],
                    "description": "Action to perform (default: \"start\"). \"stop\" shuts down a previously started dashboard if any."
                },
                "host": {
                    "type": "string",
                    "description": "Host address to bind (default: \"127.0.0.1\")"
                },
                "port": {
                    "type": "number",
                    "description": "Port to listen on; 0 picks an ephemeral port (default: 7341)"
                }
            }
        }),
    )
}

fn def_redundancy() -> ToolDefinition {
    def(
        "tokensave_redundancy",
        "Redundancy Hunt",
        "Find functionally duplicated function/method bodies via AST isomorphism, control-flow match, call-sequence match, and token-shingle Jaccard similarity. Each pair is bucketed as 'definite' (AST-identical), 'likely' (CFG or algorithmic match), or 'naming_only' (low confidence). Use when consolidating helpers or auditing code health. Computed lazily and cached per (node, body source hash) — first call on a fresh index can be slow on large repos.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "min_lines": {
                    "type": "number",
                    "description": "Skip functions shorter than this many source lines (default: 8)"
                },
                "max_pairs": {
                    "type": "number",
                    "description": "Maximum number of duplicate pairs to return (default: 20, max: 500)"
                },
                "similarity_threshold": {
                    "type": "number",
                    "description": "Drop pairs scoring below this composite similarity (default: 0.6, range 0.0-1.0)"
                },
                "include_naming_only": {
                    "type": "boolean",
                    "description": "If true, include 'naming_only' / low-confidence matches in the output (default: false)"
                }
            }
        }),
    )
}

fn def_dsm() -> ToolDefinition {
    def(
        "tokensave_dsm",
        "Design Structure Matrix",
        "Get the Design Structure Matrix: file dependency summary showing clusters, density, and layering violations.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "format": {
                    "type": "string",
                    "enum": ["stats", "clusters", "matrix"],
                    "description": "Output format (default: stats)"
                },
                "max_files": {
                    "type": "number",
                    "description": "Maximum files in matrix format (default: 30)"
                }
            }
        }),
    )
}

fn def_test_risk() -> ToolDefinition {
    def(
        "tokensave_test_risk",
        "Test Risk",
        "Find high-risk source symbols with weak or no test coverage. Risk = (complexity + 1) × (fan_in + 1) × untested_multiplier. Answers: where should the next test go?",
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 20)"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "include_tested": {
                    "type": "boolean",
                    "description": "Include already-tested functions in results (default: false)"
                }
            }
        }),
    )
}

fn def_derives() -> ToolDefinition {
    def(
        "tokensave_derives",
        "Derives on Type",
        "List `#[derive(...)]` macros attached to a type and the trait + \
         method names each one synthesizes. Prevents dead-end searches for \
         autogenerated symbols (e.g. `.clone()` from `#[derive(Clone)]`). \
         Well-known derives (`Debug`, `Clone`, `Copy`, `Default`, `PartialEq`, \
         `Eq`, `PartialOrd`, `Ord`, `Hash`, `Serialize`, `Deserialize`, \
         `Display`, `Error`) carry full trait + method info; unknown / \
         proc-macro derives surface with `well_known: false` so callers can \
         still see the derive name.",
        json!({
            "type": "object",
            "properties": {
                "qualified_name": {
                    "type": "string",
                    "description": "The type's qualified name (or short name — same lookup as tokensave_by_qualified_name)."
                },
                "node_id": {
                    "type": "string",
                    "description": "Optional: look up the type by node ID instead."
                }
            }
        }),
    )
}

fn def_diagnose() -> ToolDefinition {
    def(
        "tokensave_diagnose",
        "Diagnose Cargo Output",
        "Parse raw `cargo check` / `cargo clippy` stderr text and map each \
         diagnostic to the smallest containing graph node, with callers \
         pre-attached so you can see what the failing code is reachable \
         from. Diagnostics without a `--> file:line:col` span are dropped. \
         Pass the full stderr capture; you do not need to pre-filter.",
        json!({
            "type": "object",
            "properties": {
                "cargo_output": {
                    "type": "string",
                    "description": "Raw stderr text from `cargo check` / `cargo clippy` / `rustc`."
                },
                "severity": {
                    "type": "string",
                    "enum": ["error", "warning", "all"],
                    "description": "Filter by severity (default: all)."
                },
                "include_callers": {
                    "type": "boolean",
                    "description": "Attach up to 5 callers per diagnostic (default: true)."
                },
                "max_diagnostics": {
                    "type": "number",
                    "description": "Cap on diagnostics in the response (default: 50)."
                }
            },
            "required": ["cargo_output"]
        }),
    )
}

fn def_run_affected_tests() -> ToolDefinition {
    def_rw(
        "tokensave_run_affected_tests",
        "Run Affected Tests",
        "Run `cargo test` for tests that cover the symbols in `changed_paths` \
         (or, if omitted, the files changed in the working tree). Closes the \
         loop opened by `tokensave_test_map` / `tokensave_test_risk` — emits \
         pass/fail per test alongside the source nodes each test covers. \
         Output is the libtest summary parsed into JSON.",
        json!({
            "type": "object",
            "properties": {
                "changed_paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Explicit file paths to compute affected tests from. Defaults to `git diff --name-only` against the working tree."
                },
                "profile": {
                    "type": "string",
                    "enum": ["debug", "release"],
                    "description": "Cargo profile (default: debug)."
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "Maximum wall time before the cargo subprocess is killed (default: 300)."
                },
                "max_tests": {
                    "type": "number",
                    "description": "Cap on tests dispatched in a single invocation (default: 100)."
                }
            }
        }),
    )
}

fn memory_fact_properties() -> Value {
    json!({
        "action": {
            "type": "string",
            "enum": ["add", "search", "probe", "related", "reason", "contradict", "update", "remove", "list"],
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
        }
    })
}

fn def_fact_store() -> ToolDefinition {
    def_rw(
        "tokensave_fact_store",
        "Fact Store",
        "Add, search, probe, relate, reason over, update, remove, or list holographic memory facts. The action field selects the operation.",
        json!({
            "type": "object",
            "properties": memory_fact_properties(),
            "required": ["action"]
        }),
    )
}

fn def_fact_feedback() -> ToolDefinition {
    def_rw(
        "tokensave_fact_feedback",
        "Fact Feedback",
        "Record helpful/unhelpful feedback for a memory fact and adjust its trust score.",
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
                    "description": "Feedback action."
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
                }
            },
            "required": ["fact_id"]
        }),
    )
}

fn def_memory_status() -> ToolDefinition {
    def_rw(
        "tokensave_memory_status",
        "Memory Status",
        "Repair derived holographic memory vectors and banks, then return fact/entity counts, trust distribution, and repair stats.",
        json!({
            "type": "object",
            "properties": {}
        }),
    )
}

fn def_message_search() -> ToolDefinition {
    def(
        "tokensave_message_search",
        "Message Search",
        "Search ingested Cursor/Codex/agent transcript messages stored in tokensave's project-local session-message FTS index.",
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Full-text query to search in ingested transcript messages."
                },
                "provider": {
                    "type": "string",
                    "description": "Message provider to search (default: cursor). Use 'hermes' for Hermes agent conversation history ingested from per-profile state.db stores.",
                    "enum": ["cursor", "claude", "codex", "vibe", "cline", "roo-code", "kilo", "hermes"]
                },
                "project_key": {
                    "type": "string",
                    "description": "Optional project key/path filter. For Cursor transcripts this is the project root path."
                },
                "include_subagents": {
                    "type": "boolean",
                    "description": "Whether to include child subagent sessions in results (default: true)."
                },
                "parent_session_id": {
                    "type": "string",
                    "description": "Optional parent session id filter. Primarily useful with scope=subagents_only."
                },
                "scope": {
                    "type": "string",
                    "description": "Relationship scope for search results (default: all).",
                    "enum": ["all", "parents_only", "subagents_only"]
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of messages to return (default: 10, max: 50)."
                }
            },
            "required": ["query"]
        }),
    )
}

fn lcm_storage_scope_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["project_local", "hermes_profile"],
        "description": "Storage scope for LCM session state. Defaults to project_local. Use hermes_profile only with an explicit absolute hermes_home."
    })
}

fn lcm_hermes_home_schema() -> Value {
    json!({
        "type": "string",
        "description": "absolute Hermes profile home directory required when storage_scope is hermes_profile."
    })
}

fn lcm_pattern_array_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": description
    })
}

fn lcm_storage_scope_requires_hermes_home() -> Value {
    json!([{
        "if": {
            "properties": {
                "storage_scope": { "const": "hermes_profile" }
            },
            "required": ["storage_scope"]
        },
        "then": {
            "required": ["hermes_home"]
        }
    }])
}

fn def_lcm_status() -> ToolDefinition {
    def(
        "tokensave_lcm_status",
        "LCM Status",
        "Return LCM schema, raw-message, summary, payload, and maintenance counts plus store token estimates, summary-DAG depth distribution with compression ratio, and effective engine config defaults from project-local or Hermes profile sessions.db storage.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id to inspect (default: cursor)."
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional provider-local session id filter."
                },
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home()
        }),
    )
}

fn def_lcm_doctor() -> ToolDefinition {
    def_rw(
        "tokensave_lcm_doctor",
        "LCM Doctor",
        "Run bounded LCM diagnostics, dry-run safe repairs, optionally apply safe FTS repairs, and report retention candidates without payload body exposure.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id to inspect (default: cursor)."
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional provider-local session id filter."
                },
                "mode": {
                    "type": "string",
                    "enum": ["diagnose", "repair", "retention", "clean"],
                    "description": "diagnose reports read-only health, repair plans or applies safe repairs, retention reports read-only retention candidates, clean reports or applies safe ignore/stateless/noise cleanup."
                },
                "apply": {
                    "type": "boolean",
                    "description": "When mode=repair or mode=clean, apply safe repairs/cleanup. Defaults to false for dry-run."
                },
                "doctor_clean_apply_enabled": {
                    "type": "boolean",
                    "description": "Safety gate for mode=clean + apply. Defaults to false unless LCM_DOCTOR_CLEAN_APPLY_ENABLED is set."
                },
                "ignore_session_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for sessions that should be diagnosed as ignored cleanup candidates."),
                "stateless_session_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for stateless sessions that should be diagnosed as cleanup candidates."),
                "ignore_message_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for low-value message content to treat as storage-only noise."),
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home()
        }),
    )
}

fn def_lcm_load_session() -> ToolDefinition {
    def(
        "tokensave_lcm_load_session",
        "LCM Load Session",
        "Load ordered lossless raw session messages with stable pagination and bounded content slices from project-local or Hermes profile LCM storage.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "after_store_id": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Return rows after this raw store id."
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
                },
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home(),
            "required": ["session_id"]
        }),
    )
}

fn def_lcm_grep() -> ToolDefinition {
    def(
        "tokensave_lcm_grep",
        "LCM Grep",
        "Search bounded LCM raw-message snippets and optional summary text in project-local or Hermes profile sessions.db storage.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
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
                "session_id": {
                    "type": "string",
                    "description": "Session id used when scope is current or session."
                },
                "include_summaries": {
                    "type": "boolean",
                    "description": "Include summary node text after raw-message matches (default: true)."
                },
                "sort": {
                    "type": "string",
                    "enum": ["recency", "relevance", "hybrid"],
                    "description": "How to order matches. Defaults to recency."
                },
                "source": {
                    "type": "string",
                    "description": "Optional source/platform filter from raw-message metadata."
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
                    "description": "Optional inclusive minimum raw-message timestamp. Integer strings and timezone-aware ISO/RFC3339 strings are accepted."
                },
                "end_time": {
                    "oneOf": [
                        { "type": "integer", "minimum": 0 },
                        { "type": "string" }
                    ],
                    "description": "Optional inclusive maximum raw-message timestamp. Integer strings and timezone-aware ISO/RFC3339 strings are accepted."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum hits."
                },
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home(),
            "required": ["query"]
        }),
    )
}

fn def_lcm_describe() -> ToolDefinition {
    def(
        "tokensave_lcm_describe",
        "LCM Describe",
        "Describe one session's LCM raw-message and summary-DAG shape from project-local or Hermes profile storage without exposing full payload bodies.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "target": {
                    "type": "object",
                    "description": "Optional describe target. Omit for session overview.",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["session", "summary_node", "external_payload"]
                        },
                        "node_id": {
                            "type": "string",
                            "description": "Summary node id when kind=summary_node."
                        },
                        "payload_ref": {
                            "type": "string",
                            "description": "External payload ref when kind=external_payload."
                        }
                    }
                },
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home(),
            "required": ["session_id"]
        }),
    )
}

fn def_lcm_expand() -> ToolDefinition {
    def(
        "tokensave_lcm_expand",
        "LCM Expand",
        "Expand one raw message, summary node, or external payload through the bounded LCM query API from project-local or Hermes profile storage.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "target": {
                    "type": "object",
                    "description": "Expansion target.",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["raw_message", "summary_node", "external_payload"]
                        },
                        "store_id": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Raw-message store id when kind=raw_message."
                        },
                        "node_id": {
                            "type": "string",
                            "description": "Summary node id when kind=summary_node."
                        },
                        "payload_ref": {
                            "type": "string",
                            "description": "Payload ref when kind=external_payload."
                        }
                    },
                    "required": ["kind"]
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
                "source_offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Zero-based pagination offset into a summary node's immediate source list (summary_node targets only)."
                },
                "source_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum immediate sources returned from source_offset (summary_node targets only); resume with the response's next_source_offset. If a returned source has content_truncated=true, continue via target.kind=raw_message for that source's store_id and content_offset."
                },
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home(),
            "required": ["session_id", "target"]
        }),
    )
}

fn def_lcm_expand_query() -> ToolDefinition {
    def(
        "tokensave_lcm_expand_query",
        "LCM Expand Query",
        "Assemble bounded LCM retrieval context for a prompt from project-local or Hermes profile storage; host integrations synthesize the final answer when needs_synthesis is true.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
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
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home(),
            "required": ["session_id", "prompt"]
        }),
    )
}

fn def_lcm_preflight() -> ToolDefinition {
    def_rw(
        "tokensave_lcm_preflight",
        "LCM Preflight",
        "Run compression preflight checks against project-local or Hermes profile LCM storage.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "messages": {
                    "type": "array",
                    "description": "Current active context messages to inspect before compression.",
                    "items": {"type": "object"}
                },
                "current_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional current context token estimate."
                },
                "threshold_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional token threshold that allows preflight to request compression when current_tokens meets or exceeds it and eligible backlog exists."
                },
                "max_assembly_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional active-context cap that triggers forced overflow recovery when current_tokens meets or exceeds it."
                },
                "leaf_chunk_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional token budget for the oldest raw-message leaf chunk selected for compression."
                },
                "max_source_messages": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional source-window cap for raw messages included in one compression unit."
                },
                "summary_fan_in": {
                    "type": "integer",
                    "minimum": 2,
                    "description": "Optional fan-in threshold for condensing lower-depth summary nodes into a higher-depth node."
                },
                "incremental_max_depth": {
                    "type": "integer",
                    "description": "Optional maximum condensation depth. Values < 0 allow all depths; default is 1."
                },
                "fresh_tail_count": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional count of newest unsummarized messages preserved outside leaf compression."
                },
                "dynamic_leaf_chunk_enabled": {
                    "type": "boolean",
                    "description": "When true, leaf chunk budget may grow up to dynamic_leaf_chunk_max under backlog pressure."
                },
                "dynamic_leaf_chunk_max": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional upper bound for dynamic leaf chunk token budget."
                },
                "context_length": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional model context window used with reserve_tokens_floor to derive the assembly cap when max_assembly_tokens is unset."
                },
                "reserve_tokens_floor": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional token headroom reserved inside context_length; derives an assembly cap of context_length - reserve_tokens_floor."
                },
                "ignore_session_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for sessions to skip from active LCM ingest/compression."),
                "stateless_session_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for stateless sessions to replay without durable LCM storage."),
                "ignore_message_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for low-value message content to keep in replay but skip from LCM storage."),
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home()
        }),
    )
}

fn def_lcm_compress() -> ToolDefinition {
    def_rw(
        "tokensave_lcm_compress",
        "LCM Compress",
        "Advance the LCM compression lifecycle in project-local or Hermes profile storage without invoking an auxiliary LLM.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id."
                },
                "messages": {
                    "type": "array",
                    "description": "Current active context messages to ingest before compression.",
                    "items": {"type": "object"}
                },
                "current_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional current context token estimate."
                },
                "focus_topic": {
                    "type": "string",
                    "description": "Optional focus for the summary request prompt."
                },
                "ignore_session_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for sessions to skip from active LCM ingest/compression."),
                "stateless_session_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for stateless sessions to replay without durable LCM storage."),
                "ignore_message_patterns": lcm_pattern_array_schema("Hermes-style glob patterns for low-value message content to keep in replay but skip from LCM storage."),
                "expected_current_frontier_store_id": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional optimistic guard. Compression no-ops if the durable frontier has changed."
                },
                "threshold_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional token threshold mirrored from Hermes config for parity with preflight calls."
                },
                "max_assembly_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional active-context cap that triggers forced overflow recovery when current_tokens meets or exceeds it."
                },
                "leaf_chunk_tokens": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional token budget for the oldest raw-message leaf chunk selected for compression."
                },
                "max_source_messages": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional source-window cap for raw messages included in one compression unit."
                },
                "summary_fan_in": {
                    "type": "integer",
                    "minimum": 2,
                    "description": "Optional fan-in threshold for condensing lower-depth summary nodes into a higher-depth node."
                },
                "incremental_max_depth": {
                    "type": "integer",
                    "description": "Optional maximum condensation depth. Values < 0 allow all depths; default is 1."
                },
                "fresh_tail_count": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional count of newest unsummarized messages preserved outside leaf compression."
                },
                "dynamic_leaf_chunk_enabled": {
                    "type": "boolean",
                    "description": "When true, leaf chunk budget may grow up to dynamic_leaf_chunk_max under backlog pressure."
                },
                "dynamic_leaf_chunk_max": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional upper bound for dynamic leaf chunk token budget."
                },
                "context_length": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional model context window used with reserve_tokens_floor to derive the assembly cap when max_assembly_tokens is unset."
                },
                "reserve_tokens_floor": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional token headroom reserved inside context_length; derives an assembly cap of context_length - reserve_tokens_floor."
                },
                "summarizer": {
                    "type": "object",
                    "description": "Deterministic summarizer mode: noop, fake, provided, or hermes_auxiliary.",
                    "properties": {
                        "mode": {
                            "type": "string",
                            "enum": ["noop", "fake", "provided", "hermes_auxiliary"]
                        },
                        "summary_text": {"type": "string"},
                        "route": {"type": "string"}
                    },
                    "required": ["mode"]
                },
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home(),
            "required": ["session_id"]
        }),
    )
}

fn def_lcm_session_boundary() -> ToolDefinition {
    def_rw(
        "tokensave_lcm_session_boundary",
        "LCM Session Boundary",
        "Report a compression-boundary session start. When the old session does not match the bound session the boundary skipped carry-over and a short compression cooldown starts for the new session.",
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider id, default cursor."
                },
                "session_id": {
                    "type": "string",
                    "description": "Provider-local session id the host bound after the boundary."
                },
                "old_session_id": {
                    "type": "string",
                    "description": "Session id the host reports as having crossed the compression boundary."
                },
                "boundary_reason": {
                    "type": "string",
                    "description": "Host boundary reason; only 'compression' boundaries are evaluated."
                },
                "bound_session_id": {
                    "type": "string",
                    "description": "Session id that was bound before this boundary; a mismatch with old_session_id records the cooldown."
                },
                "storage_scope": lcm_storage_scope_schema(),
                "hermes_home": lcm_hermes_home_schema()
            },
            "allOf": lcm_storage_scope_requires_hermes_home(),
            "required": ["session_id"]
        }),
    )
}

fn def_ast_grep_rewrite() -> ToolDefinition {
    ToolDefinition {
        name: "tokensave_ast_grep_rewrite".to_string(),
        description: "Perform structural code rewrite using ast-grep. The pattern and rewrite use ast-grep's SGPattern syntax. Fails if ast-grep is not installed.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or project-relative file path"
                },
                "pattern": {
                    "type": "string",
                    "description": "ast-grep search pattern (SGPattern syntax)"
                },
                "rewrite": {
                    "type": "string",
                    "description": "ast-grep rewrite rule"
                }
            },
            "required": ["path", "pattern", "rewrite"]
        }),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "AST Structural Rewrite"
        })),
        meta: None,
    }
}

fn def_session_start() -> ToolDefinition {
    ToolDefinition {
        name: "tokensave_session_start".to_string(),
        description: "Save current health metrics as baseline for later comparison via session_end. Call this before starting work.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Session Start"
        })),
        meta: None,
    }
}

fn def_session_end() -> ToolDefinition {
    ToolDefinition {
        name: "tokensave_session_end".to_string(),
        description: "Re-scan and compare current health against session baseline (saved by session_start). Returns diff showing what improved or degraded.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
        annotations: Some(json!({
            "readOnlyHint": false,
            "title": "Session End"
        })),
        meta: None,
    }
}

fn def_body() -> ToolDefinition {
    def(
        "tokensave_body",
        "Symbol Body",
        "Return the full source body of a symbol by name (function, struct, const, etc.). \
         Collapses search + node lookup + file read into a single call. \
         When the name is ambiguous, returns multiple matches ranked by relevance.",
        json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name to look up (e.g. 'resolve_provider_api_key', 'CCH_SEED', 'GraphStats'). Qualified names are also accepted."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of matching bodies to return when the name is ambiguous (default: 3, max: 20)"
                }
            },
            "required": ["symbol"]
        }),
    )
}

fn def_todos() -> ToolDefinition {
    def(
        "tokensave_todos",
        "TODOs and FIXMEs",
        "Find TODO, FIXME, XXX, HACK, WIP, NOTE, and unimplemented markers across the project. \
         Each result includes the marker kind, file, line, the comment text, and the enclosing \
         symbol name (function/method) for quick orientation.",
        json!({
            "type": "object",
            "properties": {
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Marker kinds to include (default: TODO, FIXME, XXX, HACK, WIP, NOTE, UNIMPLEMENTED). Matched case-insensitively."
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path (relative to project root)"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of markers to return (default: 200, max: 2000)"
                }
            }
        }),
    )
}

fn def_field_sites() -> ToolDefinition {
    def(
        "tokensave_field_sites",
        "Field Read/Write Sites",
        "Find every read and write site of a named field across the codebase. \
         Returns two arrays: write_sites (assignments to the field) and \
         read_sites (everything else). Each entry includes file, line, \
         enclosing symbol, and a source snippet. Useful when renaming, \
         removing, or adding an invariant to a field — the write-site list \
         is the exact blast radius. Pattern matches `.<field>` references; \
         field-by-name is shorthand for any struct's same-named field, while \
         `Struct::field` form narrows to a specific declaration.",
        json!({
            "type": "object",
            "properties": {
                "field": {
                    "type": "string",
                    "description": "Field name. Bare name ('last_sync_at') matches across structs; qualified form ('GraphStats::last_sync_at') narrows to one struct's field."
                },
                "writes_only": {
                    "type": "boolean",
                    "description": "When true, returns only write_sites and omits reads. Default false."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum sites per kind (default: 200, max: 2000)."
                }
            },
            "required": ["field"]
        }),
    )
}

fn def_constructors() -> ToolDefinition {
    def(
        "tokensave_constructors",
        "Struct Literal Sites",
        "Find every place a given struct is instantiated as a literal \
         ({ field: value, ... }). Each result includes the file, line, the \
         field list present in that literal, and the set of fields missing \
         relative to the struct's current definition (from the graph). The \
         missing-fields list is the typical refactor signal: after adding a \
         required field, this tool surfaces every site that needs updating, \
         before cargo even compiles. Currently best-effort for Rust source; \
         pattern matching ignores `match` arms and `if let` patterns.",
        json!({
            "type": "object",
            "properties": {
                "struct": {
                    "type": "string",
                    "description": "Struct name to search literal sites of (e.g. 'GraphStats', 'Config')."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of literal sites to return (default: 100, max: 1000)."
                }
            },
            "required": ["struct"]
        }),
    )
}

fn def_signature_search() -> ToolDefinition {
    def(
        "tokensave_signature_search",
        "Signature Search",
        "Find functions and methods by signature shape: return type, parameter \
         substring, async, or path. Searches the cached `signature` column on \
         every Function/Method node. Substring-matched with case-sensitive \
         compare; combine multiple criteria for narrower hits. Use \
         tokensave_search for plain name lookups; this tool is for refactor \
         questions like 'find every function returning Result<_, MyError>' or \
         'every async fn taking &mut self'.",
        json!({
            "type": "object",
            "properties": {
                "returns": {
                    "type": "string",
                    "description": "Substring that must appear in the return-type portion of the signature (after '->'). E.g. 'Result<', 'impl Future', 'Vec<u32>'."
                },
                "params": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Substrings that must all appear in the parameter list portion of the signature. E.g. ['&mut self'], ['i32', 'String']."
                },
                "async": {
                    "type": "boolean",
                    "description": "When true, only return functions marked async. When false, exclude them. Omit to ignore async-ness."
                },
                "path": {
                    "type": "string",
                    "description": "Filter to symbols defined under this directory."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum matches to return (default: 50, max: 500)."
                }
            }
        }),
    )
}

fn def_config() -> ToolDefinition {
    def(
        "tokensave_config",
        "Config File Query",
        "Query TOML or JSON config files by dotted key path. Use 'path' for a \
         single file (e.g. Cargo.toml, tsconfig.json, pyproject.toml) or 'glob' \
         to query the same key across multiple files. The 'key' is dot-separated \
         (e.g. 'package.version', 'dependencies.tokio'). Returns each match's \
         file, parsed value, and the line where the key is defined. Format is \
         detected from extension: .toml → TOML, .json → JSON. \
         \n\nDoes not query the code graph — pure filesystem + parser. Works \
         on uninitialized projects.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Project-relative path to a single config file (e.g. 'Cargo.toml'). Mutually exclusive with 'glob'."
                },
                "glob": {
                    "type": "string",
                    "description": "Glob pattern to match multiple config files (e.g. '**/Cargo.toml', 'crates/*/Cargo.toml'). Mutually exclusive with 'path'."
                },
                "key": {
                    "type": "string",
                    "description": "Dot-separated key path (e.g. 'package.version', 'dependencies.tokio.version'). Required."
                }
            },
            "required": ["key"]
        }),
    )
}

fn def_diagnostics() -> ToolDefinition {
    def(
        "tokensave_diagnostics",
        "Compile / Type-Check Diagnostics",
        "Run the project's type-checker (cargo check for Rust, tsc for \
         TypeScript, pyright for Python) and return structured errors and \
         warnings. Each diagnostic includes file, line range, level, code, \
         message, driver, and the enclosing graph node when one can be \
         resolved. Replaces the recurring 'run cargo → parse text → read \
         file' loop with a single structured response. \
         \n\nNote: the cargo target dir is forced to .tokensave/target/ so \
         we don't race with the user's interactive cargo runs. The first \
         call against a fresh tree builds dependencies from scratch, which \
         can take several minutes on large workspaces; subsequent calls \
         are sub-second. Build scripts and proc macros from the project \
         execute as part of cargo check — same trust model as running it \
         manually.",
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["workspace", "package", "file"],
                    "description": "Run scope. Default 'workspace'. 'package' requires `name`; 'file' requires `path` and currently runs workspace + post-filter (cargo has no native single-file mode)."
                },
                "name": {
                    "type": "string",
                    "description": "Package name when scope='package' (e.g. 'tokensave', 'serde-json')."
                },
                "path": {
                    "type": "string",
                    "description": "Project-relative file path when scope='file'."
                }
            }
        }),
    )
}

fn def_unsafe_patterns() -> ToolDefinition {
    def(
        "tokensave_unsafe_patterns",
        "Risky Pattern Finder",
        "Find unwrap(), expect(), panic!(), todo!(), unimplemented!(), and unsafe \
         { } sites across the project. Each match includes the file, line, kind, \
         enclosing symbol, the source line, and an in_test flag derived from the \
         path. Use this in security/quality reviews to surface panic sites before \
         a release. Defaults to all kinds; pass `kinds` to narrow.",
        json!({
            "type": "object",
            "properties": {
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Subset of patterns to search. Default: ['unwrap', 'expect', 'panic', 'todo', 'unimplemented', 'unsafe_block']."
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory (relative to project root)."
                },
                "exclude_tests": {
                    "type": "boolean",
                    "description": "When true, skips files whose path looks like a test (default: false)."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of matches to return (default: 200, max: 2000)."
                }
            }
        }),
    )
}

fn def_implementations() -> ToolDefinition {
    def(
        "tokensave_implementations",
        "Trait / Method Implementations",
        "Find every type implementing a given trait, or every body of a given \
         method name. The 'trait' form returns each implementing type plus the \
         methods on its impl block. The 'method' form returns every function/ \
         method named X across the project, grouped by enclosing type when \
         present. Each result includes file, signature, and the method body.",
        json!({
            "type": "object",
            "properties": {
                "trait": {
                    "type": "string",
                    "description": "Trait name to look up implementations of (e.g. 'LanguageExtractor', 'Display'). Mutually exclusive with 'method'."
                },
                "method": {
                    "type": "string",
                    "description": "Method or function name to find every implementation of (e.g. 'extensions', 'count_complexity'). Mutually exclusive with 'trait'."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of implementations to return (default: 20, max: 200)"
                }
            }
        }),
    )
}

fn def_outline() -> ToolDefinition {
    def(
        "tokensave_outline",
        "File Outline",
        "Flat list of every top-level symbol defined in a file (functions, structs, \
         enums, traits, classes, impls, etc.) — like a table of contents. Sorted by \
         line number; no code bodies. Optional 'kinds' filter narrows to specific \
         node kinds. Use this as the cheapest way to orient before zooming into a \
         large file with tokensave_node, tokensave_body, or tokensave_read.",
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Project-relative path to the file (e.g. 'src/sync.rs')."
                },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional filter on node kinds. Common values: 'function', 'struct', 'enum', 'trait', 'impl', 'class', 'method', 'const'. Case-insensitive. Default: all kinds."
                }
            },
            "required": ["file"]
        }),
    )
}

fn def_read() -> ToolDefinition {
    def(
        "tokensave_read",
        "Read File (mode-aware)",
        "Read a file or its symbol map. Modes: 'full' (entire file), 'lines' \
         (1-based inclusive byte-range slice via the 'lines' arg, e.g. '120-180'), \
         'map' (flat list of every top-level symbol from the graph — no source \
         bytes touched), 'signatures' (functions and types with their cached \
         signature). Cross-session cached: a re-call on an unchanged file returns \
         a tiny stub with 'unchanged: true'.",
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Project-relative or absolute path to the file (e.g. 'src/sync.rs')."
                },
                "mode": {
                    "type": "string",
                    "enum": ["full", "lines", "map", "signatures"],
                    "description": "Read mode. Default: 'full'."
                },
                "lines": {
                    "type": "string",
                    "description": "Required when mode='lines'. Format 'A-B' or single 'A' (1-based, inclusive). E.g. '120-180' or '42'."
                }
            },
            "required": ["file"]
        }),
    )
}

fn def_call_chain() -> ToolDefinition {
    def(
        "tokensave_call_chain",
        "Call Chain",
        "Find the shortest directed call chain between two symbols, following \
         only outgoing `calls` edges. Returns the ordered sequence of nodes \
         and edges that connect `from_id` to `to_id`, or a not-found result. \
         Use `tokensave_search` or `tokensave_by_qualified_name` first to \
         resolve symbol names into node IDs.",
        json!({
            "type": "object",
            "properties": {
                "from_id": {
                    "type": "string",
                    "description": "Source node ID (the caller end of the chain)."
                },
                "to_id": {
                    "type": "string",
                    "description": "Target node ID (the callee end of the chain)."
                },
                "max_depth": {
                    "type": "number",
                    "description": "Maximum BFS depth (default: 8, max: 20)."
                }
            },
            "required": ["from_id", "to_id"]
        }),
    )
}

fn def_file_dependents() -> ToolDefinition {
    def(
        "tokensave_file_dependents",
        "File Dependents",
        "List every indexed file that imports or otherwise depends on the \
         given file. Path is interpreted relative to the project root. \
         Useful for impact analysis on file-level changes.",
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Path to the file (relative to project root)."
                }
            },
            "required": ["file"]
        }),
    )
}

fn def_replace_symbol() -> ToolDefinition {
    def_rw(
        "tokensave_replace_symbol",
        "Replace Symbol Source",
        "Replace the full source of a named symbol (function, method, struct, \
         enum, etc.) with new source text. Resolves the symbol via exact \
         qualified-name match; on ambiguity, callable kinds win, and if \
         still ambiguous the edit is refused. Preserves the surrounding \
         file untouched and reindexes the file after writing.",
        json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name. Prefer a fully qualified name for disambiguation."
                },
                "new_source": {
                    "type": "string",
                    "description": "Full replacement source — must include the symbol's own declaration line."
                }
            },
            "required": ["symbol", "new_source"]
        }),
    )
}

fn def_find_exact_symbol() -> ToolDefinition {
    def(
        "tokensave_find_exact_symbol",
        "Exact Symbol Lookup",
        "Return every node whose `name` column equals the given bare \
         identifier — a single O(log n) index probe against `idx_nodes_name`. \
         No BM25, no fuzzy match, no scoring. Use this when you already know \
         the symbol name and want the cheapest possible lookup; use \
         `tokensave_search` for relevance-ranked discovery instead.",
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Exact bare symbol name (no `::`, no glob)."
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum matches to return (default: 20, max: 200)."
                }
            },
            "required": ["name"]
        }),
    )
}

fn def_insert_at_symbol() -> ToolDefinition {
    def_rw(
        "tokensave_insert_at_symbol",
        "Insert Near Symbol",
        "Insert content immediately before or after a named symbol's source \
         range. Same resolution semantics as `tokensave_replace_symbol`. \
         Use `position=\"before\"` or `position=\"after\"` (default: after).",
        json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Symbol name. Prefer a fully qualified name for disambiguation."
                },
                "content": {
                    "type": "string",
                    "description": "Source text to insert. Newlines are preserved as-is."
                },
                "position": {
                    "type": "string",
                    "enum": ["before", "after"],
                    "description": "Where to insert relative to the symbol's range. Default: after."
                }
            },
            "required": ["symbol", "content"]
        }),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::unreadable_literal)]
mod tests {
    use super::*;

    #[test]
    fn test_explore_call_budget_tiers() {
        assert_eq!(explore_call_budget(0), 3);
        assert_eq!(explore_call_budget(5000), 3);
        assert_eq!(explore_call_budget(5001), 4);
        assert_eq!(explore_call_budget(20000), 4);
        assert_eq!(explore_call_budget(20001), 5);
        assert_eq!(explore_call_budget(80000), 5);
        assert_eq!(explore_call_budget(80001), 7);
        assert_eq!(explore_call_budget(250000), 7);
        assert_eq!(explore_call_budget(250001), 10);
    }

    #[test]
    fn test_context_description_contains_budget() {
        let desc = context_description(5000, 4);
        assert!(
            desc.contains("4 calls maximum"),
            "description should contain budget: {desc}"
        );
        assert!(
            desc.contains("5000 nodes"),
            "description should contain node count: {desc}"
        );
    }

    #[test]
    fn test_get_tool_definitions_with_budget() {
        let defs = get_tool_definitions_with_budget(10000, 4);
        let context_tool = defs.iter().find(|d| d.name == "tokensave_context").unwrap();
        assert!(context_tool.description.contains("4 calls maximum"));
        assert!(context_tool.description.contains("10000 nodes"));
    }
}

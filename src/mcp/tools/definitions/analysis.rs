//! Code-health and architecture analysis tool definitions.

use serde_json::json;

use super::{
    def, def_limit_path_tool, def_object, def_path_flag_tool, def_path_limit_tool, number_property,
    string_property,
};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_dead_code() -> ToolDefinition {
    def(
        "tracedecay_dead_code",
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
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum symbols to return (default: 100, max: 1000)"
                }
            }
        }),
    )
}

pub(super) fn def_circular() -> ToolDefinition {
    def(
        "tracedecay_circular",
        "Circular Deps",
        "Detect circular dependencies between files in the code graph.",
        json!({
            "type": "object",
            "properties": {
                "max_depth": {
                    "type": "number",
                    "description": "Maximum cycle detection depth (default: 10)"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of cycles to report, largest first (default: 25, max: 200). The response always states the total detected and how many were omitted."
                },
                "member_limit": {
                    "type": "number",
                    "description": "Maximum member files listed per reported cycle (default: 12, max: 200). Each entry states its true member_count and omitted_member_count."
                }
            }
        }),
    )
}

pub(super) fn def_hotspots() -> ToolDefinition {
    def(
        "tracedecay_hotspots",
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

pub(super) fn def_unused_imports() -> ToolDefinition {
    def(
        "tracedecay_unused_imports",
        "Unused Imports",
        "Find import/use nodes that are never referenced by any other node. The walk is paged: the response reports whether it is complete and, when partial, a next_cursor to resume from.",
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum unused imports to report in one page (default: 100, max: 500)"
                },
                "cursor": {
                    "type": "string",
                    "description": "Resume cursor from a previous partial response's next_cursor"
                }
            }
        }),
    )
}

pub(super) fn def_rank() -> ToolDefinition {
    def(
        "tracedecay_rank",
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

pub(super) fn def_largest() -> ToolDefinition {
    def_object(
        "tracedecay_largest",
        "Largest Symbols",
        "Rank nodes by size (line count). Find the largest classes, longest methods, biggest enums, etc.",
        json!({
            "node_kind": string_property("Filter by node kind (e.g. 'class', 'method', 'function', 'interface', 'enum', 'struct')"),
            "path": string_property("Filter to files under this directory path (e.g. 'src/main/java')"),
            "limit": number_property("Maximum number of results to return (default: 10)")
        }),
    )
}

pub(super) fn def_coupling() -> ToolDefinition {
    def(
        "tracedecay_coupling",
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

pub(super) fn def_inheritance_depth() -> ToolDefinition {
    def_path_limit_tool(
        "tracedecay_inheritance_depth",
        "Inheritance Depth",
        "Find the deepest class/interface inheritance hierarchies by walking extends chains.",
        "Filter to files under this directory path (e.g. 'src/main/java')",
        "Maximum number of results to return (default: 10)",
    )
}

pub(super) fn def_distribution() -> ToolDefinition {
    def_object(
        "tracedecay_distribution",
        "Distribution",
        "Show node kind distribution (classes, methods, fields, etc.) per file or directory.",
        json!({
            "path": string_property("Directory or file path prefix to filter (e.g. 'src/main/java/com/example'). Omit for entire codebase."),
            "summary": {
                "type": "boolean",
                "description": "If true, aggregate counts across all matching files instead of per-file breakdown (default: false)"
            },
            "limit": number_property("Maximum number of files in the per-file breakdown, highest node count first (default: 100, max: 1000). Ignored when summary is true; the response states total_file_count and omitted_file_count.")
        }),
    )
}

pub(super) fn def_recursion() -> ToolDefinition {
    def_path_limit_tool(
        "tracedecay_recursion",
        "Recursion",
        "Detect recursive and mutually-recursive call cycles in the call graph.",
        "Filter to files under this directory path (e.g. 'src/main/java')",
        "Maximum number of cycles to return (default: 10)",
    )
}

pub(super) fn def_complexity() -> ToolDefinition {
    def_object(
        "tracedecay_complexity",
        "Complexity",
        "Rank functions/methods by composite complexity score (lines + fan-out + fan-in).",
        json!({
            "node_kind": string_property("Filter by node kind (default: function and method)"),
            "path": string_property("Filter to files under this directory path (e.g. 'src/main/java')"),
            "limit": number_property("Maximum number of results to return (default: 10)")
        }),
    )
}

pub(super) fn def_doc_coverage() -> ToolDefinition {
    def_path_limit_tool(
        "tracedecay_doc_coverage",
        "Doc Coverage",
        "Find public symbols missing documentation (docstrings).",
        "Directory or file path prefix to filter (e.g. 'src/main'). Omit for entire codebase.",
        "Maximum number of results to return (default: 50)",
    )
}

pub(super) fn def_god_class() -> ToolDefinition {
    def_path_limit_tool(
        "tracedecay_god_class",
        "God Classes",
        "Find classes with the most members (methods + fields).",
        "Filter to files under this directory path (e.g. 'src/main/java')",
        "Maximum number of results to return (default: 10)",
    )
}

pub(super) fn def_port_status() -> ToolDefinition {
    def(
        "tracedecay_port_status",
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

pub(super) fn def_port_order() -> ToolDefinition {
    def(
        "tracedecay_port_order",
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

pub(super) fn def_simplify_scan() -> ToolDefinition {
    def(
        "tracedecay_simplify_scan",
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

pub(super) fn def_gini() -> ToolDefinition {
    def_object(
        "tracedecay_gini",
        "Gini Inequality",
        "Compute inequality (Gini coefficient) for any metric across files or symbols. Detects god files and uneven complexity distribution.",
        json!({
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
            "path": string_property("Filter to files under this directory path"),
            "limit": number_property("Number of top outliers to return (default: 10)")
        }),
    )
}

pub(super) fn def_dependency_depth() -> ToolDefinition {
    def_limit_path_tool(
        "tracedecay_dependency_depth",
        "Dependency Depth",
        "Show the longest file-level dependency chains. Files at the end of long chains are fragile to upstream changes.",
        "Maximum number of chains to return (default: 10)",
        "Filter to files under this directory path",
    )
}

pub(super) fn def_health() -> ToolDefinition {
    def_path_flag_tool(
        "tracedecay_health",
        "Health Score",
        "Get quality signal (0-10000) with root cause breakdown (acyclicity, depth, equality, redundancy, modularity). Quality signal = geometric mean of 5 dimensions — maximize this ONE number.",
        "Filter to files under this directory path",
        "details",
        "If true, include full dimension breakdown (default: false)",
    )
}

pub(super) fn def_redundancy() -> ToolDefinition {
    def(
        "tracedecay_redundancy",
        "Redundancy Hunt",
        "Find functionally duplicated function/method bodies via AST isomorphism, control-flow match, call-sequence match, token-shingle Jaccard similarity, and body-vector cosine similarity. Results include similarity, ranking_score (a rank key, not a thresholded quantity), grouped duplicate components (connected components over the returned pairs only), and signal details such as body_vector_cosine and generic_helper_downranked. Each pair is bucketed as 'definite' (AST-identical with score >= 0.8), 'likely' (CFG, algorithmic, token, body-vector, or lower-scoring AST match at score >= 0.55), or 'naming_only' (weaker signals). Use when consolidating helpers or auditing code health. Computed lazily and cached per (node, body source hash) — first call on a fresh index can be slow on large repos.",
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
                    "description": "Drop pairs only when both the composite similarity and the body-vector cosine fall below this value (default: 0.6, range 0.0-1.0). A naming-only pair whose cosine clears this and 0.55 is reclassified as 'body_vector'"
                },
                "include_naming_only": {
                    "type": "boolean",
                    "description": "If true, include pairs whose only signal is name similarity (overlap_kind 'naming'), including identical-non-generic-name pairs rescued below the score gate. Cosine-rescued 'body_vector' pairs are always included regardless of this flag (default: false)"
                },
                "include_generated_paths": {
                    "type": "boolean",
                    "description": "If true, also scan build outputs, vendored code, and worktree mirrors (dist/, build/, out/, node_modules/, vendor/, target/, .worktrees/, *.min.js). Excluded by default because generated mirrors duplicate real sources byte-for-byte (default: false)"
                }
            }
        }),
    )
}

pub(super) fn def_dsm() -> ToolDefinition {
    def(
        "tracedecay_dsm",
        "Design Structure Matrix",
        "Get the Design Structure Matrix: file dependency summary showing clusters, density, and layering violations.",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "shape": {
                    "type": "string",
                    "enum": ["stats", "clusters", "matrix"],
                    "description": "DSM data shape: stats, clusters, or matrix (default: stats)."
                },
                "max_files": {
                    "type": "number",
                    "description": "Maximum files in matrix format (default: 30)"
                }
            }
        }),
    )
}

pub(super) fn def_todos() -> ToolDefinition {
    def(
        "tracedecay_todos",
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

pub(super) fn def_unsafe_patterns() -> ToolDefinition {
    def(
        "tracedecay_unsafe_patterns",
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

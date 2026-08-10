//! Graph query and navigation tool definitions.

use serde_json::{Value, json};

use super::{
    context_description, def, def_always_load, def_object, def_required_object, number_property,
    required_object_schema, string_property, with_project_selector_properties,
};
use crate::mcp::tools::ToolDefinition;

// ── alwaysLoad tools (loaded into the model prompt immediately) ─────────

pub(super) fn def_search() -> ToolDefinition {
    def_always_load(
        "tracedecay_search",
        "Search Symbols",
        "Search for symbols (functions, structs, traits, etc.) in the active project's code graph by name or keyword.",
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
                },
                "cursor": {
                    "type": "string",
                    "description": "Authenticated opaque continuation returned as next_cursor."
                },
                "semantic_mode": {
                    "type": "string",
                    "enum": ["fallback_allowed", "strict_semantic"],
                    "description": "Optional semantic policy. fallback_allowed (default) preserves the exact existing search response when semantic retrieval is unavailable; strict_semantic returns a typed unavailable result instead."
                },
                "lazy_index_ignored_dependencies": {
                    "type": "boolean",
                    "description": "Opt in to bounded indexing of ignored dependency entry files when an import hint matches (default: false)."
                }
            },
            "required": ["query"]
        }),
    )
}

pub(super) fn def_grep() -> ToolDefinition {
    // alwaysLoad: content/text search is the single most common native-tool
    // reflex (grep/rg). Keeping it in the always-loaded set means the model
    // never has to ToolSearch for it before reaching for Bash grep, which is
    // the main leak we're plugging. Paired with tracedecay_callers below, this
    // brings the always-loaded set to 7 (the agreed cap).
    def_always_load(
        "tracedecay_grep",
        "Grep Content",
        "grep, ripgrep, rg, text search, find string. Literal/regex content search over UTF-8 text sources in the project working tree (respects .gitignore; binary and non-UTF-8 files are outside the search scope), graph-enriched: each hit resolves the enclosing symbol so the natural next call is tracedecay_body. Bounded file or line omissions and unavailable source candidates are reported as partial coverage. Routing: use this for literal/regex content search (string literals, config keys, error messages); for symbol names use tracedecay_search; for concepts use tracedecay_context. Defaults to the active project; pass project_id/project_path only when intentionally searching another registered project.",
        json!({
            "type": "object",
            "properties": with_project_selector_properties(json!({
                "pattern": {
                    "type": "string",
                    "description": "Content to search for. Treated as a regular expression unless fixed_strings is true."
                },
                "fixed_strings": {
                    "type": "boolean",
                    "description": "Treat pattern as a literal string instead of a regex (default: false)."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Match case-sensitively (default: false = case-insensitive)."
                },
                "path_glob": {
                    "type": "string",
                    "description": "Optional glob restricting which files are searched, matched against project-relative paths (e.g. 'src/**/*.rs')."
                },
                "context_lines": {
                    "type": "number",
                    "description": "Lines of surrounding context to include per hit (default: 0, max: 3)."
                },
                "max_results": {
                    "type": "number",
                    "description": "Maximum number of matching lines to return (default: 50, max: 200)."
                }
            })),
            "required": ["pattern"]
        }),
    )
}

pub(super) fn def_retrieve() -> ToolDefinition {
    def(
        "tracedecay_retrieve",
        "Retrieve Truncated Response",
        "Use `tracedecay_retrieve` with required argument `handle` to retrieve the exact cached original text for a local response handle emitted by a truncated MCP response. This does not re-run the source tool or read a file/session/node again; handles are scoped to the active project store, expire automatically, and never reference remote storage. If the original truncated response used project_id/project_path, pass the same selector here. Only call it when the missing details are needed to answer the user's request.",
        json!({
            "type": "object",
            "properties": with_project_selector_properties(json!({
                "handle": {
                    "type": "string",
                    "description": "The required `handle` argument copied exactly from a truncated MCP response envelope."
                }
            })),
            "required": ["handle"]
        }),
    )
}

pub(super) fn def_context(input_schema: Value) -> ToolDefinition {
    def_always_load(
        "tracedecay_context",
        "Task Context",
        &context_description(0, 3),
        input_schema,
    )
}

pub(super) fn def_callers_for() -> ToolDefinition {
    def(
        "tracedecay_callers_for",
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

pub(super) fn def_by_qualified_name() -> ToolDefinition {
    def_required_object(
        "tracedecay_by_qualified_name",
        "Lookup by qualified name",
        "Look up nodes by their qualified name. Multiple rows can share a \
         qualified name (overloads, generics, separate impl blocks). Useful \
         for cross-run lookups where the content-hash node ID has changed.",
        json!({
            "qualified_name": string_property("The exact qualified name to look up.")
        }),
        &["qualified_name"],
    )
}

pub(super) fn def_impls() -> ToolDefinition {
    def_object(
        "tracedecay_impls",
        "Trait Implementations",
        "List `impl` blocks matching a trait, a type, or both. With no filter \
         returns every impl in the graph (use sparingly). Both arguments \
         accept short names (e.g. `Display`) or qualified names. Surfaces \
         information that is otherwise hard to query: trait-method dispatch \
         targets, which types satisfy a given trait, and which traits a type \
         implements.",
        json!({
            "trait": string_property("Trait name to filter by (short or qualified). Omit to include all traits."),
            "type": string_property("Implementing type to filter by (short or qualified). Omit to include all types."),
            "limit": number_property("Maximum number of results to return (default: 100).")
        }),
    )
}

pub(super) fn def_signature() -> ToolDefinition {
    def(
        "tracedecay_signature",
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
                "qualified_name": string_property("The exact qualified name to look up."),
                "node_id": string_property("Optional: look up a single node by its ID instead of qualified_name.")
            },
            "anyOf": [
                { "required": ["qualified_name"] },
                { "required": ["node_id"] }
            ]
        }),
    )
}

// ── Deferred tools (discovered via ToolSearch on demand) ────────────────

pub(super) fn def_callers() -> ToolDefinition {
    // alwaysLoad: "who calls this / find references" is the second-most-common
    // native reflex after grep. It only needs a node_id, so keeping it loaded
    // lets the model chain straight from a search/context hit into a caller
    // trace. This is the 7th (and final) always-loaded tool — see def_grep.
    def_always_load(
        "tracedecay_callers",
        "Callers",
        "Who calls this, find references, find usages, call sites. Find all callers of a given node (function, method, etc.) up to a specified depth.",
        required_object_schema(
            json!({
                "node_id": string_property("The unique node ID to find callers for"),
                "max_depth": number_property("Maximum traversal depth (default: 3)")
            }),
            &["node_id"],
        ),
    )
}

pub(super) fn def_callees(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_callees",
        "Callees",
        "What does this call, outgoing calls, dependencies of a function. \
         Find all callees of a given node (function, method, etc.) up to a \
         specified depth. When a callee resolves to a trait method, the \
         concrete impl methods reachable through that trait are also \
         returned, tagged with `dispatch_via_trait: true` and a `dispatch_from` \
         pointing at the trait method. Pass `resolve_dispatch: false` to \
         disable this behaviour and get only direct call edges.",
        input_schema,
    )
}

pub(super) fn def_impact(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_impact",
        "Impact Radius",
        "Compute the impact radius of a node: all symbols that directly or indirectly depend on it.",
        input_schema,
    )
}

pub(super) fn def_node(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_node",
        "Node Details",
        "Retrieve detailed information about a single node by its ID.",
        input_schema,
    )
}

pub(super) fn def_files() -> ToolDefinition {
    def(
        "tracedecay_files",
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
                "layout": {
                    "type": "string",
                    "enum": ["flat", "grouped"],
                    "description": "File listing layout: flat (one per line) or grouped by directory (default: grouped)."
                }
            }
        }),
    )
}

pub(super) fn def_similar(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_similar",
        "Similar Symbols",
        "Find symbols with similar names using full-text search and substring matching.",
        input_schema,
    )
}

pub(super) fn def_type_hierarchy() -> ToolDefinition {
    def(
        "tracedecay_type_hierarchy",
        "Type Hierarchy",
        "Use when asked a trait/interface/class type-hierarchy question — trigger before manually grepping `impl X for` / `extends X` chains across files. Returns the full recursive tree of implementors and extenders for a resolved type node.",
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

pub(super) fn def_derives() -> ToolDefinition {
    def(
        "tracedecay_derives",
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
                    "description": "The type's qualified name (or short name — same lookup as tracedecay_by_qualified_name)."
                },
                "node_id": {
                    "type": "string",
                    "description": "Optional: look up the type by node ID instead."
                }
            },
            "anyOf": [
                { "required": ["qualified_name"] },
                { "required": ["node_id"] }
            ]
        }),
    )
}

pub(super) fn def_body() -> ToolDefinition {
    def(
        "tracedecay_body",
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
                },
                "lazy_index_ignored_dependencies": {
                    "type": "boolean",
                    "description": "Opt in to bounded indexing of ignored dependency entry files when an import hint matches (default: false)."
                }
            },
            "required": ["symbol"]
        }),
    )
}

pub(super) fn def_field_sites() -> ToolDefinition {
    def(
        "tracedecay_field_sites",
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

pub(super) fn def_constructors() -> ToolDefinition {
    def(
        "tracedecay_constructors",
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

pub(super) fn def_signature_search() -> ToolDefinition {
    def(
        "tracedecay_signature_search",
        "Signature Search",
        "Find functions and methods by signature shape: return type, parameter \
         substring, async, or path. Searches the cached `signature` column on \
         every Function/Method node. Substring-matched with case-sensitive \
         compare; combine multiple criteria for narrower hits. Use \
         tracedecay_search for plain name lookups; this tool is for refactor \
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
            },
            "anyOf": [
                { "required": ["returns"] },
                { "required": ["params"] },
                { "required": ["async"] }
            ]
        }),
    )
}

pub(super) fn def_config() -> ToolDefinition {
    def(
        "tracedecay_config",
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
            "required": ["key"],
            "anyOf": [
                { "required": ["glob"] },
                { "required": ["path"] }
            ]
        }),
    )
}

pub(super) fn def_implementations() -> ToolDefinition {
    def(
        "tracedecay_implementations",
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
            },
            "anyOf": [
                { "required": ["trait"] },
                { "required": ["method"] }
            ]
        }),
    )
}

pub(super) fn def_outline() -> ToolDefinition {
    def(
        "tracedecay_outline",
        "File Outline",
        "Flat list of every top-level symbol defined in a file (functions, structs, \
         enums, traits, classes, impls, etc.) — like a table of contents. Sorted by \
         line number; no code bodies. Includes ast-grep outline JSON when the host \
         ast-grep CLI supports outline flags from ast-grep 0.44 or newer. Optional \
         'kinds' filter narrows to specific node kinds. Use this as the cheapest way \
         to orient before zooming into a \
         large file with tracedecay_node, tracedecay_body, or tracedecay_read.",
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

pub(super) fn def_read() -> ToolDefinition {
    def(
        "tracedecay_read",
        "Read File (mode-aware)",
        "Read a file or its symbol map. Modes: 'full' (entire file), 'lines' \
         (1-based inclusive line slice via the 'lines' arg, e.g. '120-180'), \
         'map' (flat list of every top-level symbol from the graph — no source \
         bytes touched), 'signatures' (functions and types with their cached \
         signature). Line reads include overlapping symbol signatures by default; \
         full reads can opt in with include_symbols. Cross-session cached: a re-call \
         on an unchanged file returns a tiny stub with 'unchanged: true'.",
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
                },
                "include_symbols": {
                    "type": "boolean",
                    "description": "Include graph symbol context for source reads. Defaults to true for mode='lines' and false for mode='full'."
                }
            },
            "required": ["file"]
        }),
    )
}

pub(super) fn def_find_exact_symbol() -> ToolDefinition {
    def(
        "tracedecay_find_exact_symbol",
        "Exact Symbol Lookup",
        "Return every node whose `name` column equals the given bare \
         identifier — a single O(log n) index probe against `idx_nodes_name`. \
         No BM25, no fuzzy match, no scoring. Use this when you already know \
         the symbol name and want the cheapest possible lookup; use \
         `tracedecay_search` for relevance-ranked discovery instead.",
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
                },
                "lazy_index_ignored_dependencies": {
                    "type": "boolean",
                    "description": "Opt in to bounded indexing of ignored dependency entry files when an import hint matches (default: false)."
                }
            },
            "required": ["name"]
        }),
    )
}

#[cfg(test)]
mod semantic_search_tests {
    use super::def_search;

    #[test]
    fn search_schema_exposes_only_the_two_planned_semantic_modes() {
        let definition = def_search();
        assert_eq!(
            definition.input_schema["properties"]["semantic_mode"]["enum"],
            serde_json::json!(["fallback_allowed", "strict_semantic"])
        );
        assert_eq!(
            definition.input_schema["properties"]["cursor"]["type"],
            "string"
        );
    }
}

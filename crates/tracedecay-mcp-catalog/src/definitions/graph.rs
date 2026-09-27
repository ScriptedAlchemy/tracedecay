//! Graph query and navigation tool definitions.

use serde_json::Value;

use super::{CONTEXT_DESCRIPTION, def, def_always_load};
use crate::ToolDefinition;

// ── alwaysLoad tools (loaded into the model prompt immediately) ─────────

pub(super) fn def_search(input_schema: Value) -> ToolDefinition {
    def_always_load(
        "tracedecay_search",
        "Search Symbols",
        "Exact and lexical code search over the active project's code graph: find symbols (functions, structs, traits, etc.) by name, identifier fragment, signature, path, or phrase through ranked exact and lexical routes. Every response opens with a `freshness: fresh | possibly_stale` line, so no status preflight is needed. Pass known identifiers as `lexical_anchors` (each an extra ranked route) and set `prefer_symbol` to add a symbol-name route for identifier-shaped query words.",
        input_schema,
    )
}

pub(super) fn def_grep(input_schema: Value) -> ToolDefinition {
    // alwaysLoad: content/text search is the single most common native-tool
    // reflex (grep/rg). Keeping it in the always-loaded set means the model
    // never has to ToolSearch for it before reaching for Bash grep, which is
    // the main leak we're plugging. Paired with tracedecay_callers below, this
    // brings the always-loaded set to 7 (the agreed cap).
    def_always_load(
        "tracedecay_grep",
        "Grep Content",
        "grep, ripgrep, rg, text search, find string. Literal/regex content search over UTF-8 text sources in the project working tree (respects .gitignore; binary and non-UTF-8 files are outside the search scope), graph-enriched: each hit resolves the enclosing symbol so the natural next call is tracedecay_source_body with its node_id. Bounded file or line omissions and unavailable source candidates are reported as partial coverage. Routing: use this for literal/regex content search (string literals, config keys, error messages); for symbol names use tracedecay_search; for concepts use tracedecay_context. Defaults to the active project; pass project_selector.project_id only when intentionally searching another registered project.",
        input_schema,
    )
}

pub(super) fn def_retrieve(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_retrieve",
        "Retrieve Truncated Response",
        "Use `tracedecay_retrieve` with required argument `handle` to read one bounded page of a truncated MCP response's locally cached text. Pass offset and max_chars for the omitted span you need. Do not walk next_offset until has_more is false or concatenate pages back into the conversation: the stored body already exceeded the response budget. max_chars is clamped to the response-frame budget. This does not re-run the source tool or read a file/session/node again; handles are scoped to the active project store, expire automatically, and never reference remote storage. If the original truncated response used project_selector.project_id, pass the same selector here. Only call it when the missing details are needed to answer the user's request.",
        input_schema,
    )
}

pub(super) fn def_context(input_schema: Value) -> ToolDefinition {
    def_always_load(
        "tracedecay_context",
        "Task Context",
        CONTEXT_DESCRIPTION,
        input_schema,
    )
}

pub(super) fn def_by_qualified_name(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_by_qualified_name",
        "Lookup by qualified name",
        "Look up nodes by their qualified name. Multiple rows can share a \
         qualified name (overloads, generics, separate impl blocks). Useful \
         for cross-run lookups where the content-hash node ID has changed.",
        input_schema,
    )
}

pub(super) fn def_signature(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_signature",
        "Signature",
        "Return the signature-level metadata for symbols matching a qualified \
         name, visibility, signature string (generics, params, return type, \
         where clauses), docstring, async flag, and kind. No bodies. Use this \
         instead of reading source files when you only need the public-API \
         surface of a function, method, or type. Multiple rows can be \
         returned (overloads, separate impls).",
        input_schema,
    )
}

// ── Deferred tools (discovered via ToolSearch on demand) ────────────────

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

pub(super) fn def_files(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_files",
        "File List",
        "List indexed project files. Use to explore file structure without reading file contents.",
        input_schema,
    )
}

/// Family API surface for verified shared implementations.
pub(super) fn def_similar(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_similar",
        "Shared Implementations",
        "Find token-verified implementations that share exact or rename-normalized source with one symbol occurrence or source range. The operation accepts no free-text query and reports bounded coverage for each digest family.",
        input_schema,
    )
}

pub(super) fn def_redundancy(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_redundancy",
        "Repository Shared Implementations",
        "Report token-verified exact and rename-normalized implementation families in one authorized repository. Results are ranked by reviewable source bytes and carry bounded family and member coverage.",
        input_schema,
    )
}

pub(super) fn def_derives(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_derives",
        "Derives on Type",
        "List the exact `#[derive(...)]` macro names attached to a type. Each \
         name carries `syntax_exact` evidence. Generated trait implementations \
         and methods are reported unavailable because macro expansion is not \
         retained in the verified graph.",
        input_schema,
    )
}

pub(super) fn def_field_sites(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_field_sites",
        "Field Read/Write Sites",
        "Find every read and write site of a named field across the codebase. \
         Returns two arrays: write_sites (assignments to the field) and \
         read_sites (everything else). Each entry includes file, line, \
         enclosing symbol, and a source snippet. Useful when renaming, \
         removing, or adding an invariant to a field, the write-site list \
         is the exact blast radius. Pattern matches `.<field>` references; \
         field-by-name is shorthand for any struct's same-named field, while \
         `Struct::field` form narrows to a specific declaration.",
        input_schema,
    )
}

pub(super) fn def_constructors(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_constructors",
        "Struct Literal Sites",
        "Find every place a given struct is instantiated as a literal \
         ({ field: value, ... }). Each result includes the file, line, the \
         explicitly initialized fields, fields supplied by Rust struct update \
         syntax (`..base`), and fields supplied by neither path relative to \
         the struct's current graph definition. Resolution remains explicitly \
         unverified because syntax alone cannot link same-name types across \
         modules; ambiguous definitions or recovered syntax report unknown \
         field coverage instead of inferred missing fields.",
        input_schema,
    )
}

pub(super) fn def_config(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_config",
        "Config File Query",
        "Query TOML or JSON config files by dotted key path. Use 'path' for a \
         single file (e.g. Cargo.toml, tsconfig.json, pyproject.toml) or 'glob' \
         to query the same key across multiple files. The 'key' is dot-separated \
         (e.g. 'package.version', 'dependencies.tokio'). Returns each match's \
         file, parsed value, and the line where the key is defined. Format is \
         detected from extension: .toml → TOML, .json → JSON. \
         \n\nDoes not query the code graph, pure filesystem + parser. Works \
         on uninitialized projects.",
        input_schema,
    )
}

pub(super) fn def_find_exact_symbol(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_find_exact_symbol",
        "Exact Symbol Lookup",
        "Return every node whose `name` column equals the given bare \
         identifier, a single O(log n) index probe against `idx_nodes_name`. \
         No BM25, no fuzzy match, no scoring. Use this when you already know \
         the symbol name and want the cheapest possible lookup; use \
         `tracedecay_search` for relevance-ranked discovery instead.",
        input_schema,
    )
}

#[cfg(test)]
mod search_schema_tests {
    use tracedecay_contracts::retrieval::{
        SEARCH_MAX_LEXICAL_ALIASES, SEARCH_MAX_LEXICAL_ANCHORS, SEARCH_MAX_LEXICAL_PROXIMITY_GAP,
    };

    fn search_schema() -> serde_json::Value {
        crate::get_maximal_tool_definitions()
            .expect("tool definitions")
            .into_iter()
            .find(|definition| definition.name == "tracedecay_search")
            .expect("tracedecay_search definition")
            .input_schema
    }

    /// The schema a `$ref` into the tool schema's `$defs` names, or `node`.
    fn definition<'a>(
        schema: &'a serde_json::Value,
        node: &'a serde_json::Value,
    ) -> &'a serde_json::Value {
        match node["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
        {
            Some(name) => &schema["$defs"][name],
            None => node,
        }
    }

    #[test]
    fn search_schema_has_no_semantic_mode_and_keeps_the_cursor() {
        let schema = search_schema();
        assert!(schema["properties"]["semantic_mode"].is_null());
        assert_eq!(
            schema["properties"]["cursor"]["type"],
            serde_json::json!(["string", "null"])
        );
    }

    #[test]
    fn search_schema_declares_lexical_routing_parameters() {
        let schema = search_schema();
        let anchors = &schema["properties"]["lexical_anchors"];
        assert_eq!(anchors["type"], serde_json::json!(["array", "null"]));
        assert_eq!(anchors["items"]["type"], "string");
        assert_eq!(
            anchors["maxItems"],
            serde_json::json!(SEARCH_MAX_LEXICAL_ANCHORS)
        );
        assert_eq!(
            schema["properties"]["prefer_symbol"]["type"],
            serde_json::json!(["boolean", "null"])
        );
        assert_eq!(
            schema["properties"]["lexical_aliases"]["maxItems"],
            serde_json::json!(SEARCH_MAX_LEXICAL_ALIASES)
        );
        let proximity = definition(
            &schema,
            &schema["properties"]["lexical_proximities"]["items"],
        );
        assert_eq!(
            proximity["properties"]["maximum_gap"]["maximum"],
            serde_json::json!(SEARCH_MAX_LEXICAL_PROXIMITY_GAP)
        );
        let filter = definition(
            &schema,
            &schema["properties"]["lexical_field_filters"]["items"],
        );
        assert!(
            definition(&schema, &filter["properties"]["field"])["enum"]
                .as_array()
                .is_some_and(|fields| fields.contains(&serde_json::json!("documentation")))
        );
    }
}

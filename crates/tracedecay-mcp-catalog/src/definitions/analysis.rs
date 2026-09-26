//! Code-health and architecture analysis tool definitions.

use serde_json::Value;

use super::def;
use crate::ToolDefinition;

pub(super) fn def_dead_code(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_dead_code",
        "Dead Code",
        "Find symbols with no incoming edges (potentially unreachable code). \
         Always excludes `main` and `test*` functions. By default also excludes \
         `pub` items (they may be referenced outside the indexed scope), pass \
         `include_public: true` to audit pub items with zero indexed callers, \
         which is what you want for workspace-internal cleanup. Pass `path` to \
         report only one directory prefix, which keeps a fixture or benchmark \
         corpus from consuming the whole page; the prefix is applied before \
         `limit`, and references from outside it still count as callers.",
        input_schema,
    )
}

pub(super) fn def_circular(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_circular",
        "Circular Deps",
        "Detect circular dependencies between files in the code graph.",
        input_schema,
    )
}

pub(super) fn def_hotspots(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_hotspots",
        "Hotspots",
        "Find symbols with the highest connectivity (most incoming + outgoing edges).",
        input_schema,
    )
}

pub(super) fn def_unmounted_files(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_unmounted_files",
        "Unmounted Files",
        "Find source files present on disk that nothing reaches, files the code graph indexes as \
         healthy symbols while no compiler, bundler, or test runner ever loads them. Reports one \
         section per ecosystem in a typed `ecosystems` field, each carrying its own verdict, \
         counts, and blind-spot list. RUST (cargo): walks every package found by manifest sweep \
, workspace members, path dependencies, nested independent workspaces, from its own \
         roots (src/lib.rs, src/main.rs, src/bin/*, every tests/*.rs and tests/<name>/main.rs, \
         benches, examples, build.rs), follows `mod name;`, `#[path = \"...\"]`, \
         `#[cfg_attr(..., path = \"...\")]` and `include!(\"literal.rs\")`, and diffs the \
         reachable set against the .rs files under that package's source directories; a finding \
         means the compiler never parses the file, and names the nearest mounted parent module \
         plus the exact `mod` line to add. TYPESCRIPT/JAVASCRIPT (npm, incl. pnpm/npm/yarn \
         workspaces, discovered per package.json rather than per glob): walks static `import`, \
         `require`, `export ... from`, and literal dynamic `import()` from declared entry points \
, package.json main/module/browser/types/bin/exports and the file paths named in \
         `scripts`, string literals in root `*.config.*` files (how an rsbuild/vite entry or a \
         vitest setup file is found without executing the config), tsconfig `files` and `paths` \
         aliases, plus conventional roots (tests, stories, `*.d.ts`, `src/index.*`, Next.js \
         app/pages route files); a finding means only that no static import path reaches the file \
, `tsc` may still type-check it via a tsconfig `include`, so no repair line is invented. \
         Predicates are never evaluated: a cfg-gated module or a conditionally-aliased file \
         counts as mounted. Languages with no reachability model here (Python, Go, Java, C/C++, \
         Ruby, and the rest) are reported with status `unsupported` and a file count, never \
         silently omitted. Per-ecosystem blind spots ride in the response; the standing ones are \
         macro-expanded `mod`s and computed `include!` paths for Rust, and non-literal dynamic \
         imports, non-tsconfig bundler aliases, glob imports, `.vue`/`.svelte`/`.astro` files, \
         and HTML/CSS-only references for TypeScript. Paths listed in the workspace's \
         `[workspace.metadata.cargo-shear] ignored-paths` are excluded.",
        input_schema,
    )
}

pub(super) fn def_rank(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_rank",
        "Rank",
        "Rank nodes by edge count for a given relationship type (calls, implements, extends, etc.).",
        input_schema,
    )
}

pub(super) fn def_largest(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_largest",
        "Largest Symbols",
        "Rank nodes by size (line count). Find the largest classes, longest methods, biggest enums, etc.",
        input_schema,
    )
}

pub(super) fn def_coupling(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_coupling",
        "Coupling",
        "Rank files by coupling: fan_in (most depended on) or fan_out (most dependencies).",
        input_schema,
    )
}

pub(super) fn def_inheritance_depth(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_inheritance_depth",
        "Inheritance Depth",
        "Find the deepest class/interface inheritance hierarchies by walking extends chains.",
        input_schema,
    )
}

pub(super) fn def_distribution(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_distribution",
        "Distribution",
        "Show node kind distribution (classes, methods, fields, etc.) per file or directory.",
        input_schema,
    )
}

pub(super) fn def_recursion(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_recursion",
        "Recursion",
        "Detect recursive and mutually-recursive call cycles in the call graph.",
        input_schema,
    )
}

pub(super) fn def_complexity(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_complexity",
        "Complexity",
        "Rank functions/methods by composite complexity score (lines + fan-out + fan-in).",
        input_schema,
    )
}

pub(super) fn def_doc_coverage(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_doc_coverage",
        "Doc Coverage",
        "Find public symbols missing documentation (docstrings).",
        input_schema,
    )
}

pub(super) fn def_god_class(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_god_class",
        "God Classes",
        "Find classes with the most members (methods + fields).",
        input_schema,
    )
}

pub(super) fn def_port_status(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_port_status",
        "Port Status",
        "Compare symbols between source and target directories to track porting progress.",
        input_schema,
    )
}

pub(super) fn def_port_order(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_port_order",
        "Port Order",
        "Topological sort of symbols in a directory -- port leaves first, dependents after.",
        input_schema,
    )
}

pub(super) fn def_gini(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_gini",
        "Gini Inequality",
        "Compute inequality (Gini coefficient) for any metric across files or symbols. Detects god files and uneven complexity distribution.",
        input_schema,
    )
}

pub(super) fn def_dependency_depth(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_dependency_depth",
        "Dependency Depth",
        "Show the longest file-level dependency chains. Files at the end of long chains are fragile to upstream changes.",
        input_schema,
    )
}

pub(super) fn def_health(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_health",
        "Health Score",
        "Get quality signal (0-10000) with root cause breakdown (acyclicity, depth, equality, redundancy, modularity). Quality signal = geometric mean of 5 dimensions, maximize this ONE number.",
        input_schema,
    )
}

pub(super) fn def_dsm(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_dsm",
        "Design Structure Matrix",
        "Get the Design Structure Matrix: file dependency summary showing clusters, density, and layering violations.",
        input_schema,
    )
}

pub(super) fn def_todos(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_todos",
        "TODOs and FIXMEs",
        "Find TODO, FIXME, XXX, HACK, WIP, NOTE, and unimplemented markers across the project. \
         Each result includes the marker kind, file, line, the comment text, and the enclosing \
         symbol name (function/method) for quick orientation.",
        input_schema,
    )
}

pub(super) fn def_unsafe_patterns(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_unsafe_patterns",
        "Risky Pattern Finder",
        "Find unwrap(), expect(), panic!(), todo!(), unimplemented!(), and unsafe \
         { } sites across the project. Each match includes the file, line, kind, \
         enclosing symbol, the source line, and an in_test flag derived from the \
         path. Use this in security/quality reviews to surface panic sites before \
         a release. Defaults to all kinds; pass `kinds` to narrow.",
        input_schema,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::get_maximal_tool_definitions;

    /// Without an advertised `path` an agent cannot scope the report, and a
    /// fixture corpus consumes the whole page before any product source is
    /// reported.
    #[test]
    fn dead_code_advertises_an_optional_path_filter_like_its_siblings() {
        let definitions = get_maximal_tool_definitions().expect("maximal catalog");
        let definition = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_dead_code")
            .expect("dead-code definition");

        assert_eq!(
            definition.input_schema["properties"]["path"]["type"],
            json!(["string", "null"])
        );
        assert!(
            definition.input_schema.get("required").is_none(),
            "every dead-code parameter stays optional"
        );
    }
}

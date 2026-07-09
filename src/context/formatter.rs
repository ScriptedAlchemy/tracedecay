use std::collections::HashMap;
use std::fmt::Write as _;

use crate::types::TaskContext;

pub(crate) const CODE_CONTEXT_HEADING: &str = "## Code Context";
pub(crate) const CONTEXT_MEMORY_MATCHES_HEADING: &str = "### Memory Matches";
pub(crate) const CONTEXT_MEMORY_FEEDBACK_HINT: &str = "Rate what you use: call tracedecay_fact_feedback with a fact_id above — action=helpful if a fact steered you right, action=unhelpful if it was wrong or misleading. Flagging a bad fact matters as much as confirming a good one; trust is earned only from this feedback, so rate the ones you actually used.";
pub(crate) const CONTEXT_ENTRY_POINTS_HEADING: &str = "### Entry Points";
pub(crate) const CONTEXT_RELATED_SYMBOLS_HEADING: &str = "### Related Symbols";
pub(crate) const CONTEXT_CODE_HEADING: &str = "### Code";
pub(crate) const CONTEXT_INDEX_COVERAGE_HINT_HEADING: &str = "### Index Coverage Hint";
pub(crate) const CONTEXT_EXTENSION_POINTS_HEADING: &str = "### Extension Points";
pub(crate) const CONTEXT_TEST_COVERAGE_HEADING: &str = "### Test Coverage";
pub(crate) const CONTEXT_SEEN_NODE_IDS_LABEL: &str = "seen_node_ids:";
pub(crate) const CONTEXT_PRIORITY_HEADINGS: &[&str] = &[
    CONTEXT_MEMORY_MATCHES_HEADING,
    CONTEXT_ENTRY_POINTS_HEADING,
    CONTEXT_RELATED_SYMBOLS_HEADING,
    CONTEXT_INDEX_COVERAGE_HINT_HEADING,
    CONTEXT_EXTENSION_POINTS_HEADING,
    CONTEXT_TEST_COVERAGE_HEADING,
    CONTEXT_SEEN_NODE_IDS_LABEL,
    CONTEXT_CODE_HEADING,
];

/// Formats a `TaskContext` as a Markdown document suitable for LLM consumption.
///
/// The output includes sections for the query, entry points, related symbols
/// grouped by file, and extracted code blocks.
pub fn format_context_as_markdown(context: &TaskContext) -> String {
    debug_assert!(
        !context.query.is_empty(),
        "format_context_as_markdown called with empty query"
    );
    debug_assert!(
        !context.summary.is_empty(),
        "format_context_as_markdown called with empty summary"
    );
    let mut out = String::new();

    out.push_str(CODE_CONTEXT_HEADING);
    out.push('\n');
    let _ = write!(out, "**Query:** {}\n\n", context.query);

    // Entry Points
    out.push_str(CONTEXT_ENTRY_POINTS_HEADING);
    out.push('\n');
    if context.entry_points.is_empty() {
        out.push_str("_No entry points found._\n\n");
    } else {
        for node in &context.entry_points {
            let _ = writeln!(
                out,
                "- **{}** ({}) - {}:{}",
                node.name,
                node.kind.as_str(),
                node.file_path,
                node.start_line,
            );
            if let Some(ref sig) = node.signature {
                let _ = writeln!(out, "  `{sig}`");
            }
        }
        out.push('\n');
    }

    // Related Symbols grouped by file
    out.push_str(CONTEXT_RELATED_SYMBOLS_HEADING);
    out.push('\n');
    if context.subgraph.nodes.is_empty() {
        out.push_str("_No related symbols._\n\n");
    } else {
        // Group nodes by file_path
        let mut by_file: HashMap<&str, Vec<(&str, u32)>> = HashMap::new();
        for node in &context.subgraph.nodes {
            by_file
                .entry(&node.file_path)
                .or_default()
                .push((&node.name, node.start_line));
        }

        let mut files: Vec<&&str> = by_file.keys().collect();
        files.sort();

        for file in files {
            let symbols = by_file.get(*file).unwrap_or(&Vec::new()).clone();
            let formatted: Vec<String> = symbols
                .iter()
                .map(|(name, line)| format!("{name}:{line}"))
                .collect();
            let _ = writeln!(out, "- {}: {}", file, formatted.join(", "));
        }
        out.push('\n');
    }

    // Code blocks
    out.push_str(CONTEXT_CODE_HEADING);
    out.push('\n');
    if context.code_blocks.is_empty() {
        out.push_str("_No code blocks extracted._\n");
    } else {
        for block in &context.code_blocks {
            // Determine a label from the node if available
            let label = if let Some(ref node_id) = block.node_id {
                // Try to find a matching entry point name
                context
                    .entry_points
                    .iter()
                    .find(|n| &n.id == node_id)
                    .map_or_else(|| node_id.clone(), |n| n.name.clone())
            } else {
                "unknown".to_string()
            };

            let _ = writeln!(
                out,
                "#### {} ({}:{})",
                label, block.file_path, block.start_line,
            );
            let _ = writeln!(out, "```{}", markdown_fence_language(&block.file_path));
            out.push_str(&block.content);
            if !block.content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n\n");
        }
    }

    debug_assert!(
        !out.is_empty(),
        "format_context_as_markdown produced empty output"
    );
    debug_assert!(
        out.contains(CODE_CONTEXT_HEADING),
        "output missing required header"
    );
    out
}

/// Formats a `TaskContext` as pretty-printed JSON.
pub fn format_context_as_json(context: &TaskContext) -> String {
    serde_json::to_string_pretty(context).unwrap_or_default()
}

fn markdown_fence_language(file_path: &str) -> &'static str {
    let file_name = file_path
        .rsplit_once('/')
        .map_or(file_path, |(_, name)| name);
    if matches!(file_name, "Dockerfile" | "Containerfile") {
        return "dockerfile";
    }

    match file_path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("bash" | "sh" | "zsh") => "bash",
        Some("c") => "c",
        Some("cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx") => "cpp",
        Some("cs") => "csharp",
        Some("css") => "css",
        Some("dart") => "dart",
        Some("go") => "go",
        Some("html" | "htm") => "html",
        Some("java") => "java",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("json" | "jsonc") => "json",
        Some("kt" | "kts") => "kotlin",
        Some("lua") => "lua",
        Some("md" | "markdown") => "markdown",
        Some("php") => "php",
        Some("proto") => "protobuf",
        Some("py" | "pyw") => "python",
        Some("rb") => "ruby",
        Some("rs") => "rust",
        Some("scala" | "sc") => "scala",
        Some("sql") => "sql",
        Some("swift") => "swift",
        Some("toml") => "toml",
        Some("ts" | "tsx" | "mts" | "cts") => "typescript",
        Some("vue") => "vue",
        Some("xml") => "xml",
        Some("yaml" | "yml") => "yaml",
        Some("zig") => "zig",
        _ => "",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::types::*;

    fn make_test_context() -> TaskContext {
        TaskContext {
            query: "test query".to_string(),
            summary: "Test summary".to_string(),
            subgraph: Subgraph::default(),
            entry_points: vec![],
            code_blocks: vec![],
            related_files: vec![],
            seen_node_ids: vec![],
        }
    }

    #[test]
    fn test_markdown_contains_header() {
        let ctx = make_test_context();
        let md = format_context_as_markdown(&ctx);
        assert!(md.contains("## Code Context"));
        assert!(md.contains("test query"));
    }

    #[test]
    fn test_json_roundtrip() {
        let ctx = make_test_context();
        let json = format_context_as_json(&ctx);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["query"], "test query");
    }

    #[test]
    fn test_markdown_with_entry_points() {
        let ctx = TaskContext {
            query: "process".to_string(),
            summary: "Found 1 entry point".to_string(),
            subgraph: Subgraph::default(),
            entry_points: vec![Node {
                id: "function:abc123".to_string(),
                kind: NodeKind::Function,
                name: "process_data".to_string(),
                qualified_name: "src/lib.rs::process_data".to_string(),
                file_path: "src/lib.rs".to_string(),
                start_line: 10,
                attrs_start_line: 10,
                end_line: 20,
                start_column: 0,
                end_column: 1,
                signature: Some("pub fn process_data(input: &str) -> Result<()>".to_string()),
                docstring: None,
                visibility: Visibility::Pub,
                is_async: false,
                branches: 0,
                loops: 0,
                returns: 0,
                max_nesting: 0,
                unsafe_blocks: 0,
                unchecked_calls: 0,
                assertions: 0,
                updated_at: 0,
                parent_id: None,
            }],
            code_blocks: vec![],
            related_files: vec!["src/lib.rs".to_string()],
            seen_node_ids: vec![],
        };

        let md = format_context_as_markdown(&ctx);
        assert!(md.contains("**process_data**"));
        assert!(md.contains("(function)"));
        assert!(md.contains("src/lib.rs:10"));
        assert!(md.contains("`pub fn process_data(input: &str) -> Result<()>`"));
    }

    #[test]
    fn test_markdown_with_code_blocks() {
        let ctx = TaskContext {
            query: "test".to_string(),
            summary: "Summary".to_string(),
            subgraph: Subgraph::default(),
            entry_points: vec![Node {
                id: "function:abc".to_string(),
                kind: NodeKind::Function,
                name: "my_fn".to_string(),
                qualified_name: "my_fn".to_string(),
                file_path: "src/main.rs".to_string(),
                start_line: 1,
                attrs_start_line: 1,
                end_line: 3,
                start_column: 0,
                end_column: 1,
                signature: None,
                docstring: None,
                visibility: Visibility::Pub,
                is_async: false,
                branches: 0,
                loops: 0,
                returns: 0,
                max_nesting: 0,
                unsafe_blocks: 0,
                unchecked_calls: 0,
                assertions: 0,
                updated_at: 0,
                parent_id: None,
            }],
            code_blocks: vec![
                CodeBlock {
                    content: "fn my_fn() {\n    println!(\"hello\");\n}".to_string(),
                    file_path: "src/main.rs".to_string(),
                    start_line: 1,
                    end_line: 3,
                    node_id: Some("function:abc".to_string()),
                },
                CodeBlock {
                    content: "export const answer = 42;".to_string(),
                    file_path: "src/app.ts".to_string(),
                    start_line: 5,
                    end_line: 5,
                    node_id: None,
                },
                CodeBlock {
                    content: "def answer():\n    return 42".to_string(),
                    file_path: "scripts/app.py".to_string(),
                    start_line: 7,
                    end_line: 8,
                    node_id: None,
                },
                CodeBlock {
                    content: "plain text".to_string(),
                    file_path: "notes/example.unknown".to_string(),
                    start_line: 9,
                    end_line: 9,
                    node_id: None,
                },
            ],
            related_files: vec!["src/main.rs".to_string()],
            seen_node_ids: vec![],
        };

        let md = format_context_as_markdown(&ctx);
        assert!(md.contains("#### my_fn (src/main.rs:1)"));
        assert!(md.contains("```rust"));
        assert!(md.contains("#### unknown (src/app.ts:5)\n```typescript"));
        assert!(md.contains("#### unknown (scripts/app.py:7)\n```python"));
        assert!(md.contains("#### unknown (notes/example.unknown:9)\n```\nplain text"));
        assert!(md.contains("fn my_fn()"));
    }

    #[test]
    fn test_markdown_fence_language_from_file_extension() {
        assert_eq!(markdown_fence_language("src/main.rs"), "rust");
        assert_eq!(markdown_fence_language("src/app.tsx"), "typescript");
        assert_eq!(markdown_fence_language("src/App.TSX"), "typescript");
        assert_eq!(markdown_fence_language("scripts/build.py"), "python");
        assert_eq!(markdown_fence_language("Dockerfile"), "dockerfile");
        assert_eq!(
            markdown_fence_language("deploy/Containerfile"),
            "dockerfile"
        );
        assert_eq!(markdown_fence_language("Makefile"), "");
        assert_eq!(markdown_fence_language("notes/example.unknown"), "");
    }
}

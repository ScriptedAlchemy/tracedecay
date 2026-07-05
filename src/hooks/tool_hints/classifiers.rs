use super::ToolHintInput;
use crate::hooks::shell_words;

pub(super) fn is_single_file_read(input: &ToolHintInput) -> bool {
    let is_read_tool = input
        .tool_name
        .as_deref()
        .is_some_and(|name| matches_normalized(name, &["readfile", "read_file", "read"]));
    is_read_tool
        && input
            .file_path
            .as_deref()
            .is_some_and(|path| !path.is_empty())
        && input.command.as_deref().unwrap_or_default().is_empty()
        && input.prompt.as_deref().unwrap_or_default().is_empty()
        && input
            .subagent_type
            .as_deref()
            .unwrap_or_default()
            .is_empty()
}

pub(super) fn is_tracedecay_tool_descriptor_read(input: &ToolHintInput) -> bool {
    let is_read_tool = input
        .tool_name
        .as_deref()
        .is_some_and(|name| matches_normalized(name, &["readfile", "read_file", "read"]));
    is_read_tool
        && input.file_path.as_deref().is_some_and(|path| {
            (path.contains("/tools/tracedecay_") || path.contains("\\tools\\tracedecay_"))
                && std::path::Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
}

/// Matches Cursor's semantic/codebase-search tool names. Cursor's hooks docs do
/// not enumerate a matcher value for semantic search, so the post-tool-use hook
/// runs unmatched and this predicate recognizes the tool names Cursor has
/// reported for it (`SemanticSearch`, `codebase_search`, `Codebase Search`).
pub(super) fn is_semantic_search_tool(input: &ToolHintInput) -> bool {
    input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(
            name,
            &[
                "semanticsearch",
                "semantic_search",
                "codebasesearch",
                "codebase_search",
            ],
        )
    })
}

pub(super) fn is_explore_subagent(input: &ToolHintInput) -> bool {
    let is_subagent_tool = input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(name, &["subagent", "agent", "task", "subagentstart"])
    });
    let is_explore_type = input
        .subagent_type
        .as_deref()
        .is_some_and(|kind| matches_normalized(kind, &["explore", "research", "code_research"]));

    is_subagent_tool && is_explore_type
}

pub(super) fn is_shell_search_command(command: &str) -> bool {
    // The quote/escape-aware parser shared with hooks.rs: quoted arguments
    // stay single tokens, so a pattern like `grep "needle -r" file` can no
    // longer leak a fake `-r` flag (the old split_whitespace misparse).
    let tokens = shell_words(command);
    let Some(first) = tokens.first() else {
        return false;
    };
    // Tolerate a leading subshell paren (`(grep -r foo)`), which the shell
    // parser keeps attached to the first word.
    let program = first.trim_start_matches('(').to_ascii_lowercase();
    match program.as_str() {
        "rg" | "ripgrep" => true,
        "grep" => tokens
            .iter()
            .skip(1)
            .any(|token| is_recursive_grep_flag(token)),
        _ => false,
    }
}

/// Classifies a shell command as a build/type-check invocation whose output the
/// model is about to parse by hand: `cargo check|build|clippy|test`, a bare
/// `tsc`, `npx tsc`, or `pyright`. Quote-aware like the other shell classifiers
/// so a needle such as `grep "cargo check"` is data, not a program.
pub(super) fn is_build_diagnostics_command(command: &str) -> bool {
    let tokens = shell_words(command);
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = first.trim_start_matches('(').to_ascii_lowercase();
    // The program name without any directory prefix (e.g. `/usr/bin/tsc` -> `tsc`).
    let base = program.rsplit(['/', '\\']).next().unwrap_or(&program);
    match base {
        "cargo" => tokens.iter().skip(1).any(|token| {
            matches!(
                token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                "check" | "build" | "clippy" | "test"
            )
        }),
        "tsc" | "pyright" | "pyright-python" => true,
        "npx" | "pnpm" | "yarn" | "bunx" => tokens.iter().skip(1).any(|token| {
            matches!(
                token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                "tsc" | "pyright"
            )
        }),
        _ => false,
    }
}

/// True when a Write/Edit event targets a harness-memory location where a
/// durable fact belongs in `TraceDecay` memory instead: `*/.claude/**/memory/*.md`,
/// any `MEMORY.md`, or any `CLAUDE.md`.
pub(super) fn is_memory_store_edit(input: &ToolHintInput) -> bool {
    let is_edit_tool = input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(name, &["write", "edit", "multiedit", "notebookedit"])
    });
    is_edit_tool
        && input
            .file_path
            .as_deref()
            .is_some_and(is_harness_memory_path)
}

/// Matches the harness-memory file locations that should route durable facts to
/// `tracedecay_fact_store`. Normalizes `\\` to `/` so Windows paths match too.
pub(in crate::hooks) fn is_harness_memory_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let file_name = normalized.rsplit('/').next().unwrap_or(&normalized);
    if file_name.eq_ignore_ascii_case("MEMORY.md") || file_name.eq_ignore_ascii_case("CLAUDE.md") {
        return true;
    }
    // `*/.claude/**/memory/*.md`: a `.claude` segment somewhere above a `memory`
    // directory that directly holds the `.md` file.
    let is_markdown = std::path::Path::new(file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    if !is_markdown {
        return false;
    }
    let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    // The file's parent directory must be `memory`, and some ancestor `.claude`.
    let Some(parent_idx) = segments.len().checked_sub(2) else {
        return false;
    };
    if !segments[parent_idx].eq_ignore_ascii_case("memory") {
        return false;
    }
    segments[..parent_idx]
        .iter()
        .any(|segment| segment.eq_ignore_ascii_case(".claude"))
}

pub(super) fn is_project_discovery_command(command: &str) -> bool {
    let tokens = shell_words(command);
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = first.trim_start_matches('(').to_ascii_lowercase();
    match program.as_str() {
        "find" | "fd" | "fdfind" => tokens
            .iter()
            .skip(1)
            .any(|token| is_parent_or_projects_path(token)),
        "rg" | "ripgrep" | "grep" => tokens
            .iter()
            .skip(1)
            .any(|token| is_parent_or_projects_path(token)),
        _ => false,
    }
}

pub(super) fn is_parent_or_projects_path(token: &str) -> bool {
    let token = token.trim_matches(|c| matches!(c, '(' | ')' | '"' | '\''));
    token == ".."
        || token.starts_with("../")
        || token.contains("/../")
        || token.contains("/projects/")
        || token.ends_with("/projects")
}

pub(super) fn is_recursive_grep_flag(token: &str) -> bool {
    if token == "--recursive" {
        return true;
    }
    if token.starts_with("--") {
        return false;
    }
    token
        .strip_prefix('-')
        .is_some_and(|flags| flags.chars().any(|c| c == 'r'))
}

pub(super) fn combined_text(input: &ToolHintInput) -> String {
    [input.prompt.as_deref(), input.command.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

pub(super) fn asks_for_call_graph(text: &str) -> bool {
    contains_any(
        text,
        &[
            "trace function",
            "trace the function",
            "trace functions",
            "trace the functions",
            "function trace",
            "find callers",
            "find caller",
            "find callees",
            "find callee",
            "who calls",
            "what calls",
            "callers of",
            "caller of",
            "called by",
            "call graph",
            "call path",
            "call chain",
            "callees of",
            "uses of",
            "depend on",
            "depends on",
            "what depends",
        ],
    )
}

pub(super) fn asks_for_impact(text: &str) -> bool {
    contains_any(
        text,
        &[
            "impact",
            "blast radius",
            "change risk",
            "change-risk",
            "affected tests",
            "affected files",
            "test map",
            "test_map",
            "what files are affected",
            "what code is affected",
            "which tests",
            "what tests",
        ],
    )
}

pub(super) fn asks_for_broad_read(text: &str) -> bool {
    contains_any(
        text,
        &[
            "read every",
            "full contents",
            "entire codebase",
            "whole codebase",
            "scan the codebase",
            "scan the entire",
        ],
    )
}

pub(super) fn asks_for_project_context(text: &str) -> bool {
    mentions_external_project_scope(text) || asks_for_repo_discovery(text)
}

pub(super) fn mentions_external_project_scope(text: &str) -> bool {
    contains_any(
        text,
        &[
            "another repo",
            "another repository",
            "other repo",
            "other repository",
            "external repo",
            "external repository",
            "sibling repo",
            "sibling repository",
            "neighbor repo",
            "neighbor repository",
            "nearby repo",
            "nearby repository",
            "next door",
            "registered project",
            "project registry",
            "project listing",
            "project list",
            "project search",
            "cross-project",
            "cross project",
            "orchestrator repo",
            "orchestrator repository",
        ],
    )
}

pub(super) fn asks_for_repo_discovery(text: &str) -> bool {
    !mentions_current_project_scope(text)
        && contains_any(text, &[" repo", " repository"])
        && contains_any(text, &["find", "locate", "where", "which"])
}

pub(super) fn mentions_current_project_scope(text: &str) -> bool {
    contains_any(
        text,
        &[
            "this repo",
            "this repository",
            "current repo",
            "current repository",
            "current workspace",
            "this workspace",
            "in repo",
            "in repository",
            "in the repo",
            "in the repository",
            "inside repo",
            "inside the repo",
        ],
    )
}

pub(super) fn asks_for_session_recall(text: &str) -> bool {
    contains_any(
        text,
        &[
            "where did we",
            "what did we",
            "when did we",
            "did we talk",
            "talk about",
            "discuss before",
            "mentioned before",
            "prior conversation",
            "previous conversation",
            "earlier conversation",
            "session search",
            "session recall",
            "conversation history",
        ],
    )
}

pub(super) fn asks_for_symbol_lookup(text: &str) -> bool {
    contains_any(
        text,
        &[
            "symbol lookup",
            "find definition",
            "where is defined",
            "where is this defined",
        ],
    )
}

pub(super) fn asks_for_atomic_edit(text: &str) -> bool {
    contains_any(
        text,
        &[
            "edit safely",
            "safe edit",
            "mechanical edit",
            "mechanical rewrite",
            "replace this everywhere",
            "replace everywhere",
            "rewrite structurally",
            "structural rewrite",
            "ast-grep",
            "ast grep",
            "multi_str_replace",
            "ast_grep_rewrite",
        ],
    )
}

pub(super) fn asks_for_type_orientation(text: &str) -> bool {
    contains_any(
        text,
        &[
            "constructor sites",
            "constructors",
            "struct literal",
            "field use",
            "field uses",
            "field reads",
            "field writes",
            "trait impl",
            "trait impls",
            "trait implementations",
            "implementors",
            "impl blocks",
            "duplicate logic",
            "redundant",
            "similar helper",
        ],
    )
}

pub(super) fn asks_for_file_lookup(text: &str) -> bool {
    contains_any(
        text,
        &["find files", "which files", "list files", "file lookup"],
    )
}

pub(super) fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

pub(super) fn matches_normalized(value: &str, expected: &[&str]) -> bool {
    let normalized = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase();
    expected.iter().any(|candidate| normalized == *candidate)
}

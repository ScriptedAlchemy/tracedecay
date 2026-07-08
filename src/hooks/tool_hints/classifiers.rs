use super::ToolHintInput;
use crate::shell::{shell_invocations, shell_words};

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

pub(super) fn is_subagent_context_handoff(input: &ToolHintInput) -> bool {
    let is_subagent_start = input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(
            name,
            &[
                "subagent",
                "agent",
                "task",
                "subagentstart",
                "subagent_start",
            ],
        )
    });
    is_subagent_start
        && input.prompt.as_deref().is_some_and(|prompt| {
            let prompt = prompt.to_ascii_lowercase();
            contains_any(
                &prompt,
                &[
                    "handoff",
                    "focused context",
                    "context for the subagent",
                    "context to the subagent",
                    "give the subagent context",
                    "implementation agent",
                    "execution agent",
                ],
            )
        })
}

pub(super) fn is_shell_search_command(command: &str) -> bool {
    shell_invocations(command)
        .into_iter()
        .any(|invocation| match invocation.base.as_str() {
            "rg" | "ripgrep" | "ag" | "ack" => true,
            "git" => invocation
                .args
                .iter()
                .any(|token| token.eq_ignore_ascii_case("grep")),
            "grep" => invocation
                .args
                .iter()
                .any(|token| is_recursive_grep_flag(token)),
            _ => false,
        })
}

pub(super) fn is_shell_text_search_command(command: &str) -> bool {
    shell_invocations(command)
        .into_iter()
        .any(|invocation| match invocation.base.as_str() {
            "rg" | "ripgrep" | "ag" | "ack" | "grep" => true,
            "git" => invocation
                .args
                .iter()
                .any(|token| token.eq_ignore_ascii_case("grep")),
            _ => false,
        })
}

/// Classifies a shell command as a build/type-check invocation whose output the
/// model is about to parse by hand: `cargo check|build|clippy|test`, a bare
/// `tsc`, `npx tsc`, or `pyright`. Quote-aware like the other shell classifiers
/// so a needle such as `grep "cargo check"` is data, not a program.
pub(super) fn is_build_diagnostics_command(command: &str) -> bool {
    shell_invocations(command)
        .iter()
        .any(|invocation| is_build_diagnostics_invocation(&invocation.base, &invocation.args))
}

fn is_build_diagnostics_invocation(base: &str, args: &[String]) -> bool {
    match base {
        "cargo" => args.iter().any(|token| {
            matches!(
                token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                "check" | "build" | "clippy" | "test" | "nextest"
            )
        }),
        "make" => args.iter().any(|token| {
            contains_any(
                &token.to_ascii_lowercase(),
                &["check", "build", "clippy", "test", "typecheck"],
            )
        }),
        "tsc" | "pyright" | "pyright-python" | "pytest" | "py.test" => true,
        "npx" | "pnpm" | "yarn" | "bunx" => args.iter().any(|token| {
            let token = token.trim_start_matches('(').to_ascii_lowercase();
            matches!(token.as_str(), "tsc" | "pyright" | "test" | "build")
                || contains_any(&token, &["typecheck", "type-check", "check-types"])
                || matches_test_or_build_script(&token)
        }),
        "npm" => args.windows(2).any(|pair| {
            pair[0].trim_start_matches('(').eq_ignore_ascii_case("run")
                && matches!(
                    pair[1].to_ascii_lowercase().as_str(),
                    "build" | "test" | "lint" | "typecheck" | "type-check" | "check"
                )
        }),
        "bun" => args.iter().any(|token| matches_test_or_build_script(token)),
        "go" => args
            .iter()
            .any(|token| token.trim_start_matches('(').eq_ignore_ascii_case("test")),
        "mvn" | "mvnw" | "gradle" | "gradlew" | "swift" => args.iter().any(|token| {
            matches!(
                token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                "test" | "build" | "check"
            )
        }),
        _ => false,
    }
}

pub(super) fn is_diff_review_command(command: &str, text: &str) -> bool {
    let tokens = shell_words(command);
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = first.trim_start_matches('(').to_ascii_lowercase();
    let base = program.rsplit(['/', '\\']).next().unwrap_or(&program);
    match base {
        "gh" => {
            tokens
                .windows(2)
                .any(|window| window[0] == "pr" && window[1] == "diff")
                || (tokens.iter().any(|token| token == "--patch") && asks_for_review_changes(text))
        }
        "git" => {
            tokens
                .iter()
                .skip(1)
                .any(|token| matches!(token.as_str(), "diff" | "show"))
                && asks_for_review_changes(text)
        }
        _ => false,
    }
}

pub(super) fn looks_like_pasted_diagnostic(text: &str) -> bool {
    contains_any(
        text,
        &[
            "error[e",
            "error ts",
            "typeerror:",
            "syntaxerror:",
            "failed tests",
            "test result: failed",
            "panicked at",
            "thread '",
            "warning: `",
        ],
    ) && contains_any(
        text,
        &["-->", ".rs:", ".ts(", ".tsx(", ".js(", ".jsx(", "failed"],
    )
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
    shell_invocations(command).into_iter().any(|invocation| {
        matches!(
            invocation.base.as_str(),
            "find" | "fd" | "fdfind" | "rg" | "ripgrep" | "grep"
        ) && invocation
            .args
            .iter()
            .any(|token| is_parent_or_projects_path(token))
    })
}

pub(super) fn is_file_lookup_command(command: &str) -> bool {
    shell_invocations(command)
        .into_iter()
        .any(|invocation| match invocation.base.as_str() {
            "rg" | "ripgrep" => invocation.args.iter().any(|token| token == "--files"),
            "git" => invocation
                .args
                .first()
                .is_some_and(|arg| *arg == "ls-files"),
            "find" | "fd" | "fdfind" => !invocation
                .args
                .iter()
                .any(|token| is_parent_or_projects_path(token)),
            _ => false,
        })
}

pub(super) fn is_shell_file_read_command(command: &str) -> bool {
    shell_invocations(command).into_iter().any(|invocation| {
        matches!(
            invocation.base.as_str(),
            "cat" | "head" | "tail" | "sed" | "nl"
        ) && invocation
            .args
            .iter()
            .any(|token| looks_like_source_path(token))
    })
}

fn looks_like_source_path(token: &str) -> bool {
    let token = token.trim_matches(|c| matches!(c, '(' | ')' | '"' | '\''));
    let Some((_, ext)) = token.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
    )
}

pub(super) fn is_parent_or_projects_path(token: &str) -> bool {
    let token = token.trim_matches(|c| matches!(c, '(' | ')' | '"' | '\''));
    token == ".."
        || token.starts_with("../")
        || token.contains("/../")
        || token.contains("/projects/")
        || token.starts_with("~/projects/")
        || token.starts_with("$HOME/projects/")
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
        .is_some_and(|flags| flags.chars().any(|c| c.eq_ignore_ascii_case(&'r')))
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

pub(super) fn asks_for_build_diagnostics(text: &str) -> bool {
    contains_any(
        text,
        &[
            "type error",
            "type errors",
            "typeerror",
            "typeerrors",
            "compiler error",
            "compiler errors",
            "diagnostic error",
            "diagnostic errors",
            "lsp diagnostic",
            "lsp diagnostics",
            "build error",
            "build errors",
            "build failure",
            "build failing",
            "test failure",
            "test failures",
            "tests failing",
            "failing ci",
            "ci failure",
            "ci failures",
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
            "whole repo",
            "entire repo",
            "scan the codebase",
            "scan the repo",
            "scan the entire",
        ],
    )
}

pub(super) fn asks_for_project_context(text: &str) -> bool {
    mentions_external_project_scope(text)
        || mentions_project_path(text)
        || asks_for_repo_discovery(text)
        || asks_for_project_architecture(text)
}

pub(super) fn asks_for_project_architecture(text: &str) -> bool {
    if contains_any(text, &["search web", "look up", "browse"]) {
        return false;
    }
    contains_any(
        text,
        &[
            "architecture",
            "architectural",
            "system design",
            "design the",
            "design phase",
            "code health",
            "tech debt",
            "dashboard ui",
            "dashboard diagnostics",
            "lsp implementation",
            "lsp implementations",
            "lsp engine",
            "all languages",
        ],
    ) && contains_any(
        text,
        &[
            "codebase",
            "project",
            "repo",
            "repository",
            "tracedecay",
            "hook engine",
            "hint engine",
            "lsp",
            "engine",
            "system",
        ],
    )
}

pub(super) fn mentions_project_path(text: &str) -> bool {
    contains_any(text, &["/projects/", "~/projects/", "$home/projects/"])
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
    let prior_context = contains_any(
        text,
        &[
            "where did we",
            "what did we",
            "when did we",
            "did we talk",
            "remind me",
            "remember when",
            "last time",
            "earlier we",
            "talk about",
            "discuss before",
            "mentioned before",
            "previously discussed",
            "last run",
            "prior run",
            "previous run",
            "automation run",
            "memory curator",
            "self-improvement run",
            "past session",
            "prior conversation",
            "previous conversation",
            "earlier conversation",
            "context length exceeded",
            "cannot compress further",
            "compacted context",
            "compaction",
            "context was compacted",
            "session search",
            "session recall",
            "conversation history",
        ],
    );
    let raw_transcript_context =
        contains_any(
            text,
            &[
                "raw codex jsonl transcript",
                "raw codex jsonl transcripts",
                "transcript files",
                "hook input",
                "hook usage",
                "hint displayed",
                "hints displayed",
                "model gets",
                "user submitted",
            ],
        ) || (contains_any(text, &["lcm sessions", "past sessions", "prior sessions"])
            && contains_any(text, &["check", "search", "find", "look", "review"]));

    prior_context || raw_transcript_context
}

pub(super) fn asks_for_symbol_lookup(text: &str) -> bool {
    if text.contains("where is ") && text.contains(" defined") {
        return true;
    }
    contains_any(
        text,
        &[
            "symbol lookup",
            "find definition",
            "find symbol",
            "look up symbol",
            "where is defined",
            "where is this defined",
        ],
    )
}

pub(super) fn asks_for_text_search(text: &str) -> bool {
    contains_any(
        text,
        &[
            "grep for",
            "rg for",
            "search for",
            "look for references",
            "find references",
            "find usages",
            "find uses of",
            "where is referenced",
            "where referenced",
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

pub(super) fn asks_for_review_changes(text: &str) -> bool {
    contains_any(
        text,
        &[
            "review diff",
            "review the diff",
            "review changes",
            "review the changes",
            "review this pr",
            "review pr",
            "pr diff",
            "diff context",
            "changed symbols",
            "changed files",
            "address review comments",
            "address comments",
            "# diff comments",
            "pull request review",
            "review feedback",
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

fn matches_test_or_build_script(token: &str) -> bool {
    let token = token.trim_start_matches('(').to_ascii_lowercase();
    matches!(
        token.as_str(),
        "test" | "build" | "lint" | "check" | "typecheck" | "type-check"
    ) || token.contains(":test")
        || token.contains(":build")
        || token.contains(":lint")
        || token.contains(":check")
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

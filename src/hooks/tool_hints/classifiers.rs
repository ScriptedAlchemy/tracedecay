use super::ToolHintInput;
use crate::shell::shell_invocations;

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

/// Build, type-check, and compiler-lint commands for which a host-authenticated
/// failure is actionable diagnostics evidence. Behavioral test runners are
/// deliberately excluded; their failures belong to the affected-test path.
pub(super) fn is_build_or_typecheck_command(command: &str) -> bool {
    shell_invocations(command)
        .iter()
        .any(|invocation| match invocation.base.as_str() {
            "cargo" => invocation.args.iter().any(|token| {
                matches!(
                    token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                    "check" | "build" | "clippy"
                )
            }),
            "rustc" | "tsc" | "pyright" | "pyright-python" => true,
            "npx" | "pnpm" | "yarn" | "bunx" => invocation.args.iter().any(|token| {
                matches!(
                    token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                    "tsc" | "pyright" | "build" | "typecheck" | "type-check" | "check-types"
                )
            }),
            "npm" => invocation.args.windows(2).any(|pair| {
                pair[0].trim_start_matches('(').eq_ignore_ascii_case("run")
                    && matches!(
                        pair[1].to_ascii_lowercase().as_str(),
                        "build" | "typecheck" | "type-check" | "check-types"
                    )
            }),
            "python" | "python3" => invocation
                .args
                .windows(2)
                .any(|pair| pair[0] == "-m" && pair[1].eq_ignore_ascii_case("pyright")),
            _ => false,
        })
}

pub(super) fn is_diff_review_command(command: &str, text: &str) -> bool {
    shell_invocations(command)
        .into_iter()
        .any(|invocation| match invocation.base.as_str() {
            "gh" => {
                invocation
                    .args
                    .windows(2)
                    .any(|window| window[0] == "pr" && window[1] == "diff")
                    || (invocation.args.iter().any(|token| token == "--patch")
                        && asks_for_review_changes(text))
            }
            "git" => {
                invocation
                    .args
                    .iter()
                    .any(|token| matches!(token.as_str(), "diff" | "show"))
                    && asks_for_review_changes(text)
            }
            _ => false,
        })
}

pub(super) fn looks_like_pasted_diagnostic(text: &str) -> bool {
    let looks_like_test_failure = contains_any(
        text,
        &[
            "test result: failed",
            "error: test failed",
            "panicked at",
            "failures:",
        ],
    );
    let has_strong_compiler_signal = contains_any(
        text,
        &[
            "error[e",
            "error ts",
            "typeerror:",
            "syntaxerror:",
            "warning:",
            " - error:",
        ],
    );
    if looks_like_test_failure && !has_strong_compiler_signal {
        return false;
    }

    contains_any(
        text,
        &[
            "error[e",
            "error ts",
            "typeerror:",
            "syntaxerror:",
            "warning:",
            "error:",
        ],
    ) && contains_any(
        text,
        &["-->", ".rs:", ".ts(", ".tsx(", ".js(", ".jsx(", ".py:"],
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
/// `tracedecay_fact_store_add`. Normalizes `\\` to `/` so Windows paths match too.
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

/// Minimum number of added lines for an edit to count as a "meaningful" new
/// body worth a redundancy nudge. Matches the redundancy tool's default
/// `min_lines`, so a one-line rename or a tiny tweak never trips the hint.
const REDUNDANCY_EDIT_MIN_LINES: usize = 8;

/// True when a Write/Edit/MultiEdit event adds a new function-shaped body of
/// meaningful size — the case where the model may be re-implementing logic that
/// already exists. Conservative by design (prefers missing a hint over
/// spamming), and `O(len(edit_text))`: pure string scanning over the added text
/// already in hand, with no AST parsing and no file I/O.
pub(super) fn is_redundancy_candidate_edit(input: &ToolHintInput) -> bool {
    let is_edit_tool = input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(name, &["write", "edit", "multiedit", "notebookedit"])
    });
    if !is_edit_tool {
        return false;
    }
    let (Some(path), Some(text)) = (input.file_path.as_deref(), input.edit_text.as_deref()) else {
        return false;
    };
    added_text_adds_function_body(path, text)
}

/// Core heuristic for [`is_redundancy_candidate_edit`]: does `text` (the text an
/// edit adds) look like it introduces a new function/method body of at least
/// [`REDUNDANCY_EDIT_MIN_LINES`] lines for `path`'s language? String scanning
/// only.
fn added_text_adds_function_body(path: &str, text: &str) -> bool {
    // A small edit is never a duplicate-logic risk worth interrupting for.
    // Stop after MIN_LINES lines instead of counting the whole (possibly huge)
    // Write payload: if there is no MIN_LINES-th line, the edit is too small.
    if text.lines().nth(REDUNDANCY_EDIT_MIN_LINES - 1).is_none() {
        return false;
    }
    let Some(ext) = source_extension(path) else {
        return false;
    };
    text_contains_function_definition(&ext, text)
}

/// Lowercased file extension for `path`, if it has one.
fn source_extension(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .map(|ext| ext.to_ascii_lowercase().to_string_lossy().into_owned())
}

/// Whether `text` contains a function-definition shape for the language family
/// keyed by `ext`. Clear-keyword languages match on a definition keyword; the
/// C-family/Java match on a conservative brace-signature shape. Unknown
/// extensions never match, so a data/markdown edit is silent.
fn text_contains_function_definition(ext: &str, text: &str) -> bool {
    match ext {
        "rs" => text.contains("fn "),
        "py" | "pyi" | "rb" => text.contains("def "),
        "go" | "swift" => text.contains("func "),
        "kt" | "kts" => text.contains("fun "),
        "php" => text.contains("function "),
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" => {
            text.contains("function ") || text.contains("=> {")
        }
        "java" | "cs" | "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "hh" => {
            contains_brace_method_signature(text)
        }
        _ => false,
    }
}

/// Conservative detector for a brace-language method signature line: a line that
/// opens a block (`{`) after a parameter list `(...)` whose leading token is not
/// a control-flow keyword (so `if (...) {`, `for (...) {`, etc. are excluded)
/// and whose name sits directly before the `(`. Deliberately misses more than
/// it matches to avoid false positives.
fn contains_brace_method_signature(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim();
        if !trimmed.ends_with('{') || !trimmed.contains(')') {
            return false;
        }
        let Some(open) = trimmed.find('(') else {
            return false;
        };
        let before = trimmed[..open].trim_end();
        // The name must sit immediately before `(` (a signature, not a bare
        // block), and the first token must not be a control-flow keyword.
        let ends_in_name = before
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let first_token = before
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .find(|token| !token.is_empty())
            .unwrap_or_default();
        let is_control = matches!(
            first_token,
            "if" | "for" | "while" | "switch" | "catch" | "do" | "else" | "return" | "using"
        );
        ends_in_name && !is_control && !first_token.is_empty()
    })
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
            "what breaks",
            "what would break",
            "what will break",
            "will this break",
            "would this break",
            "does this break",
            "what could break",
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

/// Distinctive confusion phrases that mean the agent found changes it did not
/// make — commits, amends, force-pushes, or working-tree drift it cannot
/// account for. Grounded in real sessions that guessed instead of attributing
/// (e.g. blind `git log` after a parallel agent amended the branch). Kept
/// narrow on purpose: benign `git status`/`git commit` narration must not fire
/// this. Routes to `investigating-unexpected-changes`.
pub(super) fn signals_unexpected_change(text: &str) -> bool {
    contains_any(
        text,
        &[
            "didn't make this commit",
            "did not make this commit",
            "commit i didn't make",
            "commit i did not make",
            "commit i didn't create",
            "commit i did not create",
            "commit i don't recognize",
            "commit i didn't recognize",
            "changes i didn't make",
            "changes i did not make",
            "files i didn't write",
            "file i didn't write",
            "who committed this",
            "who pushed this",
            "who amended",
            "who force-pushed",
            "who rebased",
            "force-pushed over my",
            "force-pushed my branch",
            "amended under",
            "amended my branch",
            "rebased under me",
            "rebased my branch",
            "branch appears to have been rebased",
            "branch has moved",
            "branch moved under",
            "head changed under",
            "head moved under",
            "history was rewritten",
            "rewrote history",
            "someone amended",
            "someone else committed",
            "worked on by someone else",
            "unexpected commit",
            "unexpected commits",
        ],
    )
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
            "type hierarchy",
            "trait hierarchy",
            "class hierarchy",
            "interface hierarchy",
            "inheritance hierarchy",
            "inheritance depth",
            "extenders of",
            "subtypes of",
            "supertypes of",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(tool: &str, path: &str, text: &str) -> ToolHintInput {
        ToolHintInput {
            tool_name: Some(tool.to_string()),
            file_path: Some(path.to_string()),
            edit_text: Some(text.to_string()),
            ..ToolHintInput::default()
        }
    }

    fn rust_fn_body() -> String {
        // A new function-sized Rust body (>= REDUNDANCY_EDIT_MIN_LINES lines).
        [
            "fn compute_widget_total(items: &[Item]) -> u64 {",
            "    let mut total = 0;",
            "    for item in items {",
            "        if item.active {",
            "            total += item.count;",
            "        }",
            "    }",
            "    total",
            "}",
        ]
        .join("\n")
    }

    #[test]
    fn new_rust_function_body_is_a_redundancy_candidate() {
        assert!(is_redundancy_candidate_edit(&edit(
            "Write",
            "src/widgets.rs",
            &rust_fn_body(),
        )));
        // Edit and MultiEdit tools qualify too.
        assert!(is_redundancy_candidate_edit(&edit(
            "Edit",
            "src/widgets.rs",
            &rust_fn_body(),
        )));
        assert!(is_redundancy_candidate_edit(&edit(
            "MultiEdit",
            "src/widgets.rs",
            &rust_fn_body(),
        )));
    }

    #[test]
    fn short_or_non_function_edits_are_not_candidates() {
        // Under the line threshold, even with a `fn`.
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write",
            "src/widgets.rs",
            "fn tiny() -> u8 { 1 }",
        )));
        // Long enough, but no function definition shape (a data/const block).
        let data = (0..12)
            .map(|i| format!("    const VALUE_{i}: u32 = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write",
            "src/widgets.rs",
            &data,
        )));
    }

    #[test]
    fn requires_edit_tool_path_and_text() {
        let body = rust_fn_body();
        // Non-edit tool.
        assert!(!is_redundancy_candidate_edit(&edit(
            "Read",
            "src/widgets.rs",
            &body
        )));
        // Missing file_path.
        assert!(!is_redundancy_candidate_edit(&ToolHintInput {
            tool_name: Some("Write".to_string()),
            edit_text: Some(body.clone()),
            ..ToolHintInput::default()
        }));
        // Missing edit_text.
        assert!(!is_redundancy_candidate_edit(&ToolHintInput {
            tool_name: Some("Write".to_string()),
            file_path: Some("src/widgets.rs".to_string()),
            ..ToolHintInput::default()
        }));
    }

    #[test]
    fn language_keywords_are_recognized() {
        let cases = [
            (
                "mod.py",
                "def build_report(rows):\n    total = 0\n    for r in rows:\n        if r.ok:\n            total += r.n\n        else:\n            total -= 1\n    return total\n",
            ),
            (
                "server.go",
                "func Handle(w Writer, r Request) {\n    a := 1\n    b := 2\n    c := a + b\n    d := c * 2\n    e := d - 1\n    f := e + 3\n    _ = f\n}\n",
            ),
            (
                "app.ts",
                "export function render(state: State) {\n    const a = 1;\n    const b = 2;\n    const c = a + b;\n    const d = c * 2;\n    const e = d - 1;\n    const f = e + 3;\n    return f;\n}\n",
            ),
            (
                "view.jsx",
                "const handler = (event) => {\n    const a = 1;\n    const b = 2;\n    const c = a + b;\n    const d = c * 2;\n    const e = d - 1;\n    const f = e + 3;\n    return f;\n};\n",
            ),
        ];
        for (path, body) in cases {
            assert!(
                is_redundancy_candidate_edit(&edit("Write", path, body)),
                "{path} body should be recognized as a new function"
            );
        }
    }

    #[test]
    fn brace_method_signature_excludes_control_flow() {
        // A Java method signature qualifies.
        let method = [
            "public int total(List<Item> items) {",
            "    int total = 0;",
            "    for (Item i : items) {",
            "        if (i.active) {",
            "            total += i.count;",
            "        }",
            "    }",
            "    return total;",
            "}",
        ]
        .join("\n");
        assert!(is_redundancy_candidate_edit(&edit(
            "Write",
            "Totals.java",
            &method
        )));
        // A block of only control-flow (no signature) does not qualify.
        let control_only = [
            "if (ready) {",
            "    step();",
            "} else if (waiting) {",
            "    wait();",
            "}",
            "while (running) {",
            "    tick();",
            "}",
            "for (int i = 0; i < 3; i++) {",
            "    poll();",
            "}",
        ]
        .join("\n");
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write",
            "Totals.java",
            &control_only,
        )));
    }

    #[test]
    fn unknown_extension_never_matches() {
        let body = rust_fn_body();
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write", "notes.md", &body
        )));
        // No extension at all.
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write", "Makefile", &body
        )));
    }
}

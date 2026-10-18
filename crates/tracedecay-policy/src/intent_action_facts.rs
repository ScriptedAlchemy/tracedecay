//! Deterministic action facts: what one hook event's tool call, command, and
//! prompt text show the agent doing.
//!
//! These predicates were the hook-local hint classifier. They are pure string
//! inspection over values already in hand (no file reads, no clock), and are
//! shared by the hook hint surface and the Context Scout intent pass so both
//! read one event identically.

use tracedecay_domain::configuration::ContextScoutIntentKindV1;

use crate::shell::shell_invocations;

/// Borrowed view of one hook event. Every field is optional; a surface that
/// lacks a value leaves it `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionFactInputV1<'a> {
    pub tool_name: Option<&'a str>,
    pub command: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub file_path: Option<&'a str>,
    pub subagent_type: Option<&'a str>,
    /// Host-captured tool output from top-level `tool_response`/`tool_output`
    /// fields. Never populate this from `tool_input` or shell command text.
    pub captured_output: Option<&'a str>,
    /// The host authenticated this as a non-interrupt, non-timeout tool error.
    /// Command text alone must never set this flag.
    pub trusted_failure: bool,
    /// Text an edit tool adds (Write `content`, Edit `new_string`, the joined
    /// `MultiEdit` `new_string`s).
    pub edit_text: Option<&'a str>,
}

/// Every intent whose action fact holds for `input`, in classification
/// priority order and without repeats. The first element is the intent the
/// hook hint surface shows.
pub fn action_facts(input: &ActionFactInputV1<'_>) -> Vec<ContextScoutIntentKindV1> {
    let facts = ActionFactContext::new(input);
    let mut intents = Vec::new();
    for rule in CLASSIFICATION_RULES {
        if !intents.contains(&rule.intent) && (rule.matches)(&facts) {
            intents.push(rule.intent);
        }
    }
    intents
}

/// The highest-priority action fact for `input`, if any.
pub fn first_action_fact(input: &ActionFactInputV1<'_>) -> Option<ContextScoutIntentKindV1> {
    let facts = ActionFactContext::new(input);
    CLASSIFICATION_RULES
        .iter()
        .find(|rule| (rule.matches)(&facts))
        .map(|rule| rule.intent)
}

struct ActionFactContext<'a> {
    input: &'a ActionFactInputV1<'a>,
    text: String,
    prompt_text: String,
    command: Option<&'a str>,
    tool_name: Option<&'a str>,
    command_is_shell_search: bool,
    prompt_has_diagnostic: bool,
    captured_output_has_diagnostic: bool,
    trusted_build_failure: bool,
}

impl<'a> ActionFactContext<'a> {
    fn new(input: &'a ActionFactInputV1<'a>) -> Self {
        let text = combined_text(input);
        let prompt_text = input.prompt.unwrap_or_default().to_ascii_lowercase();
        let command = input.command;
        let prompt_has_diagnostic = looks_like_pasted_diagnostic(&prompt_text);
        let captured_output_has_diagnostic = input
            .captured_output
            .map(str::to_ascii_lowercase)
            .as_deref()
            .is_some_and(looks_like_pasted_diagnostic);
        let trusted_build_failure =
            input.trusted_failure && command.is_some_and(is_build_or_typecheck_command);
        Self {
            input,
            text,
            prompt_text,
            command,
            tool_name: input.tool_name,
            command_is_shell_search: command.is_some_and(is_shell_text_search_command),
            prompt_has_diagnostic,
            captured_output_has_diagnostic,
            trusted_build_failure,
        }
    }
}

type ClassificationPredicate = fn(&ActionFactContext<'_>) -> bool;

struct ClassificationRule {
    intent: ContextScoutIntentKindV1,
    matches: ClassificationPredicate,
}

const CLASSIFICATION_RULES: &[ClassificationRule] = &[
    ClassificationRule {
        intent: ContextScoutIntentKindV1::ExploreSubagent,
        matches: |facts| is_explore_subagent(facts.input),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::SubagentStartContext,
        matches: |facts| is_subagent_context_handoff(facts.input),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::SemanticSearch,
        matches: |facts| is_semantic_search_tool(facts.input),
    },
    // Tool-action rules precede the free-text rules: on a tool event the text
    // is the tool's own arguments (a grep pattern, a shell command), so a
    // pattern such as "impact" must not outrank the search the tool performed.
    ClassificationRule {
        intent: ContextScoutIntentKindV1::MemoryStore,
        matches: |facts| is_memory_store_edit(facts.input),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::EditRedundancy,
        matches: |facts| is_redundancy_candidate_edit(facts.input),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::FileLookup,
        matches: |facts| {
            facts
                .tool_name
                .is_some_and(|name| matches_normalized(name, &["glob", "listdir", "list_dir"]))
                || facts.command.is_some_and(is_file_lookup_command)
        },
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::FileRead,
        matches: |facts| facts.command.is_some_and(is_shell_file_read_command),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::Search,
        matches: |facts| facts.command.is_some_and(is_shell_search_command),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::Search,
        matches: |facts| {
            facts
                .tool_name
                .is_some_and(|name| matches_normalized(name, &["grep", "search"]))
        },
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::ToolDescriptorRead,
        matches: |facts| is_tracedecay_tool_descriptor_read(facts.input),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::FileRead,
        matches: |facts| is_single_file_read(facts.input),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::UnexpectedChanges,
        matches: |facts| signals_unexpected_change(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::SessionRecall,
        matches: |facts| asks_for_session_recall(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::ProjectContext,
        matches: |facts| {
            asks_for_project_context(&facts.text)
                || facts.command.is_some_and(is_project_discovery_command)
        },
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::CallGraph,
        matches: |facts| asks_for_call_graph(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::Impact,
        matches: |facts| asks_for_impact(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::BuildDiagnostics,
        matches: |facts| {
            (!facts.command_is_shell_search && asks_for_build_diagnostics(&facts.prompt_text))
                || facts.prompt_has_diagnostic
                || facts.captured_output_has_diagnostic
                || facts.trusted_build_failure
        },
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::AtomicEdit,
        matches: |facts| asks_for_atomic_edit(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::ReviewChanges,
        matches: |facts| {
            asks_for_review_changes(&facts.text)
                || facts
                    .command
                    .is_some_and(|command| is_diff_review_command(command, &facts.text))
        },
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::TypeOrientation,
        matches: |facts| asks_for_type_orientation(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::Search,
        matches: |facts| asks_for_text_search(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::BroadRead,
        matches: |facts| asks_for_broad_read(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::SymbolLookup,
        matches: |facts| asks_for_symbol_lookup(&facts.text),
    },
    ClassificationRule {
        intent: ContextScoutIntentKindV1::FileLookup,
        matches: |facts| asks_for_file_lookup(&facts.text),
    },
];

pub fn is_single_file_read(input: &ActionFactInputV1<'_>) -> bool {
    let is_read_tool = input
        .tool_name
        .is_some_and(|name| matches_normalized(name, &["readfile", "read_file", "read"]));
    is_read_tool
        && input.file_path.is_some_and(|path| !path.is_empty())
        && input.command.unwrap_or_default().is_empty()
        && input.subagent_type.unwrap_or_default().is_empty()
}

pub fn is_tracedecay_tool_descriptor_read(input: &ActionFactInputV1<'_>) -> bool {
    let is_read_tool = input
        .tool_name
        .is_some_and(|name| matches_normalized(name, &["readfile", "read_file", "read"]));
    is_read_tool
        && input.file_path.is_some_and(|path| {
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
pub fn is_semantic_search_tool(input: &ActionFactInputV1<'_>) -> bool {
    input.tool_name.is_some_and(|name| {
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

pub fn is_explore_subagent(input: &ActionFactInputV1<'_>) -> bool {
    let is_subagent_tool = input.tool_name.is_some_and(|name| {
        matches_normalized(name, &["subagent", "agent", "task", "subagentstart"])
    });
    let is_explore_type = input
        .subagent_type
        .is_some_and(|kind| matches_normalized(kind, &["explore", "research", "code_research"]));

    is_subagent_tool && is_explore_type
}

pub fn is_subagent_context_handoff(input: &ActionFactInputV1<'_>) -> bool {
    let is_subagent_start = input.tool_name.is_some_and(|name| {
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
        && input.prompt.is_some_and(|prompt| {
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

pub fn is_shell_search_command(command: &str) -> bool {
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

pub fn is_shell_text_search_command(command: &str) -> bool {
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
pub fn is_build_or_typecheck_command(command: &str) -> bool {
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

pub fn is_diff_review_command(command: &str, text: &str) -> bool {
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

pub fn looks_like_pasted_diagnostic(text: &str) -> bool {
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
pub fn is_memory_store_edit(input: &ActionFactInputV1<'_>) -> bool {
    let is_edit_tool = input.tool_name.is_some_and(|name| {
        matches_normalized(name, &["write", "edit", "multiedit", "notebookedit"])
    });
    is_edit_tool && input.file_path.is_some_and(is_harness_memory_path)
}

/// Matches the harness-memory file locations that should route durable facts to
/// `tracedecay_fact_store_add`. Normalizes `\\` to `/` so Windows paths match too.
pub fn is_harness_memory_path(path: &str) -> bool {
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
/// body worth a duplicate-logic nudge, so a one-line rename or a tiny tweak
/// never trips the hint.
const REDUNDANCY_EDIT_MIN_LINES: usize = 8;

/// True when a Write/Edit/MultiEdit event adds a new function-shaped body of
/// meaningful size, the case where the model may be re-implementing logic that
/// already exists. Conservative by design (prefers missing a hint over
/// spamming), and `O(len(edit_text))`: pure string scanning over the added text
/// already in hand, with no AST parsing and no file I/O.
pub fn is_redundancy_candidate_edit(input: &ActionFactInputV1<'_>) -> bool {
    let is_edit_tool = input.tool_name.is_some_and(|name| {
        matches_normalized(name, &["write", "edit", "multiedit", "notebookedit"])
    });
    if !is_edit_tool {
        return false;
    }
    let (Some(path), Some(text)) = (input.file_path, input.edit_text) else {
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

pub fn is_project_discovery_command(command: &str) -> bool {
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

pub fn is_file_lookup_command(command: &str) -> bool {
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

pub fn is_shell_file_read_command(command: &str) -> bool {
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

pub fn is_parent_or_projects_path(token: &str) -> bool {
    let token = token.trim_matches(|c| matches!(c, '(' | ')' | '"' | '\''));
    token == ".."
        || token.starts_with("../")
        || token.contains("/../")
        || token.contains("/projects/")
        || token.starts_with("~/projects/")
        || token.starts_with("$HOME/projects/")
        || token.ends_with("/projects")
}

pub fn is_recursive_grep_flag(token: &str) -> bool {
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

pub fn combined_text(input: &ActionFactInputV1<'_>) -> String {
    [input.prompt, input.command]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

pub fn asks_for_call_graph(text: &str) -> bool {
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

pub fn asks_for_impact(text: &str) -> bool {
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

pub fn asks_for_build_diagnostics(text: &str) -> bool {
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

pub fn asks_for_broad_read(text: &str) -> bool {
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

pub fn asks_for_project_context(text: &str) -> bool {
    mentions_external_project_scope(text)
        || asks_for_repo_discovery(text)
        || asks_for_project_architecture(text)
}

pub fn asks_for_project_architecture(text: &str) -> bool {
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

pub fn mentions_external_project_scope(text: &str) -> bool {
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

pub fn asks_for_repo_discovery(text: &str) -> bool {
    !mentions_current_project_scope(text)
        && contains_any(text, &[" repo", " repository"])
        && contains_any(text, &["find", "locate", "where", "which"])
}

pub fn mentions_current_project_scope(text: &str) -> bool {
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

pub fn asks_for_session_recall(text: &str) -> bool {
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
/// make, commits, amends, force-pushes, or working-tree drift it cannot
/// account for. Grounded in real sessions that guessed instead of attributing
/// (e.g. blind `git log` after a parallel agent amended the branch). Kept
/// narrow on purpose: benign `git status`/`git commit` narration must not fire
/// this. Routes to `investigating-unexpected-changes`.
pub fn signals_unexpected_change(text: &str) -> bool {
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

pub fn asks_for_symbol_lookup(text: &str) -> bool {
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

pub fn asks_for_text_search(text: &str) -> bool {
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

pub fn asks_for_atomic_edit(text: &str) -> bool {
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

pub fn asks_for_review_changes(text: &str) -> bool {
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

pub fn asks_for_type_orientation(text: &str) -> bool {
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

pub fn asks_for_file_lookup(text: &str) -> bool {
    contains_any(
        text,
        &["find files", "which files", "list files", "file lookup"],
    )
}

pub fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

pub fn matches_normalized(value: &str, expected: &[&str]) -> bool {
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

    fn input<'a>(
        tool_name: Option<&'a str>,
        command: Option<&'a str>,
        prompt: Option<&'a str>,
    ) -> ActionFactInputV1<'a> {
        ActionFactInputV1 {
            tool_name,
            command,
            prompt,
            ..ActionFactInputV1::default()
        }
    }

    #[test]
    fn facts_follow_classification_priority_without_repeats() {
        let facts = action_facts(&input(
            None,
            None,
            Some("who calls setup_project and which tests depend on it?"),
        ));
        assert_eq!(
            facts,
            vec![
                ContextScoutIntentKindV1::CallGraph,
                ContextScoutIntentKindV1::Impact
            ]
        );
        assert_eq!(
            first_action_fact(&input(
                None,
                None,
                Some("who calls setup_project and which tests depend on it?"),
            )),
            Some(ContextScoutIntentKindV1::CallGraph)
        );
        let search = action_facts(&input(Some("Grep"), Some("rg -n foo src"), None));
        assert_eq!(search, vec![ContextScoutIntentKindV1::Search]);
    }

    #[test]
    fn empty_input_has_no_fact() {
        assert!(action_facts(&ActionFactInputV1::default()).is_empty());
        assert_eq!(first_action_fact(&ActionFactInputV1::default()), None);
    }

    #[test]
    fn trusted_build_failure_requires_a_build_command() {
        let mut failure = input(Some("Bash"), Some("cargo check -p foo"), None);
        failure.trusted_failure = true;
        assert_eq!(
            first_action_fact(&failure),
            Some(ContextScoutIntentKindV1::BuildDiagnostics)
        );
        let mut test_failure = input(Some("Bash"), Some("cargo test -p foo"), None);
        test_failure.trusted_failure = true;
        assert_eq!(first_action_fact(&test_failure), None);
    }

    #[test]
    fn edit_facts_need_the_edit_tool() {
        let memory = ActionFactInputV1 {
            tool_name: Some("Write"),
            file_path: Some("/repo/CLAUDE.md"),
            ..ActionFactInputV1::default()
        };
        assert_eq!(
            first_action_fact(&memory),
            Some(ContextScoutIntentKindV1::MemoryStore)
        );
        let read = ActionFactInputV1 {
            tool_name: Some("Read"),
            file_path: Some("/repo/CLAUDE.md"),
            ..ActionFactInputV1::default()
        };
        assert_eq!(
            first_action_fact(&read),
            Some(ContextScoutIntentKindV1::FileRead)
        );
    }
}

use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ScenarioFamily {
    CodexPrompt,
    ClaudePrompt,
    CursorPrompt,
    CrossProject,
    ShellSearch,
    FileLookup,
    FileRead,
    BroadRead,
    ToolDescriptor,
    SemanticSearch,
    CallGraph,
    Impact,
    SymbolLookup,
    TypeOrientation,
    AtomicEdit,
    BuildDiagnostics,
    MemoryStore,
    Subagent,
    SessionRecall,
    NegativeSilence,
    Disabled,
    QuotedData,
    AdapterShape,
    Dedupe,
}

const COVERAGE_FAMILIES: &[ScenarioFamily] = &[
    ScenarioFamily::CodexPrompt,
    ScenarioFamily::ClaudePrompt,
    ScenarioFamily::CursorPrompt,
    ScenarioFamily::CrossProject,
    ScenarioFamily::ShellSearch,
    ScenarioFamily::FileLookup,
    ScenarioFamily::FileRead,
    ScenarioFamily::BroadRead,
    ScenarioFamily::ToolDescriptor,
    ScenarioFamily::SemanticSearch,
    ScenarioFamily::CallGraph,
    ScenarioFamily::Impact,
    ScenarioFamily::SymbolLookup,
    ScenarioFamily::TypeOrientation,
    ScenarioFamily::AtomicEdit,
    ScenarioFamily::BuildDiagnostics,
    ScenarioFamily::MemoryStore,
    ScenarioFamily::Subagent,
    ScenarioFamily::SessionRecall,
    ScenarioFamily::NegativeSilence,
    ScenarioFamily::Disabled,
    ScenarioFamily::QuotedData,
    ScenarioFamily::AdapterShape,
    ScenarioFamily::Dedupe,
];

const STATIC_BOILERPLATE: &[&str] = &[
    "tracedecay is available via MCP",
    "Prefer tracedecay MCP tools",
    "run `tracedecay init`",
];

#[derive(Clone)]
struct HintEval {
    name: &'static str,
    families: Vec<ScenarioFamily>,
    input: ToolHintInput,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
    must_not_contain: &'static [&'static str],
}

impl HintEval {
    fn with_families(mut self, extra: &[ScenarioFamily]) -> Self {
        self.families.extend_from_slice(extra);
        self
    }
}

fn prompt_eval(
    name: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
        ScenarioFamily::CodexPrompt,
        ToolHintInput {
            prompt: Some(prompt.to_string()),
            session_id: Some(format!("{name}-session")),
            ..ToolHintInput::default()
        },
        expected,
        must_contain,
    )
}

fn shell_eval(
    name: &'static str,
    command: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
        ScenarioFamily::ShellSearch,
        ToolHintInput {
            tool_name: Some("Bash".to_string()),
            command: Some(command.to_string()),
            prompt: Some(prompt.to_string()),
            session_id: Some(format!("{name}-session")),
            ..ToolHintInput::default()
        },
        expected,
        must_contain,
    )
}

fn dedupe_eval(
    name: &'static str,
    command: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    shell_eval(name, command, prompt, expected, must_contain)
        .with_families(&[ScenarioFamily::Dedupe])
}

fn tool_eval(
    name: &'static str,
    tool_name: &'static str,
    file_path: Option<&'static str>,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
        ScenarioFamily::AdapterShape,
        ToolHintInput {
            tool_name: Some(tool_name.to_string()),
            file_path: file_path.map(str::to_string),
            session_id: Some(format!("{name}-session")),
            ..ToolHintInput::default()
        },
        expected,
        must_contain,
    )
}

fn input_eval(
    name: &'static str,
    input: ToolHintInput,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
        ScenarioFamily::AdapterShape,
        ToolHintInput {
            session_id: Some(format!("{name}-session")),
            ..input
        },
        expected,
        must_contain,
    )
}

fn eval(
    name: &'static str,
    family: ScenarioFamily,
    input: ToolHintInput,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    let families = default_families(family, &input, expected);
    HintEval {
        name,
        families,
        input,
        expected,
        must_contain,
        must_not_contain: STATIC_BOILERPLATE,
    }
}

fn run_eval(eval: &HintEval) {
    let hint = decide_hint(&eval.input);
    assert_eq!(
        hint.as_ref().map(|hint| hint.category),
        eval.expected,
        "{}",
        eval.name
    );

    let Some(hint) = hint else {
        return;
    };
    let visible = format!("{}\n{}", hint.message, hint.context);
    let skill = category_skill(hint.category);
    assert!(
        visible.contains(&format!("Skill: tracedecay:{skill}.")),
        "{} missing bundled skill trigger `tracedecay:{skill}` in:\n{}",
        eval.name,
        visible
    );
    assert!(
        visible.len() <= 850,
        "{} hint is too verbose: {} chars\n{}",
        eval.name,
        visible.len(),
        visible
    );
    for needle in eval.must_contain {
        assert!(
            visible.contains(needle),
            "{} missing expected `{needle}` in:\n{}",
            eval.name,
            visible
        );
    }
    for needle in eval.must_not_contain {
        assert!(
            !visible.contains(needle),
            "{} leaked static boilerplate `{needle}` in:\n{}",
            eval.name,
            visible
        );
    }
}

fn coverage_families(eval: &HintEval) -> Vec<ScenarioFamily> {
    eval.families.clone()
}

fn default_families(
    family: ScenarioFamily,
    input: &ToolHintInput,
    expected: Option<HintCategory>,
) -> Vec<ScenarioFamily> {
    let mut families = vec![family];
    if input.command.is_some() {
        families.push(ScenarioFamily::ShellSearch);
    }
    if input.tool_name.is_some() || input.file_path.is_some() {
        families.push(ScenarioFamily::AdapterShape);
    }
    if !input.hints_enabled {
        families.push(ScenarioFamily::Disabled);
    }
    if expected.is_none() {
        families.push(ScenarioFamily::NegativeSilence);
    }

    match expected {
        Some(HintCategory::Search) => families.push(ScenarioFamily::ShellSearch),
        Some(HintCategory::SemanticSearch) => families.push(ScenarioFamily::SemanticSearch),
        Some(HintCategory::FileRead) => families.push(ScenarioFamily::FileRead),
        Some(HintCategory::ToolDescriptorRead) => families.push(ScenarioFamily::ToolDescriptor),
        Some(HintCategory::BroadRead) => families.push(ScenarioFamily::BroadRead),
        Some(HintCategory::CallGraph) => families.push(ScenarioFamily::CallGraph),
        Some(HintCategory::Impact | HintCategory::ReviewChanges) => {
            families.push(ScenarioFamily::Impact);
        }
        Some(HintCategory::SymbolLookup) => families.push(ScenarioFamily::SymbolLookup),
        Some(HintCategory::FileLookup) => families.push(ScenarioFamily::FileLookup),
        Some(HintCategory::ProjectContext) => families.push(ScenarioFamily::CrossProject),
        Some(HintCategory::SessionRecall) => families.push(ScenarioFamily::SessionRecall),
        Some(HintCategory::AtomicEdit) => families.push(ScenarioFamily::AtomicEdit),
        Some(HintCategory::TypeOrientation) => families.push(ScenarioFamily::TypeOrientation),
        Some(HintCategory::ExploreSubagent | HintCategory::SubagentStartContext) => {
            families.push(ScenarioFamily::Subagent);
        }
        Some(HintCategory::BuildDiagnostics) => families.push(ScenarioFamily::BuildDiagnostics),
        Some(HintCategory::MemoryStore) => families.push(ScenarioFamily::MemoryStore),
        // The edit-redundancy nudge is an edit-tool surface; it rides the
        // AdapterShape family already added for tool_name/file_path inputs.
        Some(HintCategory::EditRedundancy) | None => {}
    }

    families
}

fn real_world_prompt_cases() -> Vec<HintEval> {
    vec![
        prompt_eval(
            "raw-codex-jsonl-transcripts",
            "look at raw codex jsonl transcript files if needed as well",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search", "tracedecay_lcm_grep"],
        ),
        prompt_eval(
            "hook-verbosity-adversarial-review",
            "analyze the hook usage and verbosity and repetition in transcripts with codex where we have hints displayed",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search", "tracedecay_lcm_grep"],
        ),
        prompt_eval(
            "repo-local-dev-skill-request",
            "add more skills to .codex for helping debug tracedecay and develop on it",
            None,
            &[],
        ),
        prompt_eval(
            "generic-non-code-chat-complaint",
            "hooks should be smarter when a chat is not inside a git repo; it should be generic like lcm or sessions, not code graph parts",
            None,
            &[],
        ),
        prompt_eval(
            "what-did-we-decide-before",
            "where did we decide how memory curation should work before?",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "informal-prior-session-recall",
            "remind me what we concluded about hook hints last time",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "branch-or-pr-status",
            "What branch this on or pr",
            None,
            &[],
        ),
        prompt_eval("merge-pr-number", "Merge 64", None, &[]),
        prompt_eval(
            "generic-browser-help",
            "how do I open a new browser tab?",
            None,
            &[],
        ),
        prompt_eval(
            "render-model-visible-hook-input",
            "write a parser renderer to render cases where you can see what model gets with extra input from hooks vs what user submitted",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "prior-automation-run",
            "what happened in the last memory curator automation run?",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "sibling-rsncc-repo",
            "look in the rsncc sibling repo and check the open PR status there",
            Some(HintCategory::ProjectContext),
            &["tracedecay_project_search"],
        ),
        prompt_eval("what-repo-is-this", "what repo is this?", None, &[]),
        prompt_eval(
            "github-pr-live-status",
            "babysit PR 319 and tell me whether checks are green",
            None,
            &[],
        ),
        prompt_eval(
            "direct-code-change-request",
            "change the button text to Save and run the narrow test",
            None,
            &[],
        ),
    ]
}

#[test]
fn real_world_prompt_eval_matrix() {
    let evals = real_world_prompt_cases();

    for eval in &evals {
        run_eval(eval);
    }
}

fn dynamic_action_context_cases() -> Vec<HintEval> {
    vec![
        input_eval(
            "disabled-hints-stay-silent",
            ToolHintInput {
                tool_name: Some("SemanticSearch".to_string()),
                hints_enabled: false,
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
        input_eval(
            "explore-subagent-start",
            ToolHintInput {
                tool_name: Some("Task".to_string()),
                subagent_type: Some("code_research".to_string()),
                prompt: Some("inspect the hook engine".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::ExploreSubagent),
            &[
                "tracedecay_context",
                "tracedecay_search",
                "tracedecay_impact",
            ],
        ),
        input_eval(
            "semantic-search-tool-action",
            ToolHintInput {
                tool_name: Some("codebase_search".to_string()),
                prompt: Some("how does hook steering work?".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::SemanticSearch),
            &["tracedecay_context", "tracedecay_search", "tracedecay_grep"],
        ),
        input_eval(
            "semantic-search-tool-name-variant",
            ToolHintInput {
                tool_name: Some("Semantic Search".to_string()),
                prompt: Some("where is the hook classifier?".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::SemanticSearch),
            &["tracedecay_context"],
        ),
        input_eval(
            "glob-tool-file-lookup",
            ToolHintInput {
                tool_name: Some("Glob".to_string()),
                prompt: Some("find src hook files".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        input_eval(
            "glob-tool-no-prompt",
            ToolHintInput {
                tool_name: Some("Glob".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        input_eval(
            "literal-shell-search-in-current-repo",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("rg -n \"append_tracedecay_bootstrap_context\" src".to_string()),
                prompt: Some("find the bootstrap function in this repo".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::Search),
            &["tracedecay_grep", "tracedecay_search", "tracedecay_context"],
        ),
        input_eval(
            "shell-sed-source-read",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("sed -n '1,200p' src/hooks/tool_hints.rs".to_string()),
                prompt: Some("read the hint engine implementation".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::FileRead),
            &["tracedecay_outline", "tracedecay_body", "tracedecay_read"],
        ),
        input_eval(
            "shell-cat-config-read",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("cat Cargo.toml".to_string()),
                prompt: Some("inspect package config".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::FileRead),
            &["tracedecay_outline"],
        ),
        input_eval(
            "single-file-read-action",
            ToolHintInput {
                tool_name: Some("Read".to_string()),
                file_path: Some("src/hooks/steering.rs".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::FileRead),
            &["tracedecay_outline", "tracedecay_body", "tracedecay_read"],
        ),
        input_eval(
            "windows-tool-descriptor-read",
            ToolHintInput {
                tool_name: Some("Read".to_string()),
                file_path: Some("C:\\tmp\\plugin\\tools\\tracedecay_impact.json".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::ToolDescriptorRead),
            &["tracedecay_find_exact_symbol", "tracedecay_callers"],
        ),
        input_eval(
            "harness-memory-edit-action",
            ToolHintInput {
                tool_name: Some("Edit".to_string()),
                file_path: Some("/home/zack/.codex/memories/MEMORY.md".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::MemoryStore),
            &["tracedecay_fact_store"],
        ),
        input_eval(
            "claude-memory-edit-action",
            ToolHintInput {
                tool_name: Some("MultiEdit".to_string()),
                file_path: Some("/tmp/project/.claude/foo/memory/notes.md".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::MemoryStore),
            &["tracedecay_fact_store"],
        ),
        input_eval(
            "write-claude-md-action",
            ToolHintInput {
                tool_name: Some("Write".to_string()),
                file_path: Some("CLAUDE.md".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::MemoryStore),
            &["tracedecay_fact_store"],
        ),
        input_eval(
            "generic-git-status-action",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("git status -sb".to_string()),
                prompt: Some("what branch is this on?".to_string()),
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
        input_eval(
            "non-explore-subagent-stays-silent",
            ToolHintInput {
                tool_name: Some("Task".to_string()),
                subagent_type: Some("review".to_string()),
                prompt: Some("review this exact file only".to_string()),
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
        input_eval(
            "disabled-shell-search-stays-silent",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("rg -n \"ToolHint\" src".to_string()),
                hints_enabled: false,
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
        input_eval(
            "safe-ordinary-file-edit-action",
            ToolHintInput {
                tool_name: Some("Edit".to_string()),
                file_path: Some("src/hooks/steering.rs".to_string()),
                prompt: Some("tighten this string".to_string()),
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
        input_eval(
            "new-function-write-nudges-redundancy",
            ToolHintInput {
                tool_name: Some("Write".to_string()),
                file_path: Some("src/hooks/steering.rs".to_string()),
                edit_text: Some(
                    "fn summarize_hits(hits: &[Hit]) -> Summary {\n    \
                     let mut total = 0;\n    \
                     for hit in hits {\n        \
                     if hit.active {\n            \
                     total += hit.count;\n        \
                     }\n    \
                     }\n    \
                     Summary { total }\n}\n"
                        .to_string(),
                ),
                ..ToolHintInput::default()
            },
            Some(HintCategory::EditRedundancy),
            &["tracedecay_redundancy", "tracedecay_similar"],
        ),
        input_eval(
            "small-edit-does-not-nudge-redundancy",
            ToolHintInput {
                tool_name: Some("Edit".to_string()),
                file_path: Some("src/hooks/steering.rs".to_string()),
                edit_text: Some("fn one_liner() -> u8 { 1 }".to_string()),
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
        // Codex surface: `hook_codex_post_tool_use` maps an `apply_patch` event
        // onto this Claude-shaped input (tool_name `Edit`, patch target path,
        // and the `+`-stripped added source as edit_text), so the shared
        // redundancy classifier fires identically for Codex.
        input_eval(
            "codex-apply-patch-nudges-redundancy",
            ToolHintInput {
                agent: HintAgent::Codex,
                tool_name: Some("Edit".to_string()),
                file_path: Some("src/util.rs".to_string()),
                edit_text: Some(
                    "pub fn summarize(hits: &[Hit]) -> u32 {\n    \
                     let mut total = 0;\n    \
                     for hit in hits {\n        \
                     if hit.active {\n            \
                     total += hit.count;\n        \
                     }\n    \
                     }\n    \
                     total\n}\n"
                        .to_string(),
                ),
                ..ToolHintInput::default()
            },
            Some(HintCategory::EditRedundancy),
            &["tracedecay_redundancy", "tracedecay_similar"],
        ),
    ]
}

#[test]
fn dynamic_action_context_eval_matrix() {
    let evals = dynamic_action_context_cases();

    for eval in &evals {
        run_eval(eval);
    }
}

fn synthetic_prompt_cases() -> Vec<HintEval> {
    vec![
        shell_eval(
            "recursive-rg-current-repo",
            "rg -n \"HintCategory\" src",
            "Find the hint categories in this repo",
            Some(HintCategory::Search),
            &["tracedecay_grep", "tracedecay_search"],
        ),
        shell_eval(
            "find-sibling-repo",
            "find ../ -maxdepth 3 -type d -name '*orchestrator*'",
            "Find the orchestrator repo",
            Some(HintCategory::ProjectContext),
            &["tracedecay_project_search"],
        ),
        shell_eval(
            "cargo-check-diagnostics",
            "cargo check",
            "see whether this builds",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics", "tracedecay_diagnose"],
        ),
        shell_eval(
            "env-cargo-check-diagnostics",
            "env RUSTFLAGS=-Dwarnings cargo check",
            "see whether this builds",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics", "tracedecay_diagnose"],
        ),
        shell_eval(
            "nested-shell-rg-search",
            "cd /tmp && bash -lc \"rg 'foo bar' src/hooks\"",
            "search source for a quoted string",
            Some(HintCategory::Search),
            &["tracedecay_grep"],
        )
        .with_families(&[ScenarioFamily::QuotedData]),
        shell_eval(
            "cargo-test-diagnostics",
            "cargo test hooks::tool_hints",
            "run the hook tests",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics", "tracedecay_diagnose"],
        ),
        shell_eval(
            "pnpm-tsc-diagnostics",
            "pnpm tsc --noEmit",
            "check types",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        shell_eval(
            "npx-pyright-diagnostics",
            "npx pyright",
            "check python types",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        shell_eval(
            "current-repo-find-files",
            "find src/hooks -name '*.rs'",
            "list hook source files",
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        shell_eval(
            "rg-files-current-repo",
            "rg --files src/hooks",
            "which hook files exist?",
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        shell_eval(
            "fd-current-repo-files",
            "fd tool_hints src/hooks",
            "find hook files",
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        shell_eval(
            "parent-projects-find",
            "find /home/zack/projects -maxdepth 2 -type d -name '*tracedecay*'",
            "locate the tracedecay project",
            Some(HintCategory::ProjectContext),
            &["tracedecay_project_search"],
        ),
        shell_eval(
            "grep-recursive-uppercase",
            "grep -R \"ToolHint\" src/hooks",
            "search current repo for ToolHint",
            Some(HintCategory::Search),
            &["tracedecay_grep"],
        ),
        shell_eval(
            "quoted-compiler-command-is-search-data",
            "grep \"cargo check\" README.md",
            "look for docs mentioning cargo check",
            None,
            &[],
        )
        .with_families(&[ScenarioFamily::QuotedData]),
        shell_eval(
            "quoted-git-command-is-search-data",
            "grep \"git status\" README.md",
            "look for docs mentioning git status",
            None,
            &[],
        )
        .with_families(&[ScenarioFamily::QuotedData]),
        shell_eval(
            "git-status-no-hint",
            "git status --short --branch",
            "what changed?",
            None,
            &[],
        ),
        shell_eval(
            "gh-pr-view-no-hint",
            "gh pr view 319 --json state",
            "check PR state",
            None,
            &[],
        ),
        shell_eval(
            "shell-head-source-read",
            "head -n 60 src/hooks/tool_hints.rs",
            "inspect top of hook hints file",
            Some(HintCategory::FileRead),
            &["tracedecay_outline"],
        ),
        shell_eval(
            "shell-tail-source-read",
            "tail -n 80 src/hooks/tool_hints/classifiers.rs",
            "inspect classifier bottom",
            Some(HintCategory::FileRead),
            &["tracedecay_outline"],
        ),
        shell_eval(
            "shell-nl-source-read",
            "nl -ba src/hooks/tool_hints/evals.rs",
            "read evals with line numbers",
            Some(HintCategory::FileRead),
            &["tracedecay_outline"],
        ),
        prompt_eval(
            "call-chain-question",
            "what calls record_hint_analytics and what does it call?",
            Some(HintCategory::CallGraph),
            &["tracedecay_callers", "tracedecay_callees"],
        ),
        prompt_eval(
            "affected-tests-question",
            "which tests should I run after changing src/hooks/tool_hints.rs?",
            Some(HintCategory::Impact),
            &["tracedecay_affected", "tracedecay_test_map"],
        ),
        prompt_eval(
            "diff-impact-question",
            "what is the blast radius of this diff before I push?",
            Some(HintCategory::Impact),
            &["tracedecay_diff_context", "tracedecay_impact"],
        ),
        prompt_eval(
            "what-breaks-question",
            "what breaks if I change the signature of classify_hint?",
            Some(HintCategory::Impact),
            &["tracedecay_impact", "tracedecay_affected"],
        ),
        prompt_eval(
            "symbol-definition-question",
            "find definition of ToolHintInput",
            Some(HintCategory::SymbolLookup),
            &["tracedecay_context", "tracedecay_node"],
        ),
        prompt_eval(
            "symbol-defined-wording",
            "where is classify_hint defined?",
            Some(HintCategory::SymbolLookup),
            &["tracedecay_context"],
        ),
        prompt_eval(
            "broad-codebase-scan-question",
            "scan the entire codebase for hook hint behavior",
            Some(HintCategory::BroadRead),
            &["tracedecay_context", "tracedecay_grep"],
        ),
        prompt_eval(
            "whole-codebase-question",
            "read every source file and explain this subsystem",
            Some(HintCategory::BroadRead),
            &["tracedecay_context"],
        ),
        prompt_eval(
            "file-list-question",
            "list files under src/hooks matching hook adapters",
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        prompt_eval(
            "which-files-question",
            "which files implement Codex hook adapters?",
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        prompt_eval(
            "type-orientation-question",
            "where are ToolHintInput field writes and constructor sites?",
            Some(HintCategory::TypeOrientation),
            &["tracedecay_constructors", "tracedecay_field_sites"],
        ),
        prompt_eval(
            "duplicate-helper-question",
            "is there duplicate logic or a similar helper before I add another classifier?",
            Some(HintCategory::TypeOrientation),
            &["tracedecay_redundancy"],
        ),
        prompt_eval(
            "type-hierarchy-question",
            "what is the full trait hierarchy for HintCategory, all implementors and extenders?",
            Some(HintCategory::TypeOrientation),
            &["tracedecay_type_hierarchy"],
        ),
        prompt_eval(
            "safe-mechanical-edit",
            "replace this everywhere safely with a mechanical rewrite",
            Some(HintCategory::AtomicEdit),
            &["tracedecay_multi_str_replace"],
        ),
        tool_eval(
            "tool-descriptor-read",
            "Read",
            Some("/tmp/plugin/tools/tracedecay_callers.json"),
            Some(HintCategory::ToolDescriptorRead),
            &["tracedecay_callers"],
        ),
        tool_eval("plain-read-without-path", "Read", None, None, &[]),
        prompt_eval("thanks-only", "thanks", None, &[]),
        prompt_eval(
            "image-task-no-hint",
            "generate an image of a dashboard",
            None,
            &[],
        ),
        prompt_eval(
            "spreadsheet-task-no-hint",
            "make me a spreadsheet budget",
            None,
            &[],
        ),
        prompt_eval("simple-answer-no-hint", "what time is it?", None, &[]),
    ]
}

#[test]
fn synthetic_prompt_eval_matrix() {
    let evals = synthetic_prompt_cases();

    for eval in &evals {
        run_eval(eval);
    }
}

mod host_cases;
use host_cases::expanded_transcript_host_evals;

fn dedupe_scenario_cases() -> Vec<HintEval> {
    vec![dedupe_eval(
        "dedupe-repeated-search-trigger",
        "rg -n \"ToolHint\" src/hooks",
        "find literal matches, repeated later in the same session",
        Some(HintCategory::Search),
        &["tracedecay_grep"],
    )]
}

#[test]
fn expanded_transcript_host_scenario_eval_matrix() {
    for eval in &expanded_transcript_host_evals() {
        run_eval(eval);
    }
}

#[test]
fn scenario_coverage_reaches_high_value_target() {
    const HIGH_VALUE_SCENARIO_SLOTS: usize = 80;
    const TARGET_PERCENT: usize = 90;

    let expanded = expanded_transcript_host_evals().len();
    let mut all_cases = Vec::new();
    all_cases.extend(real_world_prompt_cases());
    all_cases.extend(dynamic_action_context_cases());
    all_cases.extend(synthetic_prompt_cases());
    all_cases.extend(expanded_transcript_host_evals());
    all_cases.extend(dedupe_scenario_cases());
    let unique_names: BTreeSet<_> = all_cases.iter().map(|eval| eval.name).collect();
    assert_eq!(
        unique_names.len(),
        all_cases.len(),
        "scenario names must be unique"
    );
    let covered = unique_names.len();
    let covered_categories: BTreeSet<_> =
        all_cases.iter().filter_map(|eval| eval.expected).collect();
    let expected_categories: BTreeSet<_> = [
        HintCategory::Search,
        HintCategory::SemanticSearch,
        HintCategory::FileRead,
        HintCategory::ToolDescriptorRead,
        HintCategory::BroadRead,
        HintCategory::CallGraph,
        HintCategory::Impact,
        HintCategory::SymbolLookup,
        HintCategory::FileLookup,
        HintCategory::ProjectContext,
        HintCategory::SessionRecall,
        HintCategory::AtomicEdit,
        HintCategory::TypeOrientation,
        HintCategory::ExploreSubagent,
        HintCategory::SubagentStartContext,
        HintCategory::BuildDiagnostics,
        HintCategory::ReviewChanges,
        HintCategory::MemoryStore,
        HintCategory::EditRedundancy,
    ]
    .into_iter()
    .collect();
    let covered_families: BTreeSet<_> = all_cases.iter().flat_map(coverage_families).collect();
    let negative_cases = all_cases
        .iter()
        .filter(|eval| eval.expected.is_none())
        .count();
    assert!(
        covered * 100 >= HIGH_VALUE_SCENARIO_SLOTS * TARGET_PERCENT,
        "covered {covered}/{HIGH_VALUE_SCENARIO_SLOTS} high-value scenarios, below {TARGET_PERCENT}%"
    );
    assert!(
        expanded >= 37,
        "expanded matrix should add at least 37 transcript/host scenarios, got {expanded}"
    );
    assert_eq!(covered_categories, expected_categories);
    assert_eq!(
        covered_families,
        COVERAGE_FAMILIES.iter().copied().collect::<BTreeSet<_>>()
    );
    assert!(
        negative_cases >= 18,
        "expected at least 18 negative/silence cases, got {negative_cases}"
    );
}

#[test]
fn session_stream_eval_rotates_repeated_hints() {
    for eval in &dedupe_scenario_cases() {
        run_eval(eval);
    }

    let mut dedupe = ToolHintDedupe::default();
    let sequence = [
        HintCategory::Search,
        HintCategory::Search,
        HintCategory::CallGraph,
        HintCategory::Search,
        HintCategory::Impact,
        HintCategory::FileRead,
        HintCategory::Search,
        HintCategory::Search,
    ];
    let decisions: Vec<HintDecision> = sequence
        .into_iter()
        .map(|category| dedupe.decide("realistic-session", category))
        .collect();

    assert_eq!(
        decisions,
        vec![
            HintDecision::Emit,
            HintDecision::SuppressedDuplicate,
            HintDecision::Emit,
            HintDecision::SuppressedDuplicate,
            HintDecision::Emit,
            HintDecision::SuppressedBudget,
            HintDecision::Escalate,
            HintDecision::SuppressedDuplicate,
        ]
    );
}

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

#[derive(Clone)]
struct HintEval {
    name: &'static str,
    family: ScenarioFamily,
    input: ToolHintInput,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
    must_not_contain: &'static [&'static str],
}

fn prompt_eval(
    name: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    HintEval {
        name,
        family: ScenarioFamily::CodexPrompt,
        input: ToolHintInput {
            prompt: Some(prompt.to_string()),
            session_id: Some(format!("{name}-session")),
            ..ToolHintInput::default()
        },
        expected,
        must_contain,
        must_not_contain: &[
            "tracedecay is available via MCP",
            "Prefer tracedecay MCP tools",
            "run `tracedecay init`",
        ],
    }
}

fn shell_eval(
    name: &'static str,
    command: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    HintEval {
        name,
        family: ScenarioFamily::ShellSearch,
        input: ToolHintInput {
            tool_name: Some("Bash".to_string()),
            command: Some(command.to_string()),
            prompt: Some(prompt.to_string()),
            session_id: Some(format!("{name}-session")),
            ..ToolHintInput::default()
        },
        expected,
        must_contain,
        must_not_contain: &[
            "tracedecay is available via MCP",
            "Prefer tracedecay MCP tools",
            "run `tracedecay init`",
        ],
    }
}

fn tool_eval(
    name: &'static str,
    tool_name: &'static str,
    file_path: Option<&'static str>,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    HintEval {
        name,
        family: ScenarioFamily::AdapterShape,
        input: ToolHintInput {
            tool_name: Some(tool_name.to_string()),
            file_path: file_path.map(str::to_string),
            session_id: Some(format!("{name}-session")),
            ..ToolHintInput::default()
        },
        expected,
        must_contain,
        must_not_contain: &[
            "tracedecay is available via MCP",
            "Prefer tracedecay MCP tools",
            "run `tracedecay init`",
        ],
    }
}

fn input_eval(
    name: &'static str,
    input: ToolHintInput,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    HintEval {
        name,
        family: ScenarioFamily::AdapterShape,
        input: ToolHintInput {
            session_id: Some(format!("{name}-session")),
            ..input
        },
        expected,
        must_contain,
        must_not_contain: &[
            "tracedecay is available via MCP",
            "Prefer tracedecay MCP tools",
            "run `tracedecay init`",
        ],
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
    if let Some(skill) = category_skill(hint.category) {
        assert!(
            visible.contains(&format!("Skill: tracedecay:{skill}.")),
            "{} missing bundled skill trigger `tracedecay:{skill}` in:\n{}",
            eval.name,
            visible
        );
    }
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
    let mut families = vec![eval.family];
    let text = [
        Some(eval.name),
        eval.input.prompt.as_deref(),
        eval.input.command.as_deref(),
        eval.input.tool_name.as_deref(),
        eval.input.file_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_ascii_lowercase();

    if text.contains("claude") {
        families.push(ScenarioFamily::ClaudePrompt);
    }
    if text.contains("cursor") {
        families.push(ScenarioFamily::CursorPrompt);
    }
    if text.contains("codex") || eval.input.prompt.is_some() {
        families.push(ScenarioFamily::CodexPrompt);
    }
    if eval.input.command.is_some() {
        families.push(ScenarioFamily::ShellSearch);
    }
    if eval.input.tool_name.is_some() || eval.input.file_path.is_some() {
        families.push(ScenarioFamily::AdapterShape);
    }
    if !eval.input.hints_enabled {
        families.push(ScenarioFamily::Disabled);
    }
    if eval.expected.is_none() {
        families.push(ScenarioFamily::NegativeSilence);
    }
    if text.contains("quoted") {
        families.push(ScenarioFamily::QuotedData);
    }
    if text.contains("sibling")
        || text.contains("cross-repo")
        || text.contains("/home/zack/projects")
        || text.contains("another project")
    {
        families.push(ScenarioFamily::CrossProject);
    }

    match eval.expected {
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
        None => {}
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
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
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
        ),
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

fn expanded_transcript_host_evals() -> Vec<HintEval> {
    vec![
        prompt_eval("continue-stays-silent", "continue", None, &[]),
        prompt_eval(
            "babysit-subagents-prompt-stays-silent",
            "babysit all with subagents",
            None,
            &[],
        ),
        prompt_eval(
            "skill-creator-link-stays-silent",
            "use [$skill-creator](/home/zack/.codex/skills/.system/skill-creator/SKILL.md) to add a dev skill",
            None,
            &[],
        ),
        prompt_eval(
            "subagent-notification-stays-silent",
            "<subagent_notification>{\"message\":\"done\"}</subagent_notification>",
            None,
            &[],
        ),
        prompt_eval(
            "web-research-stays-silent",
            "search web to see some rust lsp implementations",
            None,
            &[],
        ),
        prompt_eval(
            "pr-stack-worktree-stays-silent",
            "look at the open pull request stack and create a branch from the tip",
            None,
            &[],
        ),
        prompt_eval(
            "set-goal-stays-silent",
            "set goal to implement phase 1",
            None,
            &[],
        ),
        prompt_eval(
            "github-review-command-stays-silent",
            "run gh pr view 319 --json body,comments,reviews",
            None,
            &[],
        ),
        prompt_eval(
            "package-install-help-stays-silent",
            "do we need sudo pnpm or can we pnpm install",
            None,
            &[],
        ),
        prompt_eval(
            "raw-lcm-session-request",
            "check over lcm sessions and find where we discussed hooks",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "cursor-context-length-recall",
            "Context length exceeded and cannot compress further, find the prior session",
            Some(HintCategory::SessionRecall),
            &["tracedecay_message_search"],
        ),
        prompt_eval(
            "prompt-type-error-capability",
            "can tracedecay see type errors etc?",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics", "tracedecay_diagnose"],
        ),
        prompt_eval(
            "prompt-lsp-typeerror-backfill",
            "can LSP passively collect typeerrors for all files in background time?",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        prompt_eval(
            "prompt-dashboard-diagnostics-design",
            "design the TraceDecay dashboard UI diagnostics phase for the hook engine project",
            Some(HintCategory::ProjectContext),
            &["tracedecay_project_search"],
        ),
        prompt_eval(
            "prompt-codebase-architecture-map",
            "map architecture of the hook engine in this codebase",
            Some(HintCategory::ProjectContext),
            &["tracedecay_project_search"],
        ),
        prompt_eval(
            "prompt-current-repo-broad-scan",
            "scan the repo for all hook hint behavior",
            Some(HintCategory::BroadRead),
            &["tracedecay_context"],
        ),
        prompt_eval(
            "prompt-symbol-lookup-variant",
            "look up symbol ToolHintDedupe",
            Some(HintCategory::SymbolLookup),
            &["tracedecay_context", "tracedecay_node"],
        ),
        prompt_eval(
            "prompt-callgraph-classify-hint",
            "who calls classify_hint?",
            Some(HintCategory::CallGraph),
            &["tracedecay_callers"],
        ),
        prompt_eval(
            "prompt-impact-blast-radius",
            "what is the blast radius of changing src/hooks/tool_hints.rs?",
            Some(HintCategory::Impact),
            &["tracedecay_impact"],
        ),
        prompt_eval(
            "prompt-review-diff",
            "review the diff and changed symbols before I push",
            Some(HintCategory::ReviewChanges),
            &["tracedecay_diff_context"],
        ),
        prompt_eval(
            "prompt-type-orientation-impls",
            "find trait impls and field writes for ToolHintInput",
            Some(HintCategory::TypeOrientation),
            &["tracedecay_field_sites", "tracedecay_impls"],
        ),
        input_eval(
            "subagent-context-handoff",
            ToolHintInput {
                tool_name: Some("SubagentStart".to_string()),
                prompt: Some("handoff focused context to the implementation agent".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::SubagentStartContext),
            &["tracedecay_context", "tracedecay_search"],
        ),
        input_eval(
            "subagent-doc-writing-stays-silent",
            ToolHintInput {
                tool_name: Some("Agent".to_string()),
                subagent_type: Some("docs".to_string()),
                prompt: Some("write onboarding copy only".to_string()),
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
        shell_eval(
            "git-grep-current-repo",
            "git grep -n classify_hint -- src",
            "find literal matches",
            Some(HintCategory::Search),
            &["tracedecay_grep"],
        ),
        shell_eval(
            "recursive-grep-current-repo",
            "grep -R \"classify_hint\" src",
            "find literal matches",
            Some(HintCategory::Search),
            &["tracedecay_grep"],
        ),
        shell_eval(
            "rg-list-matches-current-repo",
            "rg -l \"manifest.json|plugin_api\" src",
            "find files containing this text",
            Some(HintCategory::Search),
            &["tracedecay_grep"],
        ),
        shell_eval(
            "gh-pr-diff-review",
            "gh pr diff 319 --patch",
            "review this pr diff",
            Some(HintCategory::ReviewChanges),
            &["tracedecay_diff_context"],
        ),
        shell_eval(
            "fd-current-repo-files",
            "fd -e rs . src/hooks",
            "list Rust hook files",
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        shell_eval(
            "find-current-dir-files",
            "find . -name '*.rs'",
            "list Rust files",
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        shell_eval(
            "find-project-root-sibling",
            "find /home/zack/projects -maxdepth 2 -type d -name '*tracedecay*'",
            "find another project checkout",
            Some(HintCategory::ProjectContext),
            &["tracedecay_project_search"],
        ),
        shell_eval(
            "sed-source-file-read",
            "sed -n '1,120p' src/hooks/tool_hints.rs",
            "read this source range",
            Some(HintCategory::FileRead),
            &["tracedecay_outline", "tracedecay_read"],
        ),
        shell_eval(
            "cat-config-file-read",
            "cat Cargo.toml",
            "read config file",
            Some(HintCategory::FileRead),
            &["tracedecay_outline", "tracedecay_read"],
        ),
        shell_eval(
            "cargo-nextest-diagnostics",
            "cargo nextest run --workspace --profile ci",
            "reproduce CI test failures",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        shell_eval(
            "pnpm-build-diagnostics",
            "pnpm build",
            "check build errors",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        shell_eval(
            "npm-typecheck-diagnostics",
            "npm run typecheck",
            "check type errors",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        shell_eval(
            "pnpm-exec-pyright-diagnostics",
            "pnpm exec pyright",
            "check python type errors",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        shell_eval(
            "make-typecheck-diagnostics",
            "make typecheck",
            "check type errors",
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        input_eval(
            "pasted-rust-diagnostic",
            ToolHintInput {
                prompt: Some(
                    "error[E0308]: mismatched types\n --> src/hooks/tool_hints.rs:12:5".to_string(),
                ),
                ..ToolHintInput::default()
            },
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        tool_eval(
            "cursor-semantic-search-alias",
            "SemanticSearch",
            None,
            Some(HintCategory::SemanticSearch),
            &["tracedecay_context"],
        ),
        tool_eval(
            "cursor-codebase-search-alias",
            "codebase_search",
            None,
            Some(HintCategory::SemanticSearch),
            &["tracedecay_context"],
        ),
        tool_eval(
            "cursor-glob-tool",
            "Glob",
            None,
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        ),
        tool_eval(
            "read-package-json",
            "Read",
            Some("package.json"),
            Some(HintCategory::FileRead),
            &["tracedecay_outline"],
        ),
        tool_eval(
            "claude-memory-file-edit",
            "Write",
            Some("/home/zack/.claude/projects/foo/memory/notes.md"),
            Some(HintCategory::MemoryStore),
            &["tracedecay_fact_store"],
        ),
        tool_eval(
            "claude-source-file-write-stays-silent",
            "Write",
            Some("src/hooks/tool_hints.rs"),
            None,
            &[],
        ),
        tool_eval(
            "delete-tool-stays-silent",
            "Delete",
            Some("src/old.rs"),
            None,
            &[],
        ),
        input_eval(
            "hermes-session-meta-row-stays-silent",
            ToolHintInput {
                prompt: Some("session_meta".to_string()),
                ..ToolHintInput::default()
            },
            None,
            &[],
        ),
    ]
}

#[test]
fn expanded_transcript_host_scenario_eval_matrix() {
    for eval in &expanded_transcript_host_evals() {
        run_eval(eval);
    }
}

#[test]
fn scenario_coverage_reaches_high_value_target() {
    const BASELINE_MATRIX_CASES_BEFORE_TRANSCRIPT_EXPANSION: usize = 35;
    const HIGH_VALUE_SCENARIO_SLOTS: usize = 80;
    const TARGET_PERCENT: usize = 90;

    let expanded = expanded_transcript_host_evals().len();
    let covered = BASELINE_MATRIX_CASES_BEFORE_TRANSCRIPT_EXPANSION + expanded;
    let mut all_cases = Vec::new();
    all_cases.extend(real_world_prompt_cases());
    all_cases.extend(dynamic_action_context_cases());
    all_cases.extend(synthetic_prompt_cases());
    all_cases.extend(expanded_transcript_host_evals());
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
    ]
    .into_iter()
    .collect();
    let mut covered_families: BTreeSet<_> = all_cases.iter().flat_map(coverage_families).collect();
    covered_families.insert(ScenarioFamily::Dedupe);
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

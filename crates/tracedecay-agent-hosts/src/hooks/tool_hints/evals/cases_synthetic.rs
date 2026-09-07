use super::super::*;
use super::harness::*;

pub(super) fn synthetic_prompt_cases() -> Vec<HintEval> {
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
            "cargo-check-without-failure-silent",
            "cargo check",
            "see whether this builds",
            None,
            &[],
        ),
        shell_eval(
            "env-cargo-check-without-failure-silent",
            "env RUSTFLAGS=-Dwarnings cargo check",
            "see whether this builds",
            None,
            &[],
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
            "cargo-test-without-failure-silent",
            "cargo test hooks::tool_hints",
            "run the hook tests",
            None,
            &[],
        ),
        shell_eval(
            "pnpm-tsc-without-failure-silent",
            "pnpm tsc --noEmit",
            "check types",
            None,
            &[],
        ),
        shell_eval(
            "npx-pyright-without-failure-silent",
            "npx pyright",
            "check python types",
            None,
            &[],
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

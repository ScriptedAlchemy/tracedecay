use super::super::*;
use super::harness::*;

pub(super) fn expanded_transcript_host_evals() -> Vec<HintEval> {
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
        )
        .with_families(&[ScenarioFamily::CursorPrompt]),
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
            "fd-current-repo-rust-files",
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
            "cargo-nextest-behavioral-failure-silent",
            "cargo nextest run --workspace --profile ci",
            "reproduce CI test failures",
            None,
            &[],
        ),
        input_eval(
            "trusted-cargo-check-failure",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("cargo check --workspace".to_string()),
                trusted_failure: true,
                ..ToolHintInput::default()
            },
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        input_eval(
            "captured-rust-compiler-output",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("cargo check --workspace".to_string()),
                captured_output: Some(
                    "error[E0308]: mismatched types\n --> src/lib.rs:42:5".to_string(),
                ),
                ..ToolHintInput::default()
            },
            Some(HintCategory::BuildDiagnostics),
            &["tracedecay_diagnostics"],
        ),
        input_eval(
            "trusted-cargo-test-behavioral-failure-silent",
            ToolHintInput {
                tool_name: Some("Bash".to_string()),
                command: Some("cargo test hooks::tool_hints".to_string()),
                captured_output: Some(
                    "test result: FAILED. 3 passed; 1 failed\nerror: test failed".to_string(),
                ),
                trusted_failure: true,
                ..ToolHintInput::default()
            },
            None,
            &[],
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
        )
        .with_families(&[ScenarioFamily::CursorPrompt]),
        tool_eval(
            "cursor-codebase-search-alias",
            "codebase_search",
            None,
            Some(HintCategory::SemanticSearch),
            &["tracedecay_context"],
        )
        .with_families(&[ScenarioFamily::CursorPrompt]),
        tool_eval(
            "cursor-glob-tool",
            "Glob",
            None,
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        )
        .with_families(&[ScenarioFamily::CursorPrompt]),
        tool_eval(
            "read-package-json",
            "Read",
            Some("package.json"),
            Some(HintCategory::FileRead),
            &["tracedecay_outline"],
        ),
        tool_eval(
            "cursor-read-file-alias",
            "read_file",
            Some("src/hooks/cursor.rs"),
            Some(HintCategory::FileRead),
            &["tracedecay_outline"],
        )
        .with_families(&[ScenarioFamily::CursorPrompt]),
        tool_eval(
            "cursor-list-dir-alias",
            "list_dir",
            Some("src/hooks"),
            Some(HintCategory::FileLookup),
            &["tracedecay_files"],
        )
        .with_families(&[ScenarioFamily::CursorPrompt]),
        tool_eval(
            "claude-memory-file-edit",
            "Write",
            Some("/home/zack/.claude/projects/foo/memory/notes.md"),
            Some(HintCategory::MemoryStore),
            &["tracedecay_fact_store_add"],
        )
        .with_families(&[ScenarioFamily::ClaudePrompt]),
        tool_eval(
            "claude-source-file-write-stays-silent",
            "Write",
            Some("src/hooks/tool_hints.rs"),
            None,
            &[],
        )
        .with_families(&[ScenarioFamily::ClaudePrompt]),
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

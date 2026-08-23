use super::super::*;
use super::harness::*;

pub(super) fn dynamic_action_context_cases() -> Vec<HintEval> {
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
            &["tracedecay_fact_store_add"],
        ),
        input_eval(
            "claude-memory-edit-action",
            ToolHintInput {
                tool_name: Some("MultiEdit".to_string()),
                file_path: Some("/tmp/project/.claude/foo/memory/notes.md".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::MemoryStore),
            &["tracedecay_fact_store_add"],
        ),
        input_eval(
            "write-claude-md-action",
            ToolHintInput {
                tool_name: Some("Write".to_string()),
                file_path: Some("CLAUDE.md".to_string()),
                ..ToolHintInput::default()
            },
            Some(HintCategory::MemoryStore),
            &["tracedecay_fact_store_add"],
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

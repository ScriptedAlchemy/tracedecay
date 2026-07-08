use super::*;

#[derive(Clone)]
struct HintEval {
    name: &'static str,
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

#[test]
fn real_world_prompt_eval_matrix() {
    let evals = [
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
            "branch-or-pr-status",
            "What branch this on or pr",
            None,
            &[],
        ),
        prompt_eval("merge-pr-number", "Merge 64", None, &[]),
    ];

    for eval in &evals {
        run_eval(eval);
    }
}

#[test]
fn dynamic_action_context_eval_matrix() {
    let evals = [
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
    ];

    for eval in &evals {
        run_eval(eval);
    }
}

#[test]
fn synthetic_prompt_eval_matrix() {
    let evals = [
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
        prompt_eval(
            "call-chain-question",
            "what calls record_hint_analytics and what does it call?",
            Some(HintCategory::CallGraph),
            &["tracedecay_callers", "tracedecay_callees"],
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
    ];

    for eval in &evals {
        run_eval(eval);
    }
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

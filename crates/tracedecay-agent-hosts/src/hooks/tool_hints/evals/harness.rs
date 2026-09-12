use super::super::*;

const STATIC_BOILERPLATE: &[&str] = &[
    "tracedecay is available via MCP",
    "Prefer tracedecay MCP tools",
    "run `tracedecay init`",
];

#[derive(Clone)]
pub(super) struct HintEval {
    pub(super) name: &'static str,
    pub(super) input: ToolHintInput,
    pub(super) expected: Option<HintCategory>,
    pub(super) must_contain: &'static [&'static str],
    pub(super) must_not_contain: &'static [&'static str],
}

pub(super) fn prompt_eval(
    name: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
        ToolHintInput {
            prompt: Some(prompt.to_string()),
            session_id: Some(format!("{name}-session")),
            ..ToolHintInput::default()
        },
        expected,
        must_contain,
    )
}

pub(super) fn shell_eval(
    name: &'static str,
    command: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
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

pub(super) fn dedupe_eval(
    name: &'static str,
    command: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    shell_eval(name, command, prompt, expected, must_contain)
}

pub(super) fn tool_eval(
    name: &'static str,
    tool_name: &'static str,
    file_path: Option<&'static str>,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
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

pub(super) fn input_eval(
    name: &'static str,
    input: ToolHintInput,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    eval(
        name,
        ToolHintInput {
            session_id: Some(format!("{name}-session")),
            ..input
        },
        expected,
        must_contain,
    )
}

pub(super) fn eval(
    name: &'static str,
    input: ToolHintInput,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    HintEval {
        name,
        input,
        expected,
        must_contain,
        must_not_contain: STATIC_BOILERPLATE,
    }
}

pub(super) fn run_eval(eval: &HintEval) {
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

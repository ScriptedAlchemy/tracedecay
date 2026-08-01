use super::super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ScenarioFamily {
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
    UnexpectedChanges,
    NegativeSilence,
    Disabled,
    QuotedData,
    AdapterShape,
    Dedupe,
}

pub(super) const COVERAGE_FAMILIES: &[ScenarioFamily] = &[
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
    ScenarioFamily::UnexpectedChanges,
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
pub(super) struct HintEval {
    pub(super) name: &'static str,
    pub(super) families: Vec<ScenarioFamily>,
    pub(super) input: ToolHintInput,
    pub(super) expected: Option<HintCategory>,
    pub(super) must_contain: &'static [&'static str],
    pub(super) must_not_contain: &'static [&'static str],
}

impl HintEval {
    pub(super) fn with_families(mut self, extra: &[ScenarioFamily]) -> Self {
        self.families.extend_from_slice(extra);
        self
    }
}

pub(super) fn prompt_eval(
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

pub(super) fn shell_eval(
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

pub(super) fn dedupe_eval(
    name: &'static str,
    command: &'static str,
    prompt: &'static str,
    expected: Option<HintCategory>,
    must_contain: &'static [&'static str],
) -> HintEval {
    shell_eval(name, command, prompt, expected, must_contain)
        .with_families(&[ScenarioFamily::Dedupe])
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

pub(super) fn input_eval(
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

pub(super) fn eval(
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

pub(super) fn coverage_families(eval: &HintEval) -> Vec<ScenarioFamily> {
    eval.families.clone()
}

pub(super) fn default_families(
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
        Some(HintCategory::UnexpectedChanges) => {
            families.push(ScenarioFamily::UnexpectedChanges);
        }
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

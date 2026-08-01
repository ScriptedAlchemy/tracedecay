use super::*;
use std::collections::BTreeSet;

mod harness;

mod cases_dynamic;
mod cases_real_world;
mod cases_synthetic;

use cases_dynamic::dynamic_action_context_cases;
use cases_real_world::real_world_prompt_cases;
use cases_synthetic::synthetic_prompt_cases;
use harness::{COVERAGE_FAMILIES, coverage_families, dedupe_eval, run_eval};

mod host_cases;
use host_cases::expanded_transcript_host_evals;

fn dedupe_scenario_cases() -> Vec<harness::HintEval> {
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
        HintCategory::UnexpectedChanges,
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

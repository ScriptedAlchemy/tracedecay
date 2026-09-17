use super::*;

mod harness;

mod cases_dynamic;
mod cases_real_world;
mod cases_synthetic;

use harness::{dedupe_eval, run_eval};

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
    let decisions: Vec<HintDeliveryDecisionV1> = sequence
        .into_iter()
        .map(|category| dedupe.decide("realistic-session", category))
        .collect();

    assert_eq!(
        decisions,
        vec![
            HintDeliveryDecisionV1::Deliver,
            HintDeliveryDecisionV1::SuppressDuplicate,
            HintDeliveryDecisionV1::Deliver,
            HintDeliveryDecisionV1::SuppressDuplicate,
            HintDeliveryDecisionV1::Deliver,
            HintDeliveryDecisionV1::SuppressBudget,
            HintDeliveryDecisionV1::SuppressBudget,
            HintDeliveryDecisionV1::SuppressBudget,
        ]
    );
}

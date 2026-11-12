//! Turns deterministic action facts and prompt inferences into the intents one
//! Context Scout intent pass may suggest.
//!
//! Pure: the observed facts, the prompt inferences, the configured score, and
//! the intents already delivered this session are all explicit inputs.

use std::collections::BTreeSet;

use tracedecay_domain::configuration::{
    ContextScoutIntentKindV1, INTENT_MAX_PERMILLE, IntentDecisionSourceV1,
};

/// One intent pass's evidence.
#[derive(Clone, Copy, Debug)]
pub struct IntentDecisionInputV1<'a> {
    /// Observed tool actions, in classification priority order.
    pub facts: &'a [ContextScoutIntentKindV1],
    /// Deterministic interpretations of the newest prompt, in priority order.
    pub prompt_inferences: &'a [ContextScoutIntentKindV1],
    /// Score every qualifying intent contributes.
    pub action_fact_score_permille: u16,
    /// Intents already delivered in this session; never suggested again.
    pub delivered: &'a BTreeSet<ContextScoutIntentKindV1>,
}

/// One intent the pass may suggest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntentCandidateV1 {
    pub intent: ContextScoutIntentKindV1,
    /// Relevance on the Scout's `u16` scale: `round(score * u16::MAX)`.
    pub relevance: u16,
    pub source: IntentDecisionSourceV1,
}

/// The undelivered intents, observed facts first and then prompt inferences,
/// each in its own priority order with repeats dropped. An intent that is both
/// an observed fact and a prompt inference is an observed fact. Empty when
/// neither names an undelivered intent.
pub fn decide_intents(input: &IntentDecisionInputV1<'_>) -> Vec<IntentCandidateV1> {
    let relevance = permille_relevance(input.action_fact_score_permille);
    let sources = input
        .facts
        .iter()
        .map(|intent| (*intent, IntentDecisionSourceV1::ActionFact))
        .chain(
            input
                .prompt_inferences
                .iter()
                .map(|intent| (*intent, IntentDecisionSourceV1::PromptInference)),
        );
    let mut candidates: Vec<IntentCandidateV1> = Vec::new();
    for (intent, source) in sources {
        if input.delivered.contains(&intent)
            || candidates
                .iter()
                .any(|candidate| candidate.intent == intent)
        {
            continue;
        }
        candidates.push(IntentCandidateV1 {
            intent,
            relevance,
            source,
        });
    }
    candidates
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn permille_relevance(permille: u16) -> u16 {
    let fraction = f64::from(permille.min(INTENT_MAX_PERMILLE)) / f64::from(INTENT_MAX_PERMILLE);
    (fraction * f64::from(u16::MAX)).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent_action_facts::{ActionFactInputV1, action_facts, observed_action_facts};
    use ContextScoutIntentKindV1 as Kind;

    fn decide(
        facts: &[Kind],
        prompt_inferences: &[Kind],
        delivered: &BTreeSet<Kind>,
    ) -> Vec<IntentCandidateV1> {
        decide_intents(&IntentDecisionInputV1 {
            facts,
            prompt_inferences,
            action_fact_score_permille: 700,
            delivered,
        })
    }

    fn intents(candidates: &[IntentCandidateV1]) -> Vec<(Kind, IntentDecisionSourceV1)> {
        candidates
            .iter()
            .map(|candidate| (candidate.intent, candidate.source))
            .collect()
    }

    #[test]
    fn no_facts_and_no_inferences_is_no_candidate() {
        assert!(decide(&[], &[], &BTreeSet::new()).is_empty());
    }

    #[test]
    fn relevance_scales_permille_to_u16() {
        assert_eq!(permille_relevance(1_000), u16::MAX);
        assert_eq!(permille_relevance(1_001), u16::MAX);
        assert_eq!(permille_relevance(0), 0);
        assert_eq!(permille_relevance(500), 32_768);
        let candidates = decide(&[Kind::Search], &[], &BTreeSet::new());
        assert_eq!(candidates[0].relevance, permille_relevance(700));
    }

    #[test]
    fn facts_keep_priority_order_and_drop_repeats() {
        let candidates = decide(
            &[Kind::BuildDiagnostics, Kind::Search, Kind::BuildDiagnostics],
            &[],
            &BTreeSet::new(),
        );
        assert_eq!(
            intents(&candidates),
            vec![
                (Kind::BuildDiagnostics, IntentDecisionSourceV1::ActionFact),
                (Kind::Search, IntentDecisionSourceV1::ActionFact),
            ]
        );
    }

    #[test]
    fn observed_facts_precede_and_absorb_prompt_inferences() {
        let candidates = decide(
            &[Kind::CallGraph],
            &[Kind::Impact, Kind::CallGraph, Kind::Impact],
            &BTreeSet::new(),
        );
        assert_eq!(
            intents(&candidates),
            vec![
                (Kind::CallGraph, IntentDecisionSourceV1::ActionFact),
                (Kind::Impact, IntentDecisionSourceV1::PromptInference),
            ]
        );
    }

    #[test]
    fn delivered_intents_are_excluded_from_every_source() {
        let delivered = BTreeSet::from([Kind::SessionRecall, Kind::Search]);
        assert!(decide(&[Kind::Search], &[Kind::SessionRecall], &delivered).is_empty());
        let rest = decide(
            &[Kind::Search, Kind::FileRead],
            &[Kind::SessionRecall, Kind::Impact],
            &delivered,
        );
        assert_eq!(
            intents(&rest),
            vec![
                (Kind::FileRead, IntentDecisionSourceV1::ActionFact),
                (Kind::Impact, IntentDecisionSourceV1::PromptInference),
            ]
        );
    }

    #[test]
    fn prompt_inferences_and_observed_actions_come_from_the_action_fact_classifier() {
        let inferred = action_facts(&ActionFactInputV1 {
            prompt: Some("What is the blast radius of changing this signature?"),
            ..ActionFactInputV1::default()
        });
        assert!(inferred.contains(&Kind::Impact));
        let observed = observed_action_facts(&ActionFactInputV1 {
            tool_name: Some("Bash"),
            command: Some("rg impact src"),
            ..ActionFactInputV1::default()
        });
        assert_eq!(observed, vec![Kind::Search]);
        let candidates = decide(&observed, &inferred, &BTreeSet::new());
        assert_eq!(
            candidates
                .first()
                .map(|candidate| (candidate.intent, candidate.source)),
            Some((Kind::Search, IntentDecisionSourceV1::ActionFact))
        );
        assert!(candidates.iter().any(|candidate| {
            candidate.intent == Kind::Impact
                && candidate.source == IntentDecisionSourceV1::PromptInference
        }));
    }
}

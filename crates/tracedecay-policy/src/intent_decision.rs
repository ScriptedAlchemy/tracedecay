//! Selects the intents one Context Scout intent pass may suggest.
//!
//! Pure: the observed action facts, the configured action-fact score, and the
//! intents already delivered this session are all explicit inputs. Every
//! candidate is backed by a tool action the session window recorded, so the
//! selection is reproducible from the transcript alone.

use std::collections::BTreeSet;

use tracedecay_domain::configuration::{
    ContextScoutIntentKindV1, INTENT_MAX_PERMILLE, IntentDecisionSourceV1,
};

/// One intent pass's evidence.
#[derive(Clone, Copy, Debug)]
pub struct IntentDecisionInputV1<'a> {
    /// Observed tool actions, in classification priority order. Only actions
    /// the session actually recorded count: prompt text is not evidence.
    pub facts: &'a [ContextScoutIntentKindV1],
    /// Score an action fact contributes for its intent.
    pub action_fact_score_permille: u16,
    /// Intents already delivered in this session; never suggested again.
    pub delivered: &'a BTreeSet<ContextScoutIntentKindV1>,
}

/// One intent the pass may suggest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntentCandidateV1 {
    pub intent: ContextScoutIntentKindV1,
    /// Relevance on the Scout's `u16` scale: `round(permille / 1000 *
    /// u16::MAX)`.
    pub relevance: u16,
    pub source: IntentDecisionSourceV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentDecisionV1 {
    /// Non-empty, in action-fact priority order. Ties keep the order the
    /// window classified them in.
    Candidates(Vec<IntentCandidateV1>),
    /// No undelivered action fact held.
    NoSignal,
}

/// Decides which intents qualify: every undelivered observed action fact, once,
/// scored by the configured `action_fact_score_permille`.
pub fn decide_intents(input: &IntentDecisionInputV1<'_>) -> IntentDecisionV1 {
    let relevance = permille_relevance(input.action_fact_score_permille);
    let mut candidates: Vec<IntentCandidateV1> = Vec::new();
    for fact in input.facts {
        if input.delivered.contains(fact) || candidates.iter().any(|seen| seen.intent == *fact) {
            continue;
        }
        candidates.push(IntentCandidateV1 {
            intent: *fact,
            relevance,
            source: IntentDecisionSourceV1::ActionFact,
        });
    }
    if candidates.is_empty() {
        return IntentDecisionV1::NoSignal;
    }
    // Stable: equal relevance keeps action-fact priority order.
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.relevance));
    IntentDecisionV1::Candidates(candidates)
}

/// Converts a whole-permille score into the Scout's `u16` relevance scale.
/// Rounds half away from zero so the boundary is exact and integer-only.
fn permille_relevance(permille: u16) -> u16 {
    let permille = u32::from(permille.min(INTENT_MAX_PERMILLE));
    let scaled = permille * u32::from(u16::MAX) + u32::from(INTENT_MAX_PERMILLE) / 2;
    #[allow(clippy::cast_possible_truncation)]
    let relevance = (scaled / u32::from(INTENT_MAX_PERMILLE)) as u16;
    relevance
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent_action_facts::{ActionFactInputV1, observed_action_facts};
    use ContextScoutIntentKindV1 as Kind;

    fn decide(facts: &[Kind], delivered: &BTreeSet<Kind>) -> IntentDecisionV1 {
        decide_intents(&IntentDecisionInputV1 {
            facts,
            action_fact_score_permille: 700,
            delivered,
        })
    }

    fn candidates(decision: IntentDecisionV1) -> Vec<IntentCandidateV1> {
        match decision {
            IntentDecisionV1::Candidates(candidates) => candidates,
            other => panic!("expected candidates, got {other:?}"),
        }
    }

    #[test]
    fn no_facts_is_no_signal() {
        let none = BTreeSet::new();
        assert_eq!(decide(&[], &none), IntentDecisionV1::NoSignal);
        assert_eq!(
            decide(&[], &BTreeSet::from([Kind::Search])),
            IntentDecisionV1::NoSignal
        );
    }

    #[test]
    fn facts_keep_priority_order_and_drop_repeats() {
        let none = BTreeSet::new();
        let decided = candidates(decide(
            &[
                Kind::BuildDiagnostics,
                Kind::Search,
                Kind::BuildDiagnostics,
                Kind::FileRead,
            ],
            &none,
        ));
        assert_eq!(
            decided.iter().map(|c| c.intent).collect::<Vec<_>>(),
            vec![Kind::BuildDiagnostics, Kind::Search, Kind::FileRead],
            "equal scores keep priority order and drop repeats"
        );
        assert!(
            decided
                .iter()
                .all(|candidate| candidate.source == IntentDecisionSourceV1::ActionFact)
        );
    }

    #[test]
    fn delivered_intents_are_excluded() {
        let delivered = BTreeSet::from([Kind::Search, Kind::BuildDiagnostics]);
        let decided = candidates(decide(
            &[Kind::Search, Kind::BuildDiagnostics, Kind::FileRead],
            &delivered,
        ));
        assert_eq!(
            decided.iter().map(|c| c.intent).collect::<Vec<_>>(),
            vec![Kind::FileRead]
        );
        assert_eq!(
            decide(&[Kind::Search, Kind::BuildDiagnostics], &delivered),
            IntentDecisionV1::NoSignal
        );
    }

    #[test]
    fn score_scales_permille_to_u16_relevance() {
        assert_eq!(permille_relevance(0), 0);
        assert_eq!(permille_relevance(500), 32_768);
        assert_eq!(permille_relevance(600), 39_321);
        assert_eq!(permille_relevance(700), 45_875);
        assert_eq!(permille_relevance(INTENT_MAX_PERMILLE), u16::MAX);
        // An out-of-range score cannot exceed the full scale.
        assert_eq!(permille_relevance(INTENT_MAX_PERMILLE + 1), u16::MAX);
    }

    #[test]
    fn candidates_carry_the_configured_score() {
        let none = BTreeSet::new();
        let scored = |permille| {
            candidates(decide_intents(&IntentDecisionInputV1 {
                facts: &[Kind::Impact],
                action_fact_score_permille: permille,
                delivered: &none,
            }))
        };
        assert_eq!(scored(700)[0].relevance, 45_875);
        assert_eq!(scored(250)[0].relevance, 16_384);
    }

    /// Only observed tool activity is evidence: a prompt that names an intent
    /// must not produce a candidate on its own, because the durable receipt
    /// claims an action fact and the window recorded none.
    #[test]
    fn prompt_interpretation_is_not_an_action_fact() {
        let none = BTreeSet::new();
        let prompt_only = observed_action_facts(&ActionFactInputV1 {
            prompt: Some("who calls setup_project and which tests depend on it?"),
            ..ActionFactInputV1::default()
        });
        assert!(prompt_only.is_empty());
        assert_eq!(decide(&prompt_only, &none), IntentDecisionV1::NoSignal);

        let observed = observed_action_facts(&ActionFactInputV1 {
            tool_name: Some("Bash"),
            command: Some("rg impact src"),
            ..ActionFactInputV1::default()
        });
        assert_eq!(observed, vec![Kind::Search]);
        assert_eq!(
            candidates(decide(&observed, &none)),
            vec![IntentCandidateV1 {
                intent: Kind::Search,
                relevance: 45_875,
                source: IntentDecisionSourceV1::ActionFact,
            }]
        );
    }
}

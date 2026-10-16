//! Combines decision-engine answers with deterministic action facts into the
//! intents one Context Scout intent pass may suggest.
//!
//! Pure: the engine answers, the observed facts, the configured questions,
//! and the intents already delivered this session are all explicit inputs.

use std::collections::BTreeSet;

use tracedecay_domain::configuration::{
    ContextScoutIntentKindV1, INTENT_MAX_PERMILLE, IntentDecisionSourceV1, IntentQuestionSettingsV1,
};

/// One intent pass's evidence.
#[derive(Clone, Copy, Debug)]
pub struct IntentDecisionInputV1<'a> {
    /// Engine probabilities per intent, or `None` when the engine did not
    /// answer (disabled, unavailable, timed out, backpressured, or invalid).
    /// An answer for an intent with no configured question is ignored: it has
    /// no threshold.
    pub answers: Option<&'a [(ContextScoutIntentKindV1, f64)]>,
    /// Deterministic action facts, in classification priority order.
    pub facts: &'a [ContextScoutIntentKindV1],
    pub questions: &'a [IntentQuestionSettingsV1],
    /// Score an action fact contributes for its intent.
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

/// The highest engine answer when no intent reached its threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntentBestAnswerV1 {
    pub intent: ContextScoutIntentKindV1,
    pub probability_permille: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentDecisionV1 {
    /// Non-empty, most relevant first. Ties keep action-fact priority order,
    /// then question order.
    Candidates(Vec<IntentCandidateV1>),
    /// The engine answered at least one undelivered question, but none
    /// qualified and no undelivered action fact held.
    BelowThreshold { best: IntentBestAnswerV1 },
    /// Neither an undelivered answer nor an undelivered fact.
    NoSignal,
}

/// Decides which intents qualify. An engine answer qualifies when its
/// probability reaches the question's threshold and, when the question sets a
/// margin, exceeds the best other answered question by at least that margin.
/// Thresholds and margins compare in whole permille so the outcome does not
/// depend on floating-point rounding at the boundary.
pub fn decide_intents(input: &IntentDecisionInputV1<'_>) -> IntentDecisionV1 {
    let answered = answered_questions(input);
    let mut candidates: Vec<IntentCandidateV1> = Vec::new();

    let fact_relevance = permille_relevance(input.action_fact_score_permille);
    for fact in input.facts {
        if input.delivered.contains(fact) || candidates.iter().any(|seen| seen.intent == *fact) {
            continue;
        }
        candidates.push(IntentCandidateV1 {
            intent: *fact,
            relevance: fact_relevance,
            source: IntentDecisionSourceV1::ActionFact,
        });
    }

    for (index, answer) in answered.iter().enumerate() {
        if input.delivered.contains(&answer.question.intent) {
            continue;
        }
        let runner_up = answered
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, other)| other.permille)
            .max()
            .unwrap_or(0);
        let reaches_threshold = answer.permille >= answer.question.threshold_permille;
        let clears_margin = answer
            .question
            .margin_permille
            .is_none_or(|margin| answer.permille.saturating_sub(runner_up) >= margin);
        if !(reaches_threshold && clears_margin) {
            continue;
        }
        let relevance = probability_relevance(answer.probability);
        match candidates
            .iter_mut()
            .find(|candidate| candidate.intent == answer.question.intent)
        {
            Some(candidate) => {
                candidate.relevance = candidate.relevance.max(relevance);
                candidate.source = IntentDecisionSourceV1::Both;
            }
            None => candidates.push(IntentCandidateV1 {
                intent: answer.question.intent,
                relevance,
                source: IntentDecisionSourceV1::Model,
            }),
        }
    }

    if !candidates.is_empty() {
        // Stable: equal relevance keeps fact priority, then question order.
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.relevance));
        return IntentDecisionV1::Candidates(candidates);
    }
    let best = answered
        .iter()
        .filter(|answer| !input.delivered.contains(&answer.question.intent))
        .fold(None::<&AnsweredQuestion<'_>>, |best, answer| match best {
            Some(best) if best.permille >= answer.permille => Some(best),
            _ => Some(answer),
        });
    match best {
        Some(best) => IntentDecisionV1::BelowThreshold {
            best: IntentBestAnswerV1 {
                intent: best.question.intent,
                probability_permille: best.permille,
            },
        },
        None => IntentDecisionV1::NoSignal,
    }
}

struct AnsweredQuestion<'a> {
    question: &'a IntentQuestionSettingsV1,
    probability: f64,
    permille: u16,
}

/// Configured questions with a usable engine answer, in question order. A
/// non-finite probability is not an answer; finite ones clamp to `[0, 1]`.
/// When the engine repeats an intent, its first answer is used.
fn answered_questions<'a>(input: &IntentDecisionInputV1<'a>) -> Vec<AnsweredQuestion<'a>> {
    let Some(answers) = input.answers else {
        return Vec::new();
    };
    input
        .questions
        .iter()
        .filter_map(|question| {
            let probability = answers
                .iter()
                .find(|(intent, _)| *intent == question.intent)
                .map(|(_, probability)| *probability)
                .filter(|probability| probability.is_finite())?
                .clamp(0.0, 1.0);
            Some(AnsweredQuestion {
                question,
                probability,
                permille: probability_permille(probability),
            })
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn probability_permille(probability: f64) -> u16 {
    (probability.clamp(0.0, 1.0) * f64::from(INTENT_MAX_PERMILLE)).round() as u16
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn probability_relevance(probability: f64) -> u16 {
    (probability.clamp(0.0, 1.0) * f64::from(u16::MAX)).round() as u16
}

fn permille_relevance(permille: u16) -> u16 {
    probability_relevance(f64::from(permille.min(INTENT_MAX_PERMILLE)) / 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ContextScoutIntentKindV1 as Kind;

    fn question(intent: Kind, threshold: u16, margin: Option<u16>) -> IntentQuestionSettingsV1 {
        IntentQuestionSettingsV1 {
            intent,
            instructions: format!("Is the user asking about {}?", intent.as_key()),
            threshold_permille: threshold,
            margin_permille: margin,
        }
    }

    fn questions() -> Vec<IntentQuestionSettingsV1> {
        vec![
            question(Kind::SessionRecall, 600, None),
            question(Kind::CallGraph, 600, None),
            question(Kind::Impact, 600, None),
        ]
    }

    fn decide(
        answers: Option<&[(Kind, f64)]>,
        facts: &[Kind],
        questions: &[IntentQuestionSettingsV1],
        delivered: &BTreeSet<Kind>,
    ) -> IntentDecisionV1 {
        decide_intents(&IntentDecisionInputV1 {
            answers,
            facts,
            questions,
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
    fn no_answers_and_no_facts_is_no_signal() {
        let none = BTreeSet::new();
        assert_eq!(
            decide(None, &[], &questions(), &none),
            IntentDecisionV1::NoSignal
        );
        assert_eq!(
            decide(Some(&[]), &[], &questions(), &none),
            IntentDecisionV1::NoSignal
        );
    }

    #[test]
    fn answer_at_threshold_qualifies_and_below_does_not() {
        let none = BTreeSet::new();
        let at = candidates(decide(
            Some(&[(Kind::SessionRecall, 0.6)]),
            &[],
            &questions(),
            &none,
        ));
        assert_eq!(
            at,
            vec![IntentCandidateV1 {
                intent: Kind::SessionRecall,
                relevance: probability_relevance(0.6),
                source: IntentDecisionSourceV1::Model,
            }]
        );
        assert_eq!(
            decide(
                Some(&[(Kind::SessionRecall, 0.5994), (Kind::CallGraph, 0.2)]),
                &[],
                &questions(),
                &none,
            ),
            IntentDecisionV1::BelowThreshold {
                best: IntentBestAnswerV1 {
                    intent: Kind::SessionRecall,
                    probability_permille: 599,
                },
            }
        );
    }

    #[test]
    fn relevance_scales_probability_to_u16() {
        assert_eq!(probability_relevance(1.0), u16::MAX);
        assert_eq!(probability_relevance(0.0), 0);
        assert_eq!(probability_relevance(0.5), 32_768);
        assert_eq!(permille_relevance(1_000), u16::MAX);
        assert_eq!(permille_relevance(700), probability_relevance(0.7));
    }

    #[test]
    fn margin_requires_clear_lead_over_runner_up() {
        let none = BTreeSet::new();
        let with_margin = vec![
            question(Kind::CallGraph, 600, Some(200)),
            question(Kind::Impact, 600, None),
        ];
        // 0.7 leads 0.5 by exactly 200 permille: qualifies, despite f64
        // rounding of 0.7 - 0.5.
        let lead = candidates(decide(
            Some(&[(Kind::CallGraph, 0.7), (Kind::Impact, 0.5)]),
            &[],
            &with_margin,
            &none,
        ));
        assert_eq!(lead.len(), 1);
        assert_eq!(lead[0].intent, Kind::CallGraph);
        // 0.75 leads 0.65 by 100: CallGraph fails its margin; Impact has none.
        let close = candidates(decide(
            Some(&[(Kind::CallGraph, 0.75), (Kind::Impact, 0.65)]),
            &[],
            &with_margin,
            &none,
        ));
        assert_eq!(
            close.iter().map(|c| c.intent).collect::<Vec<_>>(),
            vec![Kind::Impact]
        );
    }

    #[test]
    fn facts_score_configured_value_and_combine_with_model_as_both() {
        let none = BTreeSet::new();
        let combined = candidates(decide(
            Some(&[(Kind::CallGraph, 0.9), (Kind::Impact, 0.1)]),
            &[Kind::CallGraph, Kind::Search],
            &questions(),
            &none,
        ));
        assert_eq!(
            combined,
            vec![
                IntentCandidateV1 {
                    intent: Kind::CallGraph,
                    relevance: probability_relevance(0.9),
                    source: IntentDecisionSourceV1::Both,
                },
                IntentCandidateV1 {
                    intent: Kind::Search,
                    relevance: permille_relevance(700),
                    source: IntentDecisionSourceV1::ActionFact,
                },
            ]
        );
        // A weaker model answer keeps the fact score; the source is still Both.
        let weaker = candidates(decide(
            Some(&[(Kind::CallGraph, 0.65)]),
            &[Kind::CallGraph],
            &questions(),
            &none,
        ));
        assert_eq!(
            weaker,
            vec![IntentCandidateV1 {
                intent: Kind::CallGraph,
                relevance: permille_relevance(700),
                source: IntentDecisionSourceV1::Both,
            }]
        );
    }

    #[test]
    fn facts_alone_qualify_without_an_engine_answer() {
        let none = BTreeSet::new();
        let facts = candidates(decide(
            None,
            &[Kind::BuildDiagnostics, Kind::Search, Kind::BuildDiagnostics],
            &questions(),
            &none,
        ));
        assert_eq!(
            facts.iter().map(|c| c.intent).collect::<Vec<_>>(),
            vec![Kind::BuildDiagnostics, Kind::Search],
            "equal fact scores keep priority order and drop repeats"
        );
    }

    #[test]
    fn delivered_intents_are_excluded_from_every_outcome() {
        let delivered = BTreeSet::from([Kind::SessionRecall, Kind::Search]);
        assert_eq!(
            decide(
                Some(&[(Kind::SessionRecall, 0.95)]),
                &[Kind::Search],
                &questions(),
                &delivered,
            ),
            IntentDecisionV1::NoSignal
        );
        let rest = candidates(decide(
            Some(&[(Kind::SessionRecall, 0.95), (Kind::Impact, 0.8)]),
            &[Kind::Search],
            &questions(),
            &delivered,
        ));
        assert_eq!(
            rest.iter().map(|c| c.intent).collect::<Vec<_>>(),
            vec![Kind::Impact]
        );
        assert_eq!(
            decide(
                Some(&[(Kind::SessionRecall, 0.95), (Kind::Impact, 0.3)]),
                &[],
                &questions(),
                &delivered,
            ),
            IntentDecisionV1::BelowThreshold {
                best: IntentBestAnswerV1 {
                    intent: Kind::Impact,
                    probability_permille: 300,
                },
            }
        );
    }

    #[test]
    fn unquestioned_and_non_finite_answers_are_ignored() {
        let none = BTreeSet::new();
        assert_eq!(
            decide(
                Some(&[(Kind::MemoryStore, 0.99), (Kind::SessionRecall, f64::NAN)]),
                &[],
                &questions(),
                &none,
            ),
            IntentDecisionV1::NoSignal
        );
        let clamped = candidates(decide(
            Some(&[(Kind::SessionRecall, 7.0), (Kind::CallGraph, -1.0)]),
            &[],
            &questions(),
            &none,
        ));
        assert_eq!(clamped.len(), 1);
        assert_eq!(clamped[0].relevance, u16::MAX);
    }

    #[test]
    fn candidates_sort_by_relevance_then_priority() {
        let none = BTreeSet::new();
        let sorted = candidates(decide(
            Some(&[
                (Kind::SessionRecall, 0.62),
                (Kind::CallGraph, 0.97),
                (Kind::Impact, 0.62),
            ]),
            &[Kind::FileRead],
            &questions(),
            &none,
        ));
        assert_eq!(
            sorted.iter().map(|c| c.intent).collect::<Vec<_>>(),
            vec![
                Kind::CallGraph,
                Kind::FileRead,
                Kind::SessionRecall,
                Kind::Impact
            ]
        );
    }

    #[test]
    fn zero_threshold_still_needs_an_answer() {
        let none = BTreeSet::new();
        let open = vec![question(Kind::Impact, 0, None)];
        let answered = candidates(decide(Some(&[(Kind::Impact, 0.0)]), &[], &open, &none));
        assert_eq!(answered[0].relevance, 0);
        assert_eq!(
            decide(Some(&[]), &[], &open, &none),
            IntentDecisionV1::NoSignal
        );
    }
}

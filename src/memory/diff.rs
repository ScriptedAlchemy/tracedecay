//! Write-time near-duplicate / conflict classification for `add_fact`.
//!
//! `MemoryStore::add_fact` runs every new fact through this module after the
//! exact-duplicate check: FTS candidates are scored with lexical overlap plus
//! phase-cosine similarity, and the strongest match is classified as
//! `near_duplicate` or `possible_conflict`. The result is a REPORT returned to
//! the writer — nothing here auto-deletes or auto-merges, and nothing here
//! calls an LLM; review stays with the caller (agent, human, or the Hermes
//! LLM curation layer).

use super::similarity::{lexical_overlap, phase_cosine_similarity};
use super::types::AddFactDiffKind;

/// A new fact whose strongest candidate scores above this is a near-duplicate.
pub const NEAR_DUPLICATE_THRESHOLD: f64 = 0.9;
/// Negation/state-change cues only flag a conflict at or above this
/// similarity; below it, texts can share domain vocabulary without being
/// about the same subject.
pub const CONFLICT_THRESHOLD: f64 = 0.7;
/// Phase-cosine similarity only contributes to the combined score above this
/// floor: same-domain content clusters in the 0.70–0.85 band and would
/// otherwise produce false near-duplicate matches.
const COSINE_CONTRIBUTION_FLOOR: f64 = 0.85;

/// Negation / state-change cues that signal a possible supersession or
/// conflict between two similar facts.
///
/// Ported (adapted) from the mnemon project's `negationWords` list
/// (`internal/search/diff.go`, Apache-2.0 — see the repository NOTICE file).
/// Single common words like "not" are intentionally excluded — mnemon's
/// comments note they appear constantly in ordinary prose and cause false
/// CONFLICT classifications; only clear multi-word or unambiguous
/// state-change markers are kept.
const NEGATION_CUES: &[&str] = &[
    "no longer",
    "switched from",
    "instead of",
    "rather than",
    "replaced",
    "supersedes",
    "superseded",
    "deprecated",
];

/// True when `text` contains one of the negation / state-change cues.
pub fn contains_negation_cue(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    NEGATION_CUES.iter().any(|cue| lower.contains(cue))
}

/// Conservative content-normalized equivalence: equal after case folding and
/// collapsing whitespace runs. Used as the ONLY justification for skipping an
/// insert on a `>0.9` near-duplicate — anything weaker still inserts and
/// merely reports.
pub fn normalized_equivalent(a: &str, b: &str) -> bool {
    fn normalize(text: &str) -> String {
        text.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ")
    }
    normalize(a) == normalize(b)
}

/// Combined lexical + holographic similarity between a new fact and one
/// existing candidate. Token overlap is the baseline; the phase-cosine score
/// contributes only when it is high enough to be trustworthy (mnemon's
/// combination rule, adapted to phase vectors).
pub fn combined_similarity(new_content: &str, existing_content: &str, cosine: Option<f64>) -> f64 {
    let (_, token_overlap, _) = lexical_overlap(new_content, existing_content);
    let mut similarity = token_overlap;
    if let Some(cos) = cosine {
        if cos >= COSINE_CONTRIBUTION_FLOOR && cos > similarity {
            similarity = cos;
        }
    }
    similarity
}

/// Convenience wrapper for callers holding both phase vectors.
pub fn vector_similarity(a: &[f64], b: &[f64]) -> f64 {
    phase_cosine_similarity(a, b)
}

/// Classifies how a new fact relates to its strongest existing candidate.
pub fn classify_add_diff(
    similarity: f64,
    new_content: &str,
    existing_content: &str,
) -> AddFactDiffKind {
    if similarity >= CONFLICT_THRESHOLD
        && (contains_negation_cue(new_content) || contains_negation_cue(existing_content))
    {
        return AddFactDiffKind::PossibleConflict;
    }
    if similarity > NEAR_DUPLICATE_THRESHOLD {
        return AddFactDiffKind::NearDuplicate;
    }
    AddFactDiffKind::Add
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negation_cues_match_state_changes_not_bare_not() {
        assert!(contains_negation_cue("We no longer use Redis for caching"));
        assert!(contains_negation_cue("Switched from npm to pnpm"));
        assert!(contains_negation_cue("Use tokio instead of async-std"));
        assert!(contains_negation_cue("The v1 API is deprecated"));
        assert!(contains_negation_cue("ESLint replaced TSLint here"));
        assert!(contains_negation_cue(
            "pnpm supersedes the earlier npm preference"
        ));
        assert!(!contains_negation_cue("This is not a conflict marker"));
        assert!(!contains_negation_cue("Do not store secrets in memory"));
    }

    #[test]
    fn normalized_equivalence_is_whitespace_and_case_only() {
        assert!(normalized_equivalent(
            "Use  pnpm\tfor installs",
            "use pnpm for installs"
        ));
        assert!(!normalized_equivalent(
            "Use pnpm for installs",
            "Use pnpm for installs."
        ));
        assert!(!normalized_equivalent("Use pnpm", "Use npm"));
    }

    #[test]
    fn classify_add_diff_tiers() {
        assert_eq!(
            classify_add_diff(0.95, "same fact body", "same fact body"),
            AddFactDiffKind::NearDuplicate
        );
        assert_eq!(
            classify_add_diff(0.75, "we no longer use Redis", "we use Redis for caching"),
            AddFactDiffKind::PossibleConflict
        );
        assert_eq!(
            classify_add_diff(0.4, "unrelated", "facts"),
            AddFactDiffKind::Add
        );
        // Below the conflict threshold, cues do not flag conflicts.
        assert_eq!(
            classify_add_diff(0.5, "we no longer use Redis", "butterfly survey notes"),
            AddFactDiffKind::Add
        );
    }

    #[test]
    fn combined_similarity_ignores_low_cosine() {
        let sim = combined_similarity("alpha beta gamma", "alpha beta gamma", Some(0.5));
        assert!((sim - 1.0).abs() < f64::EPSILON);
        // Low token overlap + sub-floor cosine stays low.
        let sim = combined_similarity("alpha beta", "delta epsilon", Some(0.80));
        assert!(sim < 0.1);
        // High cosine lifts the score.
        let sim = combined_similarity("alpha beta", "delta epsilon", Some(0.95));
        assert!((sim - 0.95).abs() < f64::EPSILON);
    }
}

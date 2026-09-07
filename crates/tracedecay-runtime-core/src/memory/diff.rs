//! Shared content-change cues for canonical contradiction analysis.

/// A strongest match above this score is a near duplicate unless a conflict
/// cue takes precedence.
pub const NEAR_DUPLICATE_SCORE_MILLIONTHS: u32 = 900_000;
/// State-change cues classify a match at or above this score as a possible
/// conflict.
pub const POSSIBLE_CONFLICT_SCORE_MILLIONTHS: u32 = 700_000;
/// Matches below this score are not useful add-time comparison evidence.
pub const ADD_COMPARISON_REPORT_FLOOR_MILLIONTHS: u32 = 500_000;

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

/// Exact add deduplication is deliberately narrower than semantic similarity:
/// only case-folding and whitespace normalization may suppress a new commit.
pub fn normalized_equivalent(left: &str, right: &str) -> bool {
    fn normalize(value: &str) -> String {
        value
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ")
    }
    normalize(left) == normalize(right)
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
    fn normalized_equivalence_is_case_and_whitespace_only() {
        assert!(normalized_equivalent(
            "Use  pnpm\tfor installs",
            "use pnpm for installs",
        ));
        assert!(!normalized_equivalent(
            "Use pnpm for installs",
            "Use pnpm for installs.",
        ));
    }
}

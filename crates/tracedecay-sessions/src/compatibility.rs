use std::collections::HashMap;

use sha2::{Digest, Sha256};

pub const MAX_DERIVED_TEXT_CHARS: usize = 64 * 1024;
pub const MAX_DERIVED_SNIPPET_CHARS: usize = 4 * 1024;
pub const DERIVED_TRUNCATION_MARKER: &str = "\n[derived snippet truncated by tracedecay]";
pub const RERANK_OVERFETCH_FACTOR: usize = 4;

pub struct RelatedMessageCopyIdentity<'a> {
    pub provider: &'a str,
    pub family_session_id: &'a str,
    pub session_id: &'a str,
    pub is_subagent: bool,
    pub content: &'a str,
}

pub fn projected_content_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

pub fn derived_text_for_index(raw: &str) -> String {
    derived_text_with_cap(raw, MAX_DERIVED_TEXT_CHARS)
}

pub fn derived_text_for_snippet(raw: &str) -> String {
    derived_text_with_cap(raw, MAX_DERIVED_SNIPPET_CHARS)
}

pub fn rerank_fetch_limit(limit: usize, max_fetch: usize) -> usize {
    limit
        .saturating_mul(RERANK_OVERFETCH_FACTOR)
        .min(max_fetch)
        .max(limit)
}

pub fn dedupe_related_message_copies<T>(
    rows: Vec<T>,
    identity: impl for<'a> Fn(&'a T) -> RelatedMessageCopyIdentity<'a>,
) -> Vec<T> {
    let mut family_rows: HashMap<(String, String, String), usize> = HashMap::new();
    let mut representative_meta = Vec::with_capacity(rows.len());
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        let row_identity = identity(&row);
        let normalized = normalized_content_identity(row_identity.content);
        if normalized.is_empty() {
            representative_meta.push((
                row_identity.session_id.to_string(),
                row_identity.is_subagent,
            ));
            kept.push(row);
            continue;
        }
        let key = (
            row_identity.provider.to_string(),
            row_identity.family_session_id.to_string(),
            normalized,
        );
        let session_id = row_identity.session_id.to_string();
        let is_subagent = row_identity.is_subagent;
        let Some(&existing_index) = family_rows.get(&key) else {
            family_rows.insert(key, kept.len());
            representative_meta.push((session_id, is_subagent));
            kept.push(row);
            continue;
        };
        let (existing_session_id, existing_is_subagent) = &representative_meta[existing_index];
        if existing_session_id == &session_id {
            representative_meta.push((session_id, is_subagent));
            kept.push(row);
        } else if *existing_is_subagent && !is_subagent {
            representative_meta[existing_index] = (session_id, is_subagent);
            kept[existing_index] = row;
        }
    }
    kept
}

pub fn is_inventory_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let mentions_transcript_dir = lower.contains(".jsonl")
        || lower.contains("sessions/")
        || lower.contains(".claude")
        || lower.contains(".codex")
        || lower.contains("transcript");
    let looks_like_listing = text.contains("**/")
        || lower.contains("\"pattern\"")
        || lower.contains("glob(")
        || lower.contains("\"glob\"")
        || lower.starts_with("ls ")
        || lower.contains(" ls ")
        || lower.contains("find ")
        || lower.contains("rg -")
        || lower.contains("grep -");
    if mentions_transcript_dir && looks_like_listing {
        return true;
    }
    if path_list_dominated(text) {
        return true;
    }
    is_branch_inventory(&lower)
}

fn derived_text_with_cap(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() <= max_chars {
        return raw.to_string();
    }

    let marker_chars = DERIVED_TRUNCATION_MARKER.chars().count();
    let budget = max_chars.saturating_sub(marker_chars);
    let mut derived = raw.chars().take(budget).collect::<String>();
    derived.push_str(DERIVED_TRUNCATION_MARKER);
    derived
}

fn normalized_content_identity(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_branch_inventory(lower: &str) -> bool {
    const LISTING_INDICATORS: [&str; 9] = [
        "inventory",
        "roster",
        "fleet",
        "sweep",
        "listing",
        "catalog",
        "roll call",
        "index of",
        "list of",
    ];
    let mentions_branch_or_worktree = lower.contains("branch") || lower.contains("worktree");
    if !mentions_branch_or_worktree {
        return false;
    }
    if shows_substantive_work(lower) {
        return false;
    }
    LISTING_INDICATORS
        .iter()
        .any(|indicator| lower.contains(indicator))
}

fn shows_substantive_work(lower: &str) -> bool {
    const WORK_VERBS: [&str; 4] = ["implemented", "fixed", "refactored", "committed"];
    if lower.contains("diff --git") || lower.contains("@@ ") {
        return true;
    }
    WORK_VERBS
        .iter()
        .any(|verb| contains_affirmative_verb(lower, verb))
}

fn contains_affirmative_verb(lower: &str, verb: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(verb) {
        let idx = search_from + rel;
        let end = idx + verb.len();
        let before = &lower[..idx];
        let after = &lower[end..];
        let left_boundary = before
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_ascii_alphanumeric());
        let right_boundary = after
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphanumeric());
        if left_boundary && right_boundary && !negated_before(before) {
            return true;
        }
        search_from = end;
    }
    false
}

fn negated_before(before: &str) -> bool {
    const NEGATIONS: [&str; 4] = ["nothing", "never", "not", "no"];
    before
        .split_whitespace()
        .rev()
        .take(3)
        .map(|word| word.trim_matches(|character: char| !character.is_ascii_alphanumeric()))
        .any(|word| NEGATIONS.contains(&word))
}

fn path_list_dominated(text: &str) -> bool {
    let mut total = 0usize;
    let mut path_like = 0usize;
    for token in text.split_whitespace() {
        total += 1;
        if token_is_path_like(token) {
            path_like += 1;
        }
    }
    total >= 4 && path_like >= 3 && path_like * 5 >= total * 3
}

fn token_is_path_like(token: &str) -> bool {
    let token = token
        .trim_matches(|character: char| matches!(character, '"' | '\'' | ',' | '`' | '(' | ')'));
    if token.len() < 4 {
        return false;
    }
    let has_sep = token.contains('/') && !token.starts_with("//");
    let has_ext = [
        ".jsonl", ".json", ".rs", ".ts", ".tsx", ".js", ".py", ".md", ".toml", ".txt", ".log",
    ]
    .iter()
    .any(|extension| token.ends_with(extension));
    has_sep
        && (has_ext
            || token
                .chars()
                .any(|character| character.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_index_text_caps_without_mutating_source_content() {
        // Deterministic replacement for the deleted LCM ingest source-scan
        // guard: derived index text is capped through the application contract
        // while the authoritative raw payload remains lossless.
        let content = format!("{}{}", "a".repeat(300_000), "::lossless-tail");
        let derived = derived_text_for_index(&content);
        assert!(
            derived.chars().count() <= MAX_DERIVED_TEXT_CHARS,
            "derived index text must honor MAX_DERIVED_TEXT_CHARS"
        );
        assert!(
            derived.contains(DERIVED_TRUNCATION_MARKER),
            "oversized derived text must carry the application truncation marker"
        );
        assert!(
            content.ends_with("::lossless-tail"),
            "source content must remain byte-exact after derivation"
        );
        assert_eq!(
            content.chars().count(),
            300_000 + "::lossless-tail".chars().count()
        );
        assert_eq!(
            crate::lcm::MAX_DERIVED_TEXT_CHARS,
            MAX_DERIVED_TEXT_CHARS,
            "LCM must re-export the application derived-text cap, not redefine it"
        );
        assert_eq!(
            crate::lcm::DERIVED_TRUNCATION_MARKER,
            DERIVED_TRUNCATION_MARKER
        );
        assert_eq!(
            crate::lcm::derived_text_for_index(&content),
            derived,
            "LCM derived_text_for_index must be the application helper"
        );
    }

    #[test]
    fn rerank_fetch_limit_never_panics_when_limit_exceeds_cap() {
        assert_eq!(rerank_fetch_limit(500, 200), 500);
        assert_eq!(rerank_fetch_limit(10, 200), 40);
        assert_eq!(rerank_fetch_limit(80, 200), 200);
        assert_eq!(rerank_fetch_limit(0, 200), 0);
    }

    #[test]
    fn normalized_content_identity_ignores_case_and_whitespace_only() {
        assert_eq!(
            normalized_content_identity("  Open pull\n requests TO fix. "),
            "open pull requests to fix."
        );
        assert_ne!(
            normalized_content_identity("open pull requests to fix"),
            normalized_content_identity("open pull requests to review")
        );
    }

    #[test]
    fn transcript_glob_listing_is_inventory() {
        assert!(is_inventory_text(
            "Glob **/*.jsonl over .claude sessions for branch redundancy"
        ));
    }

    #[test]
    fn substantive_implementation_is_not_inventory() {
        assert!(!is_inventory_text(
            "implemented branch redundancy scoring in the ranker"
        ));
        assert!(!is_inventory_text(
            "Implementing the retrieval eval harness on codex/retrieval-evals-analytics: \
             seeded a fixture session store and scored message_search ranking with \
             recomputed precision metrics."
        ));
    }

    #[test]
    fn prose_branch_inventory_is_inventory() {
        assert!(is_inventory_text(
            "Branch inventory sweep lists codex/retrieval-evals-analytics as one of many \
             active branches, alongside codex/session-recovery-fixes, codex/redundancy-evals, \
             release-plz, and master."
        ));
        assert!(is_inventory_text(
            "Worktree fleet status again names codex/retrieval-evals-analytics amid twelve \
             other branches; nothing is implemented in this session, it is only an index of \
             branch names."
        ));
        assert!(is_inventory_text(
            "Daily branch roster mentions codex/retrieval-evals-analytics once more among the \
             archived and stale branches tracked across every worktree."
        ));
    }

    #[test]
    fn branch_listing_work_with_evidence_is_not_inventory() {
        assert!(!is_inventory_text(
            "Implemented the branch listing feature on codex/foo; diff attached."
        ));
        assert!(!is_inventory_text(
            "Fixed the worktree sweep inventory bug; here's the diff:\n\
             ```\ndiff --git a/src/sweep.rs b/src/sweep.rs\n@@ -1 +1 @@\n```"
        ));
        assert!(!is_inventory_text(
            "Refactored the branch roster listing into a shared helper and committed it."
        ));
    }

    #[test]
    fn fenced_branch_roster_stays_inventory() {
        assert!(is_inventory_text(
            "Branch inventory sweep:\n```\ncodex/foo\ncodex/bar\ncodex/baz\n```"
        ));
    }

    #[test]
    fn genuine_roster_with_negated_work_verb_stays_inventory() {
        assert!(is_inventory_text(
            "Worktree fleet status again names codex/retrieval-evals-analytics amid twelve \
             other branches; nothing is implemented in this session, it is only an index of \
             branch names."
        ));
        assert!(is_inventory_text(
            "Branch inventory listing of codex/a, codex/b, and codex/c across every worktree."
        ));
    }

    #[test]
    fn branch_mention_without_listing_vocab_is_not_inventory() {
        assert!(!is_inventory_text(
            "the literal foo-bar marker on a scoped branch"
        ));
    }

    #[test]
    fn rerank_fetch_limit_over_fetches_within_bounds() {
        assert_eq!(rerank_fetch_limit(10, 100), 40);
        assert_eq!(rerank_fetch_limit(30, 100), 100);
        assert_eq!(rerank_fetch_limit(0, 100), 0);
    }
}

//! Shared transcript-noise classification for message retrieval re-ranking.
//!
//! Both the LCM grep path (`sessions::lcm::query`, BM25-free recency/relevance
//! grep over the raw store) and the global session-message search path
//! (`GlobalDb::search_session_messages*`, BM25 over `session_messages_fts`)
//! surface the same failure mode: an *inventory/listing* message — a glob/find
//! tool call over transcript directories, a path-list dump, or a prose branch/
//! worktree roster that merely name-drops many identifiers — matches a query
//! and outranks the message that actually did the work. This module is the one
//! place the heuristic lives so the two retrieval surfaces classify noise
//! identically instead of drifting apart.
//!
//! The treatment is always a *downrank, never a drop*: callers over-fetch by
//! [`RERANK_OVERFETCH_FACTOR`] (via [`rerank_fetch_limit`]), stably move
//! inventory hits below substantive ones, then truncate to the caller's limit,
//! so a downranked hit still surfaces when it is the only match.

use std::collections::HashMap;

/// Over-fetch multiplier applied before the deterministic re-rank so that
/// substantive hits buried below inventory/listing noise in the raw ranking
/// order can still surface within the caller's `limit`.
pub const RERANK_OVERFETCH_FACTOR: usize = 4;

/// Stable identity for transcript content copied between a parent session and
/// its child agents. Providers preserve punctuation but may change surrounding
/// whitespace while forwarding the prompt, so identity ignores case and runs
/// of whitespace without otherwise rewriting the message.
pub fn normalized_content_identity(text: &str) -> String {
    text.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct RelatedMessageCopyIdentity<'a> {
    pub provider: &'a str,
    pub family_session_id: &'a str,
    pub session_id: &'a str,
    pub is_subagent: bool,
    pub content: &'a str,
}

/// Collapse copies shared by different sessions in one parent/child family.
/// Repeated rows inside one session and identical content in unrelated parent
/// sessions remain distinct. If a parent row appears after a child copy, the
/// parent replaces that copy in place without disturbing page order.
pub fn dedupe_related_message_copies<T>(
    rows: Vec<T>,
    identity: impl for<'a> Fn(&'a T) -> RelatedMessageCopyIdentity<'a>,
) -> Vec<T> {
    let mut family_rows: HashMap<(String, String, String), usize> = HashMap::new();
    let mut representative_meta: Vec<(String, bool)> = Vec::with_capacity(rows.len());
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

/// SQL predicate for provider-neutral direct-user/tool-result filtering.
/// Tool results are recognized by role, explicit kind, or bounded tool-event
/// metadata because Claude stores many tool results as role `user` messages.
pub fn message_type_predicate_sql(
    alias: &str,
    has_kind_column: bool,
    message_type: crate::SessionMessageType,
) -> Option<String> {
    use crate::SessionMessageType;

    if matches!(message_type, SessionMessageType::All) {
        return None;
    }
    let kind_clause = if has_kind_column {
        format!(" OR lower(COALESCE({alias}.kind, '')) IN ('tool_result', 'tool_output')")
    } else {
        String::new()
    };
    let tool_result = format!(
        "({alias}.role = 'tool'{kind_clause} \
         OR CASE WHEN json_valid(COALESCE({alias}.metadata_json, '')) \
              THEN EXISTS (SELECT 1 FROM json_each({alias}.metadata_json, '$.tool_events') event \
                           WHERE json_extract(event.value, '$.type') = 'tool_result') \
              ELSE 0 END)"
    );
    Some(match message_type {
        SessionMessageType::All => unreachable!(),
        SessionMessageType::DirectUser => {
            format!("({alias}.role = 'user' AND NOT {tool_result})")
        }
        SessionMessageType::ToolResult => tool_result,
    })
}

/// Fetch budget before the re-rank stage: over-fetch by
/// [`RERANK_OVERFETCH_FACTOR`], capped at `max_fetch` but never below the
/// caller's explicit `limit`. The `min`-then-`max` order (rather than
/// `clamp`) is deliberate: a limit above `max_fetch` must widen the fetch to
/// honor the request, not panic on an inverted clamp range.
pub fn rerank_fetch_limit(limit: usize, max_fetch: usize) -> usize {
    limit
        .saturating_mul(RERANK_OVERFETCH_FACTOR)
        .min(max_fetch)
        .max(limit)
}

/// Cheap, deterministic heuristic: is this message text a transcript
/// inventory/listing (a glob/find tool call over transcript or session
/// directories), a path-list-dominated dump, or a prose branch/worktree roster
/// that merely enumerates identifiers — rather than substantive conversation?
/// Such messages match many unrelated queries and drown out real answers.
/// Operates only on the supplied text — no extra DB reads.
pub fn is_inventory_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    // A listing/glob/find/grep invocation aimed at transcript or session
    // directories: the classic "inventory" tool call over `**/*.jsonl` etc.
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

/// A prose branch/worktree *inventory*: a message that mentions branches or
/// worktrees and frames itself as an enumeration (an inventory, roster, fleet
/// status, sweep, or an "index/list of" identifiers) rather than as work done.
/// These name-drop the very branch a query targets while implementing nothing,
/// so they must sit below the session that actually did the work.
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

/// True when a branch/worktree listing also contains concrete work evidence.
fn shows_substantive_work(lower: &str) -> bool {
    const WORK_VERBS: [&str; 4] = ["implemented", "fixed", "refactored", "committed"];
    if lower.contains("diff --git") || lower.contains("@@ ") {
        return true;
    }
    WORK_VERBS
        .iter()
        .any(|verb| contains_affirmative_verb(lower, verb))
}

/// True when `verb` appears as a standalone word without nearby preceding negation.
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
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        let right_boundary = after
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if left_boundary && right_boundary && !negated_before(before) {
            return true;
        }
        search_from = end;
    }
    false
}

/// True when the last three words before a work verb negate it.
fn negated_before(before: &str) -> bool {
    const NEGATIONS: [&str; 4] = ["nothing", "never", "not", "no"];
    before
        .split_whitespace()
        .rev()
        .take(3)
        .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        .any(|word| NEGATIONS.contains(&word))
}

/// True when a message is mostly a list of filesystem paths rather than prose:
/// at least three path-like tokens making up a majority of the content. A lone
/// path mentioned inside a sentence stays below the threshold.
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

/// A token counts as "path-like" when it embeds a directory separator and is
/// long enough to be a real path, or ends in a common source/transcript
/// extension.
fn token_is_path_like(token: &str) -> bool {
    let token = token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '`' | '(' | ')'));
    if token.len() < 4 {
        return false;
    }
    let has_sep = token.contains('/') && !token.starts_with("//");
    let has_ext = [
        ".jsonl", ".json", ".rs", ".ts", ".tsx", ".js", ".py", ".md", ".toml", ".txt", ".log",
    ]
    .iter()
    .any(|ext| token.ends_with(ext));
    has_sep && (has_ext || token.chars().any(|c| c.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn rerank_fetch_limit_never_panics_when_limit_exceeds_cap() {
        // limit > max_fetch used to invert the clamp range and panic
        // (e.g. `sessions search --limit 500` against the 200 fetch cap);
        // the request must widen the fetch instead.
        assert_eq!(super::rerank_fetch_limit(500, 200), 500);
        assert_eq!(super::rerank_fetch_limit(10, 200), 40);
        assert_eq!(super::rerank_fetch_limit(80, 200), 200);
        assert_eq!(super::rerank_fetch_limit(0, 200), 0);
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

    use super::*;

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

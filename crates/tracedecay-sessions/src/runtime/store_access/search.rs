use crate::runtime::SessionMessageSearchResult;

/// Upper bound on the BM25 over-fetch that precedes the inventory downrank in
/// the session-message search. Keeps the pre-rerank fetch bounded even for
/// large caller limits.
pub const SESSION_MESSAGE_SEARCH_MAX_FETCH: usize = 200;

/// Stable inventory downrank for a BM25 result page: transcript inventory/
/// listing messages and prose branch/worktree rosters are moved below
/// substantive hits while preserving the relative BM25 order within each
/// group. Applied before truncation so a downranked hit still surfaces when it
/// is the only match. Mirrors the lcm/grep re-rank.
pub fn downrank_inventory_messages(results: &mut Vec<SessionMessageSearchResult>) {
    if results.len() < 2 {
        return;
    }
    let mut substantive = Vec::with_capacity(results.len());
    let mut inventory = Vec::new();
    for result in results.drain(..) {
        if tracedecay_lcm::retrieval_content::is_inventory_text(&result.message.text) {
            inventory.push(result);
        } else {
            substantive.push(result);
        }
    }
    substantive.append(&mut inventory);
    *results = substantive;
}

/// Merge independently ranked transcript and canonical-workflow hits by rank
/// tier. Workflow facts lead each tier because they are the authoritative
/// structured representation; borrowing the paired transcript score keeps the
/// merged page comparable when project shards are ranked again by the caller.
pub fn interleave_workflow_search_results(
    transcript_results: Vec<SessionMessageSearchResult>,
    workflow_results: Vec<SessionMessageSearchResult>,
) -> Vec<SessionMessageSearchResult> {
    let capacity = transcript_results
        .len()
        .saturating_add(workflow_results.len());
    let mut transcript_results = transcript_results.into_iter();
    let mut workflow_results = workflow_results.into_iter();
    let mut merged = Vec::with_capacity(capacity);

    loop {
        let transcript_result = transcript_results.next();
        let workflow_result = workflow_results.next();
        if transcript_result.is_none() && workflow_result.is_none() {
            break;
        }
        if let Some(mut workflow_result) = workflow_result {
            if let Some(transcript_result) = transcript_result.as_ref() {
                workflow_result.score = transcript_result.score;
            }
            merged.push(workflow_result);
        }
        if let Some(transcript_result) = transcript_result {
            merged.push(transcript_result);
        }
    }

    merged
}

pub fn session_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter_map(|word| {
            let sanitized: String = word.chars().filter(|c| *c != '"').collect();
            if sanitized.is_empty() {
                None
            } else {
                Some(format!("\"{sanitized}\"*"))
            }
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

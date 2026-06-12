//! Pure analytics helpers for holographic-memory dashboard endpoints.
//!
//! Extracted from `memory_api.rs` so similarity classification, lexical overlap,
//! PCA projection, and dedup planning can be unit-tested without an HTTP harness.

use serde_json::{json, Value};

// Similarity primitives live in `crate::memory::similarity` (shared with the
// write-time diff check in `MemoryStore::add_fact`); re-exported so dashboard
// behavior and call sites stay identical.
pub(crate) use crate::memory::similarity::{
    lexical_overlap, phase_cosine_similarity, similarity_classification,
};

pub(crate) const SIMILARITY_FACT_CAP: i64 = 2000;
pub(crate) const SIMILARITY_DEFAULT_THRESHOLD: f64 = 0.85;
/// Most pairs any single `/similarity` response can return (`limit` is
/// clamped to this), and therefore the deepest prefix of the sorted pair set
/// a request can ever read.
pub(crate) const SIMILARITY_PAIR_CAP: i64 = 2000;
/// Lowest score *scored* per computation. All finite phase-cosine pairs feed
/// the score distribution; only the serveable prefix is retained afterwards
/// (see [`build_similarity_computation`]).
pub(crate) const SIMILARITY_PAIR_FLOOR: f64 = -1.0;
pub(crate) const SIMILARITY_SCORE_MIN: f64 = -1.0;
pub(crate) const SIMILARITY_SCORE_MAX: f64 = 1.0;
const SIMILARITY_DISTRIBUTION_BINS: usize = 20;

/// Top-2 principal components of the centered feature matrix, computed via
/// power iteration on the (n × n) Gram matrix. Callers cap n at
/// `PROJECTION_POINT_CAP` (2000), so the Gram build is O(n²·d) — far too
/// expensive for the async runtime; run this on the blocking pool and cache
/// the result (see `memory_api::projection`).
pub(crate) fn pca_scores(features: &[Vec<f64>]) -> Option<Vec<[f64; 2]>> {
    let n = features.len();
    let d = features.first()?.len();
    if n < 2 || d == 0 {
        return None;
    }
    let mut mean = vec![0.0; d];
    for row in features {
        for (m, v) in mean.iter_mut().zip(row) {
            *m += v;
        }
    }
    for m in &mut mean {
        *m /= n as f64;
    }
    let centered: Vec<Vec<f64>> = features
        .iter()
        .map(|row| row.iter().zip(&mean).map(|(v, m)| v - m).collect())
        .collect();

    // Gram matrix G = Fc Fc^T.
    let mut gram = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in i..n {
            let dot: f64 = centered[i]
                .iter()
                .zip(&centered[j])
                .map(|(a, b)| a * b)
                .sum();
            gram[i][j] = dot;
            gram[j][i] = dot;
        }
    }

    let mut scores = vec![[0.0_f64; 2]; n];
    let mut deflated = gram;
    for component in 0..2 {
        // Power iteration with a deterministic start vector.
        let mut v: Vec<f64> = (0..n).map(|i| 1.0 + (i as f64 % 7.0) / 7.0).collect();
        let mut eigenvalue = 0.0;
        for _ in 0..200 {
            let mut next = vec![0.0; n];
            for (i, next_i) in next.iter_mut().enumerate() {
                *next_i = deflated[i].iter().zip(&v).map(|(g, x)| g * x).sum();
            }
            let norm: f64 = next.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm < 1e-12 {
                eigenvalue = 0.0;
                break;
            }
            for x in &mut next {
                *x /= norm;
            }
            eigenvalue = norm;
            v = next;
        }
        if eigenvalue <= 1e-12 {
            break;
        }
        let scale = eigenvalue.sqrt();
        for (score, value) in scores.iter_mut().zip(&v) {
            score[component] = value * scale;
        }
        // Deflate: G ← G − λ v vᵀ.
        for i in 0..n {
            for j in 0..n {
                deflated[i][j] -= eigenvalue * v[i] * v[j];
            }
        }
    }

    let max_abs = scores
        .iter()
        .flat_map(|s| s.iter())
        .fold(0.0_f64, |acc, v| acc.max(v.abs()));
    if max_abs > 0.0 {
        for s in &mut scores {
            s[0] /= max_abs;
            s[1] /= max_abs;
        }
    }
    Some(scores)
}

/// Score all pairs above `threshold` from decoded vectored facts.
pub(crate) fn score_similar_pairs(
    decoded: &[(Value, Vec<f64>)],
    threshold: f64,
) -> Vec<(f64, usize, usize)> {
    let mut scored = Vec::new();
    for i in 0..decoded.len() {
        for j in (i + 1)..decoded.len() {
            let sim = phase_cosine_similarity(&decoded[i].1, &decoded[j].1);
            if sim >= threshold {
                scored.push((sim, i, j));
            }
        }
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
}

/// Rounds a bin edge to a sane display precision so single-multiply edge
/// computation never leaks float noise (e.g. `2.77e-17`) into the payload.
fn round_bin_edge(edge: f64) -> f64 {
    (edge * 1e9).round() / 1e9
}

/// Fixed-width histogram over the observed `[min_score, max_score]` range of
/// the computed pairs (adaptive, not a fixed `[-1, 1]` window — real HRR data
/// clusters tightly and a fixed window collapses into one bin). A degenerate
/// range (all scores equal) yields a single bin.
///
/// Two passes over the slice, no intermediate allocation: at n = 2000 facts
/// the input is ~2M pairs, and a per-request copy would be ~16 MB.
pub(crate) fn score_distribution(scored: &[(f64, usize, usize)]) -> Value {
    let mut min_seen = f64::INFINITY;
    let mut max_seen = f64::NEG_INFINITY;
    let mut sum = 0.0_f64;
    let mut total_pairs = 0_i64;
    for (score, _, _) in scored {
        if !score.is_finite() {
            continue;
        }
        total_pairs += 1;
        min_seen = min_seen.min(*score);
        max_seen = max_seen.max(*score);
        sum += *score;
    }

    if total_pairs == 0 {
        return json!({
            "min": Value::Null,
            "max": Value::Null,
            "bin_count": 0,
            "total_pairs": 0,
            "min_score": Value::Null,
            "max_score": Value::Null,
            "average_score": Value::Null,
            "bins": [],
        });
    }

    let range = max_seen - min_seen;
    if range <= 0.0 {
        return json!({
            "min": min_seen,
            "max": max_seen,
            "bin_count": 1,
            "total_pairs": total_pairs,
            "min_score": min_seen,
            "max_score": max_seen,
            "average_score": sum / total_pairs as f64,
            "bins": [{ "start": min_seen, "end": max_seen, "count": total_pairs }],
        });
    }

    let mut counts = vec![0_i64; SIMILARITY_DISTRIBUTION_BINS];
    for (score, _, _) in scored {
        if !score.is_finite() {
            continue;
        }
        let mut idx =
            ((score - min_seen) / range * SIMILARITY_DISTRIBUTION_BINS as f64).floor() as usize;
        if idx >= SIMILARITY_DISTRIBUTION_BINS {
            idx = SIMILARITY_DISTRIBUTION_BINS - 1;
        }
        counts[idx] += 1;
    }

    // Edges from one multiply per index (no accumulation drift); exact
    // observed bounds at both ends, rounded interior edges in between.
    let width = range / SIMILARITY_DISTRIBUTION_BINS as f64;
    let edge = |idx: usize| -> f64 {
        if idx == 0 {
            min_seen
        } else if idx == SIMILARITY_DISTRIBUTION_BINS {
            max_seen
        } else {
            round_bin_edge(min_seen + idx as f64 * width)
        }
    };
    let bins: Vec<Value> = counts
        .into_iter()
        .enumerate()
        .map(|(idx, count)| {
            json!({
                "start": edge(idx),
                "end": edge(idx + 1),
                "count": count,
            })
        })
        .collect();

    json!({
        "min": min_seen,
        "max": max_seen,
        "bin_count": SIMILARITY_DISTRIBUTION_BINS,
        "total_pairs": total_pairs,
        "min_score": min_seen,
        "max_score": max_seen,
        "average_score": sum / total_pairs as f64,
        "bins": bins,
    })
}

/// One retained similarity pair with its lexical-overlap analysis, computed
/// once per [`SimilarityComputation`] instead of per request (the overlap
/// tokenization used to re-run for up to 2000 pairs on every `/similarity`
/// call and again for every planner pair on `/curate`).
#[derive(Debug)]
pub(crate) struct ScoredPair {
    pub(crate) similarity: f64,
    /// Indices into [`SimilarityComputation::facts`].
    pub(crate) a: usize,
    pub(crate) b: usize,
    /// Lexical-overlap payload keys merged into the pair JSON
    /// (`token_overlap`, `overlap_coefficient`, `shared_tokens`, …).
    pub(crate) overlap: Value,
    pub(crate) classification: &'static str,
}

impl ScoredPair {
    /// Builds the pair from a raw score by running the lexical-overlap
    /// analysis on the two fact contents.
    pub(crate) fn analyze(facts: &[Value], similarity: f64, a: usize, b: usize) -> Self {
        let a_content = facts[a]
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("");
        let b_content = facts[b]
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("");
        let (overlap, token_overlap, overlap_coefficient) = lexical_overlap(a_content, b_content);
        Self {
            similarity,
            a,
            b,
            overlap,
            classification: similarity_classification(
                similarity,
                token_overlap,
                overlap_coefficient,
            ),
        }
    }
}

/// A cached O(n²·d) pairwise-similarity computation over the vectored facts.
///
/// `key` fingerprints the underlying fact-vector state. Vectors are not
/// retained — only the fact metadata needed to render pairs and plans.
#[derive(Debug)]
pub(crate) struct SimilarityComputation {
    /// Fingerprint of the vectored fact rows at compute time.
    pub(crate) key: (i64, i64, i64, u64),
    pub(crate) dim: usize,
    /// Fact metadata (`fact_id`, content, category, `trust_score`, `retrieval_count`).
    pub(crate) facts: Vec<Value>,
    /// Retained pairs, sorted by similarity descending: every pair at or
    /// above [`SIMILARITY_DEFAULT_THRESHOLD`] (the dedup planner walks them
    /// all) plus the top [`SIMILARITY_PAIR_CAP`] overall (the deepest prefix
    /// any `/similarity` request can return). Pairs below that horizon only
    /// contribute to `total_pairs` and `distribution`, so the cache holds
    /// O(cap) pairs instead of all O(n²) (~48 MB at n = 2000).
    pub(crate) pairs: Vec<ScoredPair>,
    /// Supersession hygiene candidates from every scored pair at or above the
    /// supersession floor where either side carries a negation/state-change cue.
    pub(crate) supersession_pairs: Vec<ScoredPair>,
    /// Count of all finite pairs scored, retained or not.
    pub(crate) total_pairs: i64,
    /// [`score_distribution`] over all scored pairs, precomputed so requests
    /// never re-bin the full pair set.
    pub(crate) distribution: Value,
}

/// Finalizes a similarity computation from the full scored pair set:
/// distribution + total over everything, lexical overlap only for the
/// retained serveable prefix. Runs on the blocking pool with the scoring.
pub(crate) fn build_similarity_computation(
    key: (i64, i64, i64, u64),
    dim: usize,
    facts: Vec<Value>,
    scored: Vec<(f64, usize, usize)>,
) -> SimilarityComputation {
    let distribution = score_distribution(&scored);
    let total_pairs = scored.iter().filter(|(s, _, _)| s.is_finite()).count() as i64;
    let mut retain = scored.len().min(SIMILARITY_PAIR_CAP as usize);
    while retain < scored.len() && scored[retain].0 >= SIMILARITY_DEFAULT_THRESHOLD {
        retain += 1;
    }
    let mut pairs = Vec::new();
    let mut supersession_pairs = Vec::new();
    for (idx, (similarity, a, b)) in scored.into_iter().enumerate() {
        if idx < retain {
            pairs.push(ScoredPair::analyze(&facts, similarity, a, b));
        }
        if similarity < SUPERSESSION_SIMILARITY_THRESHOLD {
            if idx >= retain {
                break;
            }
            continue;
        }
        if pair_has_supersession_cue(&facts, a, b) {
            supersession_pairs.push(ScoredPair::analyze(&facts, similarity, a, b));
        }
    }
    SimilarityComputation {
        key,
        dim,
        facts,
        pairs,
        supersession_pairs,
        total_pairs,
        distribution,
    }
}

/// Above this similarity, a pair is near-identical enough that the
/// access-count delete-reluctance rule (below) no longer blocks an automatic
/// dedup proposal.
pub(crate) const ACCESS_RELUCTANCE_EXTREME_SIMILARITY: f64 = 0.98;

/// Similarity floor for "possible supersession" hygiene entries: a
/// negation/state-change cue only signals supersession when the two facts are
/// substantially similar (mirrors the write-time conflict threshold).
pub(crate) const SUPERSESSION_SIMILARITY_THRESHOLD: f64 = 0.7;

fn pair_has_supersession_cue(facts: &[Value], a: usize, b: usize) -> bool {
    let a_content = facts[a]
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let b_content = facts[b]
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("");
    crate::memory::diff::contains_negation_cue(a_content)
        || crate::memory::diff::contains_negation_cue(b_content)
}

/// Propose hard-delete actions for `likely_duplicate` pairs from pre-scored facts.
///
/// Each fact is consumed at most once per plan: once a fact is chosen as a
/// loser it can neither lose again nor act as a winner (`duplicate_of`
/// reference) for a later pair, so an applied plan never deletes a fact that
/// another action in the same plan points at. Residual duplicate relations
/// surface again on the next preview.
///
/// Access-count delete-reluctance: the planner never auto-proposes deleting
/// the HIGHER-access fact of a pair as the loser unless the similarity is
/// extreme (≥ [`ACCESS_RELUCTANCE_EXTREME_SIMILARITY`]). A fact that recall
/// searches keep returning is demonstrably in use; when the trust-based loser
/// choice would delete it, the pair is left out of the automatic plan for
/// LLM/human review instead. (Access frequency is deliberately NOT part of
/// retrieval ranking — see `combined_score` in `memory::retrieval` — it is a
/// curation-only signal.)
pub(crate) fn propose_dedup_actions(facts: &[Value], pairs: &[ScoredPair]) -> Vec<Value> {
    let mut consumed_losers: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut actions: Vec<Value> = Vec::new();

    for pair in pairs {
        if pair.classification != "likely_duplicate" {
            continue;
        }
        let sim = pair.similarity;
        let a = &facts[pair.a];
        let b = &facts[pair.b];
        let a_content = a.get("content").and_then(Value::as_str).unwrap_or("");
        let b_content = b.get("content").and_then(Value::as_str).unwrap_or("");

        let a_id = a.get("fact_id").and_then(Value::as_i64).unwrap_or(0);
        let b_id = b.get("fact_id").and_then(Value::as_i64).unwrap_or(0);

        let a_trust = a.get("trust_score").and_then(Value::as_f64).unwrap_or(0.0);
        let b_trust = b.get("trust_score").and_then(Value::as_f64).unwrap_or(0.0);
        // Lower trust loses; on a trust tie, the higher (newer) id loses.
        let a_loses = match a_trust.total_cmp(&b_trust) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => a_id > b_id,
            std::cmp::Ordering::Greater => false,
        };
        let (loser, loser_id, loser_content, winner, winner_id) = if a_loses {
            (a, a_id, a_content, b, b_id)
        } else {
            (b, b_id, b_content, a, a_id)
        };

        // Delete-reluctance (documented above): a higher-access loser is
        // never auto-deleted below the extreme-similarity bar.
        let loser_access = loser
            .get("access_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let winner_access = winner
            .get("access_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if loser_access > winner_access && sim < ACCESS_RELUCTANCE_EXTREME_SIMILARITY {
            continue;
        }

        // Skip if either side of the pair is already being deleted by this plan.
        if consumed_losers.contains(&loser_id) || consumed_losers.contains(&winner_id) {
            continue;
        }
        consumed_losers.insert(loser_id);

        let similarity_rounded = (sim * 10_000.0).round() / 10_000.0;
        actions.push(json!({
            "op": "delete",
            "fact_id": loser_id,
            "duplicate_of": winner_id,
            "reason": format!(
                "Likely duplicate of #{winner_id} (similarity {similarity_rounded:.4})"
            ),
            "content": loser_content.chars().take(200).collect::<String>(),
            "similarity": similarity_rounded,
            "access_count": loser_access,
            "tier": "duplicate",
        }));
    }

    actions
}

fn truncated_content(fact: &Value) -> String {
    fact.get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

fn fact_i64(fact: &Value, key: &str) -> i64 {
    fact.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Deterministic hygiene PROPOSALS for the curation dry-run plan: secret-like
/// facts, transient run-output facts, and negation-cue "possible
/// supersession" pairs. Pure rule-based scanning — no model is invoked.
///
/// Entries are shaped like `/curate/apply` delete ops so an external reviewer
/// (human, or the Hermes LLM curation layer) can confirm and apply them
/// through the existing ops contract. They are NEVER auto-applied: the
/// `/curate` apply path only executes the dedup `actions` list.
pub(crate) fn propose_hygiene_actions(
    scan_facts: &[Value],
    pair_facts: &[Value],
    supersession_pairs: &[ScoredPair],
    dedup_loser_ids: &std::collections::HashSet<i64>,
) -> Value {
    let mut secret_like: Vec<Value> = Vec::new();
    let mut transient: Vec<Value> = Vec::new();
    let mut flagged: std::collections::HashSet<i64> = dedup_loser_ids.clone();

    for fact in scan_facts {
        let fact_id = fact_i64(fact, "fact_id");
        if flagged.contains(&fact_id) {
            continue;
        }
        let content = fact.get("content").and_then(Value::as_str).unwrap_or("");
        if let Some(reason) = crate::memory::hygiene::detect_secret_like(content) {
            flagged.insert(fact_id);
            secret_like.push(json!({
                "op": "delete",
                "fact_id": fact_id,
                "reason": format!("Secret-like content ({reason}); memory must not retain credentials"),
                "content": truncated_content(fact),
                "tier": "secret_like",
            }));
        } else if let Some(reason) = crate::memory::hygiene::detect_transient(content) {
            flagged.insert(fact_id);
            transient.push(json!({
                "op": "delete",
                "fact_id": fact_id,
                "reason": format!("Transient run output ({reason}); likely ephemeral, not durable knowledge"),
                "content": truncated_content(fact),
                "tier": "transient",
            }));
        }
    }

    // Possible supersession: substantially similar pairs where either side
    // carries a negation/state-change cue. Proposes deleting the OLDER fact;
    // an LLM or human confirms which one is current before applying.
    let mut supersession: Vec<Value> = Vec::new();
    let mut proposed: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for pair in supersession_pairs {
        if pair.similarity < SUPERSESSION_SIMILARITY_THRESHOLD {
            continue;
        }
        let a = &pair_facts[pair.a];
        let b = &pair_facts[pair.b];
        let a_content = a.get("content").and_then(Value::as_str).unwrap_or("");
        let b_content = b.get("content").and_then(Value::as_str).unwrap_or("");
        if !crate::memory::diff::contains_negation_cue(a_content)
            && !crate::memory::diff::contains_negation_cue(b_content)
        {
            continue;
        }
        // Older = smaller created_at; on a tie, the smaller fact_id.
        let (older, newer) = match fact_i64(a, "created_at").cmp(&fact_i64(b, "created_at")) {
            std::cmp::Ordering::Less => (a, b),
            std::cmp::Ordering::Greater => (b, a),
            std::cmp::Ordering::Equal => {
                if fact_i64(a, "fact_id") <= fact_i64(b, "fact_id") {
                    (a, b)
                } else {
                    (b, a)
                }
            }
        };
        let older_id = fact_i64(older, "fact_id");
        let newer_id = fact_i64(newer, "fact_id");
        if flagged.contains(&older_id) || !proposed.insert(older_id) {
            continue;
        }
        let similarity_rounded = (pair.similarity * 10_000.0).round() / 10_000.0;
        supersession.push(json!({
            "op": "delete",
            "fact_id": older_id,
            "superseded_by": newer_id,
            "similarity": similarity_rounded,
            "reason": format!(
                "Possible supersession: negation/state-change cue with similarity {similarity_rounded:.4} to newer fact #{newer_id}; confirm which fact is current before applying"
            ),
            "content": truncated_content(older),
            "access_count": fact_i64(older, "access_count"),
            "tier": "supersession",
        }));
    }

    json!({
        "secret_like": secret_like,
        "transient": transient,
        "supersession": supersession,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pca_scores_two_points() {
        let features = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let Some(scores) = pca_scores(&features) else {
            panic!("expected PCA scores");
        };
        assert_eq!(scores.len(), 2);
        assert!(scores[0][0].abs() > 0.0 || scores[0][1].abs() > 0.0);
    }

    #[test]
    fn score_distribution_covers_all_scores() {
        let scored = vec![(0.75, 0, 1), (0.0, 0, 2), (-0.25, 1, 2)];
        let distribution = score_distribution(&scored);
        assert_eq!(distribution["total_pairs"], 3);
        let bins = distribution["bins"]
            .as_array()
            .unwrap_or_else(|| panic!("expected distribution bins"));
        let binned_pairs: i64 = bins
            .iter()
            .map(|bin| bin["count"].as_i64().unwrap_or(0))
            .sum();
        assert_eq!(binned_pairs, 3);
        assert_eq!(distribution["min_score"], -0.25);
        assert_eq!(distribution["max_score"], 0.75);
    }

    #[test]
    fn score_distribution_adapts_bins_to_observed_range() {
        let scored = vec![(0.75, 0, 1), (0.0, 0, 2), (-0.25, 1, 2)];
        let distribution = score_distribution(&scored);
        assert_eq!(distribution["min"], -0.25);
        assert_eq!(distribution["max"], 0.75);
        assert_eq!(distribution["bin_count"], 20);
        let bins = distribution["bins"]
            .as_array()
            .unwrap_or_else(|| panic!("expected distribution bins"));
        assert_eq!(bins.len(), 20);
        assert_eq!(bins[0]["start"], -0.25);
        assert_eq!(bins[19]["end"], 0.75);
        assert_eq!(bins[0]["count"], 1, "min score lands in the first bin");
        assert_eq!(bins[19]["count"], 1, "max score lands in the last bin");
        // Bin edges must be clean values, not float-accumulation noise.
        for bin in bins {
            for key in ["start", "end"] {
                let edge = bin[key]
                    .as_f64()
                    .unwrap_or_else(|| panic!("expected numeric bin edge"));
                let rounded = (edge * 1e9).round() / 1e9;
                assert!(
                    (edge - rounded).abs() < 1e-12,
                    "bin edge {edge} should be rounded to a sane precision"
                );
            }
        }
    }

    #[test]
    fn score_distribution_degenerate_range_returns_single_bin() {
        let scored = vec![(0.5, 0, 1), (0.5, 0, 2), (0.5, 1, 2)];
        let distribution = score_distribution(&scored);
        assert_eq!(distribution["bin_count"], 1);
        assert_eq!(distribution["total_pairs"], 3);
        let bins = distribution["bins"]
            .as_array()
            .unwrap_or_else(|| panic!("expected distribution bins"));
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0]["start"], 0.5);
        assert_eq!(bins[0]["end"], 0.5);
        assert_eq!(bins[0]["count"], 3);
    }

    #[test]
    fn propose_dedup_actions_deletes_lower_trust() {
        let facts = vec![
            json!({"fact_id": 1, "content": "same text here", "trust_score": 0.9}),
            json!({"fact_id": 2, "content": "same text here", "trust_score": 0.5}),
        ];
        let pairs = vec![ScoredPair::analyze(&facts, 0.99, 0, 1)];
        let actions = propose_dedup_actions(&facts, &pairs);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["op"], "delete");
        assert_eq!(actions[0]["fact_id"], 2);
        assert_eq!(actions[0]["duplicate_of"], 1);
    }

    #[test]
    fn propose_dedup_actions_spares_higher_access_losers_below_extreme_similarity() {
        let facts = vec![
            json!({"fact_id": 1, "content": "same text here", "trust_score": 0.9, "access_count": 0}),
            json!({"fact_id": 2, "content": "same text here", "trust_score": 0.5, "access_count": 7}),
        ];
        // The trust-based loser (#2) is the higher-access fact: below the
        // extreme-similarity bar the pair is left for review, not auto-planned.
        let reluctant = propose_dedup_actions(&facts, &[ScoredPair::analyze(&facts, 0.96, 0, 1)]);
        assert!(
            reluctant.is_empty(),
            "higher-access loser must not be auto-proposed below extreme similarity"
        );
        // At extreme similarity the dedup proposal goes through.
        let extreme = propose_dedup_actions(&facts, &[ScoredPair::analyze(&facts, 0.99, 0, 1)]);
        assert_eq!(extreme.len(), 1);
        assert_eq!(extreme[0]["fact_id"], 2);
        assert_eq!(extreme[0]["access_count"], 7);
    }

    #[test]
    fn propose_hygiene_actions_flags_secret_transient_and_supersession() {
        let facts = vec![
            json!({"fact_id": 1, "content": "api_key=Zx9mQ4tR7wLp2NvK8sBd1FgH", "trust_score": 0.5, "created_at": 10}),
            json!({"fact_id": 2, "content": "dev server listening on 127.0.0.1:8081", "trust_score": 0.5, "created_at": 11}),
            json!({"fact_id": 3, "content": "We use Redis for caching sessions", "trust_score": 0.8, "created_at": 5, "access_count": 4}),
            json!({"fact_id": 4, "content": "We no longer use Redis for caching sessions", "trust_score": 0.8, "created_at": 9}),
        ];
        let pairs = vec![ScoredPair::analyze(&facts, 0.85, 2, 3)];
        let hygiene =
            propose_hygiene_actions(&facts, &facts, &pairs, &std::collections::HashSet::new());

        let secret = hygiene["secret_like"]
            .as_array()
            .unwrap_or_else(|| panic!("expected secret_like array"));
        assert_eq!(secret.len(), 1);
        assert_eq!(secret[0]["fact_id"], 1);
        assert_eq!(secret[0]["op"], "delete");
        assert_eq!(secret[0]["tier"], "secret_like");

        let transient = hygiene["transient"]
            .as_array()
            .unwrap_or_else(|| panic!("expected transient array"));
        assert_eq!(transient.len(), 1);
        assert_eq!(transient[0]["fact_id"], 2);
        assert_eq!(transient[0]["tier"], "transient");

        // Supersession proposes deleting the OLDER fact of the cue pair.
        let supersession = hygiene["supersession"]
            .as_array()
            .unwrap_or_else(|| panic!("expected supersession array"));
        assert_eq!(supersession.len(), 1);
        assert_eq!(supersession[0]["fact_id"], 3);
        assert_eq!(supersession[0]["superseded_by"], 4);
        assert_eq!(supersession[0]["access_count"], 4);
        assert_eq!(supersession[0]["tier"], "supersession");
    }

    #[test]
    fn propose_hygiene_actions_respects_exclusions_and_thresholds() {
        let facts = vec![
            json!({"fact_id": 1, "content": "scratch file at /tmp/run-output.json", "trust_score": 0.5, "created_at": 1}),
            json!({"fact_id": 2, "content": "We use Redis for caching sessions", "trust_score": 0.8, "created_at": 5}),
            json!({"fact_id": 3, "content": "We no longer use Redis for caching sessions", "trust_score": 0.8, "created_at": 9}),
        ];
        // Fact already consumed by the dedup plan is never re-proposed.
        let consumed: std::collections::HashSet<i64> = [1].into_iter().collect();
        // Below the supersession similarity floor the cue pair is ignored.
        let weak_pair = vec![ScoredPair::analyze(&facts, 0.5, 1, 2)];
        let hygiene = propose_hygiene_actions(&facts, &facts, &weak_pair, &consumed);
        assert!(hygiene["transient"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty));
        assert!(hygiene["supersession"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty));
    }

    #[test]
    fn propose_dedup_actions_never_deletes_a_referenced_winner() {
        // Pair (0,1): 1 loses to 0. Pair (1,2): if processed naively, 2 would
        // lose to 1 — but 1 is already being deleted, so the pair is skipped
        // and no action references a deleted fact.
        let facts = vec![
            json!({"fact_id": 10, "content": "duplicate fact body", "trust_score": 0.9}),
            json!({"fact_id": 11, "content": "duplicate fact body", "trust_score": 0.7}),
            json!({"fact_id": 12, "content": "duplicate fact body", "trust_score": 0.5}),
        ];
        let pairs = vec![
            ScoredPair::analyze(&facts, 0.99, 0, 1),
            ScoredPair::analyze(&facts, 0.98, 1, 2),
        ];
        let actions = propose_dedup_actions(&facts, &pairs);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["fact_id"], 11);
        assert_eq!(actions[0]["duplicate_of"], 10);
    }

    #[test]
    fn build_similarity_computation_retains_serveable_prefix_only() {
        let facts: Vec<Value> = (0..3)
            .map(|id| json!({"fact_id": id, "content": format!("fact body {id}"), "trust_score": 0.5}))
            .collect();
        // Descending scores: all three are scored, all three fit the cap.
        let scored = vec![(0.99, 0, 1), (0.5, 0, 2), (-0.2, 1, 2)];
        let computation = build_similarity_computation((3, 0, 3, 7), 4, facts, scored);
        assert_eq!(computation.total_pairs, 3);
        assert_eq!(computation.pairs.len(), 3);
        assert_eq!(computation.distribution["total_pairs"], 3);
        // The distribution covers the full scored range even when pairs
        // below the retention horizon would be dropped.
        assert_eq!(computation.distribution["min"], -0.2);
        assert_eq!(computation.distribution["max"], 0.99);
        assert!(computation.pairs[0].similarity >= computation.pairs[1].similarity);
    }

    #[test]
    fn build_similarity_computation_keeps_supersession_pairs_below_pair_cap() {
        let facts = vec![
            json!({"fact_id": 1, "content": "We use Redis for caching sessions", "trust_score": 0.8, "created_at": 1}),
            json!({"fact_id": 2, "content": "We no longer use Redis for caching sessions", "trust_score": 0.8, "created_at": 2}),
            json!({"fact_id": 3, "content": "unrelated retained pair left", "trust_score": 0.8}),
            json!({"fact_id": 4, "content": "unrelated retained pair right", "trust_score": 0.8}),
        ];
        let mut scored = vec![(0.95, 2, 3); SIMILARITY_PAIR_CAP as usize];
        scored.push((SUPERSESSION_SIMILARITY_THRESHOLD, 0, 1));

        let computation = build_similarity_computation((4, 0, 10, 7), 4, facts, scored);

        assert_eq!(computation.pairs.len(), SIMILARITY_PAIR_CAP as usize);
        let hygiene = propose_hygiene_actions(
            &computation.facts,
            &computation.facts,
            &computation.supersession_pairs,
            &std::collections::HashSet::new(),
        );
        assert_eq!(
            hygiene["supersession"].as_array().map(Vec::len),
            Some(1),
            "supersession candidates at the floor must not be lost below the response pair cap"
        );
    }
}

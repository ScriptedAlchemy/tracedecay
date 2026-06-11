//! Pure analytics helpers for holographic-memory dashboard endpoints.
//!
//! Extracted from `memory_api.rs` so similarity classification, lexical overlap,
//! PCA projection, and dedup planning can be unit-tested without an HTTP harness.

use serde_json::{json, Value};

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

const TOKEN_STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "in", "is",
    "it", "of", "on", "or", "that", "the", "this", "to", "was", "were", "with",
];

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

pub(crate) fn content_tokens(content: &str) -> std::collections::BTreeSet<String> {
    let mut tokens = std::collections::BTreeSet::new();
    let mut current = String::new();
    for ch in content.chars() {
        let lower = ch.to_ascii_lowercase();
        let is_token_char = if current.is_empty() {
            lower.is_ascii_alphanumeric()
        } else {
            lower.is_ascii_alphanumeric() || lower == '_' || lower == '\'' || lower == '-'
        };
        if is_token_char {
            current.push(lower);
        } else if !current.is_empty() {
            tokens.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.insert(current);
    }
    tokens
        .into_iter()
        .map(|t| {
            t.trim_matches(|c| c == '_' || c == '\'' || c == '-')
                .to_string()
        })
        .filter(|t| t.len() > 1 && !TOKEN_STOPWORDS.contains(&t.as_str()))
        .collect()
}

pub(crate) fn lexical_overlap(a: &str, b: &str) -> (Value, f64, f64) {
    let a_tokens = content_tokens(a);
    let b_tokens = content_tokens(b);
    let shared: Vec<&String> = a_tokens.intersection(&b_tokens).collect();
    let union = a_tokens.union(&b_tokens).count();
    let min_size = a_tokens.len().min(b_tokens.len());
    let token_overlap = if union > 0 {
        (shared.len() as f64 / union as f64 * 10_000.0).round() / 10_000.0
    } else {
        0.0
    };
    let overlap_coefficient = if min_size > 0 {
        (shared.len() as f64 / min_size as f64 * 10_000.0).round() / 10_000.0
    } else {
        0.0
    };
    let payload = json!({
        "token_overlap": token_overlap,
        "overlap_coefficient": overlap_coefficient,
        "shared_token_count": shared.len(),
        "a_token_count": a_tokens.len(),
        "b_token_count": b_tokens.len(),
        "shared_tokens": shared.iter().take(10).collect::<Vec<_>>(),
    });
    (payload, token_overlap, overlap_coefficient)
}

pub(crate) fn similarity_classification(
    similarity: f64,
    token_overlap: f64,
    overlap_coefficient: f64,
) -> &'static str {
    if similarity >= 0.95 && (overlap_coefficient >= 0.65 || token_overlap >= 0.45) {
        return "likely_duplicate";
    }
    if similarity >= 0.90 && (overlap_coefficient >= 0.35 || token_overlap >= 0.20) {
        return "merge_candidate";
    }
    if similarity >= 0.97 && overlap_coefficient >= 0.25 {
        return "merge_candidate";
    }
    "related"
}

/// Phase-cosine similarity between two equal-length phase vectors.
pub(crate) fn phase_cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| (x - y).cos()).sum::<f64>() / a.len() as f64
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
    let pairs = scored
        .into_iter()
        .take(retain)
        .map(|(similarity, a, b)| ScoredPair::analyze(&facts, similarity, a, b))
        .collect();
    SimilarityComputation {
        key,
        dim,
        facts,
        pairs,
        total_pairs,
        distribution,
    }
}

/// Propose hard-delete actions for `likely_duplicate` pairs from pre-scored facts.
///
/// Each fact is consumed at most once per plan: once a fact is chosen as a
/// loser it can neither lose again nor act as a winner (`duplicate_of`
/// reference) for a later pair, so an applied plan never deletes a fact that
/// another action in the same plan points at. Residual duplicate relations
/// surface again on the next preview.
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
        let (loser_id, loser_content, winner_id) = if a_loses {
            (a_id, a_content, b_id)
        } else {
            (b_id, b_content, a_id)
        };

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
            "tier": "duplicate",
        }));
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_tokens_strips_stopwords() {
        let tokens = content_tokens("The quick brown fox and the lazy dog");
        assert!(tokens.contains("quick"));
        assert!(tokens.contains("brown"));
        assert!(!tokens.contains("the"));
        assert!(!tokens.contains("and"));
    }

    #[test]
    fn lexical_overlap_identical_text() {
        let text = "hello world example";
        let (payload, token_overlap, overlap_coefficient) = lexical_overlap(text, text);
        assert!((token_overlap - 1.0).abs() < f64::EPSILON);
        assert!((overlap_coefficient - 1.0).abs() < f64::EPSILON);
        assert_eq!(payload["shared_token_count"], 3);
    }

    #[test]
    fn similarity_classification_duplicate() {
        assert_eq!(
            similarity_classification(0.96, 0.5, 0.7),
            "likely_duplicate"
        );
        assert_eq!(
            similarity_classification(0.92, 0.25, 0.4),
            "merge_candidate"
        );
        assert_eq!(similarity_classification(0.80, 0.1, 0.1), "related");
    }

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
    fn phase_cosine_identical_vectors() {
        let v = vec![0.1, 0.2, 0.3];
        let sim = phase_cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-10);
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
}

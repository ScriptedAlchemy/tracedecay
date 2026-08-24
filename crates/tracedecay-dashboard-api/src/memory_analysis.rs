//! Pure analytics helpers for holographic-memory dashboard endpoints.
//!
//! Extracted from `memory_api.rs` so similarity classification, lexical overlap,
//! and PCA projection can be unit-tested without an HTTP harness.

use serde_json::{Value, json};
use tracedecay_runtime_core::memory::encoding::{HolographicEncoder, HolographicEncodingError};
use tracedecay_store::FactReadControl;

// Similarity primitives live in `tracedecay_runtime_core::memory::similarity`;
// re-export them so every dashboard similarity view uses that one classifier.
pub use tracedecay_runtime_core::memory::similarity::{lexical_overlap, similarity_classification};

pub const SIMILARITY_FACT_CAP: i64 = 2000;
pub const SIMILARITY_DEFAULT_THRESHOLD: f64 = 0.85;
/// Most pairs any single `/similarity` response can return (`limit` is
/// clamped to this), and therefore the deepest prefix of the sorted pair set
/// a request can ever read.
pub const SIMILARITY_PAIR_CAP: i64 = 2000;
/// Lowest score *scored* per computation. All finite holographic pairs feed
/// the score distribution; only the serveable prefix is retained afterwards
/// (see [`build_similarity_computation`]).
pub const SIMILARITY_PAIR_FLOOR: f64 = -1.0;
pub const SIMILARITY_SCORE_MIN: f64 = -1.0;
pub const SIMILARITY_SCORE_MAX: f64 = 1.0;
const SIMILARITY_DISTRIBUTION_BINS: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryAnalysisError {
    Interrupted,
    HolographicEncoding(HolographicEncodingError),
    InvalidFactIndex { index: usize },
    MissingFactContent { index: usize },
}

impl std::fmt::Display for MemoryAnalysisError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted => formatter.write_str("memory analysis interrupted"),
            Self::HolographicEncoding(error) => error.fmt(formatter),
            Self::InvalidFactIndex { index } => {
                write!(
                    formatter,
                    "memory analysis fact index {index} is out of bounds"
                )
            }
            Self::MissingFactContent { index } => write!(
                formatter,
                "memory analysis fact at index {index} omitted authoritative content"
            ),
        }
    }
}

impl std::error::Error for MemoryAnalysisError {}

impl From<HolographicEncodingError> for MemoryAnalysisError {
    fn from(error: HolographicEncodingError) -> Self {
        Self::HolographicEncoding(error)
    }
}

/// Top-2 principal components of the centered feature matrix, computed via
/// power iteration on the (n × n) Gram matrix. Callers cap n at
/// `PROJECTION_POINT_CAP` (2000), so the Gram build is O(n²·d) — far too
/// expensive for the async runtime; run this on the blocking pool and cache
/// the result (see `memory_api::projection`).
pub fn pca_scores(
    features: &[Vec<f64>],
    read_control: &FactReadControl,
) -> Result<Option<Vec<[f64; 2]>>, MemoryAnalysisError> {
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
    }
    let n = features.len();
    let Some(d) = features.first().map(Vec::len) else {
        return Ok(None);
    };
    if n < 2 || d == 0 {
        return Ok(None);
    }
    let mut mean = vec![0.0; d];
    for row in features {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        for (m, v) in mean.iter_mut().zip(row) {
            *m += v;
        }
    }
    for m in &mut mean {
        *m /= n as f64;
    }
    let mut centered: Vec<Vec<f64>> = Vec::with_capacity(features.len());
    for row in features {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        centered.push(row.iter().zip(&mean).map(|(v, m)| v - m).collect());
    }

    // Gram matrix G = Fc Fc^T.
    let mut gram = vec![vec![0.0; n]; n];
    for i in 0..n {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
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
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        // Power iteration with a deterministic start vector.
        let mut v: Vec<f64> = (0..n).map(|i| 1.0 + (i as f64 % 7.0) / 7.0).collect();
        let mut eigenvalue = 0.0;
        for _ in 0..200 {
            if read_control.interrupted() {
                return Err(MemoryAnalysisError::Interrupted);
            }
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
            if read_control.interrupted() {
                return Err(MemoryAnalysisError::Interrupted);
            }
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
    Ok(Some(scores))
}

/// Score all pairs above `threshold` from facts encoded into FHRR vectors for this read.
pub fn score_similar_pairs(
    decoded: &[(Value, Vec<f64>)],
    threshold: f64,
    read_control: &FactReadControl,
) -> Result<Vec<(f64, usize, usize)>, MemoryAnalysisError> {
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
    }
    let encoder = HolographicEncoder::new();
    let mut scored = Vec::new();
    for i in 0..decoded.len() {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        for j in (i + 1)..decoded.len() {
            if read_control.interrupted() {
                return Err(MemoryAnalysisError::Interrupted);
            }
            let sim = encoder.similarity(&decoded[i].1, &decoded[j].1)?;
            if sim >= threshold {
                scored.push((sim, i, j));
            }
        }
    }
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
    }
    Ok(scored)
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
pub fn empty_score_distribution() -> Value {
    json!({
        "min": Value::Null,
        "max": Value::Null,
        "bin_count": 0,
        "total_pairs": 0,
        "min_score": Value::Null,
        "max_score": Value::Null,
        "average_score": Value::Null,
        "bins": [],
    })
}

pub fn score_distribution(
    scored: &[(f64, usize, usize)],
    read_control: &FactReadControl,
) -> Result<Value, MemoryAnalysisError> {
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
    }
    let mut min_seen = f64::INFINITY;
    let mut max_seen = f64::NEG_INFINITY;
    let mut sum = 0.0_f64;
    let mut total_pairs = 0_i64;
    for (score, _, _) in scored {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        if !score.is_finite() {
            continue;
        }
        total_pairs += 1;
        min_seen = min_seen.min(*score);
        max_seen = max_seen.max(*score);
        sum += *score;
    }

    if total_pairs == 0 {
        return Ok(empty_score_distribution());
    }

    let range = max_seen - min_seen;
    if range <= 0.0 {
        return Ok(json!({
            "min": min_seen,
            "max": max_seen,
            "bin_count": 1,
            "total_pairs": total_pairs,
            "min_score": min_seen,
            "max_score": max_seen,
            "average_score": sum / total_pairs as f64,
            "bins": [{ "start": min_seen, "end": max_seen, "count": total_pairs }],
        }));
    }

    let mut counts = vec![0_i64; SIMILARITY_DISTRIBUTION_BINS];
    for (score, _, _) in scored {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
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
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
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

    Ok(json!({
        "min": min_seen,
        "max": max_seen,
        "bin_count": SIMILARITY_DISTRIBUTION_BINS,
        "total_pairs": total_pairs,
        "min_score": min_seen,
        "max_score": max_seen,
        "average_score": sum / total_pairs as f64,
        "bins": bins,
    }))
}

/// One retained similarity pair with its lexical-overlap analysis, computed
/// once per [`SimilarityComputation`] instead of per request (the overlap
/// tokenization used to re-run for up to 2000 pairs on every `/similarity`
/// call).
#[derive(Debug)]
pub struct ScoredPair {
    pub similarity: f64,
    /// Indices into [`SimilarityComputation::facts`].
    pub a: usize,
    pub b: usize,
    /// Lexical-overlap payload keys merged into the pair JSON
    /// (`token_overlap`, `overlap_coefficient`, `shared_tokens`, …).
    pub overlap: Value,
    pub classification: &'static str,
}

impl ScoredPair {
    /// Builds the pair from a raw score by running the lexical-overlap
    /// analysis on the two fact contents.
    pub fn analyze(
        facts: &[Value],
        similarity: f64,
        a: usize,
        b: usize,
    ) -> Result<Self, MemoryAnalysisError> {
        let a_content = facts
            .get(a)
            .ok_or(MemoryAnalysisError::InvalidFactIndex { index: a })?
            .get("content")
            .and_then(Value::as_str)
            .ok_or(MemoryAnalysisError::MissingFactContent { index: a })?;
        let b_content = facts
            .get(b)
            .ok_or(MemoryAnalysisError::InvalidFactIndex { index: b })?
            .get("content")
            .and_then(Value::as_str)
            .ok_or(MemoryAnalysisError::MissingFactContent { index: b })?;
        let (overlap, token_overlap, overlap_coefficient) = lexical_overlap(a_content, b_content);
        Ok(Self {
            similarity,
            a,
            b,
            overlap,
            classification: similarity_classification(
                similarity,
                token_overlap,
                overlap_coefficient,
            ),
        })
    }
}

/// A cached O(n²·d) pairwise-similarity computation over query-time encodings.
///
/// `key` fingerprints the canonical fact state used for encoding. Vectors are
/// not persisted; the cache retains only the fact metadata needed to render
/// similarity pairs.
#[derive(Debug)]
pub struct SimilarityComputation {
    /// Fingerprint of the canonical fact rows encoded for this computation.
    pub key: (usize, i64, Option<String>, u64),
    pub dim: usize,
    /// Fact metadata (`fact_id`, content, category, `trust_score`, `retrieval_count`).
    pub facts: Vec<Value>,
    /// Retained pairs, sorted by similarity descending: every pair at or
    /// above [`SIMILARITY_DEFAULT_THRESHOLD`] plus the top
    /// [`SIMILARITY_PAIR_CAP`] overall (the deepest prefix any `/similarity`
    /// request can return). Pairs below that horizon only contribute to
    /// `total_pairs` and `distribution`, so the cache holds O(cap) pairs
    /// instead of all O(n²) (~48 MB at n = 2000).
    pub pairs: Vec<ScoredPair>,
    /// Count of all finite pairs scored, retained or not.
    pub total_pairs: i64,
    /// [`score_distribution`] over all scored pairs, precomputed so requests
    /// never re-bin the full pair set.
    pub distribution: Value,
}

/// Finalizes a similarity computation from the full scored pair set:
/// distribution + total over everything, lexical overlap only for the
/// retained serveable prefix. Runs on the blocking pool with the scoring.
pub fn build_similarity_computation(
    key: (usize, i64, Option<String>, u64),
    dim: usize,
    facts: Vec<Value>,
    scored: Vec<(f64, usize, usize)>,
    read_control: &FactReadControl,
) -> Result<SimilarityComputation, MemoryAnalysisError> {
    let distribution = score_distribution(&scored, read_control)?;
    let mut total_pairs = 0_i64;
    for (score, _, _) in &scored {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        if score.is_finite() {
            total_pairs += 1;
        }
    }
    let mut retain = scored.len().min(SIMILARITY_PAIR_CAP as usize);
    while retain < scored.len() && scored[retain].0 >= SIMILARITY_DEFAULT_THRESHOLD {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        retain += 1;
    }
    let mut pairs = Vec::with_capacity(retain);
    for (similarity, a, b) in scored.into_iter().take(retain) {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        pairs.push(ScoredPair::analyze(&facts, similarity, a, b)?);
    }
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
    }
    Ok(SimilarityComputation {
        key,
        dim,
        facts,
        pairs,
        total_pairs,
        distribution,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracedecay_domain::{
        FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, ProvenanceId,
    };

    fn fact_id(operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new(operation)
                        .expect("fixture provenance must be canonical"),
                },
            )
            .expect("fixture identity material must be canonical"),
        )
        .expect("fixture fact ID must derive")
    }

    fn read_control() -> FactReadControl {
        FactReadControl::new(Arc::new(|| false))
    }

    #[test]
    fn score_similar_pairs_returns_holographic_scores() {
        let vector = HolographicEncoder::new()
            .encode_text("canonical dashboard pair")
            .expect("dashboard pair fixture must encode");
        let fact_a = fact_id("dashboard.similarity.fact-a");
        let fact_b = fact_id("dashboard.similarity.fact-b");
        let decoded = vec![
            (json!({"fact_id": fact_a.as_str()}), vector.clone()),
            (json!({"fact_id": fact_b.as_str()}), vector),
        ];

        let scored = score_similar_pairs(&decoded, SIMILARITY_PAIR_FLOOR, &read_control())
            .expect("canonical holographic vectors must score");

        assert_eq!(scored, vec![(1.0, 0, 1)]);
    }

    #[test]
    fn score_similar_pairs_rejects_mismatched_dimensions() {
        let encoder = HolographicEncoder::new();
        let canonical = encoder
            .encode_text("canonical dashboard pair")
            .expect("dashboard pair fixture must encode");
        let expected = canonical.len();
        let mut truncated = canonical.clone();
        truncated
            .pop()
            .expect("canonical dashboard vector must not be empty");
        let actual = truncated.len();
        let fact_a = fact_id("dashboard.similarity.mismatch-a");
        let fact_b = fact_id("dashboard.similarity.mismatch-b");
        let decoded = vec![
            (json!({"fact_id": fact_a.as_str()}), canonical),
            (json!({"fact_id": fact_b.as_str()}), truncated),
        ];

        let error = score_similar_pairs(&decoded, SIMILARITY_PAIR_FLOOR, &read_control())
            .expect_err("dimension mismatch must fail pair scoring");

        assert_eq!(
            error,
            MemoryAnalysisError::HolographicEncoding(HolographicEncodingError::DimensionMismatch {
                expected,
                actual
            })
        );
    }

    #[test]
    fn score_similar_pairs_observes_live_interruption() {
        let control = FactReadControl::new(Arc::new(|| true));
        let error = score_similar_pairs(&[], SIMILARITY_PAIR_FLOOR, &control)
            .expect_err("interrupted analysis must fail before scoring");
        assert_eq!(error, MemoryAnalysisError::Interrupted);
    }

    #[test]
    fn similarity_metadata_uses_canonical_fact_id_strings() {
        for operation in ["dashboard.fact-a", "dashboard.fact-b", "dashboard.fact-c"] {
            let fact_id = fact_id(operation);
            fact_id
                .validate_owner(&FactOwnerV1::Profile)
                .expect("derived fixture fact ID must retain its owner binding");
            assert_eq!(json!(fact_id.as_str()).as_str(), Some(fact_id.as_str()));
        }
    }

    #[test]
    fn pca_scores_two_points() {
        let features = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let Some(scores) =
            pca_scores(&features, &read_control()).expect("PCA must not be interrupted")
        else {
            panic!("expected PCA scores");
        };
        assert_eq!(scores.len(), 2);
        assert!(scores[0][0].abs() > 0.0 || scores[0][1].abs() > 0.0);
    }

    #[test]
    fn pca_scores_observes_live_interruption() {
        let control = FactReadControl::new(Arc::new(|| true));
        let error = pca_scores(&[vec![1.0], vec![2.0]], &control)
            .expect_err("interrupted PCA must fail before projection");
        assert_eq!(error, MemoryAnalysisError::Interrupted);
    }

    #[test]
    fn score_distribution_covers_all_scores() {
        let scored = vec![(0.75, 0, 1), (0.0, 0, 2), (-0.25, 1, 2)];
        let distribution = score_distribution(&scored, &read_control())
            .expect("distribution must not be interrupted");
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
        let distribution = score_distribution(&scored, &read_control())
            .expect("distribution must not be interrupted");
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
        let distribution = score_distribution(&scored, &read_control())
            .expect("distribution must not be interrupted");
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
    fn score_distribution_observes_live_interruption() {
        let control = FactReadControl::new(Arc::new(|| true));
        let error = score_distribution(&[(0.5, 0, 1)], &control)
            .expect_err("interrupted histogram must fail without returning a result");

        assert_eq!(error, MemoryAnalysisError::Interrupted);
    }

    #[test]
    fn build_similarity_computation_retains_serveable_prefix_only() {
        let fact_a = fact_id("dashboard.computation.fact-a");
        let fact_b = fact_id("dashboard.computation.fact-b");
        let fact_c = fact_id("dashboard.computation.fact-c");
        let facts = vec![
            json!({"fact_id": fact_a.as_str(), "content": "fact body a", "trust_score": 0.5}),
            json!({"fact_id": fact_b.as_str(), "content": "fact body b", "trust_score": 0.5}),
            json!({"fact_id": fact_c.as_str(), "content": "fact body c", "trust_score": 0.5}),
        ];
        // Descending scores: all three are scored, all three fit the cap.
        let scored = vec![(0.99, 0, 1), (0.5, 0, 2), (-0.2, 1, 2)];
        let computation = build_similarity_computation(
            (3, 0, Some(fact_c.as_str().to_owned()), 7),
            4,
            facts,
            scored,
            &read_control(),
        )
        .expect("complete canonical metadata must build similarity computation");
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
    fn similarity_finalization_observes_live_interruption() {
        let control = FactReadControl::new(Arc::new(|| true));
        let error =
            build_similarity_computation((0, 0, None, 0), 0, Vec::new(), Vec::new(), &control)
                .expect_err("interrupted finalization must not produce a cacheable result");

        assert_eq!(error, MemoryAnalysisError::Interrupted);
    }
}

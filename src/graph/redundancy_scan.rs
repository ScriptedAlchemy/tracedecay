//! The AST-level functional-duplicate scan: candidate selection, fingerprint
//! computation, pairwise ranking, and the structured payload built from them.
//!
//! This lives below the MCP handler tree. The `tracedecay_redundancy` handler
//! supplies an admitted verified graph and renders the typed scan payload.
//!
//! Pipeline:
//!
//! 1. Pull all `Function` / `Method` nodes (optionally path-filtered).
//! 2. Group by file. Open each file once, parse with tree-sitter,
//!    locate every target node via its `(start_line, end_line)`, and
//!    compute a [`Fingerprint`](crate::redundancy::Fingerprint). Fingerprints
//!    remain request-owned; the code-index authority owns durable graph state.
//! 3. Bucket the resulting fingerprints by `body_tokens` (±25 % window).
//!    Within each bucket, score every pair via
//!    [`redundancy_match_score`](crate::redundancy::redundancy_match_score),
//!    which blends the composite similarity with the body-vector cosine,
//!    relabels cosine-rescued `naming` pairs as `body_vector`, and downranks
//!    generic helper names.
//! 4. Filter by threshold, sort by `ranking_score` desc (total order — ties
//!    fall through similarity, cosine, then names and node ids), and return
//!    the top N pairs plus their connected duplicate groups.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::application::semantic_runtime::{
    SemanticRedundancyGenerationV1, project_semantic_redundancy_generation,
};
use crate::errors::{Result, TraceDecayError};
use crate::redundancy::{
    Fingerprint, RedundancyMatchScore, body_token_window, compute_fingerprint, parse_file,
    redundancy_match_score, round4,
};
use crate::tracedecay::TraceDecay;
use tracedecay_domain::SourceSpan;

/// Extraction-attested symbol evidence consumed by redundancy scoring.
///
/// This intentionally is not the legacy runtime-core `Node`: the verified
/// graph projection does not publish unrelated mutable graph fields, and the
/// redundancy journey must not synthesize them merely to satisfy an old DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RedundancyCandidate {
    id: String,
    name: String,
    qualified_name: String,
    file_path: String,
    start_line: u32,
    end_line: u32,
    source_span: SourceSpan,
}

struct RedundantPair<'a> {
    score: RedundancyMatchScore,
    node_a: &'a RedundancyCandidate,
    node_b: &'a RedundancyCandidate,
    fp_a: &'a Fingerprint,
    fp_b: &'a Fingerprint,
}

struct RedundancyPairScan<'a> {
    scoped: Vec<(&'a RedundancyCandidate, &'a Fingerprint)>,
    threshold: f64,
    include_naming: bool,
    max_pairs: usize,
    outer: usize,
    inner: usize,
    found: Vec<RedundantPair<'a>>,
}

impl<'a> RedundancyPairScan<'a> {
    fn new(
        mut scoped: Vec<(&'a RedundancyCandidate, &'a Fingerprint)>,
        threshold: f64,
        include_naming: bool,
        max_pairs: usize,
    ) -> Self {
        scoped.sort_by(|(left_node, left_fp), (right_node, right_fp)| {
            left_fp
                .body_tokens
                .cmp(&right_fp.body_tokens)
                .then_with(|| left_node.id.cmp(&right_node.id))
        });
        Self {
            scoped,
            threshold,
            include_naming,
            max_pairs,
            outer: 0,
            inner: 0,
            found: Vec::new(),
        }
    }

    fn advance(&mut self, budget: usize) -> bool {
        let mut spent = 0usize;
        while self.outer < self.scoped.len() {
            let (node_a, fp_a) = self.scoped[self.outer];
            let (low, high) = body_token_window(fp_a.body_tokens);
            if self.inner == 0 {
                self.inner = self.outer + 1;
            }
            while self.inner < self.scoped.len() {
                let (node_b, fp_b) = self.scoped[self.inner];
                if fp_b.body_tokens > high {
                    break;
                }
                if fp_b.body_tokens >= low
                    && let Some(pair) = redundant_pair(
                        node_a,
                        fp_a,
                        node_b,
                        fp_b,
                        self.threshold,
                        self.include_naming,
                    )
                {
                    self.found.push(pair);
                }
                self.inner += 1;
                spent += 1;
                if spent >= budget {
                    return true;
                }
            }
            self.outer += 1;
            self.inner = 0;
        }
        false
    }

    fn finish(self) -> Vec<RedundantPair<'a>> {
        let mut found = self.found;
        found.sort_by(|left, right| {
            right
                .score
                .ranking_score
                .total_cmp(&left.score.ranking_score)
                .then_with(|| right.score.similarity.total_cmp(&left.score.similarity))
                .then_with(|| {
                    right
                        .score
                        .vector_cosine
                        .total_cmp(&left.score.vector_cosine)
                })
                .then_with(|| left.node_a.name.cmp(&right.node_a.name))
                .then_with(|| left.node_b.name.cmp(&right.node_b.name))
                .then_with(|| left.node_a.id.cmp(&right.node_a.id))
                .then_with(|| left.node_b.id.cmp(&right.node_b.id))
        });
        found.truncate(self.max_pairs);
        found
    }
}

fn redundant_pair<'a>(
    node_a: &'a RedundancyCandidate,
    fp_a: &'a Fingerprint,
    node_b: &'a RedundancyCandidate,
    fp_b: &'a Fingerprint,
    threshold: f64,
    include_naming: bool,
) -> Option<RedundantPair<'a>> {
    let score = redundancy_match_score(
        &node_a.name,
        fp_a,
        &node_b.name,
        fp_b,
        threshold,
        include_naming,
    )?;
    let left_key = (&node_a.file_path, node_a.start_line, &node_a.id);
    let right_key = (&node_b.file_path, node_b.start_line, &node_b.id);
    let (node_a, fp_a, node_b, fp_b) = if left_key <= right_key {
        (node_a, fp_a, node_b, fp_b)
    } else {
        (node_b, fp_b, node_a, fp_a)
    };
    Some(RedundantPair {
        score,
        node_a,
        node_b,
        fp_a,
        fp_b,
    })
}

#[cfg(test)]
fn find_redundant_pairs<'a>(
    scoped: Vec<(&'a RedundancyCandidate, &'a Fingerprint)>,
    threshold: f64,
    include_naming: bool,
    max_pairs: usize,
) -> Vec<RedundantPair<'a>> {
    let mut scan = RedundancyPairScan::new(scoped, threshold, include_naming, max_pairs);
    while scan.advance(usize::MAX) {}
    scan.finish()
}

fn connected_node_groups<'a>(pairs: &'a [RedundantPair<'a>]) -> Vec<Vec<&'a RedundancyCandidate>> {
    let mut groups: Vec<Vec<&RedundancyCandidate>> = Vec::new();
    for pair in pairs {
        let matching = groups
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                group
                    .iter()
                    .any(|node| node.id == pair.node_a.id || node.id == pair.node_b.id)
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if matching.is_empty() {
            groups.push(vec![pair.node_a, pair.node_b]);
            continue;
        }
        let first = matching[0];
        for node in [pair.node_a, pair.node_b] {
            if !groups[first].iter().any(|existing| existing.id == node.id) {
                groups[first].push(node);
            }
        }
        for index in matching.into_iter().skip(1).rev() {
            let merged = groups.remove(index);
            for node in merged {
                if !groups[first].iter().any(|existing| existing.id == node.id) {
                    groups[first].push(node);
                }
            }
        }
    }
    groups
}

/// The knobs a redundancy scan runs with, already resolved from whatever
/// surface requested it (MCP tool arguments or a typed port request).
pub(crate) struct RedundancyOptions<'a> {
    pub(crate) path_prefix: Option<&'a str>,
    pub(crate) min_lines: u32,
    pub(crate) max_pairs: usize,
    pub(crate) threshold: f64,
    pub(crate) include_naming: bool,
    pub(crate) include_generated: bool,
}

/// One ranked structural pair, projected into owned values so the markdown
/// renderer in the handler layer does not have to borrow the scan's interior.
#[derive(Clone)]
pub(crate) struct RedundancyNodeViewV1 {
    pub(crate) name: String,
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) id: String,
}

pub(crate) struct RedundancyPairViewV1 {
    pub(crate) a: RedundancyNodeViewV1,
    pub(crate) b: RedundancyNodeViewV1,
    pub(crate) label_a: String,
    pub(crate) label_b: String,
    pub(crate) id_a: String,
    pub(crate) id_b: String,
    pub(crate) severity: &'static str,
    pub(crate) overlap_kind: &'static str,
    pub(crate) ranking_score: f64,
    pub(crate) similarity: f64,
    pub(crate) vector_cosine: f64,
    pub(crate) generic_helper_downranked: bool,
    pub(crate) body_tokens: [usize; 2],
}

/// A completed scan: the structured payload plus the structural view the
/// markdown renderer needs.
pub(crate) struct RedundancyScanV1 {
    /// The structured payload — the exact `Value` the MCP handler emits and
    /// the port decodes into `RedundancyResultV1`.
    pub(crate) output: Value,
    /// True when a semantic generation was projected, in which case `output`
    /// carries semantic pairs that the structural markdown view cannot render.
    pub(crate) semantic_active: bool,
    pub(crate) total_candidates: usize,
    pub(crate) scanned: usize,
    pub(crate) pairs: Vec<RedundancyPairViewV1>,
    /// Connected duplicate groups, as `name (file:line)` member labels.
    pub(crate) groups: Vec<Vec<String>>,
}

/// Scored candidate pairs per uninterrupted slice of the pairwise scan.
///
/// The scan is the one place in this pipeline that runs unbounded CPU work
/// with no natural await point, so it yields to the runtime every slice. Each
/// comparison is a two-pointer merge over two shingle sets, so a few thousand
/// of them stay well inside a sub-millisecond slice on a saturated daemon —
/// small enough that a concurrent interactive query is never stuck behind the
/// scan, large enough that the yield itself stays statistically free.
const REDUNDANCY_PAIR_SLICE: usize = 2048;

/// Run the full redundancy pipeline for `options`.
pub(crate) async fn redundancy_scan(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    options: &RedundancyOptions<'_>,
) -> Result<RedundancyScanV1> {
    // 1. Collect candidate function nodes.
    let nodes = collect_candidates(
        graph,
        options.path_prefix,
        options.min_lines,
        options.include_generated,
    )?;
    let total_candidates = nodes.len();

    // 2. Compute fresh, request-owned fingerprints. The final graph authority
    // intentionally has no parallel SQLite fingerprint or pair cache.
    let fingerprints = ensure_fingerprints(cg, &nodes).await?;
    let scanned = fingerprints.len();

    // 3. Bucket by token count to keep pairwise comparison sub-quadratic, and
    //    walk the buckets in bounded slices so this CPU-bound analysis cannot
    //    pin a runtime worker for its whole duration while the daemon is
    //    serving interactive queries. The slice cursor changes only *when* the
    //    loop pauses, never which pairs it visits or how they rank, so a paced
    //    scan returns exactly what a single-shot scan returns.
    let scoped = scoped_fingerprints(&nodes, &fingerprints);
    let mut scan = RedundancyPairScan::new(
        scoped,
        options.threshold,
        options.include_naming,
        options.max_pairs,
    );
    while scan.advance(REDUNDANCY_PAIR_SLICE) {
        tokio::task::yield_now().await;
    }
    let pairs = scan.finish();

    // Connected components are the shared source of truth for the JSON `groups`
    // array and the markdown Groups section; compute them once and thread the
    // result into both so the two views can never diverge and the O(pairs²)
    // grouping runs a single time per call.
    let groups = connected_node_groups(&pairs);
    let semantic = project_semantic_redundancy_generation(cg.project_root()).await;
    let output = augment_redundancy_output(
        options,
        total_candidates,
        scanned,
        &nodes,
        &pairs,
        &groups,
        semantic.as_ref(),
    );
    Ok(RedundancyScanV1 {
        output,
        semantic_active: semantic.is_some(),
        total_candidates,
        scanned,
        pairs: pair_views(&pairs),
        groups: group_views(&groups),
    })
}

/// `name (file:line)` locator that chains into `tracedecay_body` / `_callers`.
fn node_label(node: &RedundancyCandidate) -> String {
    format!("{} ({}:{})", node.name, node.file_path, node.start_line)
}

fn pair_views(pairs: &[RedundantPair<'_>]) -> Vec<RedundancyPairViewV1> {
    pairs
        .iter()
        .map(|pair| RedundancyPairViewV1 {
            a: RedundancyNodeViewV1 {
                name: pair.node_a.name.clone(),
                file: pair.node_a.file_path.clone(),
                line: pair.node_a.start_line,
                id: pair.node_a.id.clone(),
            },
            b: RedundancyNodeViewV1 {
                name: pair.node_b.name.clone(),
                file: pair.node_b.file_path.clone(),
                line: pair.node_b.start_line,
                id: pair.node_b.id.clone(),
            },
            label_a: node_label(pair.node_a),
            label_b: node_label(pair.node_b),
            id_a: pair.node_a.id.clone(),
            id_b: pair.node_b.id.clone(),
            severity: pair.score.severity,
            overlap_kind: pair.score.overlap_kind,
            ranking_score: pair.score.ranking_score,
            similarity: pair.score.similarity,
            vector_cosine: pair.score.vector_cosine,
            generic_helper_downranked: pair.score.generic_helper_downranked,
            body_tokens: [pair.fp_a.body_tokens, pair.fp_b.body_tokens],
        })
        .collect()
}

fn group_views(groups: &[Vec<&RedundancyCandidate>]) -> Vec<Vec<String>> {
    groups
        .iter()
        .map(|group| group.iter().map(|node| node_label(node)).collect())
        .collect()
}

#[derive(Clone, Copy)]
struct SemanticPair<'a> {
    node_a: &'a RedundancyCandidate,
    node_b: &'a RedundancyCandidate,
    cosine: f64,
    distance_micros: i64,
}

fn augment_redundancy_output(
    options: &RedundancyOptions<'_>,
    total_candidates: usize,
    scanned: usize,
    nodes: &[RedundancyCandidate],
    pairs: &[RedundantPair<'_>],
    groups: &[Vec<&RedundancyCandidate>],
    semantic: Option<&SemanticRedundancyGenerationV1>,
) -> Value {
    let Some(semantic) = semantic else {
        return redundancy_output(options, total_candidates, scanned, pairs, groups);
    };
    let semantic_pairs = semantic_pairs(nodes, pairs, semantic);
    let mut ranked = Vec::with_capacity(pairs.len() + semantic_pairs.len());
    ranked.extend(pairs.iter().map(|pair| {
        let mut value = redundant_pair_json(pair);
        value["classification"] = Value::String(
            if pair.fp_a.source_hash == pair.fp_b.source_hash {
                "exact_clone"
            } else {
                "structural_near_duplicate"
            }
            .to_owned(),
        );
        (
            0_u8,
            pair.score.ranking_score,
            pair.node_a.id.as_str(),
            pair.node_b.id.as_str(),
            value,
        )
    }));
    ranked.extend(semantic_pairs.iter().map(|pair| {
        (
            1_u8,
            pair.cosine,
            pair.node_a.id.as_str(),
            pair.node_b.id.as_str(),
            semantic_pair_json(pair),
        )
    }));
    ranked.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.2.cmp(right.2))
            .then_with(|| left.3.cmp(right.3))
    });
    ranked.truncate(options.max_pairs);
    let rendered_pairs = ranked
        .into_iter()
        .map(|(_, _, _, _, value)| value)
        .collect::<Vec<_>>();
    let augmented_groups = connected_rendered_groups(&rendered_pairs, nodes);
    json!({
        "candidates": total_candidates,
        "scanned": scanned,
        "skipped_for_size": total_candidates.saturating_sub(scanned),
        "pair_count": rendered_pairs.len(),
        "pairs": rendered_pairs,
        "groups": duplicate_groups(&augmented_groups),
        "groups_scope": "connected components over the returned pairs only; raise max_pairs to see full clusters",
        "ranked_by": "structural composite pairs first; accepted semantic analogues by calibrated distance, stable node-id ties",
        "scope": options.path_prefix.unwrap_or("(whole project)"),
        "thresholds": {
            "min_lines": options.min_lines,
            "similarity_threshold": options.threshold,
            "include_naming_only": options.include_naming,
            "include_generated_paths": options.include_generated,
        },
        "semantic_generation": {
            "vector_generation": semantic.vector_generation,
            "source_generation": semantic.source_generation,
            "projection_key": semantic.projection_key,
            "scope_digest": semantic.profile.scope_digest,
            "accepted_profile_digest": semantic.profile.accepted_profile_digest,
            "calibration_profile_id": semantic.profile.calibration_profile_id,
            "calibration_digest": semantic.profile.calibration_digest,
            "redundancy_profile_digest": semantic.profile.redundancy_profile_digest,
            "maximum_distance_micros": semantic.profile.maximum_distance_micros,
        },
    })
}

/// One vectored-node projection entry used for bucketing: the node plus one of
/// its usable embedding vectors reduced to a single normalized coordinate.
struct SemanticEntry<'a> {
    /// Normalized first coordinate (`values[0] / ‖values‖`); the sort/window
    /// key. Any accepted pair differs by at most one window on this coordinate.
    key: f64,
    node: &'a RedundancyCandidate,
}

/// Reduce a raw embedding vector to the normalized first coordinate used as the
/// bucketing key, or `None` when the vector could never yield a finite cosine
/// (empty, non-finite component, or zero norm) — matching [`semantic_cosine`]'s
/// own usability preconditions, so no acceptable pair is ever excluded.
fn normalized_projection(values: &[f32]) -> Option<f64> {
    let first = f64::from(*values.first()?);
    let mut norm_sq = 0.0_f64;
    for &value in values {
        if !value.is_finite() {
            return None;
        }
        let value = f64::from(value);
        norm_sq += value * value;
    }
    (norm_sq > 0.0).then(|| first / norm_sq.sqrt())
}

fn semantic_pairs<'a>(
    nodes: &'a [RedundancyCandidate],
    structural: &[RedundantPair<'_>],
    semantic: &SemanticRedundancyGenerationV1,
) -> Vec<SemanticPair<'a>> {
    // 1. Bind vectors to nodes in O(nodes + vectors) via an exact lookup index,
    //    replacing the former O(vectors × nodes) filter-per-vector. A vector
    //    binds to the node whose file path matches and whose qualified name OR
    //    short name equals the vector's qualified name; a match that is
    //    ambiguous across distinct nodes is dropped, exactly as the prior
    //    linear scan did. Each node appears at most once per key (the short-name
    //    entry is skipped when it equals the qualified name), so a lookup slice
    //    of length one is precisely a unique node match.
    let mut nodes_by_key: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        nodes_by_key
            .entry((node.file_path.as_str(), node.qualified_name.as_str()))
            .or_default()
            .push(index);
        if node.name != node.qualified_name {
            nodes_by_key
                .entry((node.file_path.as_str(), node.name.as_str()))
                .or_default()
                .push(index);
        }
    }
    let mut vectors_by_node: HashMap<&str, Vec<&[f32]>> = HashMap::new();
    for vector in &semantic.vectors {
        let Some(matches) =
            nodes_by_key.get(&(vector.file_path.as_str(), vector.qualified_name.as_str()))
        else {
            continue;
        };
        let [index] = matches.as_slice() else {
            continue; // zero or multiple distinct nodes → ambiguous, skip.
        };
        vectors_by_node
            .entry(nodes[*index].id.as_str())
            .or_default()
            .push(&vector.values);
    }

    let structural_pairs = structural
        .iter()
        .map(|pair| canonical_pair_ids(&pair.node_a.id, &pair.node_b.id))
        .collect::<std::collections::BTreeSet<_>>();

    // 2. Bucket vectored candidates by their normalized projection so the scan
    //    compares only plausibly-similar pairs instead of all pairs. Sorting by
    //    one coordinate and sliding a window of `cosine_projection_window()`
    //    preserves perfect recall: every accepted pair has cosine above the
    //    profile threshold, hence its normalized vectors differ by at most one
    //    window on any coordinate, so both endpoints fall inside the window.
    let mut entries: Vec<SemanticEntry<'a>> = Vec::new();
    for node in nodes {
        let Some(vectors) = vectors_by_node.get(node.id.as_str()) else {
            continue;
        };
        for values in vectors {
            if let Some(key) = normalized_projection(values) {
                entries.push(SemanticEntry { key, node });
            }
        }
    }
    entries.sort_by(|left, right| {
        left.key
            .total_cmp(&right.key)
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
    let window = semantic.profile.cosine_projection_window();

    // 3. Emit candidate node pairs from the window, dedup so each unordered node
    //    pair is scored once, and apply the exact accept + overlap +
    //    structural-exclusion gate unchanged from the former all-pairs scan.
    let mut seen_pairs: std::collections::HashSet<(&str, &str)> = std::collections::HashSet::new();
    let mut semantic_pairs = Vec::new();
    for (index, left) in entries.iter().enumerate() {
        for right in entries.iter().skip(index + 1) {
            if right.key - left.key > window {
                break; // sorted by key — no later entry can be closer.
            }
            if left.node.id == right.node.id {
                continue; // same node via two of its own vectors.
            }
            let ids = canonical_pair_ids(&left.node.id, &right.node.id);
            if !seen_pairs.insert(ids) {
                continue; // node pair already scored from an earlier entry pair.
            }
            if nodes_overlap(left.node, right.node) || structural_pairs.contains(&ids) {
                continue;
            }
            let (Some(vectors_a), Some(vectors_b)) = (
                vectors_by_node.get(left.node.id.as_str()),
                vectors_by_node.get(right.node.id.as_str()),
            ) else {
                continue;
            };
            let cosine = vectors_a
                .iter()
                .flat_map(|left| {
                    vectors_b
                        .iter()
                        .filter_map(move |right| semantic_cosine(left, right))
                })
                .max_by(f64::total_cmp);
            if let Some((cosine, distance_micros)) = cosine.and_then(|cosine| {
                semantic
                    .profile
                    .accepts(cosine)
                    .map(|distance| (cosine, distance))
            }) {
                let (node_a, node_b) = if left.node.id <= right.node.id {
                    (left.node, right.node)
                } else {
                    (right.node, left.node)
                };
                semantic_pairs.push(SemanticPair {
                    node_a,
                    node_b,
                    cosine,
                    distance_micros,
                });
            }
        }
    }
    semantic_pairs.sort_by(|left, right| {
        right
            .cosine
            .total_cmp(&left.cosine)
            .then_with(|| left.node_a.id.cmp(&right.node_a.id))
            .then_with(|| left.node_b.id.cmp(&right.node_b.id))
    });
    semantic_pairs
}

fn semantic_cosine(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty() || left.len() != right.len() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;
    for (&left, &right) in left.iter().zip(right) {
        if !left.is_finite() || !right.is_finite() {
            return None;
        }
        let left = f64::from(left);
        let right = f64::from(right);
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    (left_norm > 0.0 && right_norm > 0.0)
        .then(|| (dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0))
}

fn canonical_pair_ids<'a>(left: &'a str, right: &'a str) -> (&'a str, &'a str) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn nodes_overlap(left: &RedundancyCandidate, right: &RedundancyCandidate) -> bool {
    left.file_path == right.file_path
        && left.start_line <= right.end_line
        && right.start_line <= left.end_line
}

fn semantic_pair_json(pair: &SemanticPair<'_>) -> Value {
    json!({
        "similarity": round4(pair.cosine),
        "ranking_score": 0.0,
        "severity": "review",
        "overlap_kind": "semantic",
        "classification": "semantic_analogue",
        "a": node_json(pair.node_a),
        "b": node_json(pair.node_b),
        "signals": {
            "ast_match": false,
            "cfg_match": false,
            "call_seq_match": false,
            "shingle_jaccard": 0.0,
            "body_vector_cosine": 0.0,
            "semantic_vector_cosine": round4(pair.cosine),
            "semantic_distance_micros": pair.distance_micros,
            "generic_helper_downranked": false,
            "body_tokens": [0, 0],
        },
    })
}

fn connected_rendered_groups<'a>(
    pairs: &[Value],
    nodes: &'a [RedundancyCandidate],
) -> Vec<Vec<&'a RedundancyCandidate>> {
    let by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for pair in pairs {
        let (Some(left), Some(right)) = (pair["a"]["id"].as_str(), pair["b"]["id"].as_str()) else {
            continue;
        };
        adjacency.entry(left).or_default().push(right);
        adjacency.entry(right).or_default().push(left);
    }
    let mut ids = adjacency.keys().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    let mut visited = std::collections::BTreeSet::new();
    let mut groups = Vec::new();
    for root in ids {
        if !visited.insert(root) {
            continue;
        }
        let mut stack = vec![root];
        let mut group = Vec::new();
        while let Some(id) = stack.pop() {
            if let Some(node) = by_id.get(id) {
                group.push(*node);
            }
            if let Some(neighbors) = adjacency.get(id) {
                for neighbor in neighbors {
                    if visited.insert(*neighbor) {
                        stack.push(neighbor);
                    }
                }
            }
        }
        group.sort_by(|left, right| left.id.cmp(&right.id));
        if group.len() > 1 {
            groups.push(group);
        }
    }
    groups
}

fn redundancy_output(
    options: &RedundancyOptions<'_>,
    total_candidates: usize,
    scanned: usize,
    pairs: &[RedundantPair<'_>],
    groups: &[Vec<&RedundancyCandidate>],
) -> Value {
    let rendered_pairs: Vec<Value> = pairs.iter().map(redundant_pair_json).collect();
    json!({
        "candidates": total_candidates,
        "scanned": scanned,
        "skipped_for_size": total_candidates.saturating_sub(scanned),
        "pair_count": rendered_pairs.len(),
        "pairs": rendered_pairs,
        "groups": duplicate_groups(groups),
        "groups_scope": "connected components over the returned pairs only; raise max_pairs to see full clusters",
        "ranked_by": "ranking_score desc (composite similarity plus body-vector signal, generic helpers downranked)",
        "scope": options.path_prefix.unwrap_or("(whole project)"),
        "thresholds": {
            "min_lines": options.min_lines,
            "similarity_threshold": options.threshold,
            "include_naming_only": options.include_naming,
            "include_generated_paths": options.include_generated,
        },
    })
}

// ---------------------------------------------------------------------------
// 1. Candidate selection
// ---------------------------------------------------------------------------

fn collect_candidates(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    path_prefix: Option<&str>,
    min_lines: u32,
    include_generated: bool,
) -> Result<Vec<RedundancyCandidate>> {
    const SYMBOL_PAGE_SIZE: usize = 10_000;
    const MAX_SYMBOLS_EXAMINED: usize = 500_000;

    let mut after = None;
    let mut examined = 0usize;
    let mut candidates = Vec::new();
    loop {
        let page = graph.symbols_page(after.as_ref(), SYMBOL_PAGE_SIZE)?;
        if page.symbols.is_empty() {
            if page.has_more {
                return Err(redundancy_graph_problem(
                    "verified redundancy census returned an empty continuation page",
                ));
            }
            break;
        }
        examined = examined.saturating_add(page.symbols.len());
        if examined > MAX_SYMBOLS_EXAMINED {
            return Err(redundancy_graph_problem(
                "verified redundancy census exceeded its symbol budget",
            ));
        }
        after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
        for symbol in page.symbols {
            if let Some(node) = redundancy_node(symbol)? {
                candidates.push(node);
            }
        }
        if !page.has_more {
            break;
        }
    }

    Ok(candidates
        .into_iter()
        .filter(|n| n.end_line.saturating_sub(n.start_line) + 1 >= min_lines)
        .filter(|n| include_generated || !is_generated_path(&n.file_path))
        .filter(|n| {
            path_prefix.is_none_or(|pfx| {
                let prefix = if pfx.ends_with('/') {
                    pfx.to_string()
                } else {
                    format!("{pfx}/")
                };
                n.file_path.starts_with(&prefix) || n.file_path == pfx
            })
        })
        .collect())
}

fn redundancy_node(
    symbol: tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1,
) -> Result<Option<RedundancyCandidate>> {
    let metadata = symbol.metadata.ok_or_else(|| {
        redundancy_graph_problem("verified redundancy symbol is missing extraction metadata")
    })?;
    if !matches!(metadata.kind.as_str(), "function" | "method") {
        return Ok(None);
    }
    let binding = symbol.binding.ok_or_else(|| {
        redundancy_graph_problem("verified redundancy symbol is missing its file binding")
    })?;
    let file_path = binding.logical_path.ok_or_else(|| {
        redundancy_graph_problem("verified redundancy symbol is missing its logical path")
    })?;
    let source_span = binding.source_span.ok_or_else(|| {
        redundancy_graph_problem("verified redundancy symbol is missing its source span")
    })?;
    if metadata.line_span == 0 {
        return Err(redundancy_graph_problem(
            "verified redundancy symbol has an empty line span",
        ));
    }
    let end_line = metadata
        .start_line
        .checked_add(metadata.line_span - 1)
        .ok_or_else(|| redundancy_graph_problem("verified redundancy line span overflowed"))?;
    Ok(Some(RedundancyCandidate {
        id: symbol.occurrence.as_str().to_owned(),
        name: metadata.simple_name,
        qualified_name: metadata.qualified_name,
        file_path,
        start_line: metadata.start_line,
        end_line,
        source_span,
    }))
}

fn redundancy_graph_problem(detail: &str) -> TraceDecayError {
    TraceDecayError::project_route("verified-redundancy-evidence-unavailable", false, detail)
}

/// Build outputs, vendored code, and worktree mirrors duplicate real sources
/// byte-for-byte, so their pairs are indistinguishable from true duplicates
/// at the scoring layer — they have to be excluded during candidate
/// collection (a recurring noise source in real scans: dist mirrors, package
/// twins, and `.worktrees` self-duplicates). Opt back in with
/// `include_generated_paths: true`.
///
/// Delegates to the shared [`crate::config::is_generated_path_segment`]
/// (segment list plus minified-asset suffix), which folds in this scanner's
/// former standalone `.min.js` check as the more general `*.min.*` suffix,
/// and now also picks up `.cache`, `.gradle`, `.next`, `.turbo`, `.venv`,
/// `coverage`, and `venv` — segments this scanner didn't previously
/// exclude but the other generated/vendored lists in the codebase already
/// did.
fn is_generated_path(path: &str) -> bool {
    crate::config::is_generated_path_segment(path)
}

// ---------------------------------------------------------------------------
// 2. Fingerprint computation
// ---------------------------------------------------------------------------

/// Returns a request-owned map from `node_id` to its current fingerprint.
/// Each supported source file is opened and parsed at most once per scan.
async fn ensure_fingerprints(
    cg: &TraceDecay,
    candidates: &[RedundancyCandidate],
) -> Result<HashMap<String, Fingerprint>> {
    Ok(compute_fingerprints(cg.project_root(), candidates)?.fingerprints)
}

#[derive(Debug)]
struct FingerprintLoad {
    fingerprints: HashMap<String, Fingerprint>,
    #[cfg(test)]
    parsed_files: usize,
    #[cfg(test)]
    computed_fingerprints: usize,
}

fn compute_fingerprints(
    project_root: &Path,
    candidates: &[RedundancyCandidate],
) -> Result<FingerprintLoad> {
    let registry = tracedecay_code_extraction::LanguageRegistry::new();

    // Group candidates by file so we parse each file at most once.
    let mut by_file: HashMap<String, Vec<&RedundancyCandidate>> = HashMap::new();
    for n in candidates {
        by_file.entry(n.file_path.clone()).or_default().push(n);
    }

    let mut out: HashMap<String, Fingerprint> = HashMap::new();
    #[cfg(test)]
    let mut parsed_files = 0usize;
    #[cfg(test)]
    let mut computed_fingerprints = 0usize;

    for (file_path, file_nodes) in by_file {
        // Figure out which tree-sitter language this file maps to.
        let Some(extractor) = registry.extractor_for_file(&file_path) else {
            return Err(redundancy_graph_problem(
                "verified redundancy symbol has no registered extractor",
            ));
        };
        let lang_key = extractor_to_language_key(extractor.language_name());
        let Some(lang_key) = lang_key else {
            return Err(redundancy_graph_problem(
                "verified redundancy symbol has no fingerprint language mapping",
            ));
        };

        // Read the exact source generation attested by the graph. A missing or
        // changed file is a typed stale-evidence failure, never an empty scan.
        let abs = project_root.join(&file_path);
        let source = std::fs::read_to_string(&abs).map_err(|error| {
            redundancy_graph_problem(&format!(
                "verified redundancy source `{file_path}` is unavailable: {error}"
            ))
        })?;

        let language =
            tracedecay_code_extraction::ts_provider::language(lang_key).map_err(|error| {
                redundancy_graph_problem(&format!(
                    "verified redundancy grammar `{lang_key}` is unavailable: {error}"
                ))
            })?;
        let tree = parse_file(&source, &language).ok_or_else(|| {
            redundancy_graph_problem(&format!(
                "verified redundancy source `{file_path}` could not be parsed"
            ))
        })?;
        #[cfg(test)]
        {
            parsed_files += 1;
        }

        for node in file_nodes {
            let Ok(start_byte) = usize::try_from(node.source_span.start_byte) else {
                return Err(redundancy_graph_problem(
                    "verified redundancy source span start is not addressable",
                ));
            };
            let Ok(end_byte) = usize::try_from(node.source_span.end_byte) else {
                return Err(redundancy_graph_problem(
                    "verified redundancy source span end is not addressable",
                ));
            };
            if start_byte >= end_byte || end_byte > source.len() {
                return Err(redundancy_graph_problem(
                    "verified redundancy source span is stale against the source file",
                ));
            }
            let Some(ts_node) = tree
                .root_node()
                .descendant_for_byte_range(start_byte, end_byte)
            else {
                return Err(redundancy_graph_problem(
                    "verified redundancy source span does not resolve to a syntax node",
                ));
            };
            out.insert(node.id.clone(), compute_fingerprint(&source, ts_node));
            #[cfg(test)]
            {
                computed_fingerprints += 1;
            }
        }
    }

    Ok(FingerprintLoad {
        fingerprints: out,
        #[cfg(test)]
        parsed_files,
        #[cfg(test)]
        computed_fingerprints,
    })
}

/// Map `extractor.language_name()` (e.g. "Rust", "TypeScript") to the
/// language key used by `ts_provider::language`. Returns `None` for
/// extractors whose grammar isn't wired up here (extending the map
/// extends fingerprinting to that language).
fn extractor_to_language_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "Rust" => "rust",
        "Go" => "go",
        "Java" => "java",
        "Scala" => "scala",
        "TypeScript" => "typescript",
        "TSX" => "tsx",
        "Python" => "python",
        "C" => "c",
        "C++" => "cpp",
        "C#" => "c_sharp",
        "Kotlin" => "kotlin",
        "Swift" => "swift",
        "JavaScript" => "javascript",
        "Ruby" => "ruby",
        "PHP" => "php",
        "Lua" => "lua",
        "Zig" => "zig",
        "Bash" => "bash",
        "Dart" => "dart",
        "Haskell" => "haskell",
        "OCaml" => "ocaml",
        "Elixir" => "elixir",
        "Erlang" => "erlang",
        "Clojure" => "clojure",
        "F#" => "fsharp",
        "Perl" => "perl",
        "R" => "r",
        "Julia" => "julia",
        "Nix" => "nix",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// 3. Pairwise comparison + ranking
// ---------------------------------------------------------------------------

type ScopedFingerprint<'a> = (&'a RedundancyCandidate, &'a Fingerprint);

fn scoped_fingerprints<'a>(
    nodes: &'a [RedundancyCandidate],
    fingerprints: &'a HashMap<String, Fingerprint>,
) -> Vec<ScopedFingerprint<'a>> {
    nodes
        .iter()
        .filter_map(|n| fingerprints.get(&n.id).map(|fp| (n, fp)))
        .collect()
}

fn redundant_pair_json(pair: &RedundantPair<'_>) -> Value {
    json!({
        "similarity": round4(pair.score.similarity),
        "ranking_score": round4(pair.score.ranking_score),
        "severity": pair.score.severity,
        "overlap_kind": pair.score.overlap_kind,
        "a": node_json(pair.node_a),
        "b": node_json(pair.node_b),
        "signals": {
            "ast_match": pair.fp_a.ast_hash == pair.fp_b.ast_hash,
            "cfg_match": pair.fp_a.cfg_hash == pair.fp_b.cfg_hash,
            "call_seq_match": pair.fp_a.call_seq_hash == pair.fp_b.call_seq_hash,
            "shingle_jaccard": round4(pair.score.shingle_jaccard),
            "body_vector_cosine": round4(pair.score.vector_cosine),
            "generic_helper_downranked": pair.score.generic_helper_downranked,
            "body_tokens": [pair.fp_a.body_tokens, pair.fp_b.body_tokens],
        },
    })
}

fn node_json(node: &RedundancyCandidate) -> Value {
    json!({
        "file": node.file_path,
        "line": node.start_line,
        "name": node.name,
        "id": node.id,
    })
}

fn duplicate_groups(groups: &[Vec<&RedundancyCandidate>]) -> Vec<Value> {
    groups
        .iter()
        .map(|nodes| {
            json!({
                "size": nodes.len(),
                "nodes": nodes.iter().map(|n| node_json(n)).collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::Value;

    use super::{
        RedundancyCandidate, RedundancyOptions, RedundancyPairScan, RedundantPair, SemanticPair,
        augment_redundancy_output, canonical_pair_ids, compute_fingerprints, connected_node_groups,
        find_redundant_pairs, is_generated_path, nodes_overlap, redundancy_output, semantic_cosine,
        semantic_pairs,
    };
    use crate::application::semantic_runtime::{
        SemanticRedundancyGenerationV1, SemanticRedundancyProfileV1, SemanticRedundancyVectorV1,
    };
    use crate::redundancy::{Fingerprint, RedundancyMatchScore};
    use tracedecay_domain::SourceSpan;

    #[test]
    fn generated_paths_are_excluded_from_candidates_by_default() {
        for path in [
            "dashboard/lcm/dist/index.js",
            "node_modules/lib/index.js",
            ".worktrees/feature/src/lib.rs",
            "vendor/sqlite/src/lib.rs",
            "assets/app.min.js",
        ] {
            assert!(is_generated_path(path), "{path} should count as generated");
        }
        // Segment matching must not catch prefixes of real source dirs.
        for path in [
            "src/redundancy.rs",
            "src/distributed/mod.rs",
            "builder/mod.rs",
        ] {
            assert!(!is_generated_path(path), "{path} is real source");
        }
    }

    #[test]
    fn generated_paths_gain_segments_from_the_shared_list() {
        // These segments weren't in this file's old standalone list but are
        // part of the shared GENERATED_DIR_SEGMENTS union that scan.rs and
        // migrate::inventory already recognized — closing this drift is the
        // point of routing through crate::config::is_generated_path_segment.
        for path in [
            "packages/web/coverage/lcov.info",
            "env/.venv/pyvenv.cfg",
            "apps/site/.next/server/app.js",
            "tool/.cache/entry",
            "repo/.turbo/cache",
            "android/.gradle/wrapper",
            "scripts/venv/bin/python",
            "assets/app.min.css",
        ] {
            assert!(
                is_generated_path(path),
                "{path} should now count as generated"
            );
        }
    }

    pub(super) fn test_node(id: &str, name: &str, line: u32) -> RedundancyCandidate {
        RedundancyCandidate {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: line,
            end_line: line + 10,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        }
    }

    fn test_fingerprint(body_tokens: usize) -> Fingerprint {
        Fingerprint {
            ast_hash: "ast".into(),
            cfg_hash: "cfg".into(),
            call_seq_hash: "call".into(),
            shingles: vec![1, 2, 3],
            body_tokens,
            source_hash: "src".into(),
        }
    }

    fn test_score(ranking_score: f64) -> RedundancyMatchScore {
        RedundancyMatchScore {
            similarity: 0.9,
            ranking_score,
            vector_cosine: 0.8,
            shingle_jaccard: 0.7,
            overlap_kind: "body_vector",
            severity: "high",
            generic_helper_downranked: false,
        }
    }

    #[test]
    fn disabled_semantics_preserves_structural_output_bytes_and_order() {
        let nodes = vec![
            test_node("id_a", "alpha", 10),
            test_node("id_b", "beta", 20),
            test_node("id_c", "gamma", 30),
        ];
        let fa = test_fingerprint(50);
        let fb = test_fingerprint(52);
        let fc = test_fingerprint(54);
        let pairs = vec![
            RedundantPair {
                score: test_score(0.95),
                node_a: &nodes[0],
                node_b: &nodes[1],
                fp_a: &fa,
                fp_b: &fb,
            },
            RedundantPair {
                score: test_score(0.85),
                node_a: &nodes[1],
                node_b: &nodes[2],
                fp_a: &fb,
                fp_b: &fc,
            },
        ];
        let options = RedundancyOptions {
            path_prefix: None,
            min_lines: 8,
            max_pairs: 20,
            threshold: 0.6,
            include_naming: false,
            include_generated: false,
        };
        let groups = connected_node_groups(&pairs);
        let baseline = redundancy_output(&options, 3, 3, &pairs, &groups);

        let augmented = augment_redundancy_output(&options, 3, 3, &nodes, &pairs, &groups, None);

        assert_eq!(
            serde_json::to_vec(&augmented).unwrap(),
            serde_json::to_vec(&baseline).unwrap()
        );
    }

    #[test]
    fn active_generation_classifies_semantic_only_pair_as_analogue() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/redundancy_eval_labeled.json"
        ))
        .unwrap();
        let labelled = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["label"] == "vector_rescue_renamed")
            .unwrap();
        let left_name = labelled["a_name"].as_str().unwrap();
        let right_name = labelled["b_name"].as_str().unwrap();
        let nodes = vec![
            candidate_node("left-id", left_name, "src/spans.rs", 8),
            candidate_node("right-id", right_name, "src/ranges.rs", 8),
            candidate_node("render-id", "render_page", "src/view.rs", 8),
        ];
        let generation = SemanticRedundancyGenerationV1 {
            vector_generation: "sha256:vector-generation".to_owned(),
            source_generation: "sha256:source-generation".to_owned(),
            projection_key: "sha256:projection-key".to_owned(),
            profile: SemanticRedundancyProfileV1 {
                scope_digest: "sha256:scope".to_owned(),
                accepted_profile_digest: "sha256:accepted-profile".to_owned(),
                calibration_profile_id: "calibration.semantic.v1".to_owned(),
                calibration_digest: "sha256:calibration".to_owned(),
                redundancy_profile_digest: "sha256:redundancy-profile".to_owned(),
                maximum_distance_micros: 100_000_000,
            },
            vectors: vec![
                SemanticRedundancyVectorV1 {
                    file_path: "src/spans.rs".to_owned(),
                    qualified_name: left_name.to_owned(),
                    values: vec![1.0, 0.0],
                },
                SemanticRedundancyVectorV1 {
                    file_path: "src/ranges.rs".to_owned(),
                    qualified_name: right_name.to_owned(),
                    values: vec![0.99, 0.01],
                },
                SemanticRedundancyVectorV1 {
                    file_path: "src/view.rs".to_owned(),
                    qualified_name: "render_page".to_owned(),
                    values: vec![0.0, 1.0],
                },
            ],
        };
        let options = RedundancyOptions {
            path_prefix: None,
            min_lines: 8,
            max_pairs: 20,
            threshold: 0.9,
            include_naming: false,
            include_generated: false,
        };

        let output = augment_redundancy_output(&options, 3, 0, &nodes, &[], &[], Some(&generation));

        assert_eq!(output["pair_count"], 1);
        assert_eq!(output["pairs"][0]["classification"], "semantic_analogue");
        assert_eq!(output["pairs"][0]["a"]["id"], "left-id");
        assert_eq!(output["pairs"][0]["b"]["id"], "right-id");
        assert_eq!(
            output["semantic_generation"]["vector_generation"],
            "sha256:vector-generation"
        );
        assert_eq!(
            output["semantic_generation"]["redundancy_profile_digest"],
            "sha256:redundancy-profile"
        );
    }

    #[test]
    fn structural_threshold_cannot_admit_uncalibrated_semantic_pair() {
        let nodes = vec![
            candidate_node("left-id", "left", "src/left.rs", 8),
            candidate_node("right-id", "right", "src/right.rs", 8),
        ];
        let generation = SemanticRedundancyGenerationV1 {
            vector_generation: "sha256:vector-generation".to_owned(),
            source_generation: "sha256:source-generation".to_owned(),
            projection_key: "sha256:projection-key".to_owned(),
            profile: SemanticRedundancyProfileV1 {
                scope_digest: "sha256:scope".to_owned(),
                accepted_profile_digest: "sha256:accepted-profile".to_owned(),
                calibration_profile_id: "calibration.semantic.v1".to_owned(),
                calibration_digest: "sha256:calibration".to_owned(),
                redundancy_profile_digest: "sha256:redundancy-profile".to_owned(),
                maximum_distance_micros: 100_000_000,
            },
            vectors: vec![
                SemanticRedundancyVectorV1 {
                    file_path: "src/left.rs".to_owned(),
                    qualified_name: "left".to_owned(),
                    values: vec![1.0, 0.0],
                },
                SemanticRedundancyVectorV1 {
                    file_path: "src/right.rs".to_owned(),
                    qualified_name: "right".to_owned(),
                    values: vec![0.8, 0.6],
                },
            ],
        };
        let options = RedundancyOptions {
            path_prefix: None,
            min_lines: 8,
            max_pairs: 20,
            threshold: 0.5,
            include_naming: false,
            include_generated: false,
        };

        let output = augment_redundancy_output(&options, 2, 0, &nodes, &[], &[], Some(&generation));

        assert_eq!(output["pair_count"], 0);
    }

    // --- DEFECT A bucketing parity harness ----------------------------------

    fn vec_row(file: &str, name: &str, values: Vec<f32>) -> SemanticRedundancyVectorV1 {
        SemanticRedundancyVectorV1 {
            file_path: file.to_owned(),
            qualified_name: name.to_owned(),
            values,
        }
    }

    fn generation_with(
        vectors: Vec<SemanticRedundancyVectorV1>,
        maximum_distance_micros: i64,
    ) -> SemanticRedundancyGenerationV1 {
        SemanticRedundancyGenerationV1 {
            vector_generation: "sha256:vector-generation".to_owned(),
            source_generation: "sha256:source-generation".to_owned(),
            projection_key: "sha256:projection-key".to_owned(),
            profile: SemanticRedundancyProfileV1 {
                scope_digest: "sha256:scope".to_owned(),
                accepted_profile_digest: "sha256:accepted-profile".to_owned(),
                calibration_profile_id: "calibration.semantic.v1".to_owned(),
                calibration_digest: "sha256:calibration".to_owned(),
                redundancy_profile_digest: "sha256:redundancy-profile".to_owned(),
                maximum_distance_micros,
            },
            vectors,
        }
    }

    /// Project accepted pairs to sorted `(id_a, id_b, distance)` triples so the
    /// comparison against the brute-force oracle is a pure set equality,
    /// independent of the ranked emission order.
    fn projected(pairs: &[SemanticPair<'_>]) -> Vec<(String, String, i64)> {
        let mut projected = pairs
            .iter()
            .map(|pair| {
                (
                    pair.node_a.id.clone(),
                    pair.node_b.id.clone(),
                    pair.distance_micros,
                )
            })
            .collect::<Vec<_>>();
        projected.sort();
        projected
    }

    /// The original un-bucketed all-pairs scan, kept verbatim as an independent
    /// oracle for the bucketed implementation. Returns the accepted pair set as
    /// canonically-oriented `(id_a, id_b, distance)` triples, sorted so set
    /// equality is a plain vector comparison.
    fn brute_force_semantic_pairs(
        nodes: &[RedundancyCandidate],
        structural: &[RedundantPair<'_>],
        semantic: &SemanticRedundancyGenerationV1,
    ) -> Vec<(String, String, i64)> {
        let mut vectors_by_node: std::collections::HashMap<&str, Vec<&[f32]>> =
            std::collections::HashMap::new();
        for vector in &semantic.vectors {
            let mut matches = nodes.iter().filter(|node| {
                node.file_path == vector.file_path
                    && (node.qualified_name == vector.qualified_name
                        || node.name == vector.qualified_name)
            });
            let Some(node) = matches.next() else {
                continue;
            };
            if matches.next().is_some() {
                continue;
            }
            vectors_by_node
                .entry(node.id.as_str())
                .or_default()
                .push(&vector.values);
        }
        let structural_pairs = structural
            .iter()
            .map(|pair| canonical_pair_ids(&pair.node_a.id, &pair.node_b.id))
            .collect::<std::collections::BTreeSet<_>>();
        let mut found = Vec::new();
        for (index, node_a) in nodes.iter().enumerate() {
            let Some(vectors_a) = vectors_by_node.get(node_a.id.as_str()) else {
                continue;
            };
            for node_b in nodes.iter().skip(index + 1) {
                if nodes_overlap(node_a, node_b)
                    || structural_pairs.contains(&canonical_pair_ids(&node_a.id, &node_b.id))
                {
                    continue;
                }
                let Some(vectors_b) = vectors_by_node.get(node_b.id.as_str()) else {
                    continue;
                };
                let cosine = vectors_a
                    .iter()
                    .flat_map(|left| {
                        vectors_b
                            .iter()
                            .filter_map(move |right| semantic_cosine(left, right))
                    })
                    .max_by(f64::total_cmp);
                if let Some((_, distance)) =
                    cosine.and_then(|cosine| semantic.profile.accepts(cosine).map(|d| (cosine, d)))
                {
                    let (a, b) = canonical_pair_ids(&node_a.id, &node_b.id);
                    found.push((a.to_owned(), b.to_owned(), distance));
                }
            }
        }
        found.sort();
        found
    }

    #[test]
    fn bucketed_semantic_pairs_match_brute_force_pair_set() {
        // Known duplicate pairs, orthogonal non-duplicates, an ambiguous-name
        // vector that must drop, and projection keys spread far enough that the
        // sliding window actively prunes cross-cluster comparisons.
        let nodes = vec![
            candidate_node("dup-a", "encode_span", "src/a.rs", 8),
            candidate_node("dup-b", "encode_range", "src/b.rs", 8),
            candidate_node("far-a", "render_html", "src/c.rs", 8),
            candidate_node("far-b", "parse_json", "src/d.rs", 8),
            candidate_node("mid-a", "hash_key", "src/e.rs", 8),
            candidate_node("mid-b", "digest_key", "src/f.rs", 8),
            candidate_node("amb-1", "shared", "src/g.rs", 8),
            candidate_node("amb-2", "shared", "src/g.rs", 8),
        ];
        let generation = generation_with(
            vec![
                vec_row("src/a.rs", "encode_span", vec![1.0, 0.02, 0.0]),
                vec_row("src/b.rs", "encode_range", vec![0.999, 0.0, 0.02]),
                vec_row("src/c.rs", "render_html", vec![0.0, 1.0, 0.0]),
                vec_row("src/d.rs", "parse_json", vec![0.0, 0.0, 1.0]),
                vec_row("src/e.rs", "hash_key", vec![0.6, 0.8, 0.0]),
                vec_row("src/f.rs", "digest_key", vec![0.61, 0.79, 0.0]),
                // Two distinct nodes in src/g.rs share this name → ambiguous,
                // the vector must bind to neither.
                vec_row("src/g.rs", "shared", vec![1.0, 0.0, 0.0]),
            ],
            60_000_000,
        );

        let bucketed = semantic_pairs(&nodes, &[], &generation);
        let brute = brute_force_semantic_pairs(&nodes, &[], &generation);

        // Perfect-recall proof: the bucketed pass reports exactly the oracle's
        // accepted pair set even though the window prunes the mid↔dup and
        // far↔mid comparisons.
        assert_eq!(projected(&bucketed), brute);

        let ids: Vec<(&str, &str)> = bucketed
            .iter()
            .map(|pair| (pair.node_a.id.as_str(), pair.node_b.id.as_str()))
            .collect();
        // The known duplicate pairs are found...
        assert!(ids.contains(&("dup-a", "dup-b")));
        assert!(ids.contains(&("mid-a", "mid-b")));
        // ...and the orthogonal non-duplicates are excluded, as is the
        // ambiguous-name node.
        assert!(!ids.iter().any(|(a, b)| *a == "far-a" || *b == "far-a"));
        assert!(!ids.iter().any(|(a, b)| *a == "far-b" || *b == "far-b"));
        assert!(
            !ids.iter()
                .any(|(a, b)| a.starts_with("amb") || b.starts_with("amb"))
        );
    }

    #[test]
    fn bucketed_semantic_pairs_find_high_cosine_pair_spread_on_first_coordinate() {
        // Guard against bucketing on the wrong signal: a genuinely similar pair
        // whose vectors differ most on the projected coordinate must still be
        // found. Their normalized first coordinates differ (0.196 vs 0.290) yet
        // the pair is well within the window, and cosine ≈ 0.997 is accepted.
        let nodes = vec![
            candidate_node("near-a", "alpha", "src/a.rs", 8),
            candidate_node("near-b", "beta", "src/b.rs", 8),
        ];
        let generation = generation_with(
            vec![
                vec_row("src/a.rs", "alpha", vec![0.2, 1.0, 0.0]),
                vec_row("src/b.rs", "beta", vec![0.3, 1.0, 0.0]),
            ],
            60_000_000,
        );

        let bucketed = semantic_pairs(&nodes, &[], &generation);
        let brute = brute_force_semantic_pairs(&nodes, &[], &generation);
        assert_eq!(projected(&bucketed), brute);
        assert_eq!(bucketed.len(), 1);
        assert_eq!(bucketed[0].node_a.id, "near-a");
        assert_eq!(bucketed[0].node_b.id, "near-b");
    }

    #[test]
    fn cosine_projection_window_bounds_every_accepted_pair() {
        // Randomized recall stress: many vectored nodes, each pair independently
        // checked so any window that dropped an accepted pair would surface as a
        // brute-force mismatch.
        let mut nodes = Vec::new();
        let mut vectors = Vec::new();
        // Deterministic pseudo-random vectors from a linear congruential
        // sequence so the fixture is reproducible without external crates.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as f64 / (1u64 << 31) as f64) - 1.0
        };
        for index in 0..40 {
            let file = format!("src/n{index}.rs");
            let name = format!("fn_{index}");
            let id = format!("id-{index}");
            nodes.push(candidate_node(&id, &name, &file, 8));
            let values = vec![next() as f32, next() as f32, next() as f32, next() as f32];
            vectors.push(vec_row(&file, &name, values));
        }
        // A moderately permissive window so a non-trivial number of pairs accept.
        let generation = generation_with(vectors, 250_000_000);

        let bucketed = semantic_pairs(&nodes, &[], &generation);
        let brute = brute_force_semantic_pairs(&nodes, &[], &generation);
        assert_eq!(projected(&bucketed), brute);
        assert!(!brute.is_empty(), "fixture must exercise accepted pairs");
    }

    /// Synthetic candidates spread across overlapping `body_tokens` windows so
    /// the pairwise scan visits partial windows, full windows, rejected pairs
    /// and accepted pairs — the shapes a slice boundary could disturb.
    fn paced_scan_candidates() -> (Vec<RedundancyCandidate>, Vec<Fingerprint>) {
        let mut nodes = Vec::new();
        let mut fingerprints = Vec::new();
        for index in 0..40u32 {
            let mut node = test_node(&format!("node-{index:02}"), "candidate", index);
            node.file_path = format!("src/file-{index:02}.rs");
            nodes.push(node);
            // Token counts drift slowly so each candidate's ±25 % window covers
            // a different, overlapping run of neighbours.
            let body_tokens = 40 + usize::try_from(index).unwrap();
            // Every third candidate shares a shingle set (and hashes) with its
            // neighbours, so the scan yields real accepted pairs, not an empty
            // result that would make the comparison vacuous.
            let family = index % 3;
            let shingles: Vec<u32> = (0..24u32).map(|slot| family * 1_000 + slot).collect();
            fingerprints.push(Fingerprint {
                ast_hash: format!("ast-{family}"),
                cfg_hash: format!("cfg-{family}"),
                call_seq_hash: format!("call-{family}"),
                shingles,
                body_tokens,
                source_hash: format!("source-{index:02}"),
            });
        }
        (nodes, fingerprints)
    }

    fn paced_scan_scoped<'a>(
        nodes: &'a [RedundancyCandidate],
        fingerprints: &'a [Fingerprint],
    ) -> Vec<(&'a RedundancyCandidate, &'a Fingerprint)> {
        nodes.iter().zip(fingerprints.iter()).collect()
    }

    fn pair_identity(pairs: &[RedundantPair<'_>]) -> Vec<(String, String, u64, u64)> {
        pairs
            .iter()
            .map(|pair| {
                (
                    pair.node_a.id.clone(),
                    pair.node_b.id.clone(),
                    pair.score.ranking_score.to_bits(),
                    pair.score.similarity.to_bits(),
                )
            })
            .collect()
    }

    /// The slice cursor is a pacing device only: driving the scan one
    /// comparison at a time must return the same pairs, in the same order,
    /// with bit-identical scores as the single-shot scan.
    #[test]
    fn sliced_pair_scan_matches_the_single_shot_scan_exactly() {
        let (nodes, fingerprints) = paced_scan_candidates();
        let single_shot = find_redundant_pairs(
            paced_scan_scoped(&nodes, &fingerprints),
            0.6,
            false,
            usize::MAX,
        );
        assert!(
            !single_shot.is_empty(),
            "fixture must produce pairs or the comparison proves nothing"
        );

        for budget in [1usize, 2, 7, 64, 4096] {
            let mut scan = RedundancyPairScan::new(
                paced_scan_scoped(&nodes, &fingerprints),
                0.6,
                false,
                usize::MAX,
            );
            let mut slices = 0usize;
            while scan.advance(budget) {
                slices += 1;
                assert!(slices < 1_000_000, "slice cursor failed to make progress");
            }
            assert_eq!(
                pair_identity(&scan.finish()),
                pair_identity(&single_shot),
                "slice budget {budget} changed the scan result"
            );
        }
    }

    /// `max_pairs` truncation happens after ranking, so it must survive pacing
    /// identically too.
    #[test]
    fn sliced_pair_scan_truncates_like_the_single_shot_scan() {
        let (nodes, fingerprints) = paced_scan_candidates();
        let single_shot =
            find_redundant_pairs(paced_scan_scoped(&nodes, &fingerprints), 0.6, false, 5);
        let mut scan =
            RedundancyPairScan::new(paced_scan_scoped(&nodes, &fingerprints), 0.6, false, 5);
        while scan.advance(3) {}
        assert_eq!(pair_identity(&scan.finish()), pair_identity(&single_shot));
        assert_eq!(single_shot.len(), 5);
    }

    fn candidate_node(id: &str, name: &str, file_path: &str, end_line: u32) -> RedundancyCandidate {
        let mut node = test_node(id, name, 0);
        node.file_path = file_path.to_string();
        node.end_line = end_line;
        node
    }

    #[test]
    fn fresh_fingerprint_scan_parses_each_file_once() {
        if tracedecay_code_extraction::ts_provider::language("rust").is_err() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let src_dir = temp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let alpha = "fn alpha(input: i32) -> i32 {\n    let mut total = input;\n    for value in 0..4 {\n        if value % 2 == 0 {\n            total += value;\n        }\n    }\n    total\n}\n";
        let beta = "fn beta(input: i32) -> i32 {\n    let mut total = input;\n    for value in 0..4 {\n        if value % 2 == 0 {\n            total += value;\n        }\n    }\n    total\n}\n";
        std::fs::write(src_dir.join("alpha.rs"), alpha).unwrap();
        std::fs::write(src_dir.join("beta.rs"), beta).unwrap();
        let mut nodes = vec![
            candidate_node("alpha-id", "alpha", "src/alpha.rs", 8),
            candidate_node("beta-id", "beta", "src/beta.rs", 8),
        ];
        nodes[0].source_span.end_byte = alpha.trim_end().len() as u64;
        nodes[1].source_span.end_byte = beta.trim_end().len() as u64;

        let load = compute_fingerprints(temp.path(), &nodes).unwrap();
        assert_eq!(load.parsed_files, 2);
        assert_eq!(load.computed_fingerprints, 2);
        assert_eq!(load.fingerprints.len(), 2);
        assert_eq!(load.fingerprints["alpha-id"].source_hash.len(), 16);
        assert_eq!(load.fingerprints["beta-id"].source_hash.len(), 16);
    }

    #[test]
    fn unsupported_candidates_fail_with_typed_unavailable_state() {
        let nodes = vec![candidate_node(
            "node-id",
            "candidate",
            "src/file.unsupported",
            8,
        )];
        let error = compute_fingerprints(std::path::Path::new("/unused"), &nodes).unwrap_err();
        assert!(error.to_string().contains("no registered extractor"));
    }
}

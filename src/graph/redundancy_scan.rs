//! The AST-level functional-duplicate scan: candidate selection, fingerprint
//! caching, pairwise ranking, and the structured payload built from them.
//!
//! This lives below the MCP handler tree. The `tracedecay_redundancy` handler
//! renders it, and the root engine's `GraphRuntimePort` decodes the same
//! payload typed — neither layer depends on the other.
//!
//! Pipeline:
//!
//! 1. Pull all `Function` / `Method` nodes (optionally path-filtered).
//! 2. Group by file. Open each file once, parse with tree-sitter,
//!    locate every target node via its `(start_line, end_line)`, and
//!    compute a [`Fingerprint`](crate::redundancy::Fingerprint). Cache
//!    the result keyed on `(node_id, body source hash)` so we don't pay
//!    re-parse cost on subsequent calls when the file hasn't changed.
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
use std::fmt::Write as _;
use std::future::Future;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::application::semantic_runtime::{
    SemanticRedundancyGenerationV1, project_semantic_redundancy_generation,
};
use crate::errors::Result;
use crate::redundancy::{
    Fingerprint, RedundantPair, compute_fingerprint, connected_node_groups, find_node_at_lines,
    find_redundant_pairs, parse_file, round4,
};
use crate::tracedecay::TraceDecay;
use crate::types::{Node, NodeKind};

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
pub(crate) struct RedundancyPairViewV1 {
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

/// Run the full redundancy pipeline for `options`.
pub(crate) async fn redundancy_scan(
    cg: &TraceDecay,
    options: &RedundancyOptions<'_>,
) -> Result<RedundancyScanV1> {
    // 1. Collect candidate function nodes.
    let nodes = collect_candidates(
        cg,
        options.path_prefix,
        options.min_lines,
        options.include_generated,
    )
    .await?;
    let total_candidates = nodes.len();

    // 2. Ensure each has a fresh fingerprint in memory (cache by source hash).
    // File I/O and tree-sitter parsing stay outside the database writer lane.
    let fingerprints = ensure_fingerprints(cg, &nodes).await?;
    let scanned = fingerprints.len();

    // 3. Bucket by token count to keep pairwise comparison sub-quadratic.
    let scoped = scoped_fingerprints(&nodes, &fingerprints);
    let pairs = find_redundant_pairs(
        scoped,
        options.threshold,
        options.include_naming,
        options.max_pairs,
    );

    // Persist the ranked pairs as a freshness-validated cache so other
    // surfaces (diagnose near-duplicate enrichment, the dashboard, future
    // tools) can read the last-known duplicates without recomputing. Best
    // effort: a write failure never fails the query.
    persist_redundancy_cache(cg, &fingerprints, &pairs).await;

    // Connected components are the shared source of truth for the JSON `groups`
    // array and the markdown Groups section; compute them once and thread the
    // result into both so the two views can never diverge and the O(pairs²)
    // grouping runs a single time per call.
    let groups = connected_node_groups(&pairs);
    let semantic = project_semantic_redundancy_generation(cg.project_root(), cg.db()).await;
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
fn node_label(node: &Node) -> String {
    format!("{} ({}:{})", node.name, node.file_path, node.start_line)
}

pub(crate) fn pair_views(pairs: &[RedundantPair<'_>]) -> Vec<RedundancyPairViewV1> {
    pairs
        .iter()
        .map(|pair| RedundancyPairViewV1 {
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

pub(crate) fn group_views(groups: &[Vec<&Node>]) -> Vec<Vec<String>> {
    groups
        .iter()
        .map(|group| group.iter().map(|node| node_label(node)).collect())
        .collect()
}

#[derive(Clone, Copy)]
struct SemanticPair<'a> {
    node_a: &'a Node,
    node_b: &'a Node,
    cosine: f64,
    distance_micros: i64,
}

fn augment_redundancy_output(
    options: &RedundancyOptions<'_>,
    total_candidates: usize,
    scanned: usize,
    nodes: &[Node],
    pairs: &[RedundantPair<'_>],
    groups: &[Vec<&Node>],
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
    node: &'a Node,
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
    nodes: &'a [Node],
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

fn nodes_overlap(left: &Node, right: &Node) -> bool {
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

fn connected_rendered_groups<'a>(pairs: &[Value], nodes: &'a [Node]) -> Vec<Vec<&'a Node>> {
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
    groups: &[Vec<&Node>],
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

async fn collect_candidates(
    cg: &TraceDecay,
    path_prefix: Option<&str>,
    min_lines: u32,
    include_generated: bool,
) -> Result<Vec<Node>> {
    let all = cg.get_all_nodes().await?;
    Ok(all
        .into_iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
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
// 2. Fingerprint computation + caching
// ---------------------------------------------------------------------------

/// Returns a map from `node_id` to its fingerprint. Reuses any cached row
/// whose stored `source_hash` matches the live file content for that
/// node's body; otherwise re-parses the file once, computes fingerprints
/// for all candidate nodes in that file, and persists them.
async fn ensure_fingerprints(
    cg: &TraceDecay,
    candidates: &[Node],
) -> Result<HashMap<String, Fingerprint>> {
    let project_root = cg.project_root().to_path_buf();
    let load = ensure_fingerprints_with_loader(&project_root, candidates, |node_ids| async move {
        cg.db().get_fingerprints(&node_ids).await
    })
    .await?;
    Ok(load.fingerprints)
}

struct FingerprintLoad {
    fingerprints: HashMap<String, Fingerprint>,
    // Plan 33 item 4 acceptance instrumentation: read by cfg(test)
    // cold/partial/warm call-count parity assertions.
    #[allow(dead_code)]
    parsed_files: usize,
    #[allow(dead_code)]
    computed_fingerprints: usize,
}

async fn ensure_fingerprints_with_loader<Load, LoadFuture>(
    project_root: &Path,
    candidates: &[Node],
    load_cached: Load,
) -> Result<FingerprintLoad>
where
    Load: FnOnce(Vec<String>) -> LoadFuture,
    LoadFuture: Future<Output = Result<Vec<crate::db::StoredFingerprint>>>,
{
    let registry = tracedecay_code_extraction::LanguageRegistry::new();
    let node_ids = candidates.iter().map(|node| node.id.clone()).collect();
    let mut cached_by_id: HashMap<String, Fingerprint> = load_cached(node_ids)
        .await?
        .into_iter()
        .map(|stored| (stored.node_id.clone(), stored.into()))
        .collect();

    // Group candidates by file so we parse each file at most once.
    let mut by_file: HashMap<String, Vec<&Node>> = HashMap::new();
    for n in candidates {
        by_file.entry(n.file_path.clone()).or_default().push(n);
    }

    let mut out: HashMap<String, Fingerprint> = HashMap::new();
    let mut parsed_files = 0usize;
    let mut computed_fingerprints = 0usize;

    for (file_path, file_nodes) in by_file {
        // Figure out which tree-sitter language this file maps to.
        let Some(extractor) = registry.extractor_for_file(&file_path) else {
            continue;
        };
        let lang_key = extractor_to_language_key(extractor.language_name());
        let Some(lang_key) = lang_key else {
            continue;
        };

        // Read the file contents. Silently skip on read failure (the file
        // may have been deleted between sync and this call).
        let abs = project_root.join(&file_path);
        let Ok(source) = std::fs::read_to_string(&abs) else {
            continue;
        };

        // Cheap path: every cached fingerprint whose source_hash matches
        // the current body content is reusable without re-parsing.
        let mut misses = Vec::new();
        for node in &file_nodes {
            let body = node_body_slice(&source, node);
            let expected_hash = quick_body_hash(body);
            match cached_by_id.remove(&node.id) {
                Some(cached) if cached.source_hash == expected_hash => {
                    out.insert(node.id.clone(), cached);
                }
                _ => misses.push(*node),
            }
        }

        if misses.is_empty() {
            continue;
        }

        // At least one node in this file needs a fresh fingerprint —
        // parse once and compute for every miss.
        let Ok(language) = tracedecay_code_extraction::ts_provider::language(lang_key) else {
            continue;
        };
        let Some(tree) = parse_file(&source, &language) else {
            continue;
        };
        parsed_files += 1;

        for node in misses {
            // Node.start_line / end_line are stored as raw tree-sitter
            // row indices (0-based) — see info::extract_lines docs.
            let Some(ts_node) = find_node_at_lines(&tree, node.start_line, node.end_line) else {
                continue;
            };
            out.insert(node.id.clone(), compute_fingerprint(&source, ts_node));
            computed_fingerprints += 1;
        }
    }

    Ok(FingerprintLoad {
        fingerprints: out,
        parsed_files,
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

/// Extract the inclusive 0-indexed line range from `source` as a borrowed
/// slice. Node `start_line` / `end_line` are stored as raw tree-sitter
/// row indices (see `info::extract_lines`).
fn body_slice(source: &str, start_line: u32, end_line: u32) -> &str {
    line_byte_range(source, start_line, end_line).map_or("", |range| &source[range])
}

/// Extract the exact tree-sitter node byte span, using its 0-indexed rows and
/// byte columns. Unlike [`body_slice`], this excludes indentation before the
/// node and the newline after it, matching the bytes hashed by
/// [`compute_fingerprint`].
fn node_body_slice<'a>(source: &'a str, node: &Node) -> &'a str {
    let lines = body_slice(source, node.start_line, node.end_line);
    if lines.is_empty() {
        return "";
    }
    let start = node.start_column as usize;
    let line_span = node.end_line.saturating_sub(node.start_line) as usize;
    let end_line_start = if line_span == 0 {
        0
    } else {
        let Some((offset, _)) = lines.match_indices('\n').nth(line_span - 1) else {
            return "";
        };
        offset + 1
    };
    let end = end_line_start.saturating_add(node.end_column as usize);
    lines.get(start..end).unwrap_or("")
}

fn line_byte_range(source: &str, start_line: u32, end_line: u32) -> Option<std::ops::Range<usize>> {
    let start = start_line as usize;
    let end = (end_line as usize).saturating_add(1);
    let mut offset = 0usize;
    let mut start_byte: Option<usize> = None;
    let mut end_byte: usize = source.len();
    for (i, line) in source.split_inclusive('\n').enumerate() {
        if i == start {
            start_byte = Some(offset);
        }
        if i + 1 == end {
            end_byte = offset + line.len();
            break;
        }
        offset += line.len();
    }
    let s = start_byte?;
    if end_byte <= s || end_byte > source.len() {
        return None;
    }
    Some(s..end_byte)
}

/// Cheap body hash used for cache invalidation. Matches the format used
/// by `compute_fingerprint` (first 8 bytes of SHA-256, hex-encoded).
fn quick_body_hash(body: &str) -> String {
    let mut h = Sha256::new();
    h.update(body.as_bytes());
    let d = h.finalize();
    let mut s = String::with_capacity(16);
    for b in d.iter().take(8) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---------------------------------------------------------------------------
// 3. Pairwise comparison + ranking
// ---------------------------------------------------------------------------

type ScopedFingerprint<'a> = (&'a Node, &'a Fingerprint);

fn scoped_fingerprints<'a>(
    nodes: &'a [Node],
    fingerprints: &'a HashMap<String, Fingerprint>,
) -> Vec<ScopedFingerprint<'a>> {
    nodes
        .iter()
        .filter_map(|n| fingerprints.get(&n.id).map(|fp| (n, fp)))
        .collect()
}

/// Upsert the returned duplicate pairs into the `redundancy_pairs` cache.
///
/// Each pair is stored in its canonical `(node_a, node_b)` orientation with
/// both `source_hash`es so a reader can validate freshness against the live
/// fingerprint cache. Errors are logged but never fatal — the redundancy query
/// still returns results even if the cache write fails. Node-id orphan cleanup
/// is handled by the table's `ON DELETE CASCADE`, so full-project runs need no
/// explicit deletion pass here.
async fn persist_redundancy_cache(
    cg: &TraceDecay,
    fingerprints: &HashMap<String, Fingerprint>,
    pairs: &[RedundantPair<'_>],
) {
    let mut fingerprint_rows: Vec<_> = fingerprints
        .iter()
        .map(|(node_id, fingerprint)| (node_id.as_str(), fingerprint))
        .collect();
    fingerprint_rows.sort_unstable_by(|left, right| left.0.cmp(right.0));
    let computed_at = crate::tracedecay::current_timestamp();
    let rows: Vec<crate::db::RedundancyPairWrite<'_>> = pairs
        .iter()
        .map(|pair| crate::db::RedundancyPairWrite {
            node_a_id: pair.node_a.id.as_str(),
            node_b_id: pair.node_b.id.as_str(),
            source_hash_a: pair.fp_a.source_hash.as_str(),
            source_hash_b: pair.fp_b.source_hash.as_str(),
            ranking_score: pair.score.ranking_score,
            similarity: pair.score.similarity,
            vector_cosine: pair.score.vector_cosine,
            overlap_kind: pair.score.overlap_kind,
            severity: pair.score.severity,
            generic_helper_downranked: pair.score.generic_helper_downranked,
            computed_at,
        })
        .collect();
    if let Err(e) = cg
        .db()
        .publish_redundancy_cache(&fingerprint_rows, &rows)
        .await
    {
        tracing::warn!(error = %e, "atomic redundancy cache publication failed");
    }
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

fn node_json(node: &Node) -> Value {
    json!({
        "file": node.file_path,
        "line": node.start_line,
        "name": node.name,
        "id": node.id,
    })
}

fn duplicate_groups(groups: &[Vec<&Node>]) -> Vec<Value> {
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::Value;

    use super::{
        RedundancyOptions, SemanticPair, augment_redundancy_output, body_slice, canonical_pair_ids,
        ensure_fingerprints_with_loader, is_generated_path, node_body_slice, nodes_overlap,
        redundancy_output, scoped_fingerprints, semantic_cosine, semantic_pairs,
    };
    use crate::application::semantic_runtime::{
        SemanticRedundancyGenerationV1, SemanticRedundancyProfileV1, SemanticRedundancyVectorV1,
    };
    use crate::db::StoredFingerprint;
    use crate::redundancy::{
        Fingerprint, RedundancyMatchScore, RedundantPair, connected_node_groups,
        find_redundant_pairs,
    };
    use crate::types::{Node, NodeKind, Visibility};

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

    pub(super) fn test_node(id: &str, name: &str, line: u32) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: line,
            attrs_start_line: line,
            end_line: line + 10,
            start_column: 0,
            end_column: 0,
            signature: None,
            docstring: None,
            visibility: Visibility::default(),
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 0,
            parent_id: None,
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
        nodes: &[Node],
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
                if let Some((_, distance)) = cosine
                    .and_then(|cosine| semantic.profile.accepts(cosine).map(|d| (cosine, d)))
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
        assert!(!ids.iter().any(|(a, b)| a.starts_with("amb") || b.starts_with("amb")));
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
            let values = vec![
                next() as f32,
                next() as f32,
                next() as f32,
                next() as f32,
            ];
            vectors.push(vec_row(&file, &name, values));
        }
        // A moderately permissive window so a non-trivial number of pairs accept.
        let generation = generation_with(vectors, 250_000_000);

        let bucketed = semantic_pairs(&nodes, &[], &generation);
        let brute = brute_force_semantic_pairs(&nodes, &[], &generation);
        assert_eq!(projected(&bucketed), brute);
        assert!(!brute.is_empty(), "fixture must exercise accepted pairs");
    }

    #[test]
    fn body_slice_extracts_single_line_zero_indexed() {
        let src = "alpha\nbeta\ngamma\n";
        // row 1 (0-indexed) == "beta"
        assert_eq!(body_slice(src, 1, 1), "beta\n");
    }

    #[test]
    fn body_slice_extracts_multi_line_inclusive() {
        let src = "alpha\nbeta\ngamma\ndelta\n";
        // rows 1..=2 (0-indexed) == "beta", "gamma"
        assert_eq!(body_slice(src, 1, 2), "beta\ngamma\n");
    }

    #[test]
    fn body_slice_handles_out_of_bounds() {
        let src = "alpha\nbeta\n";
        assert_eq!(body_slice(src, 5, 9), "");
    }

    #[test]
    fn node_body_slice_uses_tree_sitter_columns_and_excludes_trailing_newline() {
        let src = "impl Demo {\n    fn value() {\n        work();\n    }\n}\n";
        let node = candidate_node("value", "value", "src/lib.rs", 3);
        let node = Node {
            start_line: 1,
            start_column: 4,
            end_column: 5,
            ..node
        };

        assert_eq!(
            node_body_slice(src, &node),
            "fn value() {\n        work();\n    }"
        );
    }

    fn candidate_node(id: &str, name: &str, file_path: &str, end_line: u32) -> Node {
        let mut node = test_node(id, name, 0);
        node.file_path = file_path.to_string();
        node.end_line = end_line;
        node.end_column = 1;
        node
    }

    fn stored_fingerprints(
        nodes: &[Node],
        fingerprints: &std::collections::HashMap<String, Fingerprint>,
    ) -> Vec<StoredFingerprint> {
        nodes
            .iter()
            .map(|node| {
                let fingerprint = fingerprints.get(&node.id).unwrap();
                StoredFingerprint {
                    node_id: node.id.clone(),
                    ast_hash: fingerprint.ast_hash.clone(),
                    cfg_hash: fingerprint.cfg_hash.clone(),
                    call_seq_hash: fingerprint.call_seq_hash.clone(),
                    shingles: fingerprint.shingles.clone(),
                    body_tokens: u32::try_from(fingerprint.body_tokens).unwrap(),
                    source_hash: fingerprint.source_hash.clone(),
                }
            })
            .collect()
    }

    fn result_bytes(
        nodes: &[Node],
        fingerprints: &std::collections::HashMap<String, Fingerprint>,
    ) -> Vec<u8> {
        let scoped = scoped_fingerprints(nodes, fingerprints);
        let pairs = find_redundant_pairs(scoped, 0.6, false, 20);
        let groups = connected_node_groups(&pairs);
        let options = RedundancyOptions {
            path_prefix: None,
            min_lines: 8,
            max_pairs: 20,
            threshold: 0.6,
            include_naming: false,
            include_generated: false,
        };
        serde_json::to_vec(&redundancy_output(
            &options,
            nodes.len(),
            fingerprints.len(),
            &pairs,
            &groups,
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn cold_and_warm_cache_paths_each_issue_one_bulk_read_with_identical_results() {
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
        let nodes = vec![
            candidate_node("alpha-id", "alpha", "src/alpha.rs", 8),
            candidate_node("beta-id", "beta", "src/beta.rs", 8),
        ];

        let cold_calls = Arc::new(AtomicUsize::new(0));
        let cold_counter = Arc::clone(&cold_calls);
        let cold = ensure_fingerprints_with_loader(temp.path(), &nodes, move |node_ids| {
            cold_counter.fetch_add(1, Ordering::Relaxed);
            assert_eq!(node_ids, vec!["alpha-id", "beta-id"]);
            std::future::ready(Ok(Vec::new()))
        })
        .await
        .unwrap();
        assert_eq!(cold_calls.load(Ordering::Relaxed), 1);
        assert_eq!(cold.parsed_files, 2);
        assert_eq!(cold.computed_fingerprints, 2);

        let warm_rows = stored_fingerprints(&nodes, &cold.fingerprints);
        let partial_calls = Arc::new(AtomicUsize::new(0));
        let partial_counter = Arc::clone(&partial_calls);
        let partial_rows = vec![warm_rows[0].clone()];
        let partial = ensure_fingerprints_with_loader(temp.path(), &nodes, move |node_ids| {
            partial_counter.fetch_add(1, Ordering::Relaxed);
            assert_eq!(node_ids, vec!["alpha-id", "beta-id"]);
            std::future::ready(Ok(partial_rows))
        })
        .await
        .unwrap();
        assert_eq!(partial_calls.load(Ordering::Relaxed), 1);
        assert_eq!(partial.parsed_files, 1);
        assert_eq!(partial.computed_fingerprints, 1);
        assert_eq!(
            result_bytes(&nodes, &cold.fingerprints),
            result_bytes(&nodes, &partial.fingerprints)
        );

        let warm_calls = Arc::new(AtomicUsize::new(0));
        let warm_counter = Arc::clone(&warm_calls);
        let warm = ensure_fingerprints_with_loader(temp.path(), &nodes, move |node_ids| {
            warm_counter.fetch_add(1, Ordering::Relaxed);
            assert_eq!(node_ids, vec!["alpha-id", "beta-id"]);
            std::future::ready(Ok(warm_rows))
        })
        .await
        .unwrap();
        assert_eq!(warm_calls.load(Ordering::Relaxed), 1);
        assert_eq!(warm.parsed_files, 0);
        assert_eq!(warm.computed_fingerprints, 0);
        assert_eq!(
            result_bytes(&nodes, &cold.fingerprints),
            result_bytes(&nodes, &warm.fingerprints)
        );
    }

    #[tokio::test]
    async fn bulk_read_work_proxy_stays_one_call_for_1024_candidates() {
        let nodes: Vec<Node> = (0..1024)
            .map(|index| {
                candidate_node(
                    &format!("node-{index}"),
                    "candidate",
                    &format!("src/file-{index}.unsupported"),
                    8,
                )
            })
            .collect();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let load = ensure_fingerprints_with_loader(
            std::path::Path::new("/unused"),
            &nodes,
            move |node_ids| {
                counter.fetch_add(1, Ordering::Relaxed);
                assert_eq!(node_ids.len(), 1024);
                std::future::ready(Ok(Vec::new()))
            },
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(load.fingerprints.is_empty());
        assert_eq!(load.parsed_files, 0);
        assert_eq!(load.computed_fingerprints, 0);
    }
}

// Rust guideline compliant 2026-05-25
//! `tracedecay_redundancy` — AST-level functional-duplicate detector.
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
//!    Within each bucket, compare every pair via
//!    [`composite_similarity`](crate::redundancy::composite_similarity).
//! 4. Filter by threshold, sort by score desc, return the top N pairs.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::errors::Result;
use crate::redundancy::{
    composite_similarity, compute_fingerprint, find_node_at_lines, jaccard_similarity,
    overlap_kind, parse_file, severity_bucket, Fingerprint,
};
use crate::tracedecay::TraceDecay;
use crate::types::{Node, NodeKind};

use super::super::render;
use super::super::ToolResult;
use super::support::effective_path;

/// `tracedecay_redundancy` handler.
pub(super) async fn handle_redundancy(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let options = redundancy_options(&args, scope_prefix);

    // 1. Collect candidate function nodes.
    let nodes = collect_candidates(cg, options.path_prefix, options.min_lines).await?;
    let total_candidates = nodes.len();

    // 2. Ensure each has a fresh fingerprint (cache by source hash).
    let fingerprints = ensure_fingerprints(cg, &nodes).await?;
    let scanned = fingerprints.len();

    // 3. Bucket by token count to keep pairwise comparison sub-quadratic.
    let pairs = find_redundant_pairs(
        &nodes,
        &fingerprints,
        options.threshold,
        options.include_naming,
        options.max_pairs,
    );

    let output = redundancy_output(&options, total_candidates, scanned, &pairs);
    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult {
        value: json!({
            "content": [{ "type": "text", "text": text }]
        }),
        touched_files: vec![],
        internal_analytics: None,
    })
}

struct RedundancyOptions<'a> {
    path_prefix: Option<&'a str>,
    min_lines: u32,
    max_pairs: usize,
    threshold: f64,
    include_naming: bool,
}

fn redundancy_options<'a>(args: &'a Value, scope_prefix: Option<&'a str>) -> RedundancyOptions<'a> {
    RedundancyOptions {
        path_prefix: effective_path(args, scope_prefix),
        min_lines: args
            .get("min_lines")
            .and_then(Value::as_u64)
            .map_or(8u32, |v| u32::try_from(v).unwrap_or(8)),
        max_pairs: args
            .get("max_pairs")
            .and_then(Value::as_u64)
            .map_or(20usize, |v| usize::try_from(v.min(500)).unwrap_or(20)),
        threshold: args
            .get("similarity_threshold")
            .and_then(Value::as_f64)
            .unwrap_or(0.6)
            .clamp(0.0, 1.0),
        include_naming: args
            .get("include_naming_only")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

fn redundancy_output(
    options: &RedundancyOptions<'_>,
    total_candidates: usize,
    scanned: usize,
    pairs: &[Value],
) -> Value {
    let pair_count = pairs.len();
    json!({
        "candidates": total_candidates,
        "scanned": scanned,
        "skipped_for_size": total_candidates.saturating_sub(scanned),
        "pair_count": pair_count,
        "pairs": pairs,
        "ranked_by": "similarity desc",
        "scope": options.path_prefix.unwrap_or("(whole project)"),
        "thresholds": {
            "min_lines": options.min_lines,
            "similarity_threshold": options.threshold,
            "include_naming_only": options.include_naming,
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
) -> Result<Vec<Node>> {
    let all = cg.get_all_nodes().await?;
    Ok(all
        .into_iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .filter(|n| n.end_line.saturating_sub(n.start_line) + 1 >= min_lines)
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
    let registry = crate::extraction::LanguageRegistry::new();
    let project_root = cg.project_root().to_path_buf();

    // Group candidates by file so we parse each file at most once.
    let mut by_file: HashMap<String, Vec<&Node>> = HashMap::new();
    for n in candidates {
        by_file.entry(n.file_path.clone()).or_default().push(n);
    }

    let mut out: HashMap<String, Fingerprint> = HashMap::new();

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
        let mut needs_parse = false;
        let mut cached: HashMap<&str, Fingerprint> = HashMap::new();
        for node in &file_nodes {
            let body = body_slice(&source, node.start_line, node.end_line);
            let expected_hash = quick_body_hash(body);
            match cg.db().get_fingerprint(&node.id).await? {
                Some(stored) if stored.source_hash == expected_hash => {
                    cached.insert(
                        node.id.as_str(),
                        Fingerprint {
                            ast_hash: stored.ast_hash,
                            cfg_hash: stored.cfg_hash,
                            call_seq_hash: stored.call_seq_hash,
                            shingles: stored.shingles,
                            body_tokens: stored.body_tokens as usize,
                            source_hash: stored.source_hash,
                        },
                    );
                }
                _ => {
                    needs_parse = true;
                }
            }
        }

        // Insert cached hits.
        for (id, fp) in cached {
            out.insert(id.to_string(), fp);
        }
        if !needs_parse {
            continue;
        }

        // At least one node in this file needs a fresh fingerprint —
        // parse once and compute for every miss.
        let Ok(language) = crate::extraction::ts_provider::language(lang_key) else {
            continue;
        };
        let Some(tree) = parse_file(&source, &language) else {
            continue;
        };

        for node in &file_nodes {
            if out.contains_key(&node.id) {
                continue;
            }
            // Node.start_line / end_line are stored as raw tree-sitter
            // row indices (0-based) — see info::extract_lines docs.
            let Some(ts_node) = find_node_at_lines(&tree, node.start_line, node.end_line) else {
                continue;
            };
            let fp = compute_fingerprint(&source, ts_node);
            // Persist for next time. Errors are logged but not fatal —
            // the redundancy query still returns results.
            if let Err(e) = cg.db().upsert_fingerprint(&node.id, &fp).await {
                eprintln!("[tracedecay] redundancy: upsert_fingerprint failed: {e}");
            }
            out.insert(node.id.clone(), fp);
        }
    }

    Ok(out)
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
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
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

struct RedundantPair<'a> {
    score: f64,
    kind: &'static str,
    node_a: &'a Node,
    node_b: &'a Node,
    fp_a: &'a Fingerprint,
    fp_b: &'a Fingerprint,
}

fn find_redundant_pairs(
    nodes: &[Node],
    fingerprints: &HashMap<String, Fingerprint>,
    threshold: f64,
    include_naming: bool,
    max_pairs: usize,
) -> Vec<Value> {
    // Sort by body_tokens so the size-window check is a linear scan.
    let mut sorted = scoped_fingerprints(nodes, fingerprints);
    sorted.sort_by_key(|(_, fp)| fp.body_tokens);

    let mut found = Vec::new();
    for (i, (node_a, fp_a)) in sorted.iter().enumerate() {
        let (lo, hi) = body_token_window(fp_a.body_tokens);
        for (node_b, fp_b) in sorted.iter().skip(i + 1) {
            if fp_b.body_tokens > hi {
                break; // sorted, no need to scan further
            }
            if fp_b.body_tokens < lo {
                continue;
            }
            if let Some(pair) =
                redundant_pair(node_a, fp_a, node_b, fp_b, threshold, include_naming)
            {
                found.push(pair);
            }
        }
    }

    found.sort_by(|a: &RedundantPair<'_>, b: &RedundantPair<'_>| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    found.truncate(max_pairs);

    found.iter().map(redundant_pair_json).collect()
}

fn scoped_fingerprints<'a>(
    nodes: &'a [Node],
    fingerprints: &'a HashMap<String, Fingerprint>,
) -> Vec<ScopedFingerprint<'a>> {
    nodes
        .iter()
        .filter_map(|n| fingerprints.get(&n.id).map(|fp| (n, fp)))
        .collect()
}

fn body_token_window(body_tokens: usize) -> (usize, usize) {
    (
        (body_tokens as f64 * 0.75).floor() as usize,
        (body_tokens as f64 * 1.25).ceil() as usize,
    )
}

fn redundant_pair<'a>(
    node_a: &'a Node,
    fp_a: &'a Fingerprint,
    node_b: &'a Node,
    fp_b: &'a Fingerprint,
    threshold: f64,
    include_naming: bool,
) -> Option<RedundantPair<'a>> {
    let score = composite_similarity(fp_a, fp_b);
    if score < threshold {
        return None;
    }
    let kind = overlap_kind(fp_a, fp_b);
    if !include_naming && kind == "naming" {
        return None;
    }
    Some(RedundantPair {
        score,
        kind,
        node_a,
        node_b,
        fp_a,
        fp_b,
    })
}

fn redundant_pair_json(pair: &RedundantPair<'_>) -> Value {
    let shingle_jaccard = jaccard_similarity(&pair.fp_a.shingles, &pair.fp_b.shingles);
    let severity = severity_bucket(pair.score, pair.kind);
    json!({
        "similarity": (pair.score * 10000.0).round() / 10000.0,
        "severity": severity,
        "overlap_kind": pair.kind,
        "a": {
            "file": pair.node_a.file_path,
            "line": pair.node_a.start_line,
            "name": pair.node_a.name,
            "id": pair.node_a.id,
        },
        "b": {
            "file": pair.node_b.file_path,
            "line": pair.node_b.start_line,
            "name": pair.node_b.name,
            "id": pair.node_b.id,
        },
        "signals": {
            "ast_match": pair.fp_a.ast_hash == pair.fp_b.ast_hash,
            "cfg_match": pair.fp_a.cfg_hash == pair.fp_b.cfg_hash,
            "call_seq_match": pair.fp_a.call_seq_hash == pair.fp_b.call_seq_hash,
            "shingle_jaccard": (shingle_jaccard * 10000.0).round() / 10000.0,
            "body_tokens": [pair.fp_a.body_tokens, pair.fp_b.body_tokens],
        },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::body_slice;

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
}

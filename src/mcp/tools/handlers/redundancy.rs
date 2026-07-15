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
//!    Within each bucket, score every pair via
//!    [`redundancy_match_score`](crate::redundancy::redundancy_match_score),
//!    which blends the composite similarity with the body-vector cosine,
//!    relabels cosine-rescued `naming` pairs as `body_vector`, and downranks
//!    generic helper names.
//! 4. Filter by threshold, sort by `ranking_score` desc (total order — ties
//!    fall through similarity, cosine, then names and node ids), and return
//!    the top N pairs plus their connected duplicate groups.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::errors::Result;
use crate::redundancy::{
    Fingerprint, RedundantPair, compute_fingerprint, connected_node_groups, find_node_at_lines,
    find_redundant_pairs, parse_file, round4,
};
use crate::tracedecay::TraceDecay;
use crate::types::{Node, NodeKind};

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::effective_path;

/// `tracedecay_redundancy` handler.
pub(super) async fn handle_redundancy(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let options = redundancy_options(&args, scope_prefix);

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
    let output = redundancy_output(&options, total_candidates, scanned, &pairs, &groups);
    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        redundancy_md(&options, total_candidates, scanned, &pairs, &groups)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

struct RedundancyOptions<'a> {
    path_prefix: Option<&'a str>,
    min_lines: u32,
    max_pairs: usize,
    threshold: f64,
    include_naming: bool,
    include_generated: bool,
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
        include_generated: args
            .get("include_generated_paths")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
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

/// Typed markdown view over the same data the JSON output is built from (the
/// `RedundantPair` slice plus the scan counts and options), so the two formats
/// cannot silently drift. Bounded and compact per the repo convention: no
/// tables, the full ranked pair list, and the full member list per group.
fn redundancy_md(
    options: &RedundancyOptions<'_>,
    total_candidates: usize,
    scanned: usize,
    pairs: &[RedundantPair<'_>],
    groups: &[Vec<&Node>],
) -> String {
    let mut md = Md::new();
    md.heading(2, "Redundancy");
    md.field("candidates", &total_candidates.to_string());
    md.field("scanned", &scanned.to_string());
    md.field(
        "skipped_for_size",
        &total_candidates.saturating_sub(scanned).to_string(),
    );
    md.field("pair_count", &pairs.len().to_string());
    md.field("scope", options.path_prefix.unwrap_or("(whole project)"));
    md.field(
        "thresholds",
        &format!(
            "min_lines {}, similarity_threshold {}, include_naming_only {}, include_generated_paths {}",
            options.min_lines,
            round4(options.threshold),
            options.include_naming,
            options.include_generated
        ),
    );
    md.line(
        "groups_scope: connected components over the returned pairs only; raise max_pairs to see full clusters",
    );

    md.blank().heading(3, "Pairs");
    if pairs.is_empty() {
        md.empty_note("No redundant pairs above threshold.");
    } else {
        for pair in pairs {
            append_pair_md(&mut md, pair);
        }
    }

    md.blank().heading(3, "Groups");
    if groups.is_empty() {
        md.empty_note("No duplicate groups.");
    } else {
        for group in groups {
            append_group_md(&mut md, group);
        }
    }

    md.render()
}

/// `name (file:line)` locator that chains into `tracedecay_body` / `_callers`.
fn node_label(node: &Node) -> String {
    format!("{} ({}:{})", node.name, node.file_path, node.start_line)
}

fn append_pair_md(md: &mut Md, pair: &RedundantPair<'_>) {
    let downranked = if pair.score.generic_helper_downranked {
        ", generic-helper downranked"
    } else {
        ""
    };
    md.bullet(&format!(
        "**{} <-> {}** — {}/{}, ranking_score {}, similarity {}, cosine {}{downranked}",
        node_label(pair.node_a),
        node_label(pair.node_b),
        pair.score.severity,
        pair.score.overlap_kind,
        round4(pair.score.ranking_score),
        round4(pair.score.similarity),
        round4(pair.score.vector_cosine),
    ));
    md.line(&format!(
        "  body_tokens [{}, {}]; ids `{}`, `{}`",
        pair.fp_a.body_tokens, pair.fp_b.body_tokens, pair.node_a.id, pair.node_b.id
    ));
}

fn append_group_md(md: &mut Md, group: &[&Node]) {
    md.bullet(&format!("**Group of {}**", group.len()));
    for node in group {
        md.line(&format!("  {}", node_label(node)));
    }
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
                    cached.insert(node.id.as_str(), stored.into());
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
            out.insert(node.id.clone(), compute_fingerprint(&source, ts_node));
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
        eprintln!("[tracedecay] redundancy: atomic cache publication failed: {e}");
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
    use super::{RedundancyOptions, body_slice, is_generated_path, redundancy_md};
    use crate::redundancy::{
        Fingerprint, RedundancyMatchScore, RedundantPair, connected_node_groups,
    };
    use crate::types::{Node, NodeKind, Visibility};

    #[test]
    fn generated_paths_are_excluded_from_candidates_by_default() {
        for path in [
            "dashboard/lcm/dist/index.js",
            "node_modules/lib/index.js",
            ".worktrees/feature/src/lib.rs",
            "vendor/libsql/src/lib.rs",
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

    fn test_node(id: &str, name: &str, line: u32) -> Node {
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
    fn redundancy_md_renders_ranked_pairs_and_full_groups() {
        // Chain a->b->c->d so the connected component has more than 3 members.
        let a = test_node("id_a", "alpha", 10);
        let b = test_node("id_b", "beta", 20);
        let c = test_node("id_c", "gamma", 30);
        let d = test_node("id_d", "delta", 40);
        let fa = test_fingerprint(50);
        let fb = test_fingerprint(52);
        let fc = test_fingerprint(54);
        let fd = test_fingerprint(56);

        let pairs = vec![
            RedundantPair {
                score: test_score(0.95),
                node_a: &a,
                node_b: &b,
                fp_a: &fa,
                fp_b: &fb,
            },
            RedundantPair {
                score: test_score(0.9),
                node_a: &b,
                node_b: &c,
                fp_a: &fb,
                fp_b: &fc,
            },
            RedundantPair {
                score: test_score(0.85),
                node_a: &c,
                node_b: &d,
                fp_a: &fc,
                fp_b: &fd,
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
        let md = redundancy_md(&options, 4, 4, &pairs, &groups);

        // Ranked pair line carries the ranking_score.
        assert!(md.contains("ranking_score 0.95"), "{md}");
        // Per-pair body_tokens survive (dropped by the generic walker).
        assert!(md.contains("body_tokens [50, 52]"), "{md}");
        assert!(md.contains("`id_a`, `id_b`"), "{md}");
        // The 4-member group lists every member without truncation.
        assert!(md.contains("**Group of 4**"), "{md}");
        for name in ["alpha", "beta", "gamma", "delta"] {
            assert!(md.contains(name), "missing group member {name}: {md}");
        }
        assert!(!md.contains("(+"), "group was truncated: {md}");
        assert!(!md.contains("more)"), "group was truncated: {md}");
        // The groups_scope caveat is present.
        assert!(
            md.contains(
                "groups_scope: connected components over the returned pairs only; raise max_pairs to see full clusters"
            ),
            "{md}"
        );
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
}

// Rust guideline compliant 2026-05-25
//! `tracedecay_redundancy` — AST-level functional-duplicate detector.
//!
//! Wire surface only: argument parsing, the scan call, and rendering. The
//! pipeline itself lives in [`crate::graph::redundancy_scan`] so the root
//! engine's `GraphRuntimePort` can produce the same payload without going
//! through this handler.

use serde_json::{Value, json};

use crate::errors::Result;
use crate::graph::redundancy_scan::{RedundancyOptions, RedundancyScanV1, redundancy_scan};
use crate::redundancy::round4;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::effective_path;

/// `tracedecay_redundancy` handler.
pub(crate) async fn handle_redundancy(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let options = redundancy_options(&args, scope_prefix);
    let scan = redundancy_scan(cg, &options).await?;
    let text = render::finalize(Some(cg.project_root()), &args, &scan.output, || {
        if scan.semantic_active {
            render::generic_md(&scan.output)
        } else {
            redundancy_md(&options, &scan)
        }
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
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

/// Typed markdown view over the same data the JSON output is built from (the
/// ranked pair views plus the scan counts and options), so the two formats
/// cannot silently drift. Bounded and compact per the repo convention: no
/// tables, the full ranked pair list, and the full member list per group.
fn redundancy_md(options: &RedundancyOptions<'_>, scan: &RedundancyScanV1) -> String {
    let mut md = Md::new();
    md.heading(2, "Redundancy");
    md.field("candidates", &scan.total_candidates.to_string());
    md.field("scanned", &scan.scanned.to_string());
    md.field(
        "skipped_for_size",
        &scan
            .total_candidates
            .saturating_sub(scan.scanned)
            .to_string(),
    );
    md.field("pair_count", &scan.pairs.len().to_string());
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
    if scan.pairs.is_empty() {
        md.empty_note("No redundant pairs above threshold.");
    } else {
        for pair in &scan.pairs {
            append_pair_md(&mut md, pair);
        }
    }

    md.blank().heading(3, "Groups");
    if scan.groups.is_empty() {
        md.empty_note("No duplicate groups.");
    } else {
        for group in &scan.groups {
            append_group_md(&mut md, group);
        }
    }

    md.render()
}

fn append_pair_md(md: &mut Md, pair: &crate::graph::redundancy_scan::RedundancyPairViewV1) {
    let downranked = if pair.generic_helper_downranked {
        ", generic-helper downranked"
    } else {
        ""
    };
    md.bullet(&format!(
        "**{} <-> {}** — {}/{}, ranking_score {}, similarity {}, cosine {}{downranked}",
        pair.label_a,
        pair.label_b,
        pair.severity,
        pair.overlap_kind,
        round4(pair.ranking_score),
        round4(pair.similarity),
        round4(pair.vector_cosine),
    ));
    md.line(&format!(
        "  body_tokens [{}, {}]; ids `{}`, `{}`",
        pair.body_tokens[0], pair.body_tokens[1], pair.id_a, pair.id_b
    ));
}

fn append_group_md(md: &mut Md, group: &[String]) {
    md.bullet(&format!("**Group of {}**", group.len()));
    for label in group {
        md.line(&format!("  {label}"));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{RedundancyOptions, RedundancyScanV1, redundancy_md};
    use crate::graph::redundancy_scan::{group_views, pair_views};
    use crate::redundancy::{
        Fingerprint, RedundancyMatchScore, RedundantPair, connected_node_groups,
    };
    use crate::types::{Node, NodeKind, Visibility};

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
        let scan = RedundancyScanV1 {
            output: serde_json::Value::Null,
            semantic_active: false,
            total_candidates: 4,
            scanned: 4,
            pairs: pair_views(&pairs),
            groups: group_views(&groups),
        };
        let md = redundancy_md(&options, &scan);

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
}

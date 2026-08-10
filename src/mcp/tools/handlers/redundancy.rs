// Rust guideline compliant 2026-05-25
//! `tracedecay_redundancy` — AST-level functional-duplicate detector.
//!
//! Wire surface only: argument parsing, the scan call, and rendering. The
//! pipeline itself lives in [`crate::graph::redundancy_scan`]; this handler
//! supplies the admitted verified graph and renders the resulting payload.

use serde_json::{Value, json};
use tracedecay_application::retrieval::RedundancySurfaceRequestV1;
use tracedecay_application::retrieval::grep_analysis::RedundancyResultV1;

use crate::errors::Result;
use crate::graph::redundancy_scan::{RedundancyOptions, RedundancyScanV1, redundancy_scan};
use crate::redundancy::round4;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::decode_primitive_request;

/// `tracedecay_redundancy` handler.
pub(crate) async fn handle_redundancy(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let request: RedundancySurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_redundancy")?;
    let options = redundancy_options(&request, scope_prefix);
    let scan = redundancy_scan(cg, graph, &options).await?;
    let result = serde_json::from_value::<RedundancyResultV1>(scan.output.clone())?;
    let output = serde_json::to_value(result)?;
    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        if scan.semantic_active {
            render::generic_md(&output)
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

fn redundancy_options<'a>(
    request: &'a RedundancySurfaceRequestV1,
    scope_prefix: Option<&'a str>,
) -> RedundancyOptions<'a> {
    RedundancyOptions {
        path_prefix: request.path.as_deref().or(scope_prefix),
        min_lines: request.min_lines.unwrap_or(8),
        max_pairs: request
            .max_pairs
            .map_or(20, |value| value.min(500) as usize),
        threshold: request.similarity_threshold.unwrap_or(0.6).clamp(0.0, 1.0),
        include_naming: request.include_naming_only.unwrap_or(false),
        include_generated: request.include_generated_paths.unwrap_or(false),
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
    use crate::graph::redundancy_scan::{RedundancyNodeViewV1, RedundancyPairViewV1};

    fn test_pair(
        id_a: &str,
        name_a: &str,
        line_a: u32,
        id_b: &str,
        name_b: &str,
        line_b: u32,
        body_tokens: [usize; 2],
        ranking_score: f64,
    ) -> RedundancyPairViewV1 {
        let a = RedundancyNodeViewV1 {
            name: name_a.to_owned(),
            file: "src/lib.rs".to_owned(),
            line: line_a,
            id: id_a.to_owned(),
        };
        let b = RedundancyNodeViewV1 {
            name: name_b.to_owned(),
            file: "src/lib.rs".to_owned(),
            line: line_b,
            id: id_b.to_owned(),
        };
        RedundancyPairViewV1 {
            label_a: format!("{name_a} (src/lib.rs:{line_a})"),
            label_b: format!("{name_b} (src/lib.rs:{line_b})"),
            id_a: id_a.to_owned(),
            id_b: id_b.to_owned(),
            a,
            b,
            severity: "high",
            overlap_kind: "body_vector",
            ranking_score,
            similarity: 0.9,
            vector_cosine: 0.8,
            generic_helper_downranked: false,
            body_tokens,
        }
    }

    #[test]
    fn redundancy_md_renders_ranked_pairs_and_full_groups() {
        // Chain a->b->c->d so the connected component has more than 3 members.
        let pairs = vec![
            test_pair("id_a", "alpha", 10, "id_b", "beta", 20, [50, 52], 0.95),
            test_pair("id_b", "beta", 20, "id_c", "gamma", 30, [52, 54], 0.9),
            test_pair("id_c", "gamma", 30, "id_d", "delta", 40, [54, 56], 0.85),
        ];

        let options = RedundancyOptions {
            path_prefix: None,
            min_lines: 8,
            max_pairs: 20,
            threshold: 0.6,
            include_naming: false,
            include_generated: false,
        };

        let scan = RedundancyScanV1 {
            output: serde_json::Value::Null,
            semantic_active: false,
            total_candidates: 4,
            scanned: 4,
            pairs,
            groups: vec![vec![
                "alpha (src/lib.rs:10)".to_owned(),
                "beta (src/lib.rs:20)".to_owned(),
                "gamma (src/lib.rs:30)".to_owned(),
                "delta (src/lib.rs:40)".to_owned(),
            ]],
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

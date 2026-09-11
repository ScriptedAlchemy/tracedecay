//! `tracedecay_redundancy` — AST-level functional-duplicate detector.
//!
//! Wire surface only: argument parsing, the scan call, and rendering. The
//! pipeline itself lives in [`tracedecay_graph_query::redundancy_scan`]; this
//! handler supplies the admitted verified graph and renders the resulting payload.

use serde_json::{Value, json};
use tracedecay_application::semantic_runtime::project_semantic_redundancy_generation;
use tracedecay_code_extraction::redundancy::round4;
use tracedecay_contracts::retrieval::RedundancySurfaceRequestV1;
use tracedecay_contracts::retrieval::grep_analysis::RedundancyResultV1;
use tracedecay_domain::errors::Result;
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_graph_query::redundancy_scan::{
    RedundancyOptions, RedundancyPairViewV1, RedundancyScanV1, redundancy_scan,
};

use crate::ToolResult;
use crate::decode_primitive_request;
use crate::tools::render::{self, Md};

#[hotpath::measure(label = "mcp.health.redundancy.total")]
pub async fn handle_redundancy(
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let request: RedundancySurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_redundancy")?;
    let options = redundancy_options(&request, scope_prefix);
    let project_root = graph.project_root()?;
    let semantic = project_semantic_redundancy_generation(project_root).await;
    let scan = hotpath::future!(
        redundancy_scan(graph, &options, semantic.as_ref()),
        label = "mcp.health.redundancy.scan"
    )
    .await?;
    let result = serde_json::from_value::<RedundancyResultV1>(scan.output.clone())?;
    let output = serde_json::to_value(result)?;
    let text = render::finalize(Some(project_root), &args, &output, || {
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

fn append_pair_md(md: &mut Md, pair: &RedundancyPairViewV1) {
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

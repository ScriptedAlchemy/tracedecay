//! `tracedecay_hotspots`, churn-weighted connectivity ranking.

use std::sync::LazyLock;

use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_contracts::retrieval::{HotspotV1, HotspotsResultV1, HotspotsSurfaceRequestV1};
use tracedecay_runtime_core::git::churn::file_churn;

use super::*;

/// Manifest keys (`package.json`, `Cargo.toml`) are indexed for module
/// resolution; they have no call edges and are not code hotspots.
static EXTRACTORS: LazyLock<LanguageRegistry> = LazyLock::new(LanguageRegistry::new);

const CHURN_WINDOW_DAYS: u32 = 90;

#[tracing::instrument(name = "mcp.analysis.hotspots.total", level = "trace", skip_all)]
pub(super) async fn compute_hotspots(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: HotspotsSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_hotspots")?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    require_positive_limit(limit, "tracedecay_hotspots")?;

    let (mut symbols, edges) = {
        let _span = tracing::trace_span!("mcp.analysis.hotspots.graph").entered();
        {
            let symbols = verified_analysis_symbols(graph, scope_prefix)?;
            let edges = verified_analysis_edges(graph, &symbols, &[])?;
            (symbols, edges)
        }
    };
    let churn = file_churn(graph.project_root()?, CHURN_WINDOW_DAYS).await?;
    let mut hotspots: Vec<HotspotV1> = {
        let _span = tracing::trace_span!("mcp.analysis.hotspots.compute").entered();
        let mut incoming = HashMap::<SymbolOccurrenceId, u64>::new();
        let mut outgoing = HashMap::<SymbolOccurrenceId, u64>::new();
        for edge in edges {
            *outgoing.entry(edge.from_occurrence).or_default() += 1;
            *incoming.entry(edge.to_occurrence).or_default() += 1;
        }
        symbols.retain(|symbol| !EXTRACTORS.is_configuration_file(&symbol.path));
        symbols
            .into_iter()
            .map(|symbol| {
                let incoming = incoming.get(&symbol.occurrence).copied().unwrap_or(0);
                let outgoing = outgoing.get(&symbol.occurrence).copied().unwrap_or(0);
                let total = incoming.saturating_add(outgoing);
                let churn = churn.get(&symbol.path).copied().unwrap_or(0) as u64;
                HotspotV1 {
                    id: symbol.occurrence.as_str().to_owned(),
                    name: symbol.metadata.simple_name,
                    kind: symbol.metadata.kind,
                    file: symbol.path,
                    line: user_line(symbol.metadata.start_line),
                    incoming,
                    outgoing,
                    total,
                    churn,
                    score: total.saturating_mul(churn.saturating_add(1)),
                }
            })
            .collect()
    };
    hotspots.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.total.cmp(&left.total))
            .then_with(|| left.id.cmp(&right.id))
    });
    hotspots.truncate(limit);
    let touched_files = unique_file_paths(hotspots.iter().map(|hotspot| hotspot.file.as_str()));
    Ok(graph_tool_completion(
        GraphToolResultV1::Hotspots(HotspotsResultV1 {
            hotspot_count: hotspots.len() as u64,
            hotspots,
            freshness: None,
        }),
        touched_files,
    ))
}

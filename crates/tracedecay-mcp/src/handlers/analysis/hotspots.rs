//! `tracedecay_hotspots`, churn-weighted complexity ranking.

use std::sync::LazyLock;

use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_contracts::retrieval::{HotspotV1, HotspotsResultV1, HotspotsSurfaceRequestV1};

use super::*;

/// Manifest keys (`package.json`, `Cargo.toml`) are indexed for module
/// resolution; they have no call edges and are not code hotspots.
static EXTRACTORS: LazyLock<LanguageRegistry> = LazyLock::new(LanguageRegistry::new);

#[hotpath::measure(future = true, label = "mcp.analysis.hotspots.total")]
pub(super) async fn compute_hotspots(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: HotspotsSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_hotspots")?;
    let limit = request.limit.map_or(10, |v| v.min(100) as usize);
    require_positive_limit(limit, "tracedecay_hotspots")?;

    let (mut symbols, edges) = hotpath::measure_block!("mcp.analysis.hotspots.graph", {
        let symbols = verified_analysis_symbols(graph, scope_prefix)?;
        let edges = verified_analysis_edges(graph, &symbols, &[])?;
        (symbols, edges)
    });
    let (symbols, incoming, outgoing) = hotpath::measure_block!("mcp.analysis.hotspots.compute", {
        let mut incoming = HashMap::<SymbolOccurrenceId, u64>::new();
        let mut outgoing = HashMap::<SymbolOccurrenceId, u64>::new();
        for edge in edges {
            *outgoing.entry(edge.from_occurrence).or_default() += 1;
            *incoming.entry(edge.to_occurrence).or_default() += 1;
        }
        symbols.retain(|symbol| !EXTRACTORS.is_configuration_file(&symbol.path));
        symbols.sort_by(|left, right| {
            let left_total = incoming
                .get(&left.occurrence)
                .copied()
                .unwrap_or(0)
                .saturating_add(outgoing.get(&left.occurrence).copied().unwrap_or(0));
            let right_total = incoming
                .get(&right.occurrence)
                .copied()
                .unwrap_or(0)
                .saturating_add(outgoing.get(&right.occurrence).copied().unwrap_or(0));
            right_total
                .cmp(&left_total)
                .then_with(|| left.occurrence.cmp(&right.occurrence))
        });
        symbols.truncate(limit);
        (symbols, incoming, outgoing)
    });
    let hotspots: Vec<HotspotV1> = symbols
        .into_iter()
        .map(|symbol| {
            let incoming = incoming.get(&symbol.occurrence).copied().unwrap_or(0);
            let outgoing = outgoing.get(&symbol.occurrence).copied().unwrap_or(0);
            HotspotV1 {
                id: symbol.occurrence.as_str().to_owned(),
                name: symbol.metadata.simple_name,
                kind: symbol.metadata.kind,
                file: symbol.path,
                line: user_line(symbol.metadata.start_line),
                incoming,
                outgoing,
                total: incoming + outgoing,
            }
        })
        .collect();
    let touched_files = unique_file_paths(hotspots.iter().map(|hotspot| hotspot.file.as_str()));
    Ok(graph_tool_completion(
        GraphToolResultV1::Hotspots(HotspotsResultV1 {
            hotspot_count: hotspots.len() as u64,
            hotspots,
        }),
        touched_files,
    ))
}

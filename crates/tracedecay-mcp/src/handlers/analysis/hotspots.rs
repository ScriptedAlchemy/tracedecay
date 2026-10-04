//! `tracedecay_hotspots`, connectivity weighted by recent git churn.

use std::sync::LazyLock;

use std::collections::HashMap;

use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_contracts::retrieval::{HotspotV1, HotspotsResultV1, HotspotsSurfaceRequestV1};
use tracedecay_runtime_core::git::churn::file_churn_paths;

use super::*;

/// Manifest keys (`package.json`, `Cargo.toml`) are indexed for module
/// resolution; they have no call edges and are not code hotspots.
static EXTRACTORS: LazyLock<LanguageRegistry> = LazyLock::new(LanguageRegistry::new);

const CHURN_WINDOW_DAYS: u32 = 90;

/// `(degree + 1) * (churn + 1)`. Equal churn keeps degree order. A leaf
/// still moves when its file changed and a hub's file did not.
fn churn_weighted_rank(degree: u64, churn: u64) -> u64 {
    degree
        .saturating_add(1)
        .saturating_mul(churn.saturating_add(1))
}

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
    let (incoming, outgoing) = {
        let _span = tracing::trace_span!("mcp.analysis.hotspots.compute").entered();
        let mut incoming = HashMap::<SymbolOccurrenceId, u64>::new();
        let mut outgoing = HashMap::<SymbolOccurrenceId, u64>::new();
        for edge in edges {
            *outgoing.entry(edge.from_occurrence).or_default() += 1;
            *incoming.entry(edge.to_occurrence).or_default() += 1;
        }
        symbols.retain(|symbol| !EXTRACTORS.is_configuration_file(&symbol.path));
        (incoming, outgoing)
    };
    let mut files: Vec<String> = symbols.iter().map(|symbol| symbol.path.clone()).collect();
    files.sort();
    files.dedup();
    let project_root = graph.project_root()?.to_path_buf();
    let churn_by_file = file_churn_paths(&project_root, CHURN_WINDOW_DAYS, &files).await;
    let churn_available = churn_by_file.is_ok();
    let churn_by_file = churn_by_file.unwrap_or_default();
    let degree = |occurrence: &SymbolOccurrenceId| {
        incoming
            .get(occurrence)
            .copied()
            .unwrap_or(0)
            .saturating_add(outgoing.get(occurrence).copied().unwrap_or(0))
    };
    symbols.sort_by(|left, right| {
        let left_degree = degree(&left.occurrence);
        let right_degree = degree(&right.occurrence);
        let rank = |symbol_degree: u64, path: &str| {
            if churn_available {
                churn_weighted_rank(
                    symbol_degree,
                    u64::try_from(churn_by_file.get(path).copied().unwrap_or(0)).unwrap_or(u64::MAX),
                )
            } else {
                symbol_degree
            }
        };
        rank(right_degree, right.path.as_str())
            .cmp(&rank(left_degree, left.path.as_str()))
            .then_with(|| right_degree.cmp(&left_degree))
            .then_with(|| left.occurrence.cmp(&right.occurrence))
    });
    symbols.truncate(limit);
    let hotspots: Vec<HotspotV1> = symbols
        .into_iter()
        .map(|symbol| {
            let incoming = incoming.get(&symbol.occurrence).copied().unwrap_or(0);
            let outgoing = outgoing.get(&symbol.occurrence).copied().unwrap_or(0);
            let churn = churn_available.then(|| {
                u64::try_from(
                    churn_by_file
                        .get(symbol.path.as_str())
                        .copied()
                        .unwrap_or(0),
                )
                .unwrap_or(u64::MAX)
            });
            HotspotV1 {
                id: symbol.occurrence.as_str().to_owned(),
                name: symbol.metadata.simple_name,
                kind: symbol.metadata.kind,
                file: symbol.path,
                line: user_line(symbol.metadata.start_line),
                incoming,
                outgoing,
                total: incoming + outgoing,
                churn,
            }
        })
        .collect();
    let touched_files = unique_file_paths(hotspots.iter().map(|hotspot| hotspot.file.as_str()));
    Ok(graph_tool_completion(
        GraphToolResultV1::Hotspots(HotspotsResultV1 {
            hotspot_count: hotspots.len() as u64,
            hotspots,
            unavailable_fields: if churn_available {
                Vec::new()
            } else {
                vec!["churn".to_owned()]
            },
            freshness: None,
        }),
        touched_files,
    ))
}

#[cfg(test)]
mod tests {
    use super::churn_weighted_rank;

    #[test]
    fn churn_weighted_rank_is_degree_plus_one_times_churn_plus_one() {
        assert_eq!(churn_weighted_rank(1, 1), 4);
        assert_eq!(churn_weighted_rank(2, 1), 6);
        assert_eq!(churn_weighted_rank(0, 0), 1);
        assert_eq!(churn_weighted_rank(0, 4), 5);
    }
}

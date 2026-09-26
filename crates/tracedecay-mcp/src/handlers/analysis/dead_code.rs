//! `tracedecay_dead_code`, symbols with no incoming edges.

use tracedecay_contracts::retrieval::{
    DeadCodeResultV1, DeadCodeSurfaceRequestV1, DeadCodeSymbolV1,
};

use super::*;

#[hotpath::measure(future = true, label = "mcp.analysis.dead_code.total")]
pub(super) async fn compute_dead_code(
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: DeadCodeSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_dead_code")?;
    let kinds: Vec<NodeKind> = match &request.kinds {
        Some(kinds) => kinds
            .iter()
            .map(|kind| requested_node_kind("tracedecay_dead_code", kind))
            .collect::<Result<_>>()?,
        None => vec![NodeKind::Function, NodeKind::Method],
    };
    let include_public = request.include_public.unwrap_or(false);
    let limit = request
        .limit
        .map_or(100, |value| value.clamp(1, 1_000) as usize);
    let path_prefix = request.path.as_deref().or(scope_prefix);
    let dead = hotpath::future!(
        graph.find_dead_code(&kinds, include_public, path_prefix, limit),
        label = "mcp.analysis.dead_code.graph"
    )
    .await?;
    let symbols = hotpath::measure_block!("mcp.analysis.dead_code.compute", {
        let mut symbols = Vec::with_capacity(dead.len());
        for symbol in dead {
            let binding = symbol
                .binding
                .ok_or_else(|| TraceDecayError::ProjectRoute {
                    reason_code: "verified-dead-code-evidence-incomplete".to_owned(),
                    retryable: false,
                    detail: "a dead-code candidate has no generation-pinned file binding"
                        .to_owned(),
                })?;
            let file = binding
                .logical_path
                .ok_or_else(|| TraceDecayError::ProjectRoute {
                    reason_code: "verified-dead-code-evidence-incomplete".to_owned(),
                    retryable: false,
                    detail: "a dead-code candidate has no generation-pinned logical path"
                        .to_owned(),
                })?;
            let metadata = symbol
                .metadata
                .ok_or_else(|| TraceDecayError::ProjectRoute {
                    reason_code: "verified-dead-code-evidence-incomplete".to_owned(),
                    retryable: false,
                    detail: "a dead-code candidate has no extraction-attested symbol metadata"
                        .to_owned(),
                })?;
            symbols.push(DeadCodeSymbolV1 {
                id: symbol.occurrence.as_str().to_owned(),
                name: metadata.simple_name,
                kind: metadata.kind,
                file,
                line: user_line(metadata.start_line),
                signature: metadata.signature,
            });
        }
        symbols
    });
    let touched_files = unique_file_paths(symbols.iter().map(|symbol| symbol.file.as_str()));
    Ok(graph_tool_completion(
        GraphToolResultV1::DeadCode(DeadCodeResultV1 {
            dead_code_count: symbols.len() as u64,
            symbols,
        }),
        touched_files,
    ))
}

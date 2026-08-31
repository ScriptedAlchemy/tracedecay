//! Typed wire projections shared by the primitive graph handlers.

use tracedecay_application::retrieval::{
    PrimitiveLaneCompleteV1, PrimitiveLaneStateV1, PrimitiveLaneStatusV1, PrimitiveRecallV1,
    PrimitiveSearchCoverageV1, PrimitiveSemanticModeV1, PrimitiveSymbolLocationV1,
};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_domain::errors::Result;
use tracedecay_mcp::handlers::graph::{
    graph_symbol_end_line, required_graph_file_path, required_graph_metadata,
};

pub(super) fn semantic_search_mode(
    mode: Option<PrimitiveSemanticModeV1>,
) -> crate::mcp::server::CodeIndexSearchModeV1 {
    match mode.unwrap_or(PrimitiveSemanticModeV1::FallbackAllowed) {
        PrimitiveSemanticModeV1::FallbackAllowed => {
            crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed
        }
        PrimitiveSemanticModeV1::StrictSemantic => {
            crate::mcp::server::CodeIndexSearchModeV1::StrictSemantic
        }
    }
}

fn lane_status(status: &crate::mcp::server::CodeIndexLaneStatusV1) -> PrimitiveLaneStatusV1 {
    match status {
        crate::mcp::server::CodeIndexLaneStatusV1::Complete => {
            PrimitiveLaneStatusV1::Complete(PrimitiveLaneCompleteV1::Complete)
        }
        crate::mcp::server::CodeIndexLaneStatusV1::Stale { generation } => {
            PrimitiveLaneStatusV1::State {
                status: PrimitiveLaneStateV1::Stale,
                generation: Some(generation.clone()),
                reason: None,
            }
        }
        crate::mcp::server::CodeIndexLaneStatusV1::Partial { generation } => {
            PrimitiveLaneStatusV1::State {
                status: PrimitiveLaneStateV1::Partial,
                generation: generation.clone(),
                reason: None,
            }
        }
        crate::mcp::server::CodeIndexLaneStatusV1::Unavailable { reason } => {
            PrimitiveLaneStatusV1::State {
                status: PrimitiveLaneStateV1::Unavailable,
                generation: None,
                reason: Some((*reason).to_owned()),
            }
        }
    }
}

pub(super) fn search_coverage(
    coverage: &crate::mcp::server::CodeIndexSearchCoverageV1,
) -> PrimitiveSearchCoverageV1 {
    PrimitiveSearchCoverageV1 {
        exact: lane_status(&coverage.exact),
        lexical: lane_status(&coverage.lexical),
        graph: lane_status(&coverage.graph),
        semantic: lane_status(&coverage.semantic),
        recall: if coverage.is_degraded() {
            PrimitiveRecallV1::Partial
        } else {
            PrimitiveRecallV1::Full
        },
    }
}

pub(super) fn symbol_location(
    symbol: &CodeGraphSymbolSummaryV1,
) -> Result<PrimitiveSymbolLocationV1> {
    let metadata = required_graph_metadata(symbol)?;
    Ok(PrimitiveSymbolLocationV1 {
        node_id: symbol.occurrence.as_str().to_owned(),
        name: metadata.simple_name.clone(),
        qualified_name: metadata.qualified_name.clone(),
        kind: metadata.kind.clone(),
        file: required_graph_file_path(symbol)?.to_owned(),
        start_line: metadata.start_line.saturating_add(1),
        end_line: graph_symbol_end_line(metadata)?.saturating_add(1),
        unavailable_fields: vec!["attrs_start_line".to_owned()],
    })
}

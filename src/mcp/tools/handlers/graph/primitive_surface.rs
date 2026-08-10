//! Typed wire projections shared by the primitive graph handlers.

use tracedecay_application::retrieval::{
    ContextMemoryFactV1, ContextMemoryMatchV1, PrimitiveLaneCompleteV1, PrimitiveLaneStateV1,
    PrimitiveLaneStatusV1, PrimitiveNotFoundV1, PrimitiveRecallV1, PrimitiveSearchCoverageV1,
    PrimitiveSemanticModeV1, PrimitiveSymbolLocationV1,
};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;

use crate::errors::Result;
use crate::mcp::tools::ToolResult;

use super::super::support::text_tool_result;
use super::verified::{graph_symbol_end_line, required_graph_file_path, required_graph_metadata};

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

pub(super) fn memory_match(hit: &crate::memory::types::FactSearchResult) -> ContextMemoryMatchV1 {
    let fact = &hit.fact;
    ContextMemoryMatchV1 {
        fact: ContextMemoryFactV1 {
            fact_id: fact.fact_id,
            content: fact.content.clone(),
            category: fact.category.to_string(),
            tags: fact.tags.clone(),
            entities: fact.entities.clone(),
            trust_score: fact.trust_score,
            source: fact.source.clone(),
            retrieval_count: fact.retrieval_count,
            access_count: fact.access_count,
            helpful_count: fact.helpful_count,
            unhelpful_count: fact.unhelpful_count,
            created_at: fact.created_at,
            updated_at: fact.updated_at,
            last_retrieved_at: fact.last_retrieved_at,
            last_recalled_at: fact.last_recalled_at,
            last_feedback_at: fact.last_feedback_at,
            metadata: fact.metadata.clone(),
        },
        score: hit.score,
        fts_score: hit.fts_score,
        jaccard_score: hit.jaccard_score,
        holographic_score: hit.holographic_score,
        trust_score: hit.trust_score,
        why: hit.why.clone(),
    }
}

pub(super) fn node_not_found(node_id: &str) -> Result<ToolResult> {
    let output = PrimitiveNotFoundV1 {
        status: "not_found".to_owned(),
        reason_code: "node_not_found".to_owned(),
        node_id: node_id.to_owned(),
        message: format!("Node not found: {node_id}"),
    };
    Ok(
        text_tool_result(&serde_json::to_string_pretty(&output)?, vec![])
            .with_semantic_error(true)
            .with_failure_message(format!("node not found: {node_id}")),
    )
}

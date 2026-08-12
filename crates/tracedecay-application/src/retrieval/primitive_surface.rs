//! Canonical CLI/MCP wire contracts for the established primitive tools.
//!
//! These types own the JSON decoded by the daemon handlers and the JSON
//! schemas projected into both public SDKs. Presentation-only transport keys
//! such as `format` and registered-project selectors are removed before these
//! request bodies are decoded.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::memory::FactSearchHitV1;

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveSemanticModeV1 {
    FallbackAllowed,
    StrictSemantic,
}

impl PrimitiveSemanticModeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FallbackAllowed => "fallback_allowed",
            Self::StrictSemantic => "strict_semantic",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextModeV1 {
    Explore,
    Plan,
}

impl ContextModeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Plan => "plan",
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSurfaceRequestV1 {
    pub task: String,
    pub max_nodes: Option<u32>,
    pub include_code: Option<bool>,
    pub max_code_blocks: Option<u32>,
    pub mode: Option<ContextModeV1>,
    pub include_memory: Option<bool>,
    pub memory_limit: Option<u32>,
    pub memory_min_trust: Option<f64>,
    pub semantic_mode: Option<PrimitiveSemanticModeV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDepthSurfaceRequestV1 {
    pub node_id: String,
    pub max_depth: Option<u32>,
}

pub type ImpactSurfaceRequestV1 = NodeDepthSurfaceRequestV1;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalleesSurfaceRequestV1 {
    pub node_id: String,
    pub max_depth: Option<u32>,
    pub resolve_dispatch: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSurfaceRequestV1 {
    pub node_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SimilarSurfaceRequestV1 {
    pub symbol: String,
    pub limit: Option<u32>,
    pub semantic_mode: Option<PrimitiveSemanticModeV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewPrimitiveRequestV1 {
    pub node_id: String,
    pub new_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortStatusSurfaceRequestV1 {
    pub source_dir: String,
    pub target_dir: String,
    pub kinds: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortOrderSurfaceRequestV1 {
    pub source_dir: String,
    pub kinds: Option<Vec<String>>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RedundancySurfaceRequestV1 {
    pub path: Option<String>,
    pub min_lines: Option<u32>,
    pub max_pairs: Option<u32>,
    pub similarity_threshold: Option<f64>,
    pub include_naming_only: Option<bool>,
    pub include_generated_paths: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TodosSurfaceRequestV1 {
    pub kinds: Option<Vec<String>>,
    pub path: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveSymbolLocationV1 {
    pub node_id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub unavailable_fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCodeBlockV1 {
    pub node_id: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub code: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveLaneStateV1 {
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PrimitiveLaneStatusV1 {
    Complete(PrimitiveLaneCompleteV1),
    State {
        status: PrimitiveLaneStateV1,
        #[serde(skip_serializing_if = "Option::is_none")]
        generation: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveLaneCompleteV1 {
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveRecallV1 {
    Full,
    Partial,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveSearchCoverageV1 {
    pub exact: PrimitiveLaneStatusV1,
    pub lexical: PrimitiveLaneStatusV1,
    pub graph: PrimitiveLaneStatusV1,
    pub semantic: PrimitiveLaneStatusV1,
    pub recall: PrimitiveRecallV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextResultV1 {
    pub task: String,
    pub mode: ContextModeV1,
    pub code_generation: String,
    pub symbols: Vec<PrimitiveSymbolLocationV1>,
    pub related_symbols: Vec<PrimitiveSymbolLocationV1>,
    pub code: Vec<ContextCodeBlockV1>,
    pub coverage: PrimitiveSearchCoverageV1,
    pub memory_matches: Vec<FactSearchHitV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_matches_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalleeV1 {
    pub node_id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub edge_kind: String,
    pub dispatch_via_trait: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_from: Option<String>,
}

pub type CalleesResultV1 = Vec<CalleeV1>;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactNodeV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub depth: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactResultV1 {
    pub node_count: usize,
    pub complete: bool,
    pub unavailable_fields: Vec<String>,
    pub nodes: Vec<ImpactNodeV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeExpansionCostV1 {
    pub body: u64,
    pub full_file: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDetailsV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub qualified_name: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub visibility: String,
    pub branches: u32,
    pub loops: u32,
    pub max_nesting: u32,
    pub cyclomatic_complexity: u32,
    pub cost_to_expand: NodeExpansionCostV1,
    pub unavailable_fields: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveNotFoundV1 {
    pub status: String,
    pub reason_code: String,
    pub node_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum NodeResultV1 {
    Found(NodeDetailsV1),
    NotFound(PrimitiveNotFoundV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SimilarSymbolV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub signature: Option<String>,
    pub utility_micros: u64,
}

pub type SimilarResultV1 = Vec<SimilarSymbolV1>;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewNodeV1 {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub snippet: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewReferenceV1 {
    pub from_node_id: String,
    pub from_name: String,
    pub from_kind: String,
    pub edge_kind: String,
    pub file: String,
    pub line: u32,
    pub snippet: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewTextOnlyMatchV1 {
    pub file: String,
    pub text_only_count: usize,
    pub note: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenamePreviewPrimitiveResultV1 {
    pub read_only: bool,
    pub note: String,
    pub symbol: String,
    pub new_name: Option<String>,
    pub node: RenamePreviewNodeV1,
    pub reference_count: usize,
    pub references: Vec<RenamePreviewReferenceV1>,
    pub text_only_matches: Vec<RenamePreviewTextOnlyMatchV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum RenamePreviewPrimitiveOutcomeV1 {
    Preview(RenamePreviewPrimitiveResultV1),
    NotFound(PrimitiveNotFoundV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortMatchedSymbolV1 {
    pub name: String,
    pub source_kind: String,
    pub target_kind: String,
    pub source_file: String,
    pub target_file: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortUnmatchedSymbolV1 {
    pub name: String,
    pub kind: String,
    pub line: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortTargetOnlySymbolV1 {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortStatusResultV1 {
    pub source_dir: String,
    pub target_dir: String,
    pub source_count: usize,
    pub target_count: usize,
    pub matched: usize,
    pub unmatched: usize,
    pub target_only: usize,
    pub coverage_percent: f64,
    pub unmatched_by_file: BTreeMap<String, Vec<PortUnmatchedSymbolV1>>,
    pub matched_symbols: Vec<PortMatchedSymbolV1>,
    pub target_only_symbols: Vec<PortTargetOnlySymbolV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortOrderSymbolV1 {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortOrderLevelV1 {
    pub level: usize,
    pub description: String,
    pub symbols: Vec<PortOrderSymbolV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortCycleFileV1 {
    pub file: String,
    pub members_in_cycle: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortCycleSymbolV1 {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub in_cycle_out_degree: usize,
    pub in_cycle_in_degree: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortCycleAnchorV1 {
    pub name: String,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortCycleV1 {
    pub size: usize,
    pub files: Vec<PortCycleFileV1>,
    pub symbols: Vec<PortCycleSymbolV1>,
    pub entry_point: Option<PortCycleAnchorV1>,
    pub break_point_candidate: Option<PortCycleAnchorV1>,
    pub note: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortOrderResultV1 {
    pub source_dir: String,
    pub total_symbols: usize,
    pub returned: usize,
    pub levels: Vec<PortOrderLevelV1>,
    pub cycles: Vec<PortCycleV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TodoMarkerV1 {
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub text: String,
    pub enclosing: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TodosResultV1 {
    pub match_count: usize,
    pub by_kind: BTreeMap<String, u64>,
    pub markers: Vec<TodoMarkerV1>,
}

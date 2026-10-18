//! Shipped dependency-depth result and the coverage record primitive evidence
//! publishes. Lexical grep, AST grep, and dependency depth are GraphTool
//! operations; they have no primitive request or authority port.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::result::CoverageCompleteness;
use crate::retrieval::PrimitiveSearchFreshnessV1;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DependencyDepthChainV1 {
    pub file: String,
    pub depth: u64,
    pub chain: Vec<String>,
}

/// `ideal_depth` is `ceil(log2(file_count))`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DependencyDepthResultV1 {
    pub max_depth: u64,
    pub ideal_depth: u64,
    pub depth_score: f64,
    pub chains: Vec<DependencyDepthChainV1>,
    /// The worktree verdict a served graph read opens with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<PrimitiveSearchFreshnessV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveCoverageV1 {
    pub completeness: CoverageCompleteness,
    pub visited: Option<u64>,
    pub eligible: Option<u64>,
    pub returned: u64,
    pub unsupported_languages: Vec<String>,
}

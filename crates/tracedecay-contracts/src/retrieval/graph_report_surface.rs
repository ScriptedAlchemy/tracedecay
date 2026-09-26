//! Canonical CLI/MCP wire contracts for the graph-backed report tools the
//! project's graph-tool owner answers: code health, test attribution, and
//! compiler-diagnostic mapping.
//!
//! Presentation-only transport keys such as `format` are removed before these
//! request bodies are decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::ComplexityAnalysisV1;

/// Metric whose distribution the Gini report measures.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GiniMetricV1 {
    Complexity,
    Lines,
    FanIn,
    FanOut,
    Members,
}

/// Unit the Gini report aggregates its metric over.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GiniScopeV1 {
    File,
    Symbol,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GiniSurfaceRequestV1 {
    /// Metric to measure inequality for (default: complexity).
    pub metric: Option<GiniMetricV1>,
    /// Aggregate per file or per symbol (default: file).
    pub scope: Option<GiniScopeV1>,
    /// Filter to files under this directory path.
    pub path: Option<String>,
    /// Number of top outliers to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GiniOutlierV1 {
    pub name: String,
    pub value: f64,
    pub pct_of_max: f64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GiniResultV1 {
    pub gini: f64,
    pub interpretation: String,
    pub total_items: u64,
    pub metric: GiniMetricV1,
    pub scope: GiniScopeV1,
    /// Symbols left out because their bounded complexity walk did not cover
    /// the body; their counters are lower bounds and measure nothing here.
    pub incomplete_complexity_symbols: u64,
    pub outliers: Vec<GiniOutlierV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyDepthSurfaceRequestV1 {
    /// Filter to files under this directory path.
    pub path: Option<String>,
    /// Maximum number of chains to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSurfaceRequestV1 {
    /// Filter to files under this directory path.
    pub path: Option<String>,
    /// If true, include full dimension breakdown (default: false).
    pub details: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthAcyclicityV1 {
    pub score: f64,
    pub edges_in_cycles: u64,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthDepthV1 {
    pub score: f64,
    pub max_chain: u64,
    pub ideal_chain: u64,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthEqualityV1 {
    pub score: f64,
    pub gini: f64,
    pub interpretation: String,
    pub incomplete_complexity_symbols: u64,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthRedundancyV1 {
    pub score: f64,
    pub dead_count: u64,
    pub total_fns: u64,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthModularityV1 {
    pub score: f64,
    pub interpretation: String,
    pub components_after_hub_removal: u64,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthCoverageDisciplineV1 {
    pub score: f64,
    pub skip_test_coverage_count: u64,
    pub total_fns: u64,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthDimensionsV1 {
    pub acyclicity: HealthAcyclicityV1,
    pub depth: HealthDepthV1,
    pub equality: HealthEqualityV1,
    pub redundancy: HealthRedundancyV1,
    pub modularity: HealthModularityV1,
    pub coverage_discipline: HealthCoverageDisciplineV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthWeightsV1 {
    pub note: String,
}

/// `dimensions` and `weights` are present exactly when the request asked for
/// `details`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResultV1 {
    /// Geometric mean of the dimensions, scaled to 0-10000.
    pub quality_signal: u32,
    pub files_analyzed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<HealthDimensionsV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<HealthWeightsV1>,
}

/// Shape of the design-structure-matrix report.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DsmShapeV1 {
    #[default]
    Stats,
    Clusters,
    Matrix,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DsmSurfaceRequestV1 {
    /// Filter to files under this directory path.
    pub path: Option<String>,
    /// DSM data shape: stats, clusters, or matrix (default: stats).
    pub shape: Option<DsmShapeV1>,
    /// Maximum files in matrix format (default: 30, at most 200).
    pub max_files: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DsmStatsV1 {
    pub files: u64,
    pub edges: u64,
    pub density: f64,
    pub clusters: u64,
    pub largest_cluster: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DsmClusterV1 {
    pub directory: String,
    pub file_count: u64,
    pub internal_edges: u64,
    pub outgoing_edges: u64,
    pub incoming_edges: u64,
    pub boundary_edges: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DsmMatrixV1 {
    pub files: Vec<String>,
    pub matrix: Vec<Vec<u8>>,
    pub note: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DsmResultV1 {
    pub shape: DsmShapeV1,
    pub stats: DsmStatsV1,
    pub clusters: Vec<DsmClusterV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix: Option<DsmMatrixV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestMapSurfaceRequestV1 {
    /// Source file path to find test coverage for.
    pub file: Option<String>,
    /// Specific node ID to find test coverage for (alternative to file).
    #[serde(alias = "id")]
    pub node_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestMapTestV1 {
    pub test_name: String,
    pub test_file: String,
    pub test_line: u32,
    pub attribution_depth: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestMapSourceCoverageV1 {
    pub source_name: String,
    pub source_id: String,
    pub source_file: String,
    pub source_line: u32,
    pub tests: Vec<TestMapTestV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestMapUncoveredV1 {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestMapResultV1 {
    pub covered_symbols: u64,
    pub uncovered_symbols: u64,
    pub test_files: Vec<String>,
    pub coverage: Vec<TestMapSourceCoverageV1>,
    pub uncovered: Vec<TestMapUncoveredV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRiskSurfaceRequestV1 {
    /// Maximum number of results to return (default: 20, at most 200).
    pub limit: Option<u32>,
    /// Filter to files under this directory path.
    pub path: Option<String>,
    /// Include already-tested functions in results (default: false).
    pub include_tested: Option<bool>,
}

/// How a source symbol's static test evidence was attributed.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestAttributionMethodV1 {
    None,
    DirectUnit,
    Closure,
}

/// What the reported coverage percentage bounds.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestRiskConfidenceV1 {
    StaticLowerBound,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRiskEntryV1 {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: u32,
    /// `None` when `complexity_analysis` reports the bounded walk did not
    /// cover the body; `risk` then weighs the lower-bound counters.
    pub complexity: Option<u32>,
    pub complexity_analysis: ComplexityAnalysisV1,
    pub fan_in: u64,
    pub has_test: bool,
    pub attribution_method: TestAttributionMethodV1,
    pub attribution_depth: Option<u64>,
    pub risk: f64,
    pub churn: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRiskAttributionSummaryV1 {
    pub depth: u64,
    pub direct_unit_attributed: u64,
    pub closure_attributed: u64,
    pub trait_resolved_attributed: u64,
    pub public_api_attributed: u64,
    pub cli_entry_attributed: u64,
    pub total_attributed: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRiskBucketSummaryV1 {
    pub attributed: u64,
    pub reachable_unattributed: u64,
    pub orphan_entry: u64,
    pub excluded: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRiskSummaryV1 {
    pub total_functions: u64,
    pub tested: u64,
    pub skipped: u64,
    pub coverage_pct: f64,
    pub top_risk_untested: String,
    pub top_risk_unattributed: String,
    pub attribution: TestRiskAttributionSummaryV1,
    pub buckets: TestRiskBucketSummaryV1,
    pub confidence: TestRiskConfidenceV1,
    pub confidence_note: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestRiskResultV1 {
    pub risks: Vec<TestRiskEntryV1>,
    pub summary: TestRiskSummaryV1,
}

/// Compiler severity the diagnose report keeps.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnoseSeverityFilterV1 {
    Error,
    Warning,
    #[default]
    All,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnoseSurfaceRequestV1 {
    /// Raw stderr text from `cargo check` / `cargo clippy` / `rustc`.
    pub cargo_output: String,
    /// Filter by severity (default: all).
    pub severity: Option<DiagnoseSeverityFilterV1>,
    /// Attach up to 5 callers per diagnostic (default: true).
    pub include_callers: Option<bool>,
    /// Cap on diagnostics in the response (default: 50, at most 500).
    pub max_diagnostics: Option<u32>,
}

/// Severity of one parsed compiler diagnostic.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnoseSeverityV1 {
    Error,
    Warning,
    Note,
    Help,
}

/// The graph symbol a diagnostic line falls in, or one of its callers.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnoseSymbolV1 {
    pub node_id: String,
    pub name: String,
    pub kind: String,
    pub qualified_name: String,
    pub file: String,
    /// One-based display line.
    pub line: u32,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnoseItemV1 {
    pub severity: DiagnoseSeverityV1,
    pub code: Option<String>,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub node: Option<DiagnoseSymbolV1>,
    /// Absent when the request turned caller attachment off.
    pub callers: Option<Vec<DiagnoseSymbolV1>>,
}

/// Outcome of publishing the parsed diagnostics into the managed store.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosePublicationV1 {
    Skipped {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unresolved: Option<Vec<String>>,
    },
    Published {
        generation: String,
        publication_revision: u64,
        inserted: u64,
        cleared: u64,
        unresolved: Vec<String>,
        rejected: Vec<String>,
    },
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnoseResultV1 {
    pub diagnostics_parsed: u64,
    pub diagnostics_returned: u64,
    pub mapped_to_node: u64,
    pub unmapped: u64,
    pub truncated: bool,
    pub published: DiagnosePublicationV1,
    pub diagnostics: Vec<DiagnoseItemV1>,
}

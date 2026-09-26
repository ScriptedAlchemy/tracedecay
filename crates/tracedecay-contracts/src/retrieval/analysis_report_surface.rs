//! Canonical CLI/MCP wire contracts for the structural-analysis reports the
//! project's graph-tool owner answers: dead code, cycles, rankings,
//! distributions, documentation coverage, risky-pattern scans, and the
//! struct-literal and field-site scans.
//!
//! Presentation-only transport keys such as `format` are removed before these
//! request bodies are decoded.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::ComplexityAnalysisV1;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeadCodeSurfaceRequestV1 {
    /// Filter reported symbols to files under this directory path (e.g.
    /// 'crates/tracedecay-mcp'). Omit for the entire codebase.
    pub path: Option<String>,
    /// Node kinds to check (default: ["function", "method"]).
    pub kinds: Option<Vec<String>>,
    /// When true, do NOT exclude pub items. Default false.
    pub include_public: Option<bool>,
    /// Maximum symbols to return (default: 100, max: 1000).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeadCodeSymbolV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeadCodeResultV1 {
    pub dead_code_count: u64,
    pub symbols: Vec<DeadCodeSymbolV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CircularSurfaceRequestV1 {
    /// Maximum number of cycles to report, largest first (default: 25, max:
    /// 200). The response always states the total detected and how many were
    /// omitted.
    pub limit: Option<u32>,
    /// Maximum member files listed per reported cycle (default: 12, max:
    /// 200). Each entry states its true member_count and
    /// omitted_member_count.
    pub member_limit: Option<u32>,
}

/// One reported file cycle: the members that fit the member bound, plus the
/// component's true size so the omission is stated rather than hidden.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CircularCycleV1 {
    pub members: Vec<String>,
    pub member_count: u64,
    pub omitted_member_count: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CircularResultV1 {
    pub cycle_count: u64,
    pub reported_cycle_count: u64,
    pub omitted_cycle_count: u64,
    pub limit: u64,
    pub member_limit: u64,
    pub cycles: Vec<CircularCycleV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HotspotsSurfaceRequestV1 {
    /// Maximum number of hotspots to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HotspotV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub incoming: u64,
    pub outgoing: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HotspotsResultV1 {
    pub hotspot_count: u64,
    pub hotspots: Vec<HotspotV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnmountedFilesSurfaceRequestV1 {
    /// Filter findings to files under this directory path (e.g. 'src/daemon').
    /// The whole project is still walked, reachability is not a per-directory
    /// question.
    pub path: Option<String>,
    /// Filter findings to one ecosystem ('rust' or 'typescript'). Every
    /// ecosystem section is still reported, so the scope of the answer stays
    /// visible.
    pub ecosystem: Option<String>,
    /// Maximum unmounted files to return (default: 200, max: 2000). The
    /// response always states the true total and how many rows were omitted.
    pub limit: Option<u32>,
}

/// Whether an ecosystem was audited, absent, or recognised but unmodelled.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmountedEcosystemStatusV1 {
    Audited,
    NotPresent,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnmountedEcosystemV1 {
    pub ecosystem: String,
    pub status: UnmountedEcosystemStatusV1,
    pub package_count: u64,
    pub entry_point_count: u64,
    pub scanned_file_count: u64,
    pub mounted_file_count: u64,
    pub unclaimed_file_count: u64,
    pub unmounted_file_count: u64,
    pub verdict: String,
    pub blind_spots: Vec<String>,
    pub note: Option<String>,
    pub excluded_path_globs: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnmountedFileV1 {
    pub file: String,
    pub ecosystem: String,
    pub package: String,
    pub manifest: String,
    pub nearest_mounted_parent: Option<String>,
    pub suggested_declaration: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnmountedFilesResultV1 {
    pub unmounted_file_count: u64,
    pub returned_count: u64,
    pub omitted_count: u64,
    pub complete: bool,
    pub ecosystems: Vec<UnmountedEcosystemV1>,
    pub limit: u64,
    pub path: Option<String>,
    pub ecosystem: Option<String>,
    pub unmounted: Vec<UnmountedFileV1>,
}

/// Relationship the rank report counts.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankEdgeKindV1 {
    Implements,
    Extends,
    Calls,
    Uses,
    Contains,
    Annotates,
    DerivesMacro,
    TypeOf,
    Returns,
    Receives,
}

/// Which endpoint of each counted edge the rank report ranks.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankDirectionV1 {
    #[default]
    Incoming,
    Outgoing,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RankSurfaceRequestV1 {
    /// The relationship type to rank by (e.g. 'implements' to find
    /// most-implemented interfaces).
    pub edge_kind: RankEdgeKindV1,
    /// Edge direction: 'incoming' ranks targets (default, e.g.
    /// most-implemented interface), 'outgoing' ranks sources (e.g. class that
    /// implements the most interfaces).
    pub direction: Option<RankDirectionV1>,
    /// Optional filter for node kind (e.g. 'interface', 'class', 'trait',
    /// 'function', 'method').
    pub node_kind: Option<String>,
    /// Filter to files under this directory path (e.g. 'src/main/java').
    pub path: Option<String>,
    /// Maximum number of results to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RankEntryV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub count: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RankResultV1 {
    pub edge_kind: RankEdgeKindV1,
    pub direction: RankDirectionV1,
    pub node_kind_filter: Option<String>,
    pub result_count: u64,
    pub ranking: Vec<RankEntryV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LargestSurfaceRequestV1 {
    /// Filter by node kind (e.g. 'class', 'method', 'function', 'interface',
    /// 'enum', 'struct').
    pub node_kind: Option<String>,
    /// Filter to files under this directory path (e.g. 'src/main/java').
    pub path: Option<String>,
    /// Maximum number of results to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LargestEntryV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    pub lines: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LargestResultV1 {
    pub node_kind_filter: Option<String>,
    pub result_count: u64,
    pub ranking: Vec<LargestEntryV1>,
}

/// Which side of a file dependency the coupling report counts.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CouplingDirectionV1 {
    #[default]
    FanIn,
    FanOut,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CouplingSurfaceRequestV1 {
    /// fan_in: files depended on by the most others. fan_out: files that
    /// depend on the most others (default: fan_in).
    pub direction: Option<CouplingDirectionV1>,
    /// Filter to files under this directory path (e.g. 'src/main/java').
    pub path: Option<String>,
    /// Maximum number of results to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CouplingEntryV1 {
    pub file: String,
    pub coupled_files: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CouplingResultV1 {
    pub direction: CouplingDirectionV1,
    pub result_count: u64,
    pub ranking: Vec<CouplingEntryV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InheritanceDepthSurfaceRequestV1 {
    /// Filter to files under this directory path (e.g. 'src/main/java').
    pub path: Option<String>,
    /// Maximum number of results to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InheritanceDepthEntryV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub depth: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InheritanceDepthResultV1 {
    pub result_count: u64,
    pub ranking: Vec<InheritanceDepthEntryV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionSurfaceRequestV1 {
    /// Directory or file path prefix to filter (e.g.
    /// 'src/main/java/com/example'). Omit for entire codebase.
    pub path: Option<String>,
    /// If true, aggregate counts across all matching files instead of
    /// per-file breakdown (default: false).
    pub summary: Option<bool>,
    /// Maximum number of files in the per-file breakdown, highest node count
    /// first (default: 100, max: 1000). Ignored when summary is true; the
    /// response states total_file_count and omitted_file_count.
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionKindCountV1 {
    pub kind: String,
    pub count: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionFileV1 {
    pub file: String,
    pub kinds: Vec<DistributionKindCountV1>,
}

/// The distribution report's view, tagged by `mode`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DistributionViewV1 {
    Summary {
        total_kinds: u64,
        distribution: Vec<DistributionKindCountV1>,
    },
    PerFile {
        file_count: u64,
        total_file_count: u64,
        omitted_file_count: u64,
        files: Vec<DistributionFileV1>,
    },
}

/// Flattening the tagged view keeps `path_filter` ahead of `mode` on the
/// wire; serde cannot deny unknown fields through a flattened member.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
pub struct DistributionResultV1 {
    pub path_filter: Option<String>,
    #[serde(flatten)]
    pub view: DistributionViewV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecursionSurfaceRequestV1 {
    /// Filter to files under this directory path (e.g. 'src/main/java').
    pub path: Option<String>,
    /// Maximum number of cycles to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecursionSymbolV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

/// One call cycle; `chain` repeats its first symbol at the end.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecursionCycleV1 {
    pub length: u64,
    pub chain: Vec<RecursionSymbolV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecursionResultV1 {
    pub cycle_count: u64,
    pub cycles: Vec<RecursionCycleV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexitySurfaceRequestV1 {
    /// Filter by node kind (default: every kind).
    pub node_kind: Option<String>,
    /// Filter to files under this directory path (e.g. 'src/main/java').
    pub path: Option<String>,
    /// Maximum number of results to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityReportEntryV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub lines: u32,
    /// The counters are `None` when `complexity_analysis` reports the bounded
    /// walk did not cover the body.
    pub cyclomatic_complexity: Option<u32>,
    pub branches: Option<u32>,
    pub loops: Option<u32>,
    pub max_nesting: Option<u32>,
    pub complexity_analysis: ComplexityAnalysisV1,
    pub fan_out: u64,
    pub fan_in: u64,
    pub score: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityReportV1 {
    pub formula: String,
    pub note: String,
    pub result_count: u64,
    pub ranking: Vec<ComplexityReportEntryV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocCoverageSurfaceRequestV1 {
    /// Directory or file path prefix to filter (e.g. 'src/main'). Omit for
    /// entire codebase.
    pub path: Option<String>,
    /// Maximum number of results to return (default: 50, at most 500).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocCoverageSymbolV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocCoverageFileV1 {
    pub file: String,
    pub count: u64,
    pub symbols: Vec<DocCoverageSymbolV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocCoverageResultV1 {
    pub path_filter: Option<String>,
    pub total_undocumented: u64,
    pub returned_count: u64,
    pub omitted_count: u64,
    pub complete: bool,
    pub limit: u64,
    pub file_count: u64,
    pub files: Vec<DocCoverageFileV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GodClassSurfaceRequestV1 {
    /// Filter to files under this directory path (e.g. 'src/main/java').
    pub path: Option<String>,
    /// Maximum number of results to return (default: 10, at most 100).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GodClassEntryV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub methods: u64,
    pub fields: u64,
    pub total_members: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GodClassResultV1 {
    pub result_count: u64,
    pub ranking: Vec<GodClassEntryV1>,
}

/// Risky construct the pattern scan looks for.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsafePatternKindV1 {
    Unwrap,
    Expect,
    Panic,
    Todo,
    Unimplemented,
    UnsafeBlock,
}

impl UnsafePatternKindV1 {
    pub const ALL: [Self; 6] = [
        Self::Unwrap,
        Self::Expect,
        Self::Panic,
        Self::Todo,
        Self::Unimplemented,
        Self::UnsafeBlock,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unwrap => "unwrap",
            Self::Expect => "expect",
            Self::Panic => "panic",
            Self::Todo => "todo",
            Self::Unimplemented => "unimplemented",
            Self::UnsafeBlock => "unsafe_block",
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnsafePatternsSurfaceRequestV1 {
    /// Subset of patterns to search. Default (or an empty list): every kind.
    pub kinds: Option<Vec<UnsafePatternKindV1>>,
    /// Filter to files under this directory (relative to project root).
    pub path: Option<String>,
    /// When true, skips files whose path looks like a test (default: false).
    pub exclude_tests: Option<bool>,
    /// Maximum number of matches to return (default: 200, max: 2000).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnsafePatternMatchV1 {
    pub kind: UnsafePatternKindV1,
    pub file: String,
    pub line: u32,
    pub snippet: String,
    pub enclosing: Option<String>,
    pub in_test: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnsafePatternsResultV1 {
    pub match_count: u64,
    /// Match count per pattern kind, keyed by its wire spelling.
    pub by_kind: BTreeMap<String, u64>,
    pub matches: Vec<UnsafePatternMatchV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConstructorsSurfaceRequestV1 {
    /// Struct name to search literal sites of (e.g. 'GraphStats', 'Config').
    #[serde(rename = "struct")]
    pub struct_name: String,
    /// Maximum number of literal sites to return (default: 100, max: 1000).
    pub limit: Option<u32>,
}

/// Whether a literal site's field lists are complete relative to the
/// struct's graph definition.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstructorFieldCoverageV1 {
    Complete,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConstructorSiteV1 {
    pub file: String,
    pub line: u32,
    pub fields: Vec<String>,
    pub update_fields: Vec<String>,
    pub missing_fields: Vec<String>,
    pub field_coverage: ConstructorFieldCoverageV1,
}

/// Syntax alone cannot link same-name types across modules, so every
/// resolution is unverified.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstructorResolutionStatusV1 {
    Unverified,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstructorResolutionReasonV1 {
    AmbiguousSimpleName,
    SyntaxOnlySimpleName,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConstructorsNotFoundV1 {
    /// Always `false`: no struct, class, or case class has the name.
    pub found: bool,
    #[serde(rename = "struct")]
    pub struct_name: String,
    pub message: String,
    pub match_count: u64,
    pub sites: Vec<ConstructorSiteV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConstructorsReportV1 {
    #[serde(rename = "struct")]
    pub struct_name: String,
    pub candidate_count: u64,
    pub resolution_status: ConstructorResolutionStatusV1,
    pub resolution_reason: ConstructorResolutionReasonV1,
    /// `None` when the name resolves to more than one definition.
    pub expected_fields: Option<Vec<String>>,
    pub match_count: u64,
    pub sites: Vec<ConstructorSiteV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ConstructorsResultV1 {
    NotFound(ConstructorsNotFoundV1),
    Report(ConstructorsReportV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSitesSurfaceRequestV1 {
    /// Field name. Bare name ('last_sync_at') matches across structs;
    /// qualified form ('GraphStats::last_sync_at') narrows to one struct's
    /// field.
    pub field: String,
    /// When true, returns only write_sites and omits reads. Default false.
    pub writes_only: Option<bool>,
    /// Maximum sites per kind (default: 200, max: 2000).
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSiteV1 {
    pub file: String,
    pub line: u32,
    pub enclosing: Option<String>,
    pub snippet: String,
}

/// `read_count` and `read_sites` are absent when the request asked for
/// writes only.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSitesResultV1 {
    pub field: String,
    pub qualifier: Option<String>,
    pub qualifier_applied: bool,
    pub write_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_count: Option<u64>,
    pub write_sites: Vec<FieldSiteV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_sites: Option<Vec<FieldSiteV1>>,
}

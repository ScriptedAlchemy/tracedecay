//! Typed terminals for the graph- and git-backed reads and reports whose
//! results are their catalog result schemas, plus the files they report beside
//! the result.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::InvocationAnalyticsV1;
use crate::retrieval::{
    AffectedResultV1, AstGrepSearchResultV1, BranchDiffResultV1, BranchListResultV1,
    BranchSearchResultV1, ByQualifiedNameResultV1, ChangelogResultV1, CircularResultV1,
    CommitContextResultV1, ComplexityReportV1, ConfigResultV1, ConstructorsResultV1,
    ContextResultV1, CouplingResultV1, DeadCodeResultV1, DependencyDepthResultV1, DerivesResultV1,
    DiagnoseResultV1, DiffContextResultV1, DistributionResultV1, DocCoverageResultV1, DsmResultV1,
    FieldSitesResultV1, FilesResultV1, FindExactSymbolResultV1, GiniResultV1, GodClassResultV1,
    GrepSearchResultV1, HealthResultV1, HotspotsResultV1, ImpactResultV1, InheritanceDepthResultV1,
    LargestResultV1, NodeResultV1, PortOrderResultV1, PortStatusResultV1, PrContextResultV1,
    RankResultV1, RecursionResultV1, RedundancyResultV1, RenamePreviewPrimitiveOutcomeV1,
    SignatureResultV1, SimilarResultV1, TestMapResultV1, TestRiskResultV1, TodosResultV1,
    UnmountedFilesResultV1, UnsafePatternsResultV1,
};

/// One graph read's typed result, tagged by its operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", content = "result", rename_all = "snake_case")]
pub enum GraphToolResultV1 {
    Context(Box<ContextResultV1>),
    Node(NodeResultV1),
    Impact(ImpactResultV1),
    Similar(SimilarResultV1),
    Redundancy(RedundancyResultV1),
    RenamePreview(RenamePreviewPrimitiveOutcomeV1),
    PortStatus(PortStatusResultV1),
    PortOrder(PortOrderResultV1),
    Todos(TodosResultV1),
    TestMap(TestMapResultV1),
    TestRisk(TestRiskResultV1),
    Gini(GiniResultV1),
    DependencyDepth(DependencyDepthResultV1),
    Health(HealthResultV1),
    Dsm(DsmResultV1),
    Diagnose(DiagnoseResultV1),
    DeadCode(DeadCodeResultV1),
    Circular(CircularResultV1),
    Hotspots(HotspotsResultV1),
    UnmountedFiles(UnmountedFilesResultV1),
    Rank(RankResultV1),
    Largest(LargestResultV1),
    Coupling(CouplingResultV1),
    InheritanceDepth(InheritanceDepthResultV1),
    Distribution(DistributionResultV1),
    Recursion(RecursionResultV1),
    Complexity(ComplexityReportV1),
    DocCoverage(DocCoverageResultV1),
    GodClass(GodClassResultV1),
    UnsafePatterns(UnsafePatternsResultV1),
    Constructors(ConstructorsResultV1),
    FieldSites(FieldSitesResultV1),
    FindExactSymbol(FindExactSymbolResultV1),
    ByQualifiedName(ByQualifiedNameResultV1),
    Signature(SignatureResultV1),
    Derives(DerivesResultV1),
    Grep(GrepSearchResultV1),
    AstGrepSearch(AstGrepSearchResultV1),
    Affected(AffectedResultV1),
    DiffContext(DiffContextResultV1),
    Changelog(ChangelogResultV1),
    CommitContext(CommitContextResultV1),
    PrContext(PrContextResultV1),
    BranchSearch(BranchSearchResultV1),
    BranchDiff(BranchDiffResultV1),
    BranchList(BranchListResultV1),
    Files(FilesResultV1),
    Config(ConfigResultV1),
}

impl GraphToolResultV1 {
    /// Decodes `operation`'s catalog result body.
    pub fn from_result_value(
        operation: tracedecay_tool_catalog::ApplicationSurfaceOperation,
        value: serde_json::Value,
    ) -> serde_json::Result<Self> {
        use tracedecay_tool_catalog::ApplicationSurfaceOperation as Operation;
        Ok(match operation {
            Operation::Context => Self::Context(serde_json::from_value(value)?),
            Operation::Node => Self::Node(serde_json::from_value(value)?),
            Operation::Impact => Self::Impact(serde_json::from_value(value)?),
            Operation::Similar => Self::Similar(serde_json::from_value(value)?),
            Operation::Redundancy => Self::Redundancy(serde_json::from_value(value)?),
            Operation::RenamePreview => Self::RenamePreview(serde_json::from_value(value)?),
            Operation::PortStatus => Self::PortStatus(serde_json::from_value(value)?),
            Operation::PortOrder => Self::PortOrder(serde_json::from_value(value)?),
            Operation::Todos => Self::Todos(serde_json::from_value(value)?),
            Operation::TestMap => Self::TestMap(serde_json::from_value(value)?),
            Operation::TestRisk => Self::TestRisk(serde_json::from_value(value)?),
            Operation::Gini => Self::Gini(serde_json::from_value(value)?),
            Operation::DependencyDepth => Self::DependencyDepth(serde_json::from_value(value)?),
            Operation::Health => Self::Health(serde_json::from_value(value)?),
            Operation::Dsm => Self::Dsm(serde_json::from_value(value)?),
            Operation::Diagnose => Self::Diagnose(serde_json::from_value(value)?),
            Operation::DeadCode => Self::DeadCode(serde_json::from_value(value)?),
            Operation::Circular => Self::Circular(serde_json::from_value(value)?),
            Operation::Hotspots => Self::Hotspots(serde_json::from_value(value)?),
            Operation::UnmountedFiles => Self::UnmountedFiles(serde_json::from_value(value)?),
            Operation::Rank => Self::Rank(serde_json::from_value(value)?),
            Operation::Largest => Self::Largest(serde_json::from_value(value)?),
            Operation::Coupling => Self::Coupling(serde_json::from_value(value)?),
            Operation::InheritanceDepth => Self::InheritanceDepth(serde_json::from_value(value)?),
            Operation::Distribution => Self::Distribution(serde_json::from_value(value)?),
            Operation::Recursion => Self::Recursion(serde_json::from_value(value)?),
            Operation::Complexity => Self::Complexity(serde_json::from_value(value)?),
            Operation::DocCoverage => Self::DocCoverage(serde_json::from_value(value)?),
            Operation::GodClass => Self::GodClass(serde_json::from_value(value)?),
            Operation::UnsafePatterns => Self::UnsafePatterns(serde_json::from_value(value)?),
            Operation::Constructors => Self::Constructors(serde_json::from_value(value)?),
            Operation::FieldSites => Self::FieldSites(serde_json::from_value(value)?),
            Operation::FindExactSymbol => Self::FindExactSymbol(serde_json::from_value(value)?),
            Operation::ByQualifiedName => Self::ByQualifiedName(serde_json::from_value(value)?),
            Operation::Signature => Self::Signature(serde_json::from_value(value)?),
            Operation::Derives => Self::Derives(serde_json::from_value(value)?),
            Operation::Grep => Self::Grep(serde_json::from_value(value)?),
            Operation::AstGrepSearch => Self::AstGrepSearch(serde_json::from_value(value)?),
            Operation::Affected => Self::Affected(serde_json::from_value(value)?),
            Operation::DiffContext => Self::DiffContext(serde_json::from_value(value)?),
            Operation::Changelog => Self::Changelog(serde_json::from_value(value)?),
            Operation::CommitContext => Self::CommitContext(serde_json::from_value(value)?),
            Operation::PrContext => Self::PrContext(serde_json::from_value(value)?),
            Operation::BranchSearch => Self::BranchSearch(serde_json::from_value(value)?),
            Operation::BranchDiff => Self::BranchDiff(serde_json::from_value(value)?),
            Operation::BranchList => Self::BranchList(serde_json::from_value(value)?),
            Operation::Files => Self::Files(serde_json::from_value(value)?),
            Operation::Config => Self::Config(serde_json::from_value(value)?),
            operation => {
                return Err(serde::de::Error::custom(format!(
                    "{} is not a graph-tool operation",
                    operation.as_str()
                )));
            }
        })
    }

    /// The result body alone, the shape its catalog result schema names.
    pub fn result_value(&self) -> serde_json::Result<serde_json::Value> {
        match self {
            Self::Context(result) => serde_json::to_value(result),
            Self::Node(result) => serde_json::to_value(result),
            Self::Impact(result) => serde_json::to_value(result),
            Self::Similar(result) => serde_json::to_value(result),
            Self::Redundancy(result) => serde_json::to_value(result),
            Self::RenamePreview(result) => serde_json::to_value(result),
            Self::PortStatus(result) => serde_json::to_value(result),
            Self::PortOrder(result) => serde_json::to_value(result),
            Self::Todos(result) => serde_json::to_value(result),
            Self::TestMap(result) => serde_json::to_value(result),
            Self::TestRisk(result) => serde_json::to_value(result),
            Self::Gini(result) => serde_json::to_value(result),
            Self::DependencyDepth(result) => serde_json::to_value(result),
            Self::Health(result) => serde_json::to_value(result),
            Self::Dsm(result) => serde_json::to_value(result),
            Self::Diagnose(result) => serde_json::to_value(result),
            Self::DeadCode(result) => serde_json::to_value(result),
            Self::Circular(result) => serde_json::to_value(result),
            Self::Hotspots(result) => serde_json::to_value(result),
            Self::UnmountedFiles(result) => serde_json::to_value(result),
            Self::Rank(result) => serde_json::to_value(result),
            Self::Largest(result) => serde_json::to_value(result),
            Self::Coupling(result) => serde_json::to_value(result),
            Self::InheritanceDepth(result) => serde_json::to_value(result),
            Self::Distribution(result) => serde_json::to_value(result),
            Self::Recursion(result) => serde_json::to_value(result),
            Self::Complexity(result) => serde_json::to_value(result),
            Self::DocCoverage(result) => serde_json::to_value(result),
            Self::GodClass(result) => serde_json::to_value(result),
            Self::UnsafePatterns(result) => serde_json::to_value(result),
            Self::Constructors(result) => serde_json::to_value(result),
            Self::FieldSites(result) => serde_json::to_value(result),
            Self::FindExactSymbol(result) => serde_json::to_value(result),
            Self::ByQualifiedName(result) => serde_json::to_value(result),
            Self::Signature(result) => serde_json::to_value(result),
            Self::Derives(result) => serde_json::to_value(result),
            Self::Grep(result) => serde_json::to_value(result),
            Self::AstGrepSearch(result) => serde_json::to_value(result),
            Self::Affected(result) => serde_json::to_value(result),
            Self::DiffContext(result) => serde_json::to_value(result),
            Self::Changelog(result) => serde_json::to_value(result),
            Self::CommitContext(result) => serde_json::to_value(result),
            Self::PrContext(result) => serde_json::to_value(result),
            Self::BranchSearch(result) => serde_json::to_value(result),
            Self::BranchDiff(result) => serde_json::to_value(result),
            Self::BranchList(result) => serde_json::to_value(result),
            Self::Files(result) => serde_json::to_value(result),
            Self::Config(result) => serde_json::to_value(result),
        }
    }
}

/// A completed graph read: the typed result and what it reports beside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphToolCompletionV1 {
    pub result: GraphToolResultV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_graph: Option<crate::retrieval::ServedCodeGraphGenerationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analytics: Option<InvocationAnalyticsV1>,
    /// What a metered read cost the stores that served it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<crate::RequestCostReceiptV1>,
}

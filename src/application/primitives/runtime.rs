//! Concrete composition root for the PR12 compatibility primitive families.
//!
//! This module owns wiring only. Every supplied dependency remains the
//! production authority for its concern, including cursor authentication,
//! source caching, test attribution, lexical matching, health, configuration,
//! diagnostics, project discovery, and administrative reads.

use tracedecay_application::retrieval::grep_analysis::{
    LexicalGrepAuthorityV1, RedundancyAuthorityV1,
};
use tracedecay_application::retrieval::{AffectedTestsRetrievalPort, SourceRetrievalPort};

use super::grep_analysis::{
    ProductionGrepAnalysisOperationsV1, production_grep_analysis_operations,
};
use super::operations::{
    AdminReadOperations, ConfigurationReadOperations, DiagnosticsReadOperations,
    FilesReadOperations, HealthReadOperations, ModuleApiReadOperations,
    OperationalPrimitiveOwner, ProjectReadOperations, StatusReadOperations,
};
use super::source::{CanonicalSourcePrimitiveOwner, SourceReadOperations};
use super::symbol_graph::{CanonicalSymbolGraphAdapter, SymbolGraphCursorPort};
use super::tests::{
    AffectedFileTestOperations, CanonicalTestPrimitiveOwner, TestMapOperations,
};
use crate::TraceDecay;

/// Production dependencies required by the PR12 primitive composition root.
///
/// The bundle makes ownership explicit without introducing another service
/// locator. Callers pass the existing single-root authorities they already
/// own; [`Pr12PrimitiveRuntime::new`] only groups them behind the canonical
/// application ports.
pub struct Pr12PrimitiveDependencies<'a, SR, SL, TM, AF, ST, LG, RA> {
    pub graph: &'a TraceDecay,
    pub symbol_graph_cursors: &'a dyn SymbolGraphCursorPort,
    pub source_read: SR,
    pub source_lines: SL,
    pub test_map: TM,
    pub affected_file_tests: AF,
    pub affected_tests: ST,
    pub lexical_grep: LG,
    pub redundancy: RA,
    pub project: &'a dyn ProjectReadOperations,
    pub status: &'a dyn StatusReadOperations,
    pub files: &'a dyn FilesReadOperations,
    pub module_api: &'a dyn ModuleApiReadOperations,
    pub configuration: &'a dyn ConfigurationReadOperations,
    pub diagnostics: &'a dyn DiagnosticsReadOperations,
    pub health: &'a dyn HealthReadOperations,
    pub admin_read: &'a dyn AdminReadOperations,
}

/// Fully composed PR12 primitive owners ready for transport handler wiring.
///
/// The runtime does not parse requests, encode cursors, read files, compute
/// health, or apply authorization itself. Those responsibilities stay with
/// the dependencies, while the canonical owners preserve request context,
/// cancellation, authorization, and bounded-result contracts end to end.
pub struct Pr12PrimitiveRuntime<'a, SR, SL, TM, AF, ST, LG, RA> {
    pub symbol_graph: CanonicalSymbolGraphAdapter<'a>,
    pub source: CanonicalSourcePrimitiveOwner<SR, SL>,
    pub tests: CanonicalTestPrimitiveOwner<TM, AF, ST>,
    pub grep_analysis: ProductionGrepAnalysisOperationsV1<'a, LG, RA>,
    pub operations: OperationalPrimitiveOwner<'a>,
}

impl<'a, SR, SL, TM, AF, ST, LG, RA> Pr12PrimitiveRuntime<'a, SR, SL, TM, AF, ST, LG, RA>
where
    SR: SourceReadOperations,
    SL: SourceRetrievalPort,
    TM: TestMapOperations,
    AF: AffectedFileTestOperations,
    ST: AffectedTestsRetrievalPort,
    LG: LexicalGrepAuthorityV1,
    RA: RedundancyAuthorityV1,
{
    pub fn new(
        dependencies: Pr12PrimitiveDependencies<'a, SR, SL, TM, AF, ST, LG, RA>,
    ) -> Self {
        let Pr12PrimitiveDependencies {
            graph,
            symbol_graph_cursors,
            source_read,
            source_lines,
            test_map,
            affected_file_tests,
            affected_tests,
            lexical_grep,
            redundancy,
            project,
            status,
            files,
            module_api,
            configuration,
            diagnostics,
            health,
            admin_read,
        } = dependencies;

        Self {
            symbol_graph: CanonicalSymbolGraphAdapter::new(graph, symbol_graph_cursors),
            source: CanonicalSourcePrimitiveOwner::new(source_read, source_lines),
            tests: CanonicalTestPrimitiveOwner::new(
                test_map,
                affected_file_tests,
                affected_tests,
            ),
            grep_analysis: production_grep_analysis_operations(graph, lexical_grep, redundancy),
            operations: OperationalPrimitiveOwner::new(
                project,
                status,
                files,
                module_api,
                configuration,
                diagnostics,
                health,
                admin_read,
            ),
        }
    }
}

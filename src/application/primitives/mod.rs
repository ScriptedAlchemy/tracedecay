//! Production adapters for PR12 compatibility primitive families.

pub mod concrete;
pub mod grep_analysis;
pub mod runtime;
pub mod symbol_graph;

pub use concrete::{
    AuthenticatedSymbolGraphCursorAdapter, Pr12SourceReadAdapter,
    SymbolGraphCursorSnapshotAuthority,
};
pub use grep_analysis::{
    ProductionGrepAnalysisOperationsV1, TraceDecayAstGrepAuthorityV1,
    TraceDecayComplexityAuthorityV1, TraceDecayDependencyDepthAuthorityV1,
    production_grep_analysis_operations,
};
pub use runtime::{
    CallChainPrimitiveRequest, CallChainPrimitiveResult, DiagnosticPrimitiveRecord,
    DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveResult, DiagnosticsPrimitiveScope,
    FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult, FileMetadataPrimitiveRequest,
    FileMetadataPrimitiveResult, FileMetadataRecord, ModuleApiPrimitiveRequest,
    ModuleApiPrimitiveResult, OwnedPr12PrimitiveRuntime, Pr12ExtendedPrimitiveFuture,
    Pr12ExtendedPrimitivePort, Pr12OperationalPrimitive, Pr12OperationalPrimitiveFuture,
    Pr12OperationalPrimitivePort, Pr12OperationalPrimitiveRequest, Pr12PrimitiveDispatch,
    Pr12PrimitiveDispatchFuture, Pr12PrimitiveInvocation, Pr12PrimitiveProjectRuntime,
    Pr12PrimitiveRequest, QualifiedNamePrimitiveRequest, QualifiedNamePrimitiveResult,
    SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult, SourceOutlinePrimitiveRequest,
    SourceOutlinePrimitiveResult, StorageStatusPrimitiveRequest, StorageStatusPrimitiveResult,
    open_pr12_primitive_project_runtime,
};
pub use symbol_graph::{CanonicalSymbolGraphAdapter, SymbolGraphCursorPort};

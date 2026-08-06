//! Production adapters for compatibility primitive families.

pub mod concrete;
pub mod grep_analysis;
pub mod production;
pub mod runtime;
mod support;
pub mod symbol_graph;

pub use concrete::{
    AuthenticatedSymbolGraphCursorAdapter, SourceReadAdapter,
    SymbolGraphCursorSnapshotAuthority,
};
pub use grep_analysis::{
    ProductionGrepAnalysisOperationsV1, TraceDecayAstGrepAuthorityV1,
    TraceDecayComplexityAuthorityV1, TraceDecayDependencyDepthAuthorityV1,
    production_grep_analysis_operations,
};
pub use production::{
    ProductionPrimitiveOpenRequestV1, TraceDecayAffectedTestsPortV1,
    admitted_root_uri_for_project, locator_digest_for_project,
    open_production_primitive_runtime,
};
pub use runtime::{
    CallChainPrimitiveRequest, CallChainPrimitiveResult, DiagnosticPrimitiveRecord,
    DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveResult, DiagnosticsPrimitiveScope,
    FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult, FileMetadataPrimitiveRequest,
    FileMetadataPrimitiveResult, FileMetadataRecord, ManagedTestRunCurrentIdentity,
    ManagedTestRunCurrentIdentityFuture, ManagedTestRunCurrentScopePort, ModuleApiPrimitiveRequest,
    ModuleApiPrimitiveResult, OwnedPrimitiveRuntime, ExtendedPrimitiveFuture,
    ExtendedPrimitivePort, OperationalPrimitive, OperationalPrimitiveFuture,
    OperationalPrimitivePort, OperationalPrimitiveRequest, PrimitiveDispatch,
    PrimitiveDispatchFuture, PrimitiveInvocation, PrimitiveProjectRuntime,
    PrimitiveRequest, QualifiedNamePrimitiveRequest, QualifiedNamePrimitiveResult,
    SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult, SourceOutlinePrimitiveRequest,
    SourceOutlinePrimitiveResult, StorageStatusHistoryPointV1, StorageStatusPrimitiveRequest,
    StorageStatusPrimitiveResult, open_primitive_project_runtime,
};
pub use symbol_graph::{CanonicalSymbolGraphAdapter, SymbolGraphCursorPort};

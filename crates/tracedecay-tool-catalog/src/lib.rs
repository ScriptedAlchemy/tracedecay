//! Inert, versioned capability catalog contracts for TraceDecay V2.
//!
//! This crate defines immutable metadata and pure snapshot validation only. It
//! does not execute capabilities, route requests, open storage, render output,
//! or implement any transport adapter.

#![forbid(unsafe_code)]

mod binding;
mod executable;
mod id;
mod manifest;
mod mcp;
mod profile;
mod retrieval;
mod snapshot;
mod validation;

pub use binding::{
    BindingDeprecation, BindingStatus, BindingSurface, ProtocolRevisionRange,
    SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName,
};
pub use executable::{
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableCodecV1, ExecutableUnavailableDispositionV1, ExecutionOwnerV1, RouteExposureV1,
    SchemaBodyAuthorityV1,
};
pub use id::{
    BindingId, CapabilityId, CatalogDigest, CatalogDigestError, CodecBindingKey, ContributionId,
    FeatureId, IdentifierError, MAX_CATALOG_IDENTIFIER_BYTES, OperationId, ProfileId, RetrieverId,
    SchemaId, ServiceId, SortContractId, UseCaseId,
};
pub use manifest::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityManifestInputV1, CapabilityManifestV1, DeadlineBehavior, DeadlineContract,
    DeniedDisclosurePolicy, EffectClass, IdempotencyContract, InverseContract,
    InverseUnavailableReason, LifecycleClass, PaginationContract, PrivacyClass, ReceiptContract,
    ReconciliationContract, RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamResumeContract, StreamingContract, TerminalState,
    TerminalStateContract, UnavailabilityReason,
};
pub use mcp::{
    MCP_DISPATCH_CONTRACT_VERSION, McpDeadlineContractV1, McpDispatchAvailability,
    McpDispatchCatalogError, McpDispatchCatalogV1, McpDispatchContractInputV1,
    McpDispatchContractV1, McpDispatchUnavailableReason, McpIdempotencyContract,
    McpInverseContract, McpInverseUnavailableReason, McpTerminalState,
};
pub use profile::{
    ProfileBudget, ProfileDefinition, ProfileDefinitionInputV1, ProfileKind,
    RoutingFixtureExpectation, RoutingFixtureV1,
};
pub use retrieval::{
    ContributionContractRef, CoverageContractRef, OmissionContractRef, RetrievalFamily,
    RetrievalPrimitiveManifestInputV1, RetrievalPrimitiveManifestV1, ScoringContractRef,
    SortContract, TemporalMode,
};
pub use snapshot::{
    ApplicationHandlerDescriptorV1, CatalogContributionInputV1, CatalogContributionV1,
    CatalogSnapshotBuilderV1, CatalogSnapshotV1,
};
pub use validation::CatalogValidationError;

//! Inert, versioned capability catalog contracts for TraceDecay V2.
//!
//! This crate defines immutable metadata and pure snapshot validation only. It
//! does not execute capabilities, route requests, open storage, render output,
//! or implement any transport adapter.

#![forbid(unsafe_code)]

mod binding;
mod id;
mod manifest;
mod profile;
mod retrieval;
mod snapshot;
mod validation;

pub use binding::{
    BindingDeprecation, BindingStatus, BindingSurface, ProtocolRevisionRange,
    SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName,
};
pub use id::{
    BindingId, CapabilityId, CatalogDigest, CatalogDigestError, ContributionId, FeatureId,
    IdentifierError, MAX_CATALOG_IDENTIFIER_BYTES, ProfileId, RetrieverId, SchemaId,
    SortContractId, UseCaseId,
};
pub use manifest::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityManifestInputV1, CapabilityManifestV1, DeadlineBehavior, DeadlineContract,
    DeniedDisclosurePolicy, DeprecationWindow, EffectClass, IdempotencyContract, LifecycleClass,
    PaginationContract, PrivacyClass, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaRef, ScopeDimension,
    ScopeRequirement, StreamResumeContract, StreamingContract, TerminalState,
    TerminalStateContract, UnavailabilityReason,
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

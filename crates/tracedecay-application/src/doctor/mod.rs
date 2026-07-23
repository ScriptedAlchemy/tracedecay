//! Doctor kernel contract (Plan 09 §PR14).
//!
//! Contract-only scaffold: typed finding families, evidence states, coverage,
//! and owner-supplied remediation references for the one Doctor application use
//! case. This module owns no store, transport, provider runtime, or health
//! formula, and is intentionally not wired into the catalog, handler
//! descriptors, or any surface binding yet. The composing use case lands in a
//! later PR14 slice.

mod types;

pub use types::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorStorageFindingKindV1,
};

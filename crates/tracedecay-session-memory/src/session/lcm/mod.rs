//! Application-owned LCM authority boundary.
//!
//! Retrieval contracts and compression policy live in `tracedecay-lcm`.
//! Hydration shaping lives in `tracedecay-session-temporal-store`. Neither
//! surface is re-exported here; callers already depend on those crates.

pub mod authority;

pub use authority::{
    LcmAuthorityFuture, LcmAuthorityInvocation, LcmAuthorityOperation, LcmAuthorityOutcome,
    LcmAuthorityPayload, LcmAuthorityPort, LcmAuthorityReceipt, LcmAuthorityRequest,
    LcmAuthorityResponse, LcmAuthorityTarget, LcmAuthorityUnavailableReason, LcmCompactionCommand,
    LcmCompressionEvidence, LcmDoctorQuery, LcmHostProtocol, LcmStatusQuery,
    lcm_authority_operation_identity,
};

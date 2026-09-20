//! Application ownership of the LCM authority boundary.
//!
//! `contracts` holds the retrieval value types and their containment rules;
//! `compression_policy` holds provider-neutral token, overflow, and atomic
//! chunk-selection rules. Hydration shaping lives in
//! `tracedecay-session-temporal-store` and is not re-exported here. The LCM
//! engine crate owns the contract and policy surfaces, and the registered
//! temporal adapters depend on them directly, so neither side has to reach
//! through the other.

pub mod authority;
pub use tracedecay_lcm::compression_policy;
pub use tracedecay_lcm::contracts;

pub use authority::{
    LcmAuthorityFuture, LcmAuthorityInvocation, LcmAuthorityOperation, LcmAuthorityOutcome,
    LcmAuthorityPayload, LcmAuthorityPort, LcmAuthorityReceipt, LcmAuthorityRequest,
    LcmAuthorityResponse, LcmAuthorityTarget, LcmAuthorityUnavailableReason, LcmCompactionCommand,
    LcmCompressionEvidence, LcmDoctorQuery, LcmHostProtocol, LcmStatusQuery,
    LcmTranscriptIngestCommand, lcm_authority_operation_identity,
};
pub use contracts::{
    LcmContentRange, LcmContentSlice, LcmDescribeExternalPayload, LcmDescribeRequest,
    LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget,
    LcmError, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget,
    LcmExpandedSummarySource, LcmPayloadExpansion, LcmPayloadRef, LcmRawMessage,
    LcmRawMessageOverview, LcmSourceRef, LcmStorageKind, LcmSummaryNode, LcmSummaryNodeOverview,
    validate_payload_ref,
};

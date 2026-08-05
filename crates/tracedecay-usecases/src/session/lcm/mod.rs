//! Application ownership of the DB-free LCM compatibility surface.
//!
//! `contracts` holds the retrieval value types and their containment rules;
//! `compression_policy` holds provider-neutral token, overflow, and atomic
//! chunk-selection rules; `render` holds the truncation and typed-omission
//! shaping applied after canonical hydration. The session LCM engine
//! re-exports these under `sessions::lcm`, and the registered temporal adapters
//! depend on them directly, so neither side has to reach through the other.

pub mod authority;
pub mod contracts;
pub mod render;
pub use tracedecay_sessions::lcm::compression_policy;

pub use authority::{
    LcmAuthorityFuture, LcmAuthorityInvocation, LcmAuthorityOperation, LcmAuthorityOutcome,
    LcmAuthorityPayload, LcmAuthorityPort, LcmAuthorityReceipt, LcmAuthorityRequest,
    LcmAuthorityResponse, LcmAuthorityUnavailableReason, LcmCompactionCommand,
    LcmCompressionEvidence, LcmHostProtocol, LcmStatusQuery, LcmTranscriptIngestCommand,
    lcm_authority_operation_identity,
};
pub use contracts::{
    LcmContentRange, LcmContentSlice, LcmDescribeExternalPayload, LcmDescribeRequest,
    LcmDescribeResponse, LcmDescribeSourceOverview, LcmDescribeSummaryNode, LcmDescribeTarget,
    LcmError, LcmExpandRequest, LcmExpandResponse, LcmExpandSourcePagination, LcmExpandTarget,
    LcmExpandedSummarySource, LcmPayloadExpansion, LcmPayloadRef, LcmRawMessage,
    LcmRawMessageOverview, LcmSourceRef, LcmStorageKind, LcmSummaryNode, LcmSummaryNodeOverview,
    validate_payload_ref,
};

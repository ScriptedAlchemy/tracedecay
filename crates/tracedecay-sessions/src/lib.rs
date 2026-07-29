//! Provider-neutral session contracts.
//!
//! Physical storage runtimes and host transcript normalization remain in their
//! owning crates. This crate defines the stable values exchanged across those
//! boundaries.

mod authorization;
mod ingest;
pub mod lcm;
mod provider;
mod workflow;

pub use authorization::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionRetrievalScope,
};
pub use ingest::{NewRows, StoredCursor, TranscriptIngestStats};
pub use provider::{
    EXPECTED_MESSAGE_SEARCH_PROVIDER, MESSAGE_SEARCH_PROVIDER_IDS, ProviderScope, SessionProvider,
};
pub use workflow::{
    WorkflowAgent, WorkflowGitScope, WorkflowIndexReadPort, WorkflowIndexState, WorkflowReadError,
    WorkflowRun, WorkflowRunDetail, WorkflowRunDetailFuture, WorkflowRunDetailOutcome,
    WorkflowRunDetailRequest, WorkflowRunListFuture, WorkflowRunListOutcome,
    WorkflowRunListRequest, WorkflowRunScope, WorkflowScopeFilter, WorkflowStatus,
};

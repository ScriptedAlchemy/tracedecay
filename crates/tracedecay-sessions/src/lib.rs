//! Provider-neutral session contracts.
//!
//! Physical storage runtimes and host transcript normalization remain in their
//! owning crates. This crate defines the stable values exchanged across those
//! boundaries.

pub mod admission;
mod authorization;
pub mod compatibility;
pub mod host_ports;
mod ingest;
pub mod lcm;
pub mod observation;
mod orchestration;
mod provider;
pub mod repository_provenance;
pub mod runtime;
mod workflow;

pub use authorization::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionRetrievalScope,
};
pub use ingest::{NewRows, StoredCursor, TranscriptIngestStats};
pub use orchestration::{ProviderRunFailure, ProviderRunFold, ProviderRunOutcome};
pub use provider::{
    EXPECTED_MESSAGE_SEARCH_PROVIDER, MESSAGE_SEARCH_PROVIDER_IDS, ProviderScope, SessionProvider,
    decode_kiro_workspace_path,
};
pub use workflow::{
    WorkflowAgent, WorkflowGitScope, WorkflowIndexReadPort, WorkflowIndexState, WorkflowReadError,
    WorkflowRun, WorkflowRunDetail, WorkflowRunDetailFuture, WorkflowRunDetailOutcome,
    WorkflowRunDetailRequest, WorkflowRunListFuture, WorkflowRunListOutcome,
    WorkflowRunListRequest, WorkflowRunScope, WorkflowScopeFilter, WorkflowStatus,
};

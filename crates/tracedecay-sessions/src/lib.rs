//! Provider-neutral session contracts.
//!
//! Physical storage runtimes and host transcript normalization remain in their
//! owning crates. This crate defines the stable values exchanged across those
//! boundaries.

mod ingest;
mod provider;
mod workflow;

pub use ingest::{NewRows, StoredCursor, TranscriptIngestStats};
pub use provider::{
    EXPECTED_MESSAGE_SEARCH_PROVIDER, MESSAGE_SEARCH_PROVIDER_IDS, ProviderScope, SessionProvider,
};
pub use workflow::{
    WorkflowAgent, WorkflowRun, WorkflowScopeFilter, WorkflowStatus,
};

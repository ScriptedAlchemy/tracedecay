//! Provider-neutral session contracts.
//!
//! Physical storage runtimes and host transcript normalization remain in their
//! owning crates. This crate defines the stable values exchanged across those
//! boundaries.
//!
//! ## Must never depend on `tracedecay-global-db`
//!
//! `tracedecay-global-db` depends on this crate (session runtime, retrieval
//! content, `lcm::contracts`); the edge cannot run the other way without a
//! cycle. The registered database is reached through narrower session-runtime
//! ports, including [`runtime::ingest::SessionIngestAuthority`], instead of
//! the concrete `RegisteredGlobalDb` type.
//!
//! ## Sealed benchmark provenance
//!
//! These manifests and results carry file-identity digests pinned to their
//! exact paths and contents. Re-seal them together from a clean source
//! commit if they ever need to move or regenerate — never hand-edit:
//!
//! - `benchmarks/claude-observation/workload-v1.json` and
//!   `benchmarks/claude-observation/result-2026-07-26-dc17dd73.json`
//! - `benchmarks/session-temporal/workload-v1.json` and
//!   `benchmarks/session-temporal/result-provisional.json`
//! - `tests/fixtures/transcript_golden/cline_like/manifest.json` and
//!   `tests/fixtures/transcript_golden/cline_like/expected/parser_provenance.json`
//! - `tests/fixtures/provider_normalization/codex/README.md`
//!
//! Their `include_str!`/`include_bytes!` sites resolve for workspace builds
//! but reach outside this crate's package root (repo-root `benchmarks/` and
//! `tests/fixtures/`), so `cargo package`/`cargo publish` for this crate
//! cannot see them. `publish = false` is set, so this is accepted rather than
//! vendoring the fixtures under `crates/tracedecay-sessions/tests/`.

pub mod admission;
mod authorization;
pub mod host_ports;
mod ingest;
pub mod lcm;
pub mod observation;
mod orchestration;
mod provider;
pub mod repository_provenance;
pub mod retrieval_content;
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

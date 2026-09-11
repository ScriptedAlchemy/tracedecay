//! Native `gix` implementations of the read-only Git ports and values that
//! `tracedecay-contracts` declares.
//!
//! The contract crate keeps the request/result/error types and ports; this
//! crate, which already links `gix` for historical retrieval, owns the
//! repository opens so every consumer (use cases, the MCP branch tools, the
//! search evaluator) mounts one production read and the contracts stay free
//! of native dependencies.

mod branch_snapshots;
mod historical_blob;

pub use branch_snapshots::{local_branch_revision_controlled, local_branch_snapshots_controlled};
pub use historical_blob::NativeHistoricalBlobReaderV1;

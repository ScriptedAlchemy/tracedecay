//! Consolidated capture-crate integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. `canonical_envelope_allocations` (counting global
//! allocator) and `hotpath_coverage` (sets `HOTPATH_*` process environment
//! variables) stay their own binaries.

mod fixture_provenance;
mod kiro;
mod provider_identity;
mod provider_usage_capture;

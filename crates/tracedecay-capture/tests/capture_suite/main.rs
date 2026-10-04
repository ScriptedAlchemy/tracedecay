//! Consolidated capture-crate integration suite.
//!
//! Each module was previously a standalone integration-test binary under
//! `tests/<module>.rs`; every one of them linked the same dependency closure.
//! Compiled as modules of one binary, each test keeps its old binary name as
//! its module prefix. `canonical_envelope_allocations` (counting global
//! allocator) stays its own binary.

mod kiro;
mod provider_identity;
mod provider_usage_capture;

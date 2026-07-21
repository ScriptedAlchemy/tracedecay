//! End-to-end storage-runtime integration tests.
//!
//! Keeping the graph and session cases in one integration-test crate avoids an
//! additional link step while preserving domain-specific module names.

mod support;

mod graph;
mod session;

//! Root-free agent-host profile schemas and configuration parsing.
//!
//! The root crate keeps the host lifecycle and filesystem/error policy. This
//! crate owns the deterministic profile transformation kernel so it can be
//! tested and reused without a backedge into the composition root.

pub mod agents;

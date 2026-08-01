//! Compatibility shim for the extracted `agents` subsystem.
//!
//! The implementation moved to `crates/tracedecay-agent-hosts` (together with
//! `automation`, which it is mutually recursive with). This glob re-export
//! keeps every previously public path resolving unchanged — both leaf items
//! (`crate::agents::AgentIntegration`) and the host submodules
//! (`crate::agents::claude::…`, `crate::agents::host_bundle_v2::…`), since a
//! `pub mod` is itself a re-exportable item.
//!
//! Items that were `pub(crate)` in the old tree are deliberately NOT covered:
//! they are now private to `tracedecay-agent-hosts`. Root call sites that
//! reached them are cataloged in
//! `crates/tracedecay-agent-hosts/SEAMS.md`.
pub use tracedecay_agent_hosts::agents::*;

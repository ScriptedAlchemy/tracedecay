//! Compatibility shim for the extracted `automation` subsystem.
//!
//! The implementation moved to `crates/tracedecay-agent-hosts` (together with
//! `agents`, which it is mutually recursive with). This glob re-export keeps
//! every previously public path resolving unchanged — both leaf items
//! (`crate::automation::runner::…` entry points) and the submodules
//! themselves, since a `pub mod` is itself a re-exportable item.
//!
//! Items that were `pub(crate)` in the old tree are deliberately NOT covered:
//! they are now private to `tracedecay-agent-hosts`. Root call sites that
//! reached them are cataloged in
//! `crates/tracedecay-agent-hosts/SEAMS.md`.
pub use tracedecay_agent_hosts::automation::*;

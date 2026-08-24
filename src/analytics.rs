//! Provider-neutral assistant usage taxonomy.
//!
//! Moved to `tracedecay-agent-hosts::analytics` (see that module's doc
//! comment for the rationale). This is a thin shim so every
//! `crate::analytics::…` path in the root crate keeps resolving.

pub use tracedecay_agent_hosts::analytics::*;

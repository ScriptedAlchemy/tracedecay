//! Agent host integrations (`agents`) and self-improvement automation
//! (`automation`) for TraceDecay.
//!
//! These two subsystems are mutually recursive — `agents` reaches into
//! `automation` for skill/memory installation targets and `automation` reaches
//! back into `agents` for host discovery and bundle composition — so they are
//! extracted from the root crate as a single unit. Inside this crate the
//! former `crate::agents::*` / `crate::automation::*` paths keep resolving
//! unchanged, which is why the module names are preserved verbatim.
//!
//! The root crate re-exports both modules from `src/agents.rs` and
//! `src/automation.rs` so every previously public path
//! (`tracedecay::agents::…`, `tracedecay::automation::…`) still resolves.
//!
//! Root couplings that this crate cannot yet satisfy (`crate::errors`,
//! `crate::db`, `crate::storage`, `crate::daemon`, `crate::mcp`, …) are
//! cataloged in `SEAMS.md`; they resolve when the kernel/daemon/mcp crates
//! land in the same split.

pub mod agents;
pub mod automation;

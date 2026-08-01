//! The session-evidence contract automation states its queries in.
//!
//! `automation::runner` builds evidence for the memory curator, session
//! reflector, and skill writer by grepping stored session transcripts. The
//! LCM query engine that answers those greps lives in `tracedecay-sessions`
//! behind a runtime this crate must not open for itself, so the request
//! selectors and the hit shape are declared here and the execution arrives
//! through `runner::retrieval`'s `AutomationSessionRetrieval` port.
//!
//! These deliberately mirror `sessions::lcm`'s selectors rather than reusing
//! them: this crate states *what evidence automation wants*, and the session
//! runtime decides how to satisfy it. The serde representations match, so the
//! root adapter is a field-for-field conversion.
//!
//! Root wiring: the root converts between these and
//! `sessions::lcm::{LcmScope, LcmGrepSort, LcmGrepHit}` in the adapter it
//! registers as `AutomationSessionRetrieval`. `SEAMS.md` tracks the row.

/// Canonical definitions live in `tracedecay_sessions::runtime::lcm::types`;
/// re-exported here so this crate's port keeps its historical path.
pub use tracedecay_sessions::runtime::lcm::types::{LcmGrepHit, LcmGrepSort, LcmScope};

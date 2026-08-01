//! The authoritative configuration snapshot Context Scout pins against.
//!
//! A **downward move**, not a port: the value carries only `tracedecay_domain`
//! types, and it crosses the boundary in both directions — the daemon resolves
//! it and hands it to `agents::context_scout_ports` to pin and revalidate
//! against. One definition therefore has to serve both sides.
//!
//! Root wiring: `src/application/configuration/ports.rs` drops its own
//! declaration and re-exports this one, so
//! `crate::application::configuration::ConfigurationCurrentStateV1` keeps
//! resolving for every root call site.

use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};

/// The configuration revision in force, with the snapshot it resolved to.
///
/// Both halves matter to a pin: the revision id detects that configuration
/// moved at all, and the snapshot detects that effective behaviour changed
/// even when a revision bump left the values Scout reads untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationCurrentStateV1 {
    pub revision_id: ConfigurationRevisionId,
    pub snapshot: ConfigurationSnapshotV1,
}

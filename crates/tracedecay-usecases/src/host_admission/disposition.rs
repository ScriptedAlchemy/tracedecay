//! Disposition and telemetry classification for host admission.
//!
//! The canonical definitions live in
//! [`tracedecay_sessions::admission::disposition`]; this module re-exports them
//! so the use-case layer keeps a single source of truth for the status and
//! disposition-class enums, the privacy-bounded telemetry disposition, and the
//! reason-code predicates surfaced at the daemon/host boundary.

pub use tracedecay_sessions::admission::disposition::*;

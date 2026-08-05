//! Compatibility paths for configuration surface requests now owned by
//! `tracedecay-application`.
//!
//! The root binary's `src/application_surface.rs` is the whole HTTP/MCP request
//! envelope; it stays at the composition root because most of its variants
//! carry adapter types. Only these two are reached from below, and both are
//! plain `tracedecay-domain` DTOs, so they moved down here rather than pulling
//! the envelope with them.
//!
//! Existing root/use-case imports remain source-compatible re-exports; there is
//! one Serde and schema authority for each request.

pub use tracedecay_application::{
    ConfigurationProtectedApplyRequestV1 as ConfigurationProtectedApplySurfaceRequest,
    ConfigurationProtectedPreviewRequestV1 as ConfigurationProtectedPreviewSurfaceRequest,
    ConfigurationRollbackApplyRequestV1 as ConfigurationRollbackApplySurfaceRequest,
};

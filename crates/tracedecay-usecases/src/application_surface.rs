//! The two configuration surface requests the doctor remediation use case
//! constructs.
//!
//! The root binary's `src/application_surface.rs` is the whole HTTP/MCP request
//! envelope; it stays at the composition root because most of its variants
//! carry adapter types. Only these two are reached from below, and both are
//! plain `tracedecay-domain` DTOs, so they moved down here rather than pulling
//! the envelope with them.
//!
//! Root must delete its copies and re-export from here — two `deny_unknown_fields`
//! serde shapes must not be defined twice. See SEAMS.md.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use tracedecay_domain::ManifestDigest;
use tracedecay_domain::configuration::{
    ChangePlanId, ConfigurationIdempotencyKey, ConfigurationRevisionId, ProtectedChange,
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationProtectedPreviewSurfaceRequest {
    pub change: ProtectedChange,
    pub expected_revision: ConfigurationRevisionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationProtectedApplySurfaceRequest {
    pub plan_id: ChangePlanId,
    pub expected_base_revision_id: ConfigurationRevisionId,
    pub operation_digest: ManifestDigest,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

/// Rollback apply reuses the protected-apply envelope, exactly as at root.
pub type ConfigurationRollbackApplySurfaceRequest = ConfigurationProtectedApplySurfaceRequest;

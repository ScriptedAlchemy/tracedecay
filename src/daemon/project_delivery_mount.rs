//! Project-open ordering for the delivery and observability authority.

use super::*;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;

pub(super) async fn ensure_project_delivery_settlement(
    invocation: &DaemonInvocationState,
    project_root: &Path,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    scope: &ResolvedScope,
    access: &ProjectSourceAccessSnapshot,
) -> Result<ManifestDigest> {
    let configuration_policy_digest = canonical_sha256(&(
        "tracedecay.daemon.configuration-policy.v1",
        &scope.scope_digest,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("project-open configuration policy digest failed: {error}"),
    })?;
    invocation
        .service
        .mount_observability_producer(
            project_root.to_path_buf(),
            session_db,
            scope.project_id.clone(),
            access.configuration_digest.clone(),
            configuration_policy_digest.clone(),
        )
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("project-open observability producer registration failed: {error}"),
        })?;
    Ok(configuration_policy_digest)
}

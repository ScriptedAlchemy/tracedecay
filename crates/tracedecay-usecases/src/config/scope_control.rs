//! Pure protected-change planning for source bindings, restrictive policy,
//! and topology policy. Normal scalar setting changes do not enter this path.

use std::path::Path;

use thiserror::Error;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, ChangePlanId, ConfigurationRevisionId,
    ProtectedApplyRequest, ProtectedChange, ProtectedChangePlan, RedactedConfigurationChangeV1,
    RollbackModeV1, SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding, SettingKey, SourceBindingId,
    SourceKindV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, DomainError, LocatorDigest, ManifestDigest, ProjectId, UtcMicros,
};

/// Canonical identifier of the one daemon-owned binding that authorizes the
/// daemon to act on a project it registered. Project-open resolves the daemon's
/// source access from exactly one binding keyed by
/// `(SourceKindV1::Cursor, AuthorityRef::Project(project_id))`, so this id must
/// stay stable: a second binding on that key is a contract violation, not an
/// additive grant.
pub const DAEMON_PROJECT_SOURCE_BINDING_ID: &str = "binding.tracedecay-daemon.project-open";

/// The source kind the daemon owns for its own project-open access.
pub const DAEMON_PROJECT_SOURCE_KIND: SourceKindV1 = SourceKindV1::Cursor;

/// Build the daemon-owned source binding for a project the daemon has already
/// registered.
///
/// This is not path-inferred authority. Both components restate identity the
/// caller already holds: the authority is the project's own resolved id, and
/// the locator digest is the same project-open locator digest the daemon
/// derives for that registered root. It grants the daemon nothing it does not
/// already own; it only makes the daemon's own binding durable so the
/// project-open authority check has an exact binding to verify against.
pub fn daemon_owned_project_source_binding(
    project_id: &ProjectId,
    project_root: &Path,
) -> Result<ScopeSourceBinding, DomainError> {
    let locator = crate::primitives::locator_digest_for_project(project_root)
        .map_err(|_| DomainError::NonCanonical {
            field: "daemon project source binding locator digest",
        })?;
    ScopeSourceBinding::new(
        SourceBindingId::new(DAEMON_PROJECT_SOURCE_BINDING_ID.to_owned())?,
        DAEMON_PROJECT_SOURCE_KIND,
        LocatorDigest::new(locator.as_str().to_owned())?,
        AuthorityRef::Project(project_id.clone()),
    )
}

#[derive(Debug, Error)]
pub enum ProtectedChangePlanningError {
    #[error("protected change contains an invalid domain value: {0}")]
    Domain(#[from] DomainError),
    #[error("protected change expiry must be after creation")]
    InvalidExpiry,
}

/// Inputs frozen by a dry-run. Scope resolution and authorization are
/// rechecked by the application/store boundary immediately before apply.
#[derive(Clone, Debug)]
pub struct ProtectedChangePlanDraftV1 {
    pub plan_id: ChangePlanId,
    pub actor_id: ActorId,
    pub base_revision_id: ConfigurationRevisionId,
    pub resolved_scope_digest: ManifestDigest,
    pub membership_digest: Option<ManifestDigest>,
    pub authorization_policy_digest: AccessPolicyDigest,
    pub policy_epoch: u64,
    pub created_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub before_digest: Option<ManifestDigest>,
    pub after_digest: Option<ManifestDigest>,
}

/// Produce a redacted, actor-bound plan for exactly one protected operation.
/// This function does not mutate desired/effective configuration.
pub fn plan_protected_change(
    draft: ProtectedChangePlanDraftV1,
    change: ProtectedChange,
) -> Result<ProtectedChangePlan, ProtectedChangePlanningError> {
    change.validate()?;
    if draft.expires_at <= draft.created_at {
        return Err(ProtectedChangePlanningError::InvalidExpiry);
    }
    let setting_key = protected_setting_key(&change)?;
    let operation_digest = change.compute_digest()?;
    let plan = ProtectedChangePlan {
        plan_id: draft.plan_id,
        actor_id: draft.actor_id,
        base_revision_id: draft.base_revision_id,
        operation_digest,
        resolved_scope_digest: draft.resolved_scope_digest,
        membership_digest: draft.membership_digest,
        authorization_policy_digest: draft.authorization_policy_digest,
        policy_epoch: draft.policy_epoch,
        expires_at: draft.expires_at,
        created_at: draft.created_at,
        redacted_changes: vec![RedactedConfigurationChangeV1 {
            setting_key,
            operation: change.operation_kind(),
            before_digest: draft.before_digest,
            after_digest: draft.after_digest,
        }],
    };
    plan.validate()?;
    Ok(plan)
}

/// A rollback is always forward-only: the application evaluates historical
/// typed values against current schema, scope, policy, and base revision, then
/// uses this same plan/apply shape to create a new child revision.
#[derive(Clone, Debug)]
pub struct RollbackPlanRequestV1 {
    pub target_revision_id: ConfigurationRevisionId,
    pub mode: RollbackModeV1,
}

impl RollbackPlanRequestV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.target_revision_id.validate()
    }
}

/// Verify only immutable dry-run/request binding. Expiry, current revision,
/// scope, membership, and policy epoch must be rechecked by the caller with
/// current authoritative data before committing.
pub fn validate_apply_binding(
    plan: &ProtectedChangePlan,
    request: &ProtectedApplyRequest,
    now: UtcMicros,
) -> Result<(), ProtectedChangePlanningError> {
    request.validate_against(plan, now)?;
    Ok(())
}

fn protected_setting_key(change: &ProtectedChange) -> Result<SettingKey, DomainError> {
    let key = match change {
        ProtectedChange::BindSource(_)
        | ProtectedChange::RebindSource(_)
        | ProtectedChange::UnbindSource { .. } => SOURCE_BINDINGS_SETTING_KEY,
        ProtectedChange::UpsertAccessRule(_) | ProtectedChange::RemoveAccessRule { .. } => {
            ACCESS_RULES_SETTING_KEY
        }
        ProtectedChange::ReplaceWorkTopologyPolicy(_) => WORK_TOPOLOGY_POLICY_SETTING_KEY,
    };
    SettingKey::new(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::{
        AuthorityRef, ScopeSourceBinding, SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::{LocatorDigest, ProjectId};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn protected_dry_run_is_redacted_and_actor_bound() {
        let change = ProtectedChange::BindSource(
            ScopeSourceBinding::new(
                id::<SourceBindingId>("binding.fixture"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                AuthorityRef::Project(id::<ProjectId>("project.fixture")),
            )
            .unwrap(),
        );
        let plan = plan_protected_change(
            ProtectedChangePlanDraftV1 {
                plan_id: id("plan.fixture"),
                actor_id: id("actor.fixture"),
                base_revision_id: id("revision.fixture"),
                resolved_scope_digest: digest('b'),
                membership_digest: None,
                authorization_policy_digest: id(
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                ),
                policy_epoch: 1,
                created_at: UtcMicros(1),
                expires_at: UtcMicros(2),
                before_digest: Some(digest('d')),
                after_digest: Some(digest('e')),
            },
            change,
        )
        .unwrap();
        let rendered = serde_json::to_value(&plan).unwrap();
        assert!(rendered.get("source_locator").is_none());
        assert!(rendered.get("credential").is_none());
    }
}

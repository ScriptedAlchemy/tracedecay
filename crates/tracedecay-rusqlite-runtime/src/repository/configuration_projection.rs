//! Immutable configuration projections shared by the repository writer and
//! the global-db writer.
//!
//! Both drivers execute the statements produced here so a revision's
//! projection rows cannot drift between writers.

use serde::Serialize;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, ConfigurationSnapshotV1, ConfigurationValueV1,
    ProtectedRefDispositionV1, ProtectedRefSelectorV1, RuleEffect, SOURCE_BINDINGS_SETTING_KEY,
    SettingKey, SourceKindV1, WORK_TOPOLOGY_POLICY_SETTING_KEY, WorktreePlacementModeV1,
};
use tracedecay_domain::{CapabilityId, ConfigurationRevisionId, DomainError, canonical_sha256};

use crate::exact_sql::{ExactSqlStatement, ExactSqlValue, optional_text, text};

const SOURCE_BINDINGS_PROJECTION_SQL: &str = "INSERT INTO configuration_source_bindings (
    revision_id, binding_id, source_kind, locator_digest,
    authority_kind, project_id, user_profile_id, provenance_digest
 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

const ACCESS_RULES_PROJECTION_SQL: &str = "INSERT INTO configuration_access_rules (
    revision_id, rule_id, subject_kind, subject_id, actor_kind, actor_id,
    operation_kind, source_kind, authority_kind, project_id, user_profile_id,
    capability_encoding, effect, expires_at
 ) VALUES (?1, ?2, 'scope_access_subject_v1', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";

const TOPOLOGY_POLICY_PROJECTION_SQL: &str = "INSERT INTO configuration_topology_policies (
    revision_id, schema_version, topology_policy_digest, placement_kind,
    default_cross_merge_mode, allow_cross_repository, cleanliness_kind,
    review_kind, require_fresh_preflight, maximum_preflight_age_seconds,
    history_rewrite_kind, escalation_kind, automatic_gc_kind, notification_level,
    sealed_policy_value
 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)";

const TOPOLOGY_ROOT_PROJECTION_SQL: &str = "INSERT INTO configuration_topology_roots (
    revision_id, root_ordinal, root_id, locator_digest,
    repository_scope_digest, maximum_active_worktrees
 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

const PROTECTED_REF_PROJECTION_SQL: &str = "INSERT INTO configuration_topology_protected_refs (
    revision_id, rule_ordinal, selector_kind, selector_digest, disposition
 ) VALUES (?1, ?2, ?3, ?4, ?5)";

fn integer(value: impl Into<i64>) -> ExactSqlValue {
    ExactSqlValue::Integer(value.into())
}

fn statement(sql: &str, params: Vec<ExactSqlValue>) -> ExactSqlStatement {
    ExactSqlStatement {
        sql: sql.to_owned(),
        params,
    }
}

fn projection_encoding<T: Serialize>(value: &T) -> Result<String, DomainError> {
    match serde_json::to_value(value).map_err(|error| {
        DomainError::CanonicalSerialization(format!("encode configuration projection: {error}"))
    })? {
        serde_json::Value::String(value) => Ok(value),
        value => serde_json::to_string(&value).map_err(|error| {
            DomainError::CanonicalSerialization(format!(
                "encode structured configuration projection: {error}"
            ))
        }),
    }
}

fn authority_projection(
    authority: &AuthorityRef,
) -> (&'static str, Option<String>, Option<String>) {
    match authority {
        AuthorityRef::Project(project_id) => {
            ("project", Some(project_id.as_str().to_owned()), None)
        }
        AuthorityRef::ProjectlessHermes(user_profile_id) => (
            "projectless_hermes",
            None,
            Some(user_profile_id.as_str().to_owned()),
        ),
    }
}

fn source_kind_projection(source_kind: SourceKindV1) -> &'static str {
    match source_kind {
        SourceKindV1::Claude => "claude",
        SourceKindV1::Codex => "codex",
        SourceKindV1::Cursor => "cursor",
        SourceKindV1::GitHub => "github",
        SourceKindV1::Hermes => "hermes",
        SourceKindV1::Kiro => "kiro",
    }
}

fn rule_effect_projection(effect: RuleEffect) -> &'static str {
    match effect {
        RuleEffect::Allow => "allow",
        RuleEffect::Deny => "deny",
    }
}

/// The projection statements one revision commits, in apply order.
///
/// The repository writer executes these with rusqlite params and the
/// global-db writer with engine params; both end up with byte-identical rows.
pub fn configuration_projection_statements(
    revision_id: &ConfigurationRevisionId,
    snapshot: &ConfigurationSnapshotV1,
) -> Result<Vec<ExactSqlStatement>, DomainError> {
    let mut statements = Vec::new();
    let source_bindings_key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)?;
    if let Some(ConfigurationValueV1::SourceBindings(bindings)) =
        snapshot.effective_values.get(&source_bindings_key)
    {
        let candidates = snapshot
            .provenance
            .get(&source_bindings_key)
            .cloned()
            .unwrap_or_default();
        for binding in bindings {
            binding.validate()?;
            let (authority_kind, project_id, user_profile_id) =
                authority_projection(&binding.authority);
            let provenance_digest = canonical_sha256(&(
                "tracedecay.configuration.source-binding-projection.v1",
                binding,
                &candidates,
            ))?;
            statements.push(statement(
                SOURCE_BINDINGS_PROJECTION_SQL,
                vec![
                    text(revision_id.as_str()),
                    text(binding.binding_id.as_str()),
                    text(source_kind_projection(binding.source_kind)),
                    text(binding.source_locator_digest.as_str()),
                    text(authority_kind),
                    optional_text(project_id),
                    optional_text(user_profile_id),
                    text(provenance_digest.as_str()),
                ],
            ));
        }
    }

    let access_rules_key = SettingKey::new(ACCESS_RULES_SETTING_KEY)?;
    if let Some(ConfigurationValueV1::AccessRules(rules)) =
        snapshot.effective_values.get(&access_rules_key)
    {
        for rule in rules {
            rule.validate()?;
            let (authority_kind, project_id, user_profile_id) =
                authority_projection(&rule.authority);
            let subject_id = canonical_sha256(&(
                "tracedecay.configuration.access-rule-subject.v1",
                &rule.subject,
            ))?;
            let actor_id = rule
                .subject
                .actor
                .as_ref()
                .map(|actor| actor.as_str().to_owned());
            let actor_kind = actor_id.as_ref().map(|_| "actor");
            let operation_kind = rule
                .subject
                .operation
                .map(|operation| projection_encoding(&operation))
                .transpose()?;
            let source_kind = rule
                .subject
                .source_kind
                .map(source_kind_projection)
                .map(str::to_owned);
            let capabilities = rule
                .capabilities
                .iter()
                .map(CapabilityId::as_str)
                .collect::<Vec<_>>()
                .join(",");
            statements.push(statement(
                ACCESS_RULES_PROJECTION_SQL,
                vec![
                    text(revision_id.as_str()),
                    text(rule.rule_id.as_str()),
                    text(subject_id.as_str()),
                    optional_text(actor_kind.map(str::to_owned)),
                    optional_text(actor_id),
                    optional_text(operation_kind),
                    optional_text(source_kind),
                    text(authority_kind),
                    optional_text(project_id),
                    optional_text(user_profile_id),
                    text(capabilities),
                    text(rule_effect_projection(rule.effect)),
                    rule.expires_at
                        .map_or(ExactSqlValue::Null, |value| ExactSqlValue::Integer(value.0)),
                ],
            ));
        }
    }

    let topology_key = SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY)?;
    if let Some(ConfigurationValueV1::WorkTopologyPolicy(policy)) =
        snapshot.effective_values.get(&topology_key)
    {
        policy.validate()?;
        let policy_digest = policy.compute_digest()?;
        let placement_kind = match &policy.placement {
            WorktreePlacementModeV1::ExistingWorktreeOnly => "existing_worktree_only",
            WorktreePlacementModeV1::SiblingOfPrimaryCheckout => "sibling_of_primary_checkout",
            WorktreePlacementModeV1::RepositoryLocalRoot => "repository_local_root",
            WorktreePlacementModeV1::ConfiguredRoot(_) => "configured_root",
        };
        let sealed_policy = serde_json::to_vec(policy).map_err(|error| {
            DomainError::CanonicalSerialization(format!("encode sealed topology policy: {error}"))
        })?;
        statements.push(statement(
            TOPOLOGY_POLICY_PROJECTION_SQL,
            vec![
                text(revision_id.as_str()),
                integer(i64::from(policy.schema_version)),
                text(policy_digest.0.as_str()),
                text(placement_kind),
                text(projection_encoding(&policy.cross_merge.default_mode)?),
                integer(i64::from(u8::from(
                    policy.cross_merge.allow_cross_repository,
                ))),
                text(projection_encoding(&policy.gates.cleanliness)?),
                text(projection_encoding(&policy.review_topology)?),
                integer(i64::from(u8::from(policy.gates.require_fresh_preflight))),
                integer(i64::from(policy.gates.maximum_preflight_age_seconds.get())),
                text(projection_encoding(&policy.history_rewrite)?),
                text(projection_encoding(&policy.escalation)?),
                text(projection_encoding(&policy.retention.automatic_gc)?),
                text(projection_encoding(&policy.notifications)?),
                ExactSqlValue::Blob(sealed_policy),
            ],
        ));

        for (root_ordinal, root) in policy.roots.iter().enumerate() {
            let repository_scope_digest = canonical_sha256(&(
                "tracedecay.configuration.topology-root-repository-scope.v1",
                &root.repository_scope,
            ))?;
            statements.push(statement(
                TOPOLOGY_ROOT_PROJECTION_SQL,
                vec![
                    text(revision_id.as_str()),
                    integer(i64::try_from(root_ordinal).map_err(|_| {
                        DomainError::InvalidRange {
                            field: "topology root ordinal",
                        }
                    })?),
                    text(root.root_id.as_str()),
                    text(root.locator.locator_digest.as_str()),
                    text(repository_scope_digest.as_str()),
                    integer(i64::from(root.maximum_active_worktrees.get())),
                ],
            ));
        }

        for (rule_ordinal, rule) in policy.protected_refs.iter().enumerate() {
            let selector_kind = match &rule.selector {
                ProtectedRefSelectorV1::NativeDefaultBranch => "native_default_branch",
                ProtectedRefSelectorV1::Exact(_) => "exact",
                ProtectedRefSelectorV1::Prefix(_) => "prefix",
            };
            let selector_digest = canonical_sha256(&(
                "tracedecay.configuration.protected-ref-selector.v1",
                &rule.selector,
            ))?;
            let disposition = match rule.disposition {
                ProtectedRefDispositionV1::Reject => "reject",
                ProtectedRefDispositionV1::RequireHumanApprovalAndIndependentReview => {
                    "require_human_approval_and_independent_review"
                }
            };
            statements.push(statement(
                PROTECTED_REF_PROJECTION_SQL,
                vec![
                    text(revision_id.as_str()),
                    integer(i64::try_from(rule_ordinal).map_err(|_| {
                        DomainError::InvalidRange {
                            field: "protected ref ordinal",
                        }
                    })?),
                    text(selector_kind),
                    text(selector_digest.as_str()),
                    text(disposition),
                ],
            ));
        }
    }
    Ok(statements)
}

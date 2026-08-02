//! Configuration payload codecs and immutable projections.

use super::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, ChangePlanId, ConfigurationAuditEvent,
    ConfigurationCandidateV1, ConfigurationError, ConfigurationMutationReceiptV1,
    ConfigurationProtectedOperationV1, ConfigurationProtectedPlanRecordV1, ConfigurationRevisionId,
    ConfigurationSnapshotV1, ConfigurationStoreError, ConfigurationStoreResult,
    ConfigurationValueV1, DirectConfigurationMutation, Executor, ManifestDigest, ProtectedChange,
    ProtectedChangePlan, RollbackModeV1, Row, RuleEffect, SOURCE_BINDINGS_SETTING_KEY, SettingKey,
    SourceKindV1, WORK_TOPOLOGY_POLICY_SETTING_KEY, canonical_sha256, params,
};
use serde::{Deserialize, Serialize};

pub(super) const CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION: u16 = 1;
pub(super) const CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION: u16 = 2;
pub(super) const CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION: u16 = 1;
pub(super) const CONFIGURATION_SEALED_AUDIT_TARGET_SCHEMA_VERSION: u16 = 1;
pub(super) const CONFIGURATION_AUTHORIZATION_NOT_RECORDED: &str =
    "not_recorded_by_configuration_store_v1";
pub(super) const CONFIGURATION_ACTIVATION_DESIRED_RECORDED: &str = "desired_recorded_v1";

/// `configuration_entries` remains the per-setting storage table, but its
/// payload must retain the full resolver snapshot. The indexed layer columns
/// are copied only from an already-typed candidate; they never create or
/// upgrade an authority reference.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredConfigurationSnapshotEntryV1 {
    pub(super) schema_version: u16,
    pub(super) value: Option<ConfigurationValueV1>,
    pub(super) provenance: Vec<ConfigurationCandidateV1>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum StoredConfigurationProtectedOperationV1 {
    Change(Box<ProtectedChange>),
    Rollback {
        target_revision_id: ConfigurationRevisionId,
        mode: RollbackModeV1,
    },
}

impl From<&ConfigurationProtectedOperationV1> for StoredConfigurationProtectedOperationV1 {
    fn from(operation: &ConfigurationProtectedOperationV1) -> Self {
        match operation {
            ConfigurationProtectedOperationV1::Change(change) => Self::Change(change.clone()),
            ConfigurationProtectedOperationV1::Rollback {
                target_revision_id,
                mode,
            } => Self::Rollback {
                target_revision_id: target_revision_id.clone(),
                mode: *mode,
            },
        }
    }
}

impl From<StoredConfigurationProtectedOperationV1> for ConfigurationProtectedOperationV1 {
    fn from(operation: StoredConfigurationProtectedOperationV1) -> Self {
        match operation {
            StoredConfigurationProtectedOperationV1::Change(change) => Self::Change(change),
            StoredConfigurationProtectedOperationV1::Rollback {
                target_revision_id,
                mode,
            } => Self::Rollback {
                target_revision_id,
                mode,
            },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredConfigurationPlanPayloadV2 {
    pub(super) schema_version: u16,
    pub(super) plan: ProtectedChangePlan,
    pub(super) operation: StoredConfigurationProtectedOperationV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredConfigurationAuditPayloadV1 {
    pub(super) schema_version: u16,
    pub(super) event: ConfigurationAuditEvent,
}

/// This payload never crosses the audit read API. The current crypto contract
/// provides canonical integrity commitments, not a database-key encryption
/// lifecycle, so the reference is kept in a private BLOB while readers receive
/// only its event-scoped commitment.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SealedAuditTargetReferenceV1<T> {
    pub(super) schema_version: u16,
    pub(super) target: T,
}

#[derive(Debug)]
pub(super) struct StoredRevisionMetadata {
    pub(super) revision_id: String,
    pub(super) parent_revision_id: Option<String>,
    pub(super) snapshot_id: String,
    pub(super) effective_behavior_digest: String,
    pub(super) resolution_provenance_digest: String,
    pub(super) actor_id: String,
    pub(super) operation_kind: String,
    pub(super) created_at: i64,
}

#[derive(Debug)]
pub(super) struct StoredMutationReceipt {
    pub(super) receipt: ConfigurationMutationReceiptV1,
    pub(super) plan_id: Option<ChangePlanId>,
    pub(super) authorization_policy_digest: String,
    pub(super) activation_status: String,
}

#[derive(Serialize)]
pub(super) struct RedactedDirectConfigurationAuditTargetV1 {
    target_scope_digest: ManifestDigest,
    setting_keys: Vec<SettingKey>,
}

pub(super) fn redacted_direct_audit_target(
    mutation: &DirectConfigurationMutation,
) -> Result<RedactedDirectConfigurationAuditTargetV1, ConfigurationError> {
    Ok(RedactedDirectConfigurationAuditTargetV1 {
        target_scope_digest: mutation.target_scope_digest()?,
        setting_keys: mutation.touched_keys()?.into_iter().collect(),
    })
}

pub(super) fn invalid_store_data(message: impl Into<String>) -> ConfigurationStoreError {
    ConfigurationStoreError::InvalidData(message.into())
}

pub(super) fn unavailable_store<E>(_error: E) -> ConfigurationStoreError {
    ConfigurationStoreError::Unavailable
}

pub(super) fn decode_id<T>(value: String, field: &'static str) -> ConfigurationStoreResult<T>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    T::try_from(value).map_err(|error| {
        invalid_store_data(format!("invalid stored configuration {field}: {error}"))
    })
}

pub(super) fn projection_encoding<T: Serialize>(value: &T) -> ConfigurationStoreResult<String> {
    match serde_json::to_value(value)
        .map_err(|error| invalid_store_data(format!("encode configuration projection: {error}")))?
    {
        serde_json::Value::String(value) => Ok(value),
        value => serde_json::to_string(&value).map_err(|error| {
            invalid_store_data(format!(
                "encode structured configuration projection: {error}"
            ))
        }),
    }
}

pub(super) fn authority_projection(
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

pub(super) fn source_kind_projection(source_kind: SourceKindV1) -> &'static str {
    match source_kind {
        SourceKindV1::Claude => "claude",
        SourceKindV1::Codex => "codex",
        SourceKindV1::Cursor => "cursor",
        SourceKindV1::GitHub => "github",
        SourceKindV1::Hermes => "hermes",
        SourceKindV1::Kiro => "kiro",
    }
}

pub(super) fn rule_effect_projection(effect: RuleEffect) -> &'static str {
    match effect {
        RuleEffect::Allow => "allow",
        RuleEffect::Deny => "deny",
    }
}

pub(super) async fn insert_configuration_projections(
    transaction: &impl Executor,
    revision_id: &ConfigurationRevisionId,
    snapshot: &ConfigurationSnapshotV1,
) -> ConfigurationStoreResult<()> {
    let source_bindings_key =
        SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).map_err(ConfigurationStoreError::from)?;
    if let Some(ConfigurationValueV1::SourceBindings(bindings)) =
        snapshot.effective_values.get(&source_bindings_key)
    {
        let candidates = snapshot
            .provenance
            .get(&source_bindings_key)
            .cloned()
            .unwrap_or_default();
        for binding in bindings {
            binding.validate().map_err(ConfigurationStoreError::from)?;
            let (authority_kind, project_id, user_profile_id) =
                authority_projection(&binding.authority);
            let provenance_digest = canonical_sha256(&(
                "tracedecay.configuration.source-binding-projection.v1",
                binding,
                &candidates,
            ))
            .map_err(ConfigurationStoreError::from)?;
            transaction
                .execute(
                    "INSERT INTO configuration_source_bindings (
                        revision_id, binding_id, source_kind, locator_digest,
                        authority_kind, project_id, user_profile_id, provenance_digest
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        revision_id.as_str(),
                        binding.binding_id.as_str(),
                        source_kind_projection(binding.source_kind),
                        binding.source_locator_digest.as_str(),
                        authority_kind,
                        project_id,
                        user_profile_id,
                        provenance_digest.as_str(),
                    ],
                )
                .await
                .map_err(unavailable_store)?;
        }
    }

    let access_rules_key =
        SettingKey::new(ACCESS_RULES_SETTING_KEY).map_err(ConfigurationStoreError::from)?;
    if let Some(ConfigurationValueV1::AccessRules(rules)) =
        snapshot.effective_values.get(&access_rules_key)
    {
        for rule in rules {
            rule.validate().map_err(ConfigurationStoreError::from)?;
            let (authority_kind, project_id, user_profile_id) =
                authority_projection(&rule.authority);
            let subject_id = canonical_sha256(&(
                "tracedecay.configuration.access-rule-subject.v1",
                &rule.subject,
            ))
            .map_err(ConfigurationStoreError::from)?;
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
                .map(tracedecay_domain::CapabilityId::as_str)
                .collect::<Vec<_>>()
                .join(",");
            transaction
                .execute(
                    "INSERT INTO configuration_access_rules (
                        revision_id, rule_id, subject_kind, subject_id, actor_kind, actor_id,
                        operation_kind, source_kind, authority_kind, project_id, user_profile_id,
                        capability_encoding, effect, expires_at
                     ) VALUES (?1, ?2, 'scope_access_subject_v1', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        revision_id.as_str(),
                        rule.rule_id.as_str(),
                        subject_id.as_str(),
                        actor_kind,
                        actor_id,
                        operation_kind,
                        source_kind,
                        authority_kind,
                        project_id,
                        user_profile_id,
                        capabilities,
                        rule_effect_projection(rule.effect),
                        rule.expires_at.map(|value| value.0),
                    ],
                )
                .await
                .map_err(unavailable_store)?;
        }
    }

    let topology_key =
        SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY).map_err(ConfigurationStoreError::from)?;
    if let Some(ConfigurationValueV1::WorkTopologyPolicy(policy)) =
        snapshot.effective_values.get(&topology_key)
    {
        policy.validate().map_err(ConfigurationStoreError::from)?;
        let policy_digest = policy
            .compute_digest()
            .map_err(ConfigurationStoreError::from)?;
        let placement_kind = match &policy.placement {
            tracedecay_domain::configuration::WorktreePlacementModeV1::ExistingWorktreeOnly => {
                "existing_worktree_only"
            }
            tracedecay_domain::configuration::WorktreePlacementModeV1::SiblingOfPrimaryCheckout => {
                "sibling_of_primary_checkout"
            }
            tracedecay_domain::configuration::WorktreePlacementModeV1::RepositoryLocalRoot => {
                "repository_local_root"
            }
            tracedecay_domain::configuration::WorktreePlacementModeV1::ConfiguredRoot(_) => {
                "configured_root"
            }
        };
        transaction
            .execute(
                "INSERT INTO configuration_topology_policies (
                    revision_id, schema_version, topology_policy_digest, placement_kind,
                    default_cross_merge_mode, allow_cross_repository, cleanliness_kind,
                    review_kind, require_fresh_preflight, maximum_preflight_age_seconds,
                    history_rewrite_kind, escalation_kind, automatic_gc_kind, notification_level,
                    sealed_policy_value
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    revision_id.as_str(),
                    i64::from(policy.schema_version),
                    policy_digest.0.as_str(),
                    placement_kind,
                    projection_encoding(&policy.cross_merge.default_mode)?,
                    i64::from(u8::from(policy.cross_merge.allow_cross_repository)),
                    projection_encoding(&policy.gates.cleanliness)?,
                    projection_encoding(&policy.review_topology)?,
                    i64::from(u8::from(policy.gates.require_fresh_preflight)),
                    i64::from(policy.gates.maximum_preflight_age_seconds.get()),
                    projection_encoding(&policy.history_rewrite)?,
                    projection_encoding(&policy.escalation)?,
                    projection_encoding(&policy.retention.automatic_gc)?,
                    projection_encoding(&policy.notifications)?,
                    serde_json::to_vec(policy).map_err(|error| {
                        invalid_store_data(format!("encode sealed topology policy: {error}"))
                    })?,
                ],
            )
            .await
            .map_err(unavailable_store)?;

        for (root_ordinal, root) in policy.roots.iter().enumerate() {
            let repository_scope_digest = canonical_sha256(&(
                "tracedecay.configuration.topology-root-repository-scope.v1",
                &root.repository_scope,
            ))
            .map_err(ConfigurationStoreError::from)?;
            transaction
                .execute(
                    "INSERT INTO configuration_topology_roots (
                        revision_id, root_ordinal, root_id, locator_digest,
                        repository_scope_digest, maximum_active_worktrees
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        revision_id.as_str(),
                        i64::try_from(root_ordinal).map_err(|_| {
                            invalid_store_data("topology root ordinal exceeds SQLite range")
                        })?,
                        root.root_id.as_str(),
                        root.locator.locator_digest.as_str(),
                        repository_scope_digest.as_str(),
                        i64::from(root.maximum_active_worktrees.get()),
                    ],
                )
                .await
                .map_err(unavailable_store)?;
        }

        for (rule_ordinal, rule) in policy.protected_refs.iter().enumerate() {
            let selector_kind = match &rule.selector {
                tracedecay_domain::configuration::ProtectedRefSelectorV1::NativeDefaultBranch => {
                    "native_default_branch"
                }
                tracedecay_domain::configuration::ProtectedRefSelectorV1::Exact(_) => "exact",
                tracedecay_domain::configuration::ProtectedRefSelectorV1::Prefix(_) => "prefix",
            };
            let selector_digest = canonical_sha256(&(
                "tracedecay.configuration.protected-ref-selector.v1",
                &rule.selector,
            ))
            .map_err(ConfigurationStoreError::from)?;
            let disposition = match rule.disposition {
                tracedecay_domain::configuration::ProtectedRefDispositionV1::Reject => "reject",
                tracedecay_domain::configuration::ProtectedRefDispositionV1::RequireHumanApprovalAndIndependentReview => {
                    "require_human_approval_and_independent_review"
                }
            };
            transaction
                .execute(
                    "INSERT INTO configuration_topology_protected_refs (
                        revision_id, rule_ordinal, selector_kind, selector_digest, disposition
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        revision_id.as_str(),
                        i64::try_from(rule_ordinal).map_err(|_| {
                            invalid_store_data("protected ref ordinal exceeds SQLite range")
                        })?,
                        selector_kind,
                        selector_digest.as_str(),
                        disposition,
                    ],
                )
                .await
                .map_err(unavailable_store)?;
        }
    }
    Ok(())
}

pub(super) fn decode_plan_row(
    row: &Row,
) -> ConfigurationStoreResult<ConfigurationProtectedPlanRecordV1> {
    let stored_plan_id = row
        .get::<String>(0)
        .map_err(|error| invalid_store_data(format!("read configuration plan id: {error}")))?;
    let stored_actor_id = row.get::<String>(1).map_err(|error| {
        invalid_store_data(format!("read configuration plan actor id: {error}"))
    })?;
    let stored_base_revision_id = row.get::<String>(2).map_err(|error| {
        invalid_store_data(format!("read configuration plan base revision id: {error}"))
    })?;
    let stored_operation_digest = row.get::<String>(3).map_err(|error| {
        invalid_store_data(format!("read configuration plan operation digest: {error}"))
    })?;
    let stored_scope_digest = row.get::<String>(4).map_err(|error| {
        invalid_store_data(format!("read configuration plan scope digest: {error}"))
    })?;
    let stored_membership_digest = row.get::<Option<String>>(5).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan membership digest: {error}"
        ))
    })?;
    let stored_policy_digest = row.get::<String>(6).map_err(|error| {
        invalid_store_data(format!("read configuration plan policy digest: {error}"))
    })?;
    let stored_policy_epoch = row.get::<i64>(7).map_err(|error| {
        invalid_store_data(format!("read configuration plan policy epoch: {error}"))
    })?;
    let stored_expires_at = row
        .get::<i64>(8)
        .map_err(|error| invalid_store_data(format!("read configuration plan expiry: {error}")))?;
    let stored_created_at = row.get::<i64>(9).map_err(|error| {
        invalid_store_data(format!("read configuration plan creation time: {error}"))
    })?;
    let sequence = row.get::<Option<i64>>(10).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan operation sequence: {error}"
        ))
    })?;
    let payload_schema_revision = row.get::<Option<i64>>(11).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan payload schema revision: {error}"
        ))
    })?;
    let sealed_payload = row.get::<Option<Vec<u8>>>(12).map_err(|error| {
        invalid_store_data(format!("read configuration plan sealed payload: {error}"))
    })?;
    let operation_digest = row.get::<Option<String>>(13).map_err(|error| {
        invalid_store_data(format!(
            "read configuration plan operation digest payload: {error}"
        ))
    })?;

    if sequence != Some(0)
        || payload_schema_revision != Some(i64::from(CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION))
    {
        return Err(invalid_store_data(
            "configuration plan does not contain its canonical initial operation payload",
        ));
    }
    let Some(sealed_payload) = sealed_payload else {
        return Err(invalid_store_data(
            "configuration plan operation payload is missing",
        ));
    };
    let payload = serde_json::from_slice::<StoredConfigurationPlanPayloadV2>(&sealed_payload)
        .map_err(|error| {
            invalid_store_data(format!("decode configuration plan payload: {error}"))
        })?;
    if payload.schema_version != CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION {
        return Err(invalid_store_data(
            "unsupported configuration plan payload schema version",
        ));
    }
    let record = ConfigurationProtectedPlanRecordV1 {
        plan: payload.plan,
        operation: payload.operation.into(),
    };
    record.validate().map_err(ConfigurationStoreError::from)?;
    let stored_policy_epoch = u64::try_from(stored_policy_epoch)
        .map_err(|_| invalid_store_data("configuration plan policy epoch is negative"))?;
    if record.plan.plan_id.as_str() != stored_plan_id
        || record.plan.actor_id.as_str() != stored_actor_id
        || record.plan.base_revision_id.as_str() != stored_base_revision_id
        || record.plan.operation_digest.as_str() != stored_operation_digest
        || record.plan.operation_digest.as_str() != operation_digest.as_deref().unwrap_or_default()
        || record.plan.resolved_scope_digest.as_str() != stored_scope_digest
        || record
            .plan
            .membership_digest
            .as_ref()
            .map(ManifestDigest::as_str)
            != stored_membership_digest.as_deref()
        || record.plan.authorization_policy_digest.as_str() != stored_policy_digest
        || record.plan.policy_epoch != stored_policy_epoch
        || record.plan.expires_at.0 != stored_expires_at
        || record.plan.created_at.0 != stored_created_at
    {
        return Err(invalid_store_data(
            "configuration plan payload does not match immutable projections",
        ));
    }
    Ok(record)
}

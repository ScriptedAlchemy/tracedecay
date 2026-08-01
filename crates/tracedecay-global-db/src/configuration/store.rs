//! `SQLite` primitives for configuration migration receipts and quarantine.
//!
//! The shared global-db lifecycle wires this adapter into its transaction
//! boundary. This module intentionally does not register itself or mutate the
//! legacy configuration file.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use tracedecay_domain::configuration::{
    ACCESS_RULES_SETTING_KEY, AuthorityRef, CandidateDispositionV1, ChangePlanId,
    ConfigurationAuditEvent, ConfigurationAuditEventId, ConfigurationAuditEventKindV1,
    ConfigurationCandidateV1, ConfigurationIdempotencyKey, ConfigurationLayerIdV1,
    ConfigurationReceiptId, ConfigurationRevisionId, ConfigurationSnapshotId,
    ConfigurationSnapshotV1, ConfigurationValueV1, CredentialKindV1, CredentialReferenceId,
    CredentialReferenceMetadataV1, ProtectedChange, ProtectedChangePlan,
    ProtectedChangeSnapshotError, RedactedConfigurationChangeV1, RollbackModeV1, RuleEffect,
    SOURCE_BINDINGS_SETTING_KEY, ScopeControlOperationV1, ScopeSourceBinding, SettingKey,
    SourceKindV1, WORK_TOPOLOGY_POLICY_SETTING_KEY,
};
use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_store::configuration::{
    ConfigurationCommitV1, ConfigurationMutationReceiptV1, ConfigurationProtectedOperationV1,
    ConfigurationProtectedPlanRecordV1, ConfigurationRevisionRecordV1, ConfigurationRevisionStore,
    ConfigurationStoreError, ConfigurationStoreResult,
};
use zeroize::Zeroizing;

use super::contracts::{
    AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT, ComponentConfigurationState,
    ConfigurationAuditPage, ConfigurationAuditQuery, ConfigurationControlStore,
    ConfigurationCurrentStateV1, ConfigurationError, ConfigurationMutationAuthority,
    ConfigurationMutationReceipt, ConfigurationOperationFuture, ConfigurationRollbackRequest,
    CredentialWritePort, DirectConfigurationMutation, ScopeRevalidationEvidenceV1,
    WriteOnlyCredentialMutation,
};
use super::migration::{
    CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME, ConfigurationMigrationError,
    ConfigurationMigrationQuarantineEntryV1, ConfigurationMigrationReceiptV1,
    ConfigurationMigrationStore, LegacyConfigurationSourceKindV1,
};
use super::registry::ConfigurationRegistry;
use super::resolver::{ConfigurationResolutionV1, registry_default_candidate};
use super::schema::ConfigurationSchemaError;
#[cfg(test)]
use super::schema::ensure_configuration_schema;
use crate::RegisteredGlobalDb;
#[cfg(test)]
use tracedecay_runtime_core::db::engine::{Connection, TestConnection, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};

mod audit;
mod read;
mod write;

use audit::*;
use read::*;
use write::*;

#[derive(Debug, Error)]
pub enum ConfigurationStorageError {
    #[error("configuration schema error: {0}")]
    Schema(#[from] ConfigurationSchemaError),
    #[error("configuration storage error: {0}")]
    Sql(#[from] tracedecay_runtime_core::db::engine::Error),
    #[error("configuration storage encoded invalid data: {0}")]
    Encoding(String),
}

/// Connection-local SQL helper used only behind the registered database's writer and
/// read-snapshot lanes. It is deliberately not a public authority surface.
#[cfg(test)]
struct ConfigurationSqlStore<'a> {
    connection: &'a Connection,
}

#[cfg(test)]
impl<'a> ConfigurationSqlStore<'a> {
    fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    async fn migration_receipt(
        &self,
        receipt_name: &str,
        source_snapshot_digest: &ManifestDigest,
    ) -> Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationStorageError> {
        let mut rows = self
            .connection
            .query(
                "SELECT initial_revision_id, initial_snapshot_id, created_at
                 FROM configuration_migration_receipts
                 WHERE receipt_name = ?1 AND source_snapshot_digest = ?2",
                params![receipt_name, source_snapshot_digest.as_str()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let initial_revision_id = ConfigurationRevisionId::new(row.get::<String>(0)?)
            .map_err(|error| ConfigurationStorageError::Encoding(error.to_string()))?;
        let initial_snapshot_id = ConfigurationSnapshotId::new(row.get::<String>(1)?)
            .map_err(|error| ConfigurationStorageError::Encoding(error.to_string()))?;
        let created_at = tracedecay_domain::UtcMicros(row.get::<i64>(2)?);
        let receipt_name = match receipt_name {
            super::migration::CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME => {
                super::migration::CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME
            }
            _ => {
                return Err(ConfigurationStorageError::Encoding(
                    "unrecognized configuration migration receipt name".to_owned(),
                ));
            }
        };
        Ok(Some(ConfigurationMigrationReceiptV1 {
            receipt_name,
            source_snapshot_digest: source_snapshot_digest.clone(),
            initial_revision_id,
            initial_snapshot_id,
            created_at,
        }))
    }
}

fn source_kind_name(source_kind: LegacyConfigurationSourceKindV1) -> &'static str {
    source_kind.as_str()
}

#[cfg(test)]
impl ConfigurationMigrationStore for ConfigurationSqlStore<'_> {
    fn receipt(
        &self,
        receipt_name: &'static str,
        source_snapshot_digest: &ManifestDigest,
    ) -> impl Future<
        Output = Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationMigrationError>,
    > + Send {
        async move {
            self.migration_receipt(receipt_name, source_snapshot_digest)
                .await
                .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))
        }
    }

    fn commit_initial_migration(
        &self,
        receipt: &ConfigurationMigrationReceiptV1,
        resolution: &ConfigurationResolutionV1,
        quarantine: &[ConfigurationMigrationQuarantineEntryV1],
    ) -> impl Future<Output = Result<(), ConfigurationMigrationError>> + Send {
        async move {
            resolution
                .snapshot
                .validate()
                .map_err(ConfigurationMigrationError::Domain)?;
            if receipt.receipt_name != CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME
                || receipt.initial_snapshot_id != resolution.snapshot.snapshot_id
            {
                return Err(ConfigurationMigrationError::Store(
                    "migration receipt does not bind the initial snapshot".to_owned(),
                ));
            }

            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
            let outcome = commit_initial_migration_transaction(
                &transaction,
                receipt,
                resolution,
                quarantine,
                false,
            )
            .await;
            match outcome {
                Ok(()) => transaction
                    .commit()
                    .await
                    .map_err(|error| ConfigurationMigrationError::Store(error.to_string())),
                Err(error) => {
                    let _ = transaction.rollback().await;
                    Err(error)
                }
            }
        }
    }
}

async fn commit_initial_migration_transaction(
    transaction: &impl Executor,
    receipt: &ConfigurationMigrationReceiptV1,
    resolution: &ConfigurationResolutionV1,
    quarantine: &[ConfigurationMigrationQuarantineEntryV1],
    fault_after_receipt: bool,
) -> Result<(), ConfigurationMigrationError> {
    validate_snapshot_registry_completeness(&resolution.snapshot)
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    if migration_receipt_exists(
        transaction,
        receipt.receipt_name,
        &receipt.source_snapshot_digest,
        &receipt.initial_revision_id,
        &receipt.initial_snapshot_id,
    )
    .await?
    {
        return Ok(());
    }

    transaction
        .execute(
            "INSERT INTO configuration_revisions (
                revision_id, parent_revision_id, snapshot_id,
                effective_behavior_digest, resolution_provenance_digest,
                actor_id, operation_kind, created_at
             ) VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                receipt.initial_revision_id.as_str(),
                receipt.initial_snapshot_id.as_str(),
                resolution.snapshot.effective_behavior_digest.as_str(),
                resolution.snapshot.resolution_provenance_digest.as_str(),
                "actor.configuration-migration",
                "legacy_read_only_migration",
                receipt.created_at.0,
            ],
        )
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;

    insert_snapshot_entries(
        transaction,
        &receipt.initial_revision_id,
        &resolution.snapshot,
    )
    .await
    .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    insert_configuration_projections(
        transaction,
        &receipt.initial_revision_id,
        &resolution.snapshot,
    )
    .await
    .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;

    for entry in quarantine {
        transaction
            .execute(
                "INSERT OR IGNORE INTO configuration_migration_quarantine (
                    source_kind, source_key_digest, reason_code,
                    redacted_value_digest, quarantined_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    source_kind_name(entry.source_kind),
                    entry.source_key_digest.as_str(),
                    entry.reason.as_str(),
                    entry.redacted_value_digest.as_str(),
                    entry.quarantined_at.0,
                ],
            )
            .await
            .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    }

    transaction
        .execute(
            "INSERT INTO configuration_migration_receipts (
                receipt_name, source_snapshot_digest, initial_revision_id,
                initial_snapshot_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                receipt.receipt_name,
                receipt.source_snapshot_digest.as_str(),
                receipt.initial_revision_id.as_str(),
                receipt.initial_snapshot_id.as_str(),
                receipt.created_at.0,
            ],
        )
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;

    if fault_after_receipt {
        return Err(ConfigurationMigrationError::Store(
            "injected migration crash after receipt".to_owned(),
        ));
    }

    let audit_digest = canonical_sha256(&(
        "tracedecay.configuration.migration-audit.v1",
        receipt.receipt_name,
        &receipt.source_snapshot_digest,
        &receipt.initial_revision_id,
        &receipt.initial_snapshot_id,
    ))
    .map_err(ConfigurationMigrationError::Domain)?;
    let audit_suffix = audit_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| ConfigurationMigrationError::Store("invalid audit digest".to_owned()))?;
    let audit_event = ConfigurationAuditEvent {
        event_id: decode_id(
            format!("configuration.audit.migration.{audit_suffix}"),
            "migration audit event id",
        )
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?,
        event_kind: ConfigurationAuditEventKindV1::Recovered,
        actor_id: decode_id(
            "actor.configuration-migration".to_owned(),
            "migration actor id",
        )
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?,
        idempotency_key: None,
        base_revision_id: receipt.initial_revision_id.clone(),
        result_revision_id: Some(receipt.initial_revision_id.clone()),
        operation_digest: audit_digest.clone(),
        target_commitment: audit_digest,
        receipt_id: None,
        safe_reason_code: None,
        occurred_at: receipt.created_at,
    };
    insert_audit_event_with_receipt_digest(transaction, &audit_event, None, None)
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    Ok(())
}

async fn migration_receipt_exists(
    transaction: &impl QueryExecutor,
    receipt_name: &str,
    source_snapshot_digest: &ManifestDigest,
    initial_revision_id: &ConfigurationRevisionId,
    initial_snapshot_id: &ConfigurationSnapshotId,
) -> Result<bool, ConfigurationMigrationError> {
    let mut rows = transaction
        .query(
            "SELECT initial_revision_id, initial_snapshot_id
             FROM configuration_migration_receipts
             WHERE receipt_name = ?1 AND source_snapshot_digest = ?2",
            params![receipt_name, source_snapshot_digest.as_str()],
        )
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?
    else {
        return Ok(false);
    };
    let stored_revision = row
        .get::<String>(0)
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    let stored_snapshot = row
        .get::<String>(1)
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    if stored_revision != initial_revision_id.as_str()
        || stored_snapshot != initial_snapshot_id.as_str()
    {
        return Err(ConfigurationMigrationError::Store(
            "configuration migration replay conflicts with stored receipt".to_owned(),
        ));
    }
    Ok(true)
}

async fn migration_receipt_from_transaction(
    transaction: &impl QueryExecutor,
    receipt_name: &str,
    source_snapshot_digest: &ManifestDigest,
) -> Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationMigrationError> {
    let mut rows = transaction
        .query(
            "SELECT initial_revision_id, initial_snapshot_id, created_at
             FROM configuration_migration_receipts
             WHERE receipt_name = ?1 AND source_snapshot_digest = ?2",
            params![receipt_name, source_snapshot_digest.as_str()],
        )
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?
    else {
        return Ok(None);
    };
    let initial_revision_id = ConfigurationRevisionId::new(
        row.get::<String>(0)
            .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?,
    )
    .map_err(ConfigurationMigrationError::Domain)?;
    let initial_snapshot_id = ConfigurationSnapshotId::new(
        row.get::<String>(1)
            .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?,
    )
    .map_err(ConfigurationMigrationError::Domain)?;
    let created_at = UtcMicros(
        row.get::<i64>(2)
            .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?,
    );
    let receipt_name = match receipt_name {
        CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME => {
            CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME
        }
        _ => {
            return Err(ConfigurationMigrationError::Store(
                "unrecognized configuration migration receipt name".to_owned(),
            ));
        }
    };
    Ok(Some(ConfigurationMigrationReceiptV1 {
        receipt_name,
        source_snapshot_digest: source_snapshot_digest.clone(),
        initial_revision_id,
        initial_snapshot_id,
        created_at,
    }))
}

const CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION: u16 = 1;
const CONFIGURATION_PLAN_PAYLOAD_SCHEMA_VERSION: u16 = 2;
const CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION: u16 = 1;
const CONFIGURATION_SEALED_AUDIT_TARGET_SCHEMA_VERSION: u16 = 1;
const CONFIGURATION_AUTHORIZATION_NOT_RECORDED: &str = "not_recorded_by_configuration_store_v1";
const CONFIGURATION_ACTIVATION_DESIRED_RECORDED: &str = "desired_recorded_v1";

/// `configuration_entries` remains the per-setting storage table, but its
/// payload must retain the full resolver snapshot. The indexed layer columns
/// are copied only from an already-typed candidate; they never create or
/// upgrade an authority reference.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfigurationSnapshotEntryV1 {
    schema_version: u16,
    value: Option<ConfigurationValueV1>,
    provenance: Vec<ConfigurationCandidateV1>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum StoredConfigurationProtectedOperationV1 {
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
struct StoredConfigurationPlanPayloadV2 {
    schema_version: u16,
    plan: ProtectedChangePlan,
    operation: StoredConfigurationProtectedOperationV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfigurationAuditPayloadV1 {
    schema_version: u16,
    event: ConfigurationAuditEvent,
}

/// This payload never crosses the audit read API. The current crypto contract
/// provides canonical integrity commitments, not a database-key encryption
/// lifecycle, so the reference is kept in a private BLOB while readers receive
/// only its event-scoped commitment.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct SealedAuditTargetReferenceV1<T> {
    schema_version: u16,
    target: T,
}

#[derive(Debug)]
struct StoredRevisionMetadata {
    revision_id: String,
    parent_revision_id: Option<String>,
    snapshot_id: String,
    effective_behavior_digest: String,
    resolution_provenance_digest: String,
    actor_id: String,
    operation_kind: String,
    created_at: i64,
}

#[derive(Debug)]
struct StoredMutationReceipt {
    receipt: ConfigurationMutationReceiptV1,
    plan_id: Option<ChangePlanId>,
    authorization_policy_digest: String,
    activation_status: String,
}

#[derive(Serialize)]
struct RedactedDirectConfigurationAuditTargetV1 {
    target_scope_digest: ManifestDigest,
    setting_keys: Vec<SettingKey>,
}

fn redacted_direct_audit_target(
    mutation: &DirectConfigurationMutation,
) -> Result<RedactedDirectConfigurationAuditTargetV1, ConfigurationError> {
    Ok(RedactedDirectConfigurationAuditTargetV1 {
        target_scope_digest: mutation.target_scope_digest()?,
        setting_keys: mutation.touched_keys()?.into_iter().collect(),
    })
}

fn invalid_store_data(message: impl Into<String>) -> ConfigurationStoreError {
    ConfigurationStoreError::InvalidData(message.into())
}

fn unavailable_store<E>(_error: E) -> ConfigurationStoreError {
    ConfigurationStoreError::Unavailable
}

fn decode_id<T>(value: String, field: &'static str) -> ConfigurationStoreResult<T>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    T::try_from(value).map_err(|error| {
        invalid_store_data(format!("invalid stored configuration {field}: {error}"))
    })
}

fn projection_encoding<T: Serialize>(value: &T) -> ConfigurationStoreResult<String> {
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

async fn insert_configuration_projections(
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

async fn insert_revision(
    transaction: &impl Executor,
    revision: &ConfigurationRevisionRecordV1,
) -> ConfigurationStoreResult<()> {
    revision.validate().map_err(ConfigurationStoreError::from)?;
    validate_snapshot_registry_completeness(&revision.snapshot)?;
    let parent_revision_id = revision
        .parent_revision_id
        .as_ref()
        .map(|value| value.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_revisions (
                revision_id, parent_revision_id, snapshot_id,
                effective_behavior_digest, resolution_provenance_digest,
                actor_id, operation_kind, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                revision.revision_id.as_str(),
                parent_revision_id,
                revision.snapshot.snapshot_id.as_str(),
                revision.snapshot.effective_behavior_digest.as_str(),
                revision.snapshot.resolution_provenance_digest.as_str(),
                revision.actor_id.as_str(),
                revision.operation_kind.as_str(),
                revision.created_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    insert_snapshot_entries(transaction, &revision.revision_id, &revision.snapshot).await?;
    insert_configuration_projections(transaction, &revision.revision_id, &revision.snapshot).await
}

fn decode_plan_row(row: &Row) -> ConfigurationStoreResult<ConfigurationProtectedPlanRecordV1> {
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

async fn insert_dry_run_audit_event(
    transaction: &impl Executor,
    record: &ConfigurationProtectedPlanRecordV1,
) -> ConfigurationStoreResult<()> {
    let event_kind = match &record.operation {
        ConfigurationProtectedOperationV1::Change(_) => {
            ConfigurationAuditEventKindV1::DryRunCreated
        }
        ConfigurationProtectedOperationV1::Rollback { .. } => {
            ConfigurationAuditEventKindV1::RollbackDryRunCreated
        }
    };
    let event_id = derived_identifier(
        "configuration.audit.v1",
        &canonical_sha256(&(
            "tracedecay.configuration.dry-run-audit-event.v1",
            &record.plan.plan_id,
            event_kind,
        ))
        .map_err(ConfigurationStoreError::from)?,
        "configuration audit event id",
    )
    .map_err(|error| invalid_store_data(error.to_string()))?;
    let (sealed_target_reference, target_commitment) = seal_audit_target(
        transaction,
        &event_id,
        &record.plan.redacted_changes,
        record.plan.created_at,
    )
    .await?;
    let event = ConfigurationAuditEvent {
        event_id,
        event_kind,
        actor_id: record.plan.actor_id.clone(),
        idempotency_key: None,
        base_revision_id: record.plan.base_revision_id.clone(),
        result_revision_id: None,
        operation_digest: record.plan.operation_digest.clone(),
        target_commitment,
        receipt_id: None,
        safe_reason_code: None,
        occurred_at: record.plan.created_at,
    };
    insert_audit_event_with_receipt_digest(
        transaction,
        &event,
        None,
        Some(&sealed_target_reference),
    )
    .await
}

fn decode_audit_row(
    row: &Row,
) -> ConfigurationStoreResult<(ConfigurationAuditEvent, Option<Vec<u8>>)> {
    let stored_event_id = row.get::<String>(0).map_err(|error| {
        invalid_store_data(format!("read configuration audit event id: {error}"))
    })?;
    let stored_actor_id = row.get::<String>(1).map_err(|error| {
        invalid_store_data(format!("read configuration audit actor id: {error}"))
    })?;
    let stored_idempotency_key = row.get::<Option<String>>(2).map_err(|error| {
        invalid_store_data(format!("read configuration audit idempotency key: {error}"))
    })?;
    let encoded_payload = row.get::<String>(3).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit operation payload: {error}"
        ))
    })?;
    let stored_base_revision_id = row.get::<String>(4).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit base revision id: {error}"
        ))
    })?;
    let stored_result_revision_id = row.get::<Option<String>>(5).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit result revision id: {error}"
        ))
    })?;
    let sealed_target_reference = row.get::<Option<Vec<u8>>>(6).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit sealed target reference: {error}"
        ))
    })?;
    let stored_target_commitment = row.get::<String>(7).map_err(|error| {
        invalid_store_data(format!(
            "read configuration audit target commitment: {error}"
        ))
    })?;
    let stored_receipt_digest = row.get::<Option<String>>(8).map_err(|error| {
        invalid_store_data(format!("read configuration audit receipt digest: {error}"))
    })?;
    let stored_safe_reason_code = row.get::<Option<String>>(9).map_err(|error| {
        invalid_store_data(format!("read configuration audit safe reason: {error}"))
    })?;
    let stored_occurred_at = row
        .get::<i64>(10)
        .map_err(|error| invalid_store_data(format!("read configuration audit time: {error}")))?;

    let payload = serde_json::from_str::<StoredConfigurationAuditPayloadV1>(&encoded_payload)
        .map_err(|error| {
            invalid_store_data(format!("decode configuration audit payload: {error}"))
        })?;
    if payload.schema_version != CONFIGURATION_AUDIT_PAYLOAD_SCHEMA_VERSION {
        return Err(invalid_store_data(
            "unsupported configuration audit payload schema version",
        ));
    }
    payload
        .event
        .validate()
        .map_err(ConfigurationStoreError::from)?;
    let event = payload.event;
    if event.event_id.as_str() != stored_event_id
        || event.actor_id.as_str() != stored_actor_id
        || event
            .idempotency_key
            .as_ref()
            .map(ConfigurationIdempotencyKey::as_str)
            != stored_idempotency_key.as_deref()
        || event.base_revision_id.as_str() != stored_base_revision_id
        || event
            .result_revision_id
            .as_ref()
            .map(ConfigurationRevisionId::as_str)
            != stored_result_revision_id.as_deref()
        || event.target_commitment.as_str() != stored_target_commitment
        || event.receipt_id.is_some() != stored_receipt_digest.is_some()
        || event.safe_reason_code.as_deref() != stored_safe_reason_code.as_deref()
        || event.occurred_at.0 != stored_occurred_at
    {
        return Err(invalid_store_data(
            "configuration audit payload does not match immutable projections",
        ));
    }
    Ok((event, sealed_target_reference))
}

async fn read_audit_event_from_transaction(
    transaction: &impl QueryExecutor,
    event_id: &ConfigurationAuditEventId,
) -> ConfigurationStoreResult<Option<ConfigurationAuditEvent>> {
    let mut rows = transaction
        .query(
            "SELECT event_id, actor_id, idempotency_key, operation_kind,
                    base_revision_id, result_revision_id, sealed_target_reference,
                    event_scoped_target_commitment, receipt_digest, safe_reason_code, occurred_at
             FROM configuration_audit_events
             WHERE event_id = ?1",
            params![event_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let (event, sealed_target_reference) = decode_audit_row(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration audit event id resolved to multiple rows",
        ));
    }
    validate_sealed_audit_target(transaction, &event, sealed_target_reference.as_deref()).await?;
    Ok(Some(event))
}

async fn insert_audit_event_with_receipt_digest(
    transaction: &impl Executor,
    event: &ConfigurationAuditEvent,
    receipt_digest: Option<&ManifestDigest>,
    sealed_target_reference: Option<&[u8]>,
) -> ConfigurationStoreResult<()> {
    event.validate().map_err(ConfigurationStoreError::from)?;
    validate_sealed_audit_target(transaction, event, sealed_target_reference).await?;
    let encoded_payload = encode_audit_payload(event)?;
    let idempotency_key = event
        .idempotency_key
        .as_ref()
        .map(|value| value.as_str().to_owned());
    let result_revision_id = event
        .result_revision_id
        .as_ref()
        .map(|value| value.as_str().to_owned());
    let receipt_digest = receipt_digest.map(|value| value.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_audit_events (
                event_id, actor_id, idempotency_key, operation_kind,
                base_revision_id, result_revision_id, sealed_target_reference,
                event_scoped_target_commitment, receipt_digest, correlation_id,
                safe_reason_code, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11)",
            params![
                event.event_id.as_str(),
                event.actor_id.as_str(),
                idempotency_key,
                encoded_payload,
                event.base_revision_id.as_str(),
                result_revision_id,
                sealed_target_reference,
                event.target_commitment.as_str(),
                receipt_digest,
                event.safe_reason_code.clone(),
                event.occurred_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

fn terminal_plan_event_kind(event_kind: ConfigurationAuditEventKindV1) -> Option<&'static str> {
    match event_kind {
        ConfigurationAuditEventKindV1::Applied => Some("applied"),
        ConfigurationAuditEventKindV1::RollbackApplied => Some("rollback_applied"),
        _ => None,
    }
}

fn is_terminal_plan_event(event_kind: &str) -> bool {
    matches!(event_kind, "applied" | "rollback_applied")
}

async fn append_terminal_plan_event(
    transaction: &impl Executor,
    plan: &ProtectedChangePlan,
    audit_event: &ConfigurationAuditEvent,
) -> ConfigurationStoreResult<()> {
    let Some(terminal_kind) = terminal_plan_event_kind(audit_event.event_kind) else {
        return Err(invalid_store_data(
            "configuration commit with a plan requires an applied terminal audit event",
        ));
    };
    let mut rows = transaction
        .query(
            "SELECT sequence, event_kind
             FROM configuration_change_plan_events
             WHERE plan_id = ?1
             ORDER BY sequence ASC",
            params![plan.plan_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut saw_dry_run = false;
    let mut terminal_count = 0usize;
    let mut last_sequence = None;
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        let sequence = row.get::<i64>(0).map_err(|error| {
            invalid_store_data(format!("read configuration plan event sequence: {error}"))
        })?;
        let event_kind = row.get::<String>(1).map_err(|error| {
            invalid_store_data(format!("read configuration plan event kind: {error}"))
        })?;
        if sequence < 0 || last_sequence.is_some_and(|previous| sequence <= previous) {
            return Err(invalid_store_data(
                "configuration plan events are not strictly ordered",
            ));
        }
        if sequence == 0 && event_kind == "dry_run_created" {
            saw_dry_run = true;
        }
        if is_terminal_plan_event(&event_kind) {
            terminal_count += 1;
        }
        last_sequence = Some(sequence);
    }
    drop(rows);
    if !saw_dry_run || terminal_count != 0 {
        return Err(ConfigurationStoreError::PlanStale);
    }
    let sequence = last_sequence
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid_store_data("configuration plan event sequence overflow"))?;
    transaction
        .execute(
            "INSERT INTO configuration_change_plan_events (
                plan_id, sequence, event_kind, safe_reason_code, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                plan.plan_id.as_str(),
                sequence,
                terminal_kind,
                audit_event.safe_reason_code.clone(),
                audit_event.occurred_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

async fn has_matching_terminal_plan_event(
    transaction: &impl QueryExecutor,
    plan: &ProtectedChangePlan,
    audit_event: &ConfigurationAuditEvent,
) -> ConfigurationStoreResult<bool> {
    let Some(expected_kind) = terminal_plan_event_kind(audit_event.event_kind) else {
        return Ok(false);
    };
    let mut rows = transaction
        .query(
            "SELECT event_kind
             FROM configuration_change_plan_events
             WHERE plan_id = ?1",
            params![plan.plan_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut terminal_count = 0usize;
    let mut matched = false;
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        let event_kind = row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read configuration plan terminal event: {error}"))
        })?;
        if is_terminal_plan_event(&event_kind) {
            terminal_count += 1;
            matched |= event_kind == expected_kind;
        }
    }
    Ok(terminal_count == 1 && matched)
}

fn decode_stored_mutation_receipt(row: &Row) -> ConfigurationStoreResult<StoredMutationReceipt> {
    let receipt_id: ConfigurationReceiptId = decode_id(
        row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read configuration receipt id: {error}"))
        })?,
        "receipt id",
    )?;
    let plan_id: Option<ChangePlanId> = row
        .get::<Option<String>>(1)
        .map_err(|error| {
            invalid_store_data(format!("read configuration receipt plan id: {error}"))
        })?
        .map(|value| decode_id(value, "receipt plan id"))
        .transpose()?;
    let actor_id: ActorId = decode_id(
        row.get::<String>(2).map_err(|error| {
            invalid_store_data(format!("read configuration receipt actor id: {error}"))
        })?,
        "receipt actor id",
    )?;
    let idempotency_key: ConfigurationIdempotencyKey = decode_id(
        row.get::<String>(3).map_err(|error| {
            invalid_store_data(format!(
                "read configuration receipt idempotency key: {error}"
            ))
        })?,
        "receipt idempotency key",
    )?;
    let base_revision_id: ConfigurationRevisionId = decode_id(
        row.get::<String>(4).map_err(|error| {
            invalid_store_data(format!(
                "read configuration receipt base revision id: {error}"
            ))
        })?,
        "receipt base revision id",
    )?;
    let result_revision_id: ConfigurationRevisionId = decode_id(
        row.get::<String>(5).map_err(|error| {
            invalid_store_data(format!(
                "read configuration receipt result revision id: {error}"
            ))
        })?,
        "receipt result revision id",
    )?;
    let operation_digest = ManifestDigest::new(row.get::<String>(6).map_err(|error| {
        invalid_store_data(format!(
            "read configuration receipt operation digest: {error}"
        ))
    })?)
    .map_err(ConfigurationStoreError::from)?;
    let authorization_policy_digest = row.get::<String>(7).map_err(|error| {
        invalid_store_data(format!(
            "read configuration receipt authorization digest: {error}"
        ))
    })?;
    let activation_status = row.get::<String>(8).map_err(|error| {
        invalid_store_data(format!(
            "read configuration receipt activation status: {error}"
        ))
    })?;
    let receipt_digest = ManifestDigest::new(row.get::<String>(9).map_err(|error| {
        invalid_store_data(format!("read configuration receipt digest: {error}"))
    })?)
    .map_err(ConfigurationStoreError::from)?;
    let created_at = row
        .get::<i64>(10)
        .map_err(|error| invalid_store_data(format!("read configuration receipt time: {error}")))?;
    let receipt = ConfigurationMutationReceiptV1 {
        receipt_id,
        actor_id,
        idempotency_key,
        base_revision_id,
        result_revision_id,
        operation_digest,
        receipt_digest,
        created_at: UtcMicros(created_at),
    };
    receipt.validate().map_err(ConfigurationStoreError::from)?;
    Ok(StoredMutationReceipt {
        receipt,
        plan_id,
        authorization_policy_digest,
        activation_status,
    })
}

async fn receipt_for_idempotency_from_transaction(
    transaction: &impl QueryExecutor,
    actor_id: &ActorId,
    idempotency_key: &ConfigurationIdempotencyKey,
) -> ConfigurationStoreResult<Option<StoredMutationReceipt>> {
    let mut rows = transaction
        .query(
            "SELECT receipt_id, plan_id, actor_id, idempotency_key,
                    base_revision_id, result_revision_id, operation_digest,
                    authorization_policy_digest, activation_status, receipt_digest, created_at
             FROM configuration_mutation_receipts
             WHERE actor_id = ?1 AND idempotency_key = ?2",
            params![actor_id.as_str(), idempotency_key.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let receipt = decode_stored_mutation_receipt(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration idempotency key resolved to multiple receipts",
        ));
    }
    Ok(Some(receipt))
}

fn authorization_policy_digest_for_commit(commit: &ConfigurationCommitV1) -> String {
    commit.change_plan.as_ref().map_or_else(
        || CONFIGURATION_AUTHORIZATION_NOT_RECORDED.to_owned(),
        |plan| plan.authorization_policy_digest.as_str().to_owned(),
    )
}

async fn insert_mutation_receipt(
    transaction: &impl Executor,
    commit: &ConfigurationCommitV1,
) -> ConfigurationStoreResult<()> {
    commit
        .receipt
        .validate()
        .map_err(ConfigurationStoreError::from)?;
    let plan_id = commit
        .change_plan
        .as_ref()
        .map(|plan| plan.plan_id.as_str().to_owned());
    transaction
        .execute(
            "INSERT INTO configuration_mutation_receipts (
                receipt_id, plan_id, actor_id, idempotency_key,
                base_revision_id, result_revision_id, operation_digest,
                authorization_policy_digest, activation_status, receipt_digest, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                commit.receipt.receipt_id.as_str(),
                plan_id,
                commit.receipt.actor_id.as_str(),
                commit.receipt.idempotency_key.as_str(),
                commit.receipt.base_revision_id.as_str(),
                commit.receipt.result_revision_id.as_str(),
                commit.receipt.operation_digest.as_str(),
                authorization_policy_digest_for_commit(commit),
                CONFIGURATION_ACTIVATION_DESIRED_RECORDED,
                commit.receipt.receipt_digest.as_str(),
                commit.receipt.created_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

#[derive(Clone, Debug)]
struct StoredComponentActivationState {
    component: String,
    desired_revision_id: ConfigurationRevisionId,
    observed_revision_id: Option<ConfigurationRevisionId>,
    last_working_revision_id: Option<ConfigurationRevisionId>,
    restart_required: bool,
    activation_error_code: Option<String>,
}

fn validate_component_name(component: &str) -> ConfigurationStoreResult<()> {
    if component.is_empty()
        || component.trim() != component
        || component.len() > 256
        || component.chars().any(char::is_control)
    {
        return Err(invalid_store_data(
            "configuration component name is not canonical",
        ));
    }
    Ok(())
}

fn validate_activation_error_code(code: Option<&str>) -> ConfigurationStoreResult<()> {
    let Some(code) = code else {
        return Ok(());
    };
    if code.is_empty()
        || code.trim() != code
        || code.len() > 256
        || code.chars().any(char::is_control)
    {
        return Err(invalid_store_data(
            "configuration activation error code is not canonical",
        ));
    }
    Ok(())
}

fn decode_component_activation_state(
    row: &Row,
) -> ConfigurationStoreResult<StoredComponentActivationState> {
    let component = row
        .get::<String>(0)
        .map_err(|error| invalid_store_data(format!("read configuration component: {error}")))?;
    validate_component_name(&component)?;
    let desired_revision_id = decode_id(
        row.get::<String>(1).map_err(|error| {
            invalid_store_data(format!("read desired configuration revision: {error}"))
        })?,
        "desired component revision id",
    )?;
    let observed_revision_id = row
        .get::<Option<String>>(2)
        .map_err(|error| {
            invalid_store_data(format!("read observed configuration revision: {error}"))
        })?
        .map(|value| decode_id(value, "observed component revision id"))
        .transpose()?;
    let last_working_revision_id = row
        .get::<Option<String>>(3)
        .map_err(|error| {
            invalid_store_data(format!("read last working configuration revision: {error}"))
        })?
        .map(|value| decode_id(value, "last working component revision id"))
        .transpose()?;
    let restart_required = match row
        .get::<i64>(4)
        .map_err(|error| invalid_store_data(format!("read configuration restart state: {error}")))?
    {
        0 => false,
        1 => true,
        _ => {
            return Err(invalid_store_data(
                "stored configuration restart state is invalid",
            ));
        }
    };
    let activation_error_code = row
        .get::<Option<String>>(5)
        .map_err(|error| invalid_store_data(format!("read activation error code: {error}")))?;
    validate_activation_error_code(activation_error_code.as_deref())?;
    Ok(StoredComponentActivationState {
        component,
        desired_revision_id,
        observed_revision_id,
        last_working_revision_id,
        restart_required,
        activation_error_code,
    })
}

async fn latest_component_activation_states(
    transaction: &impl QueryExecutor,
) -> ConfigurationStoreResult<Vec<StoredComponentActivationState>> {
    let mut rows = transaction
        .query(
            "SELECT event.component, event.desired_revision_id, event.observed_revision_id,
                    event.last_working_revision_id, event.restart_required,
                    event.activation_error_code
             FROM configuration_component_activation_events AS event
             WHERE event.event_id = (
                 SELECT MAX(candidate.event_id)
                 FROM configuration_component_activation_events AS candidate
                 WHERE candidate.component = event.component
             )
             ORDER BY event.component ASC",
            (),
        )
        .await
        .map_err(unavailable_store)?;
    let mut states = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        states.push(decode_component_activation_state(&row)?);
    }
    Ok(states)
}

async fn latest_component_activation_state(
    transaction: &impl QueryExecutor,
    component: &str,
) -> ConfigurationStoreResult<Option<StoredComponentActivationState>> {
    let mut rows = transaction
        .query(
            "SELECT component, desired_revision_id, observed_revision_id,
                    last_working_revision_id, restart_required, activation_error_code
             FROM configuration_component_activation_events
             WHERE component = ?1
             ORDER BY event_id DESC
             LIMIT 1",
            params![component],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let state = decode_component_activation_state(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration component latest activation resolved to multiple rows",
        ));
    }
    Ok(Some(state))
}

async fn insert_component_activation_event(
    transaction: &impl Executor,
    state: &StoredComponentActivationState,
    occurred_at: UtcMicros,
) -> ConfigurationStoreResult<()> {
    validate_component_name(&state.component)?;
    validate_activation_error_code(state.activation_error_code.as_deref())?;
    transaction
        .execute(
            "INSERT INTO configuration_component_activation_events (
                component, desired_revision_id, observed_revision_id,
                last_working_revision_id, restart_required, activation_error_code, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                state.component.clone(),
                state.desired_revision_id.as_str(),
                state
                    .observed_revision_id
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
                state
                    .last_working_revision_id
                    .as_ref()
                    .map(|value| value.as_str().to_owned()),
                i64::from(u8::from(state.restart_required)),
                state.activation_error_code.clone(),
                occurred_at.0,
            ],
        )
        .await
        .map_err(unavailable_store)?;
    Ok(())
}

async fn advance_component_desired_state(
    transaction: &impl Executor,
    desired_revision_id: &ConfigurationRevisionId,
    occurred_at: UtcMicros,
) -> ConfigurationStoreResult<()> {
    for prior in latest_component_activation_states(transaction).await? {
        let observed_revision_id = prior
            .observed_revision_id
            .clone()
            .or_else(|| prior.last_working_revision_id.clone());
        let restart_required = observed_revision_id.as_ref() != Some(desired_revision_id)
            || prior.activation_error_code.is_some();
        insert_component_activation_event(
            transaction,
            &StoredComponentActivationState {
                component: prior.component,
                desired_revision_id: desired_revision_id.clone(),
                observed_revision_id,
                last_working_revision_id: prior.last_working_revision_id,
                restart_required,
                activation_error_code: prior.activation_error_code,
            },
            occurred_at,
        )
        .await?;
    }
    Ok(())
}

fn validate_commit_bindings(commit: &ConfigurationCommitV1) -> ConfigurationStoreResult<()> {
    commit.validate().map_err(ConfigurationStoreError::from)?;
    if commit.next_revision.parent_revision_id.as_ref() != Some(&commit.expected_base_revision_id) {
        return Err(invalid_store_data(
            "configuration commit next revision does not name the expected base revision",
        ));
    }
    if commit.audit_event.actor_id != commit.receipt.actor_id
        || commit.audit_event.idempotency_key.as_ref() != Some(&commit.receipt.idempotency_key)
        || commit.audit_event.base_revision_id != commit.receipt.base_revision_id
        || commit.audit_event.result_revision_id.as_ref()
            != Some(&commit.receipt.result_revision_id)
        || commit.audit_event.operation_digest != commit.receipt.operation_digest
        || commit.audit_event.receipt_id.as_ref() != Some(&commit.receipt.receipt_id)
    {
        return Err(invalid_store_data(
            "configuration audit event does not bind the mutation receipt",
        ));
    }
    if let Some(plan) = &commit.change_plan {
        if plan.actor_id != commit.receipt.actor_id
            || plan.base_revision_id != commit.expected_base_revision_id
            || plan.operation_digest != commit.receipt.operation_digest
        {
            return Err(invalid_store_data(
                "configuration change plan does not bind the mutation receipt",
            ));
        }
        if terminal_plan_event_kind(commit.audit_event.event_kind).is_none() {
            return Err(invalid_store_data(
                "configuration change plan commit lacks a terminal applied audit event",
            ));
        }
    }
    Ok(())
}

async fn replay_matches_commit(
    transaction: &impl QueryExecutor,
    stored: &StoredMutationReceipt,
    commit: &ConfigurationCommitV1,
) -> ConfigurationStoreResult<bool> {
    if stored.receipt != commit.receipt
        || stored.authorization_policy_digest != authorization_policy_digest_for_commit(commit)
        || stored.activation_status != CONFIGURATION_ACTIVATION_DESIRED_RECORDED
    {
        return Ok(false);
    }
    let expected_plan_id = commit.change_plan.as_ref().map(|plan| &plan.plan_id);
    if stored.plan_id.as_ref() != expected_plan_id {
        return Ok(false);
    }
    let stored_revision =
        read_revision_from_executor(transaction, &commit.next_revision.revision_id).await?;
    if stored_revision.as_ref() != Some(&commit.next_revision) {
        return Ok(false);
    }
    let stored_audit_event =
        read_audit_event_from_transaction(transaction, &commit.audit_event.event_id).await?;
    if stored_audit_event.as_ref() != Some(&commit.audit_event) {
        return Ok(false);
    }
    if let Some(plan) = &commit.change_plan {
        let stored_plan = read_change_plan_from_executor(transaction, &plan.plan_id).await?;
        if stored_plan.as_ref().map(|record| &record.plan) != Some(plan)
            || !has_matching_terminal_plan_event(transaction, plan, &commit.audit_event).await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn commit_configuration_transaction(
    transaction: &impl Executor,
    commit: &ConfigurationCommitV1,
    fault_after_revision: bool,
    sealed_target_reference: Option<&[u8]>,
) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
    if let Some(stored) = receipt_for_idempotency_from_transaction(
        transaction,
        &commit.receipt.actor_id,
        &commit.receipt.idempotency_key,
    )
    .await?
    {
        return if replay_matches_commit(transaction, &stored, commit).await? {
            Ok(stored.receipt)
        } else {
            Err(ConfigurationStoreError::IdempotencyConflict)
        };
    }

    let current_revision_id = current_revision_id_from_executor(transaction).await?;
    if current_revision_id != commit.expected_base_revision_id {
        return Err(ConfigurationStoreError::RevisionConflict);
    }
    if let Some(plan) = &commit.change_plan {
        let stored_plan = read_change_plan_from_executor(transaction, &plan.plan_id).await?;
        if stored_plan.as_ref().map(|record| &record.plan) != Some(plan) {
            return Err(ConfigurationStoreError::PlanStale);
        }
    }

    insert_revision(transaction, &commit.next_revision).await?;
    if fault_after_revision {
        return Err(invalid_store_data(
            "injected configuration commit crash after revision",
        ));
    }
    insert_mutation_receipt(transaction, commit).await?;
    advance_component_desired_state(
        transaction,
        &commit.next_revision.revision_id,
        commit.receipt.created_at,
    )
    .await?;
    if let Some(plan) = &commit.change_plan {
        append_terminal_plan_event(transaction, plan, &commit.audit_event).await?;
    }
    insert_audit_event_with_receipt_digest(
        transaction,
        &commit.audit_event,
        Some(&commit.receipt.receipt_digest),
        sealed_target_reference,
    )
    .await?;
    Ok(commit.receipt.clone())
}

#[cfg(test)]
impl ConfigurationSqlStore<'_> {
    pub async fn current_revision(
        &self,
    ) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
        let revision_id = current_revision_id_from_executor(self.connection).await?;
        read_revision_from_executor(self.connection, &revision_id)
            .await?
            .ok_or_else(|| invalid_store_data("current configuration revision disappeared"))
    }

    pub async fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
        revision_id
            .validate()
            .map_err(ConfigurationStoreError::from)?;
        read_revision_from_executor(self.connection, revision_id).await
    }

    pub async fn save_change_plan(
        &self,
        plan: &ConfigurationProtectedPlanRecordV1,
    ) -> ConfigurationStoreResult<()> {
        plan.validate().map_err(ConfigurationStoreError::from)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(unavailable_store)?;
        let outcome = match read_change_plan_from_executor(&transaction, &plan.plan.plan_id).await {
            Ok(Some(existing)) if existing == *plan => Ok(()),
            Ok(Some(_)) => Err(invalid_store_data(
                "configuration change plan id conflicts with immutable prior input",
            )),
            Ok(None) => insert_change_plan(&transaction, plan).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => transaction.commit().await.map_err(unavailable_store),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>> {
        plan_id.validate().map_err(ConfigurationStoreError::from)?;
        read_change_plan_from_executor(self.connection, plan_id).await
    }

    pub async fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
        validate_commit_bindings(&commit)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(unavailable_store)?;
        let outcome = commit_configuration_transaction(&transaction, &commit, false, None).await;
        match outcome {
            Ok(receipt) => transaction
                .commit()
                .await
                .map(|()| receipt)
                .map_err(unavailable_store),
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> ConfigurationStoreResult<Vec<ConfigurationAuditEvent>> {
        if limit == 0 {
            return Err(invalid_store_data(
                "configuration audit limit must be non-zero",
            ));
        }
        let limit = i64::try_from(limit).map_err(|_| {
            invalid_store_data("configuration audit limit exceeds SQLite integer range")
        })?;
        let cursor = if let Some(after) = after {
            after.validate().map_err(ConfigurationStoreError::from)?;
            let mut rows = self
                .connection
                .query(
                    "SELECT occurred_at FROM configuration_audit_events WHERE event_id = ?1",
                    params![after.as_str()],
                )
                .await
                .map_err(unavailable_store)?;
            let Some(row) = rows.next().await.map_err(unavailable_store)? else {
                return Err(invalid_store_data(
                    "configuration audit cursor does not exist",
                ));
            };
            let occurred_at = row.get::<i64>(0).map_err(|error| {
                invalid_store_data(format!("read configuration audit cursor time: {error}"))
            })?;
            Some((occurred_at, after.as_str().to_owned()))
        } else {
            None
        };
        let mut rows = match cursor {
            Some((occurred_at, event_id)) => self
                .connection
                .query(
                    "SELECT event_id, actor_id, idempotency_key, operation_kind,
                            base_revision_id, result_revision_id, sealed_target_reference,
                            event_scoped_target_commitment, receipt_digest, safe_reason_code, occurred_at
                     FROM configuration_audit_events
                     WHERE occurred_at > ?1 OR (occurred_at = ?1 AND event_id > ?2)
                     ORDER BY occurred_at ASC, event_id ASC
                     LIMIT ?3",
                    params![occurred_at, event_id, limit],
                )
                .await
                .map_err(unavailable_store)?,
            None => self
                .connection
                .query(
                    "SELECT event_id, actor_id, idempotency_key, operation_kind,
                            base_revision_id, result_revision_id, sealed_target_reference,
                            event_scoped_target_commitment, receipt_digest, safe_reason_code, occurred_at
                     FROM configuration_audit_events
                     ORDER BY occurred_at ASC, event_id ASC
                     LIMIT ?1",
                    params![limit],
                )
                .await
                .map_err(unavailable_store)?,
        };
        let mut events = Vec::new();
        while let Some(row) = rows.next().await.map_err(unavailable_store)? {
            let (event, sealed_target_reference) = decode_audit_row(&row)?;
            if sealed_target_reference.is_some() {
                return Err(invalid_store_data(
                    "connection-local test store cannot authorize sealed audit targets",
                ));
            }
            events.push(event);
        }
        Ok(events)
    }
}

#[cfg(test)]
impl ConfigurationRevisionStore for ConfigurationSqlStore<'_> {
    async fn current_revision(&self) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
        ConfigurationSqlStore::current_revision(self).await
    }

    async fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
        ConfigurationSqlStore::read_revision(self, revision_id).await
    }

    async fn save_change_plan(
        &self,
        plan: &ConfigurationProtectedPlanRecordV1,
    ) -> ConfigurationStoreResult<()> {
        ConfigurationSqlStore::save_change_plan(self, plan).await
    }

    async fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>> {
        ConfigurationSqlStore::read_change_plan(self, plan_id).await
    }

    async fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
        ConfigurationSqlStore::commit(self, commit).await
    }

    async fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> ConfigurationStoreResult<Vec<ConfigurationAuditEvent>> {
        ConfigurationSqlStore::audit(self, after, limit).await
    }
}

fn map_protected_change_snapshot_error(error: ProtectedChangeSnapshotError) -> ConfigurationError {
    match error {
        ProtectedChangeSnapshotError::Stale => ConfigurationError::PlanStale,
        ProtectedChangeSnapshotError::Domain(error) => ConfigurationError::validation(error),
        ProtectedChangeSnapshotError::IncompatibleValue(message) => {
            ConfigurationError::validation_message(message)
        }
    }
}

fn map_store_error(error: ConfigurationStoreError) -> ConfigurationError {
    match error {
        ConfigurationStoreError::RevisionConflict => ConfigurationError::RevisionConflict,
        ConfigurationStoreError::PlanExpired => ConfigurationError::PlanExpired,
        ConfigurationStoreError::PlanStale => ConfigurationError::PlanStale,
        ConfigurationStoreError::IdempotencyConflict => ConfigurationError::IdempotencyConflict,
        ConfigurationStoreError::InvalidData(message) => {
            ConfigurationError::validation_message(message)
        }
        ConfigurationStoreError::Unavailable => ConfigurationError::Unavailable,
    }
}

fn derived_identifier<T>(
    prefix: &str,
    digest: &ManifestDigest,
    field: &'static str,
) -> Result<T, ConfigurationError>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    let digest = digest.as_str().strip_prefix("sha256:").ok_or_else(|| {
        ConfigurationError::validation_message("configuration digest is missing its sha256 prefix")
    })?;
    T::try_from(format!("{prefix}.{digest}")).map_err(|error| {
        ConfigurationError::validation_message(format!("invalid {field}: {error}"))
    })
}

fn direct_operation_digest(
    mutation: &DirectConfigurationMutation,
) -> Result<ManifestDigest, ConfigurationError> {
    canonical_sha256(&("tracedecay.configuration.direct-mutation.v1", mutation))
        .map_err(ConfigurationError::validation)
}

fn direct_idempotency_key(
    authority: &ConfigurationMutationAuthority,
    operation_digest: &ManifestDigest,
) -> Result<ConfigurationIdempotencyKey, ConfigurationError> {
    let digest = canonical_sha256(&(
        "tracedecay.configuration.direct-idempotency.v1",
        &authority.receipt.receipt_id,
        operation_digest,
    ))
    .map_err(ConfigurationError::validation)?;
    derived_identifier(
        "configuration.idempotency.direct.v1",
        &digest,
        "direct idempotency key",
    )
}

fn result_revision_id(
    expected_revision_id: &ConfigurationRevisionId,
    idempotency_key: &ConfigurationIdempotencyKey,
    operation_digest: &ManifestDigest,
) -> Result<ConfigurationRevisionId, ConfigurationError> {
    let digest = canonical_sha256(&(
        "tracedecay.configuration.result-revision.v1",
        expected_revision_id,
        idempotency_key,
        operation_digest,
    ))
    .map_err(ConfigurationError::validation)?;
    derived_identifier(
        "configuration.revision.v1",
        &digest,
        "configuration result revision id",
    )
}

fn mutation_provenance(
    layer: &ConfigurationLayerIdV1,
    revision_id: &ConfigurationRevisionId,
) -> Vec<ConfigurationCandidateV1> {
    vec![ConfigurationCandidateV1 {
        layer: layer.clone(),
        revision_id: revision_id.clone(),
        disposition: CandidateDispositionV1::Winning,
        safe_reason: None,
    }]
}

fn replace_direct_effective_value(
    effective_values: &mut BTreeMap<SettingKey, ConfigurationValueV1>,
    provenance: &mut BTreeMap<SettingKey, Vec<ConfigurationCandidateV1>>,
    key: SettingKey,
    value: ConfigurationValueV1,
    layer: &ConfigurationLayerIdV1,
    revision_id: &ConfigurationRevisionId,
) {
    effective_values.insert(key.clone(), value);
    provenance.insert(key, mutation_provenance(layer, revision_id));
}

fn apply_direct_mutation_to_snapshot(
    current: &ConfigurationSnapshotV1,
    mutation: &DirectConfigurationMutation,
    revision_id: &ConfigurationRevisionId,
    registry: &ConfigurationRegistry,
) -> Result<ConfigurationSnapshotV1, ConfigurationError> {
    fn apply(
        effective_values: &mut BTreeMap<SettingKey, ConfigurationValueV1>,
        provenance: &mut BTreeMap<SettingKey, Vec<ConfigurationCandidateV1>>,
        mutation: &DirectConfigurationMutation,
        revision_id: &ConfigurationRevisionId,
        registry: &ConfigurationRegistry,
    ) -> Result<(), ConfigurationError> {
        match mutation {
            DirectConfigurationMutation::Set { layer, key, value } => {
                registry
                    .validate_layer(key, layer)
                    .map_err(ConfigurationError::validation)?;
                registry
                    .validate_value(key, value)
                    .map_err(ConfigurationError::validation)?;
                replace_direct_effective_value(
                    effective_values,
                    provenance,
                    key.clone(),
                    value.clone(),
                    layer,
                    revision_id,
                );
            }
            DirectConfigurationMutation::Unset { layer, key } => {
                registry
                    .validate_layer(key, layer)
                    .map_err(ConfigurationError::validation)?;
                let definition = registry
                    .definition(key)
                    .map_err(ConfigurationError::validation)?;
                effective_values.insert(key.clone(), definition.default_value.clone());
                provenance.insert(
                    key.clone(),
                    vec![registry_default_candidate().map_err(ConfigurationError::validation)?],
                );
            }
            DirectConfigurationMutation::Batch { mutations } => {
                for mutation in mutations {
                    apply(
                        effective_values,
                        provenance,
                        mutation,
                        revision_id,
                        registry,
                    )?;
                }
            }
        }
        Ok(())
    }

    mutation.touched_keys()?;
    let mut effective_values = current.effective_values.clone();
    let mut provenance = current.provenance.clone();
    apply(
        &mut effective_values,
        &mut provenance,
        mutation,
        revision_id,
        registry,
    )?;
    let snapshot = ConfigurationSnapshotV1::new(effective_values, provenance)
        .map_err(ConfigurationError::validation)?;
    validate_snapshot_registry_completeness(&snapshot).map_err(map_store_error)?;
    Ok(snapshot)
}

fn validate_direct_control_mutation(
    mutation: &DirectConfigurationMutation,
) -> Result<(), ConfigurationError> {
    match mutation {
        DirectConfigurationMutation::Set { key, value, .. } => {
            if [
                SOURCE_BINDINGS_SETTING_KEY,
                ACCESS_RULES_SETTING_KEY,
                WORK_TOPOLOGY_POLICY_SETTING_KEY,
            ]
            .contains(&key.as_str())
            {
                return Err(ConfigurationError::PolicyWideningForbidden);
            }
            if matches!(value, ConfigurationValueV1::CredentialReference(_)) {
                return Err(ConfigurationError::validation_message(
                    "credential references require the write-only credential operation",
                ));
            }
            value.validate().map_err(ConfigurationError::validation)
        }
        DirectConfigurationMutation::Unset { key, .. } => {
            if [
                SOURCE_BINDINGS_SETTING_KEY,
                ACCESS_RULES_SETTING_KEY,
                WORK_TOPOLOGY_POLICY_SETTING_KEY,
            ]
            .contains(&key.as_str())
            {
                return Err(ConfigurationError::PolicyWideningForbidden);
            }
            key.validate().map_err(ConfigurationError::validation)
        }
        DirectConfigurationMutation::Batch { mutations } => {
            mutation.touched_keys()?;
            for mutation in mutations {
                validate_direct_control_mutation(mutation)?;
            }
            Ok(())
        }
    }
}

struct ConfigurationCommitDraft<'a, T> {
    expected_base_revision_id: &'a ConfigurationRevisionId,
    next_revision_id: ConfigurationRevisionId,
    snapshot: ConfigurationSnapshotV1,
    actor_id: &'a ActorId,
    operation_kind: &'static str,
    operation_digest: ManifestDigest,
    idempotency_key: ConfigurationIdempotencyKey,
    change_plan: Option<ProtectedChangePlan>,
    event_kind: ConfigurationAuditEventKindV1,
    created_at: UtcMicros,
    target: &'a T,
}

async fn build_configuration_commit<T: Serialize>(
    transaction: &impl Executor,
    draft: ConfigurationCommitDraft<'_, T>,
) -> Result<(ConfigurationCommitV1, Vec<u8>), ConfigurationError> {
    let ConfigurationCommitDraft {
        expected_base_revision_id,
        next_revision_id,
        snapshot,
        actor_id,
        operation_kind,
        operation_digest,
        idempotency_key,
        change_plan,
        event_kind,
        created_at,
        target,
    } = draft;
    let receipt_id: ConfigurationReceiptId = derived_identifier(
        "configuration.receipt.v1",
        &canonical_sha256(&(
            "tracedecay.configuration.receipt.v1",
            actor_id,
            &idempotency_key,
            expected_base_revision_id,
            &next_revision_id,
            &operation_digest,
        ))
        .map_err(ConfigurationError::validation)?,
        "configuration receipt id",
    )?;
    let receipt_digest = canonical_sha256(&(
        "tracedecay.configuration.receipt-digest.v1",
        &receipt_id,
        actor_id,
        &idempotency_key,
        expected_base_revision_id,
        &next_revision_id,
        &operation_digest,
        created_at,
    ))
    .map_err(ConfigurationError::validation)?;
    let receipt = ConfigurationMutationReceiptV1 {
        receipt_id: receipt_id.clone(),
        actor_id: actor_id.clone(),
        idempotency_key: idempotency_key.clone(),
        base_revision_id: expected_base_revision_id.clone(),
        result_revision_id: next_revision_id.clone(),
        operation_digest: operation_digest.clone(),
        receipt_digest,
        created_at,
    };
    let event_id = derived_identifier(
        "configuration.audit.v1",
        &canonical_sha256(&(
            "tracedecay.configuration.audit-event.v1",
            &receipt_id,
            &event_kind,
        ))
        .map_err(ConfigurationError::validation)?,
        "configuration audit event id",
    )?;
    let (sealed_target_reference, target_commitment) =
        seal_audit_target(transaction, &event_id, target, created_at)
            .await
            .map_err(map_store_error)?;
    let audit_event = ConfigurationAuditEvent {
        event_id,
        event_kind,
        actor_id: actor_id.clone(),
        idempotency_key: Some(idempotency_key),
        base_revision_id: expected_base_revision_id.clone(),
        result_revision_id: Some(next_revision_id.clone()),
        operation_digest: operation_digest.clone(),
        target_commitment,
        receipt_id: Some(receipt_id),
        safe_reason_code: None,
        occurred_at: created_at,
    };
    Ok((
        ConfigurationCommitV1 {
            expected_base_revision_id: expected_base_revision_id.clone(),
            next_revision: ConfigurationRevisionRecordV1 {
                revision_id: next_revision_id,
                parent_revision_id: Some(expected_base_revision_id.clone()),
                snapshot,
                actor_id: actor_id.clone(),
                operation_kind: operation_kind.to_owned(),
                created_at,
            },
            receipt,
            change_plan,
            audit_event,
        },
        sealed_target_reference,
    ))
}

fn validate_apply_request(
    request: &tracedecay_domain::configuration::ProtectedApplyRequest,
) -> Result<(), ConfigurationError> {
    request
        .plan_id
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .actor_id
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .expected_base_revision_id
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .operation_digest
        .validate()
        .map_err(ConfigurationError::validation)?;
    request
        .idempotency_key
        .validate()
        .map_err(ConfigurationError::validation)
}

fn validate_plan_evidence(
    plan: &ProtectedChangePlan,
    evidence: &ScopeRevalidationEvidenceV1,
) -> Result<(), ConfigurationError> {
    if plan.resolved_scope_digest != evidence.resolved_scope_digest
        || plan.membership_digest != evidence.membership_digest
        || plan.authorization_policy_digest != evidence.authorization_policy_digest
        || plan.policy_epoch != evidence.policy_epoch
    {
        return Err(ConfigurationError::PlanStale);
    }
    Ok(())
}

async fn audit_from_transaction(
    transaction: &impl QueryExecutor,
    after: Option<&ConfigurationAuditEventId>,
    limit: usize,
) -> ConfigurationStoreResult<Vec<ConfigurationAuditEvent>> {
    if limit == 0 {
        return Err(invalid_store_data(
            "configuration audit limit must be non-zero",
        ));
    }
    let limit = i64::try_from(limit).map_err(|_| {
        invalid_store_data("configuration audit limit exceeds SQLite integer range")
    })?;
    let cursor = if let Some(after) = after {
        after.validate().map_err(ConfigurationStoreError::from)?;
        let mut rows = transaction
            .query(
                "SELECT occurred_at FROM configuration_audit_events WHERE event_id = ?1",
                params![after.as_str()],
            )
            .await
            .map_err(unavailable_store)?;
        let Some(row) = rows.next().await.map_err(unavailable_store)? else {
            return Err(invalid_store_data(
                "configuration audit cursor does not exist",
            ));
        };
        Some((
            row.get::<i64>(0).map_err(|error| {
                invalid_store_data(format!("read configuration audit cursor time: {error}"))
            })?,
            after.as_str().to_owned(),
        ))
    } else {
        None
    };
    let mut rows = match cursor {
        Some((occurred_at, event_id)) => transaction
            .query(
                "SELECT event_id, actor_id, idempotency_key, operation_kind,
                        base_revision_id, result_revision_id, sealed_target_reference,
                        event_scoped_target_commitment, receipt_digest, safe_reason_code, occurred_at
                 FROM configuration_audit_events
                 WHERE occurred_at > ?1 OR (occurred_at = ?1 AND event_id > ?2)
                 ORDER BY occurred_at ASC, event_id ASC
                 LIMIT ?3",
                params![occurred_at, event_id, limit],
            )
            .await
            .map_err(unavailable_store)?,
        None => transaction
            .query(
                "SELECT event_id, actor_id, idempotency_key, operation_kind,
                        base_revision_id, result_revision_id, sealed_target_reference,
                        event_scoped_target_commitment, receipt_digest, safe_reason_code, occurred_at
                 FROM configuration_audit_events
                 ORDER BY occurred_at ASC, event_id ASC
                 LIMIT ?1",
                params![limit],
            )
            .await
            .map_err(unavailable_store)?,
    };
    let mut events = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        let (event, sealed_target_reference) = decode_audit_row(&row)?;
        validate_sealed_audit_target(transaction, &event, sealed_target_reference.as_deref())
            .await?;
        events.push(event);
    }
    Ok(events)
}

fn redacted_value_digest(
    value: Option<&ConfigurationValueV1>,
) -> Result<Option<ManifestDigest>, ConfigurationError> {
    value
        .map(|value| canonical_sha256(&("tracedecay.configuration.rollback-value.v1", value)))
        .transpose()
        .map_err(ConfigurationError::validation)
}

fn rollback_redacted_changes(
    current: &ConfigurationSnapshotV1,
    target: &ConfigurationSnapshotV1,
) -> Result<Vec<RedactedConfigurationChangeV1>, ConfigurationError> {
    let keys = current
        .effective_values
        .keys()
        .chain(target.effective_values.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter_map(|setting_key| {
            let before = current.effective_values.get(&setting_key);
            let after = target.effective_values.get(&setting_key);
            (before != after).then_some((setting_key, before, after))
        })
        .map(|(setting_key, before, after)| {
            Ok(RedactedConfigurationChangeV1 {
                setting_key,
                operation: ScopeControlOperationV1::Rollback,
                before_digest: redacted_value_digest(before)?,
                after_digest: redacted_value_digest(after)?,
            })
        })
        .collect()
}

async fn current_state_from_transaction(
    transaction: &impl QueryExecutor,
) -> Result<ConfigurationCurrentStateV1, ConfigurationError> {
    let revision_id = current_revision_id_from_executor(transaction)
        .await
        .map_err(map_store_error)?;
    let revision = read_revision_from_executor(transaction, &revision_id)
        .await
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ConfigurationError::validation_message("current configuration revision disappeared")
        })?;
    Ok(ConfigurationCurrentStateV1 {
        revision_id: revision.revision_id,
        snapshot: revision.snapshot,
    })
}

async fn replay_control_receipt(
    transaction: &impl QueryExecutor,
    actor_id: &ActorId,
    idempotency_key: &ConfigurationIdempotencyKey,
    expected_base_revision_id: &ConfigurationRevisionId,
    operation_digest: &ManifestDigest,
    expected_plan_id: Option<&ChangePlanId>,
) -> Result<Option<ConfigurationMutationReceipt>, ConfigurationError> {
    let Some(stored) =
        receipt_for_idempotency_from_transaction(transaction, actor_id, idempotency_key)
            .await
            .map_err(map_store_error)?
    else {
        return Ok(None);
    };
    if stored.receipt.base_revision_id != *expected_base_revision_id
        || stored.receipt.operation_digest != *operation_digest
        || stored.plan_id.as_ref() != expected_plan_id
    {
        return Err(ConfigurationError::IdempotencyConflict);
    }
    let revision = read_revision_from_executor(transaction, &stored.receipt.result_revision_id)
        .await
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ConfigurationError::validation_message(
                "configuration receipt result revision disappeared",
            )
        })?;
    Ok(Some(ConfigurationMutationReceipt {
        receipt_id: stored.receipt.receipt_id,
        base_revision_id: stored.receipt.base_revision_id,
        result_revision_id: stored.receipt.result_revision_id,
        snapshot_id: revision.snapshot.snapshot_id,
        operation_digest: stored.receipt.operation_digest,
        created_at: stored.receipt.created_at,
    }))
}

async fn credential_reference_from_transaction(
    transaction: &impl QueryExecutor,
    reference_id: &CredentialReferenceId,
) -> Result<Option<CredentialReferenceMetadataV1>, ConfigurationError> {
    let mut rows = transaction
        .query(
            "SELECT kind, reference_digest, created_at, rotation
             FROM configuration_credential_references
             WHERE reference_id = ?1",
            params![reference_id.as_str()],
        )
        .await
        .map_err(|_| ConfigurationError::Unavailable)?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|_| ConfigurationError::Unavailable)?
    else {
        return Ok(None);
    };
    let kind = row
        .get::<String>(0)
        .map_err(|_| ConfigurationError::Unavailable)?;
    let kind = serde_json::from_value::<CredentialKindV1>(serde_json::Value::String(kind))
        .map_err(|_| ConfigurationError::validation_message("stored credential kind is invalid"))?;
    let reference_digest = ManifestDigest::new(
        row.get::<String>(1)
            .map_err(|_| ConfigurationError::Unavailable)?,
    )
    .map_err(ConfigurationError::validation)?;
    let created_at = UtcMicros(
        row.get::<i64>(2)
            .map_err(|_| ConfigurationError::Unavailable)?,
    );
    let rotation = u64::try_from(
        row.get::<i64>(3)
            .map_err(|_| ConfigurationError::Unavailable)?,
    )
    .map_err(|_| ConfigurationError::validation_message("stored credential rotation is invalid"))?;
    let metadata = CredentialReferenceMetadataV1::new(
        reference_id.clone(),
        kind,
        reference_digest,
        created_at,
        rotation,
    )
    .map_err(ConfigurationError::validation)?;
    if rows
        .next()
        .await
        .map_err(|_| ConfigurationError::Unavailable)?
        .is_some()
    {
        return Err(ConfigurationError::validation_message(
            "credential reference resolved to multiple rows",
        ));
    }
    Ok(Some(metadata))
}

/// Concrete control-plane adapter over one already-open owned session store.
/// It never accepts an arbitrary connection, opens a fallback database, or
/// owns policy resolution; every write obtains the selected store's serialized
/// immediate transaction and commits all durable effects together.
pub struct GlobalDbConfigurationControlStore<'db> {
    db: &'db RegisteredGlobalDb,
}

/// Repairs only the exact profile shape stored before activation gained an
/// accepted-profile digest. The historical revision retains that selection;
/// the forward child keeps download/resource intent but disables semantic
/// influence until a newly evaluated profile can mint a current receipt.
fn repair_pre_digest_semantic_configuration(
    encoded: &str,
) -> Result<Option<String>, ConfigurationError> {
    if let Ok(current) =
        serde_json::from_str::<crate::configuration::semantic::SemanticConfig>(encoded)
    {
        current.validate().map_err(|_| {
            ConfigurationError::validation_message(
                "semantic runtime configuration is invalid under the current schema",
            )
        })?;
        return Ok(None);
    }

    let mut document = serde_json::from_str::<serde_json::Value>(encoded).map_err(|_| {
        ConfigurationError::validation_message("semantic runtime configuration is not valid JSON")
    })?;
    let object = document.as_object_mut().ok_or_else(|| {
        ConfigurationError::validation_message("semantic runtime configuration is not an object")
    })?;
    let active = object
        .get("active_profile")
        .filter(|value| !value.is_null());
    let rollback = object
        .get("rollback_profile")
        .filter(|value| !value.is_null());
    if active.is_none()
        || rollback.is_some_and(|rollback| Some(rollback) == active)
        || !active
            .into_iter()
            .chain(rollback)
            .all(is_pre_digest_semantic_profile)
    {
        return Err(ConfigurationError::validation_message(
            "semantic runtime configuration cannot be repaired without profile authority",
        ));
    }

    object.insert("active_profile".to_owned(), serde_json::Value::Null);
    object.insert("rollback_profile".to_owned(), serde_json::Value::Null);
    let repaired =
        serde_json::from_value::<crate::configuration::semantic::SemanticConfig>(document)
            .map_err(|_| {
                ConfigurationError::validation_message(
                    "semantic runtime configuration cannot be repaired under the current schema",
                )
            })?;
    repaired.validate().map_err(|_| {
        ConfigurationError::validation_message(
            "semantic runtime configuration cannot be repaired under the current schema",
        )
    })?;
    serde_json::to_string(&repaired).map(Some).map_err(|_| {
        ConfigurationError::validation_message(
            "semantic runtime configuration forward repair could not be encoded",
        )
    })
}

fn is_pre_digest_semantic_profile(value: &serde_json::Value) -> bool {
    let Some(profile) = value.as_object() else {
        return false;
    };
    let expected = ["artifact_digest", "artifact_path", "profile_id"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let actual = profile.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let Some(profile_id) = profile
        .get("profile_id")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(artifact_digest) = profile
        .get("artifact_digest")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(artifact_path) = profile
        .get("artifact_path")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let artifact_path = std::path::Path::new(artifact_path);
    actual == expected
        && !profile_id.trim().is_empty()
        && profile_id.len() <= 128
        && artifact_digest.len() == 64
        && artifact_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && artifact_path.is_absolute()
        && !artifact_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn complete_snapshot_for_current_registry(
    snapshot: &ConfigurationSnapshotV1,
) -> Result<ConfigurationSnapshotV1, ConfigurationError> {
    let registry = ConfigurationRegistry::core().map_err(ConfigurationError::validation)?;
    let expected = registry
        .definitions()
        .map(|definition| definition.key.clone())
        .collect::<BTreeSet<_>>();
    if snapshot
        .effective_values
        .keys()
        .any(|key| !expected.contains(key))
    {
        return Err(ConfigurationError::validation_message(
            "configuration snapshot contains a setting outside the current registry",
        ));
    }
    let mut effective_values = snapshot.effective_values.clone();
    let mut provenance = snapshot.provenance.clone();
    for definition in registry.definitions() {
        if !effective_values.contains_key(&definition.key) {
            effective_values.insert(definition.key.clone(), definition.default_value.clone());
            provenance.insert(
                definition.key.clone(),
                vec![registry_default_candidate().map_err(ConfigurationError::validation)?],
            );
        }
    }
    let semantic_key =
        SettingKey::new(tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY)
            .map_err(ConfigurationError::validation)?;
    if let Some(ConfigurationValueV1::Text(encoded)) = effective_values.get_mut(&semantic_key)
        && let Some(repaired) = repair_pre_digest_semantic_configuration(encoded)?
    {
        *encoded = repaired;
    }
    ConfigurationSnapshotV1::new(effective_values, provenance)
        .map_err(ConfigurationError::validation)
}

impl<'db> GlobalDbConfigurationControlStore<'db> {
    pub const fn new_registered(db: &'db RegisteredGlobalDb) -> Self {
        Self { db }
    }

    /// Appends the daemon-owned binding to revisions created before canonical
    /// genesis carried it. Competing authority stays denied; only an absent
    /// key or the stable daemon binding after a repository move is repaired.
    pub fn ensure_daemon_source_binding(
        &self,
        binding: ScopeSourceBinding,
        occurred_at: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        Box::pin(async move {
            binding.validate().map_err(ConfigurationError::validation)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                let current = current_state_from_transaction(&transaction).await?;
                let base_snapshot = complete_snapshot_for_current_registry(&current.snapshot)?;
                let snapshot_was_repaired = base_snapshot != current.snapshot;
                let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)
                    .map_err(ConfigurationError::validation)?;
                let configured = match base_snapshot.effective_values.get(&key) {
                    Some(ConfigurationValueV1::SourceBindings(bindings)) => bindings,
                    Some(_) => {
                        return Err(ConfigurationError::validation_message(
                            "source bindings setting has an incompatible typed value",
                        ));
                    }
                    None => {
                        return Err(ConfigurationError::validation_message(
                            "source bindings setting is missing from the configuration snapshot",
                        ));
                    }
                };
                let authority_bindings = configured
                    .iter()
                    .filter(|candidate| {
                        candidate.source_kind == binding.source_kind
                            && candidate.authority == binding.authority
                    })
                    .collect::<Vec<_>>();
                if authority_bindings.len() > 1 {
                    return Err(ConfigurationError::validation_message(
                        "daemon source binding repair found ambiguous authority",
                    ));
                }
                let exact_binding = authority_bindings.first().is_some_and(|candidate| {
                    candidate.source_locator_digest == binding.source_locator_digest
                });
                let canonical_binding = authority_bindings
                    .first()
                    .is_some_and(|candidate| exact_binding && candidate.binding_id == binding.binding_id);
                if canonical_binding && !snapshot_was_repaired {
                    return Ok(current);
                }
                let change = match authority_bindings.first() {
                    Some(_) if canonical_binding => {
                        ProtectedChange::RebindSource(binding)
                    }
                    Some(_) if exact_binding => {
                        return Err(ConfigurationError::validation_message(
                            "daemon source binding registry repair found a non-canonical binding id",
                        ));
                    }
                    Some(candidate) if candidate.binding_id == binding.binding_id => {
                        ProtectedChange::RebindSource(binding)
                    }
                    Some(_) => {
                        return Err(ConfigurationError::validation_message(
                            "daemon source binding repair found a conflicting protected binding",
                        ));
                    }
                    None
                        if configured
                            .iter()
                            .any(|candidate| candidate.binding_id == binding.binding_id) =>
                    {
                        return Err(ConfigurationError::validation_message(
                            "daemon source binding repair found the canonical id under another authority",
                        ));
                    }
                    None => ProtectedChange::BindSource(binding),
                };
                let operation_digest = canonical_sha256(&(
                    "tracedecay.configuration.daemon-source-binding-repair.v1",
                    &current.revision_id,
                    &base_snapshot.snapshot_id,
                    &change,
                ))
                .map_err(ConfigurationError::validation)?;
                let idempotency_digest = canonical_sha256(&(
                    "tracedecay.configuration.daemon-source-binding-repair-idempotency.v1",
                    &current.revision_id,
                    &operation_digest,
                ))
                .map_err(ConfigurationError::validation)?;
                let idempotency_key: ConfigurationIdempotencyKey = derived_identifier(
                    "configuration.idempotency.daemon-source-binding-repair.v1",
                    &idempotency_digest,
                    "daemon source binding repair idempotency key",
                )?;
                let next_revision_id = result_revision_id(
                    &current.revision_id,
                    &idempotency_key,
                    &operation_digest,
                )?;
                let snapshot = base_snapshot
                    .apply_protected_change(&change, &next_revision_id)
                    .map_err(map_protected_change_snapshot_error)?;
                let actor_id = ActorId::new("actor.configuration-migration".to_owned())
                    .map_err(ConfigurationError::validation)?;
                let sealed_target =
                    StoredConfigurationProtectedOperationV1::Change(Box::new(change));
                let (commit, sealed_target_reference) = build_configuration_commit(
                    &transaction,
                    ConfigurationCommitDraft {
                        expected_base_revision_id: &current.revision_id,
                        next_revision_id,
                        snapshot,
                        actor_id: &actor_id,
                        operation_kind: "daemon_source_binding_repair",
                        operation_digest,
                        idempotency_key,
                        change_plan: None,
                        event_kind: ConfigurationAuditEventKindV1::Recovered,
                        created_at: occurred_at,
                        target: &sealed_target,
                    },
                )
                .await?;
                commit_configuration_transaction(
                    &transaction,
                    &commit,
                    false,
                    Some(&sealed_target_reference),
                )
                .await
                .map_err(map_store_error)?;
                current_state_from_transaction(&transaction).await
            }
            .await;
            match outcome {
                Ok(current) => transaction
                    .commit()
                    .await
                    .map(|()| current)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    /// Reports whether this persisted control-plane store has no revision yet.
    ///
    /// The caller may seed registry defaults only in this state. Any
    /// non-empty store with an unreadable current revision remains an error;
    /// falling back to defaults would replace durable authority.
    pub fn is_uninitialized(&self) -> ConfigurationOperationFuture<'_, bool> {
        Box::pin(async move {
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            // A durable database whose configuration tables were never created
            // (for example a sessions.db seeded by another component before any
            // configuration migration ran) holds no revision by definition.
            // Counting rows in absent tables would raise a SQL error and be
            // misreported as an availability failure, so table presence is
            // checked first.
            let mut table_rows = read
                .query(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table'
                       AND name IN (
                           'configuration_revisions',
                           'configuration_migration_receipts'
                       )",
                    (),
                )
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let table_count = table_rows
                .next()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?
                .ok_or_else(|| {
                    ConfigurationError::validation_message(
                        "configuration table presence query returned no row",
                    )
                })?
                .get::<i64>(0)
                .map_err(|_| {
                    ConfigurationError::validation_message(
                        "configuration table count is not an integer",
                    )
                })?;
            if table_count < 2 {
                return Ok(true);
            }
            let mut rows = read
                .query(
                    "SELECT
                        (SELECT COUNT(*) FROM configuration_revisions),
                        (SELECT COUNT(*) FROM configuration_migration_receipts)",
                    (),
                )
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let row = rows
                .next()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?
                .ok_or_else(|| {
                    ConfigurationError::validation_message(
                        "configuration initialization query returned no row",
                    )
                })?;
            let revision_count = row.get::<i64>(0).map_err(|_| {
                ConfigurationError::validation_message(
                    "configuration revision count is not an integer",
                )
            })?;
            let migration_receipt_count = row.get::<i64>(1).map_err(|_| {
                ConfigurationError::validation_message(
                    "configuration migration receipt count is not an integer",
                )
            })?;
            if rows
                .next()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?
                .is_some()
            {
                return Err(ConfigurationError::validation_message(
                    "configuration initialization query returned multiple rows",
                ));
            }
            Ok(revision_count == 0 && migration_receipt_count == 0)
        })
    }

    /// Records a daemon/component activation result. Failed activation keeps
    /// the prior last-working observed revision while advancing desired state.
    pub fn record_component_activation(
        &self,
        component: String,
        observed_revision_id: Option<ConfigurationRevisionId>,
        activation_error_code: Option<String>,
        occurred_at: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ()> {
        Box::pin(async move {
            validate_component_name(&component).map_err(map_store_error)?;
            validate_activation_error_code(activation_error_code.as_deref())
                .map_err(map_store_error)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                let current = current_state_from_transaction(&transaction).await?;
                if let Some(observed_revision_id) = &observed_revision_id
                    && read_revision_from_executor(&transaction, observed_revision_id)
                        .await
                        .map_err(map_store_error)?
                        .is_none()
                {
                    return Err(ConfigurationError::PlanStale);
                }
                let prior = latest_component_activation_state(&transaction, &component)
                    .await
                    .map_err(map_store_error)?;
                let prior_last_working = prior
                    .as_ref()
                    .and_then(|state| state.last_working_revision_id.clone())
                    .or_else(|| {
                        prior
                            .as_ref()
                            .and_then(|state| state.observed_revision_id.clone())
                    });
                let failed = activation_error_code.is_some();
                let last_working_revision_id = if failed {
                    prior_last_working.clone()
                } else {
                    observed_revision_id
                        .clone()
                        .or_else(|| prior_last_working.clone())
                };
                let observed_revision_id = if failed {
                    prior_last_working
                } else {
                    observed_revision_id
                };
                let restart_required =
                    observed_revision_id.as_ref() != Some(&current.revision_id) || failed;
                insert_component_activation_event(
                    &transaction,
                    &StoredComponentActivationState {
                        component,
                        desired_revision_id: current.revision_id,
                        observed_revision_id,
                        last_working_revision_id,
                        restart_required,
                        activation_error_code,
                    },
                    occurred_at,
                )
                .await
                .map_err(map_store_error)
            }
            .await;
            match outcome {
                Ok(()) => transaction
                    .commit()
                    .await
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }
}

/// Cloneable configuration authority for daemon-owned runtimes.
///
/// The adapter retains the daemon's already-open project runtime database and
/// creates a scoped borrowed adapter for each operation. It therefore reuses
/// the canonical transaction, revision, and compare-and-swap implementation
/// without opening another database or resolving configuration independently.
/// It does not extend the owning daemon's database-authority lease: writes fail
/// closed after that owner exits.
#[derive(Clone)]
pub struct OwnedGlobalDbConfigurationControlStore {
    /// The exact daemon-registered project-runtime database handle. Production
    /// composition always supplies it at construction; no later attachment,
    /// path reopen, or authority substitution is available.
    db: Arc<RegisteredGlobalDb>,
}

impl OwnedGlobalDbConfigurationControlStore {
    pub fn from_registered_project_runtime_db(db: Arc<RegisteredGlobalDb>) -> Self {
        Self { db }
    }

    fn database(&self) -> Arc<RegisteredGlobalDb> {
        Arc::clone(&self.db)
    }

    pub fn record_component_activation(
        &self,
        component: String,
        observed_revision_id: Option<ConfigurationRevisionId>,
        activation_error_code: Option<String>,
        occurred_at: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ()> {
        let db = self.database();
        Box::pin(async move {
            let store = GlobalDbConfigurationControlStore::new_registered(db.as_ref());
            store
                .record_component_activation(
                    component,
                    observed_revision_id,
                    activation_error_code,
                    occurred_at,
                )
                .await
        })
    }
}

/// Forwards one owned-store method to a freshly registered borrowed store.
///
/// Every method below is the same shape: clone the borrowed arguments so the
/// returned future owns them, retain the database handle, then open the
/// registered store inside the future and delegate. Only the argument list and
/// the delegated call differ, so they are all the macro takes.
macro_rules! forward_to_registered {
    ($self:ident, [$($owned:ident),* $(,)?], |$store:ident| $call:expr) => {{
        let db = $self.database();
        $(let $owned = $owned.clone();)*
        Box::pin(async move {
            let $store = GlobalDbConfigurationControlStore::new_registered(db.as_ref());
            $call.await
        })
    }};
}

impl ConfigurationControlStore for OwnedGlobalDbConfigurationControlStore {
    fn current(&self) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        forward_to_registered!(self, [], |store| store.current())
    }

    fn save_plan(
        &self,
        plan: &ProtectedChangePlan,
        operation: &ProtectedChange,
    ) -> ConfigurationOperationFuture<'_, ()> {
        forward_to_registered!(self, [plan, operation], |store| store
            .save_plan(&plan, &operation))
    }

    fn load_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationOperationFuture<'_, Option<ProtectedChangePlan>> {
        forward_to_registered!(self, [plan_id], |store| store.load_plan(&plan_id))
    }

    fn commit_direct(
        &self,
        authority: &ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        forward_to_registered!(self, [authority, mutation, expected_revision], |store| {
            store.commit_direct(&authority, &mutation, &expected_revision)
        })
    }

    fn commit_protected(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        forward_to_registered!(self, [authority, request, plan, evidence], |store| store
            .commit_protected(&authority, &request, &plan, &evidence))
    }

    fn dry_run_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        rollback: &ConfigurationRollbackRequest,
        now: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        forward_to_registered!(self, [authority, rollback], |store| store
            .dry_run_rollback(&authority, &rollback, now))
    }

    fn apply_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        forward_to_registered!(self, [authority, request, plan, evidence], |store| store
            .apply_rollback(&authority, &request, &plan, &evidence))
    }

    fn audit(
        &self,
        actor: &AuthorizedActor,
        query: &ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        // Disambiguated: the registered store also has an inherent `audit`.
        forward_to_registered!(self, [actor, query], |store| {
            ConfigurationControlStore::audit(&store, &actor, &query)
        })
    }

    fn observed_state(
        &self,
        actor: &AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        forward_to_registered!(self, [actor], |store| store.observed_state(&actor))
    }
}

impl CredentialWritePort for OwnedGlobalDbConfigurationControlStore {
    fn write_reference(
        &self,
        authority: &ConfigurationMutationAuthority,
        write: &WriteOnlyCredentialMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1> {
        forward_to_registered!(self, [authority, write, expected_revision], |store| store
            .write_reference(&authority, &write, &expected_revision))
    }
}

pub struct ConfigurationDirectCommitOutcomeV1 {
    pub receipt: ConfigurationMutationReceipt,
    pub current: ConfigurationCurrentStateV1,
}

pub async fn commit_direct_in_transaction<E>(
    transaction: &E,
    authority: &ConfigurationMutationAuthority,
    mutation: &DirectConfigurationMutation,
    expected_revision: &ConfigurationRevisionId,
) -> Result<ConfigurationDirectCommitOutcomeV1, ConfigurationError>
where
    E: QueryExecutor + Executor + Sync,
{
    authority.validate_integrity()?;
    expected_revision
        .validate()
        .map_err(ConfigurationError::validation)?;
    validate_direct_control_mutation(mutation)?;
    if authority.receipt.scope_digest != mutation.target_scope_digest()? {
        return Err(ConfigurationError::MutationAuthorityRejected);
    }
    let operation_digest = direct_operation_digest(mutation)?;
    let idempotency_key = direct_idempotency_key(authority, &operation_digest)?;
    let next_revision_id =
        result_revision_id(expected_revision, &idempotency_key, &operation_digest)?;
    if let Some(receipt) = replay_control_receipt(
        transaction,
        &authority.receipt.actor_id,
        &idempotency_key,
        expected_revision,
        &operation_digest,
        None,
    )
    .await?
    {
        let current = current_state_from_transaction(transaction).await?;
        return Ok(ConfigurationDirectCommitOutcomeV1 { receipt, current });
    }
    let current = current_state_from_transaction(transaction).await?;
    if &current.revision_id != expected_revision {
        return Err(ConfigurationError::RevisionConflict);
    }
    let snapshot = apply_direct_mutation_to_snapshot(
        &current.snapshot,
        mutation,
        &next_revision_id,
        &ConfigurationRegistry::core().map_err(ConfigurationError::validation)?,
    )?;
    let audit_target = redacted_direct_audit_target(mutation)?;
    let (commit, sealed_target_reference) = build_configuration_commit(
        transaction,
        ConfigurationCommitDraft {
            expected_base_revision_id: expected_revision,
            next_revision_id,
            snapshot,
            actor_id: &authority.receipt.actor_id,
            operation_kind: "direct_mutation",
            operation_digest,
            idempotency_key,
            change_plan: None,
            event_kind: ConfigurationAuditEventKindV1::Applied,
            created_at: authority.receipt.issued_at,
            target: &audit_target,
        },
    )
    .await?;
    let receipt = commit_configuration_transaction(
        transaction,
        &commit,
        false,
        Some(&sealed_target_reference),
    )
    .await
    .map_err(map_store_error)?;
    let receipt = ConfigurationMutationReceipt {
        receipt_id: receipt.receipt_id,
        base_revision_id: receipt.base_revision_id,
        result_revision_id: receipt.result_revision_id,
        snapshot_id: commit.next_revision.snapshot.snapshot_id,
        operation_digest: receipt.operation_digest,
        created_at: receipt.created_at,
    };
    let current = current_state_from_transaction(transaction).await?;
    Ok(ConfigurationDirectCommitOutcomeV1 { receipt, current })
}

impl ConfigurationControlStore for GlobalDbConfigurationControlStore<'_> {
    fn current(&self) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1> {
        Box::pin(async move {
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            current_state_from_transaction(&read).await
        })
    }

    fn save_plan(
        &self,
        plan: &ProtectedChangePlan,
        operation: &ProtectedChange,
    ) -> ConfigurationOperationFuture<'_, ()> {
        let plan = plan.clone();
        let operation = operation.clone();
        Box::pin(async move {
            let record = ConfigurationProtectedPlanRecordV1 {
                plan,
                operation: ConfigurationProtectedOperationV1::Change(Box::new(operation)),
            };
            record.validate().map_err(ConfigurationError::validation)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = match read_change_plan_from_executor(&transaction, &record.plan.plan_id)
                .await
                .map_err(map_store_error)?
            {
                Some(existing) if existing == record => Ok(()),
                Some(_) => Err(ConfigurationError::IdempotencyConflict),
                None => {
                    async {
                        insert_change_plan(&transaction, &record)
                            .await
                            .map_err(map_store_error)?;
                        insert_dry_run_audit_event(&transaction, &record)
                            .await
                            .map_err(map_store_error)
                    }
                    .await
                }
            };
            match outcome {
                Ok(()) => transaction
                    .commit()
                    .await
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn load_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> ConfigurationOperationFuture<'_, Option<ProtectedChangePlan>> {
        let plan_id = plan_id.clone();
        Box::pin(async move {
            plan_id.validate().map_err(ConfigurationError::validation)?;
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            read_change_plan_from_executor(&read, &plan_id)
                .await
                .map_err(map_store_error)
                .map(|record| record.map(|record| record.plan))
        })
    }

    fn commit_direct(
        &self,
        authority: &ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        let authority = authority.clone();
        let mutation = mutation.clone();
        let expected_revision = expected_revision.clone();
        Box::pin(async move {
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = commit_direct_in_transaction(
                &transaction,
                &authority,
                &mutation,
                &expected_revision,
            )
            .await;
            match outcome {
                Ok(outcome) => transaction
                    .commit()
                    .await
                    .map(|()| outcome.receipt)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn commit_protected(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        let authority = authority.clone();
        let request = request.clone();
        let plan = plan.clone();
        let evidence = evidence.clone();
        Box::pin(async move {
            authority.validate_integrity()?;
            validate_apply_request(&request)?;
            plan.validate().map_err(ConfigurationError::validation)?;
            validate_plan_evidence(&plan, &evidence)?;
            if request.actor_id != authority.receipt.actor_id
                || request.expected_base_revision_id != plan.base_revision_id
                || request.operation_digest != plan.operation_digest
            {
                return Err(ConfigurationError::PlanStale);
            }
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                if let Some(receipt) = replay_control_receipt(
                    &transaction,
                    &authority.receipt.actor_id,
                    &request.idempotency_key,
                    &plan.base_revision_id,
                    &request.operation_digest,
                    Some(&plan.plan_id),
                )
                .await?
                {
                    return Ok(receipt);
                }
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != plan.base_revision_id {
                    return Err(ConfigurationError::PlanStale);
                }
                let record = read_change_plan_from_executor(&transaction, &plan.plan_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(ConfigurationError::PlanStale)?;
                if record.plan != plan {
                    return Err(ConfigurationError::PlanStale);
                }
                let ConfigurationProtectedOperationV1::Change(change) = &record.operation else {
                    return Err(ConfigurationError::PlanStale);
                };
                if record
                    .operation
                    .operation_digest()
                    .map_err(ConfigurationError::validation)?
                    != request.operation_digest
                {
                    return Err(ConfigurationError::PlanStale);
                }
                let next_revision_id = result_revision_id(
                    &plan.base_revision_id,
                    &request.idempotency_key,
                    &request.operation_digest,
                )?;
                let snapshot = current
                    .snapshot
                    .apply_protected_change(change, &next_revision_id)
                    .map_err(map_protected_change_snapshot_error)?;
                let sealed_target =
                    StoredConfigurationProtectedOperationV1::from(&record.operation);
                let (commit, sealed_target_reference) = build_configuration_commit(
                    &transaction,
                    ConfigurationCommitDraft {
                        expected_base_revision_id: &plan.base_revision_id,
                        next_revision_id,
                        snapshot,
                        actor_id: &authority.receipt.actor_id,
                        operation_kind: "protected_apply",
                        operation_digest: request.operation_digest.clone(),
                        idempotency_key: request.idempotency_key.clone(),
                        change_plan: Some(plan.clone()),
                        event_kind: ConfigurationAuditEventKindV1::Applied,
                        created_at: authority.receipt.issued_at,
                        target: &sealed_target,
                    },
                )
                .await?;
                let receipt = commit_configuration_transaction(
                    &transaction,
                    &commit,
                    false,
                    Some(&sealed_target_reference),
                )
                .await
                .map_err(map_store_error)?;
                Ok(ConfigurationMutationReceipt {
                    receipt_id: receipt.receipt_id,
                    base_revision_id: receipt.base_revision_id,
                    result_revision_id: receipt.result_revision_id,
                    snapshot_id: commit.next_revision.snapshot.snapshot_id,
                    operation_digest: receipt.operation_digest,
                    created_at: receipt.created_at,
                })
            }
            .await;
            match outcome {
                Ok(receipt) => transaction
                    .commit()
                    .await
                    .map(|()| receipt)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn dry_run_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        rollback: &ConfigurationRollbackRequest,
        now: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan> {
        let authority = authority.clone();
        let rollback = rollback.clone();
        Box::pin(async move {
            if rollback.mode == RollbackModeV1::Partial {
                return Err(ConfigurationError::Unavailable);
            }
            authority.validate_integrity()?;
            rollback
                .target_revision_id
                .validate()
                .map_err(ConfigurationError::validation)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != authority.receipt.expected_configuration_revision {
                    return Err(ConfigurationError::RevisionConflict);
                }
                let target =
                    read_revision_from_executor(&transaction, &rollback.target_revision_id)
                        .await
                        .map_err(map_store_error)?
                        .ok_or(ConfigurationError::PlanStale)?;
                let operation = ConfigurationProtectedOperationV1::Rollback {
                    target_revision_id: rollback.target_revision_id.clone(),
                    mode: rollback.mode,
                };
                let operation_digest = operation
                    .operation_digest()
                    .map_err(ConfigurationError::validation)?;
                let plan_id = derived_identifier(
                    "configuration.plan.rollback.v1",
                    &canonical_sha256(&(
                        "tracedecay.configuration.rollback-plan.v1",
                        &authority.receipt.actor_id,
                        &current.revision_id,
                        &operation_digest,
                        &authority.receipt.scope_digest,
                        authority.receipt.policy_epoch,
                        &authority.receipt.policy_digest,
                        now,
                    ))
                    .map_err(ConfigurationError::validation)?,
                    "configuration rollback plan id",
                )?;
                let changes = rollback_redacted_changes(&current.snapshot, &target.snapshot)?;
                if changes.is_empty() {
                    return Err(ConfigurationError::PlanStale);
                }
                let plan = ProtectedChangePlan {
                    plan_id,
                    actor_id: authority.receipt.actor_id.clone(),
                    base_revision_id: current.revision_id,
                    operation_digest,
                    resolved_scope_digest: authority.receipt.scope_digest.clone(),
                    membership_digest: None,
                    authorization_policy_digest: authority.receipt.policy_digest.clone(),
                    policy_epoch: authority.receipt.policy_epoch,
                    expires_at: UtcMicros(now.0.saturating_add(300_000_000)),
                    created_at: now,
                    redacted_changes: changes,
                };
                let record = ConfigurationProtectedPlanRecordV1 {
                    plan: plan.clone(),
                    operation,
                };
                record.validate().map_err(ConfigurationError::validation)?;
                match read_change_plan_from_executor(&transaction, &plan.plan_id)
                    .await
                    .map_err(map_store_error)?
                {
                    Some(existing) if existing == record => Ok(plan),
                    Some(_) => Err(ConfigurationError::IdempotencyConflict),
                    None => {
                        insert_change_plan(&transaction, &record)
                            .await
                            .map_err(map_store_error)?;
                        insert_dry_run_audit_event(&transaction, &record)
                            .await
                            .map_err(map_store_error)?;
                        Ok(plan)
                    }
                }
            }
            .await;
            match outcome {
                Ok(plan) => transaction
                    .commit()
                    .await
                    .map(|()| plan)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn apply_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &tracedecay_domain::configuration::ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt> {
        let authority = authority.clone();
        let request = request.clone();
        let plan = plan.clone();
        let evidence = evidence.clone();
        Box::pin(async move {
            authority.validate_integrity()?;
            validate_apply_request(&request)?;
            plan.validate().map_err(ConfigurationError::validation)?;
            validate_plan_evidence(&plan, &evidence)?;
            if request.actor_id != authority.receipt.actor_id
                || request.expected_base_revision_id != plan.base_revision_id
                || request.operation_digest != plan.operation_digest
            {
                return Err(ConfigurationError::PlanStale);
            }
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                if let Some(receipt) = replay_control_receipt(
                    &transaction,
                    &authority.receipt.actor_id,
                    &request.idempotency_key,
                    &plan.base_revision_id,
                    &request.operation_digest,
                    Some(&plan.plan_id),
                )
                .await?
                {
                    return Ok(receipt);
                }
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != plan.base_revision_id {
                    return Err(ConfigurationError::PlanStale);
                }
                let record = read_change_plan_from_executor(&transaction, &plan.plan_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(ConfigurationError::PlanStale)?;
                if record.plan != plan {
                    return Err(ConfigurationError::PlanStale);
                }
                let ConfigurationProtectedOperationV1::Rollback {
                    target_revision_id,
                    mode,
                } = &record.operation
                else {
                    return Err(ConfigurationError::PlanStale);
                };
                if *mode == RollbackModeV1::Partial {
                    return Err(ConfigurationError::Unavailable);
                }
                if record
                    .operation
                    .operation_digest()
                    .map_err(ConfigurationError::validation)?
                    != request.operation_digest
                {
                    return Err(ConfigurationError::PlanStale);
                }
                let target = read_revision_from_executor(&transaction, target_revision_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(ConfigurationError::PlanStale)?;
                let next_revision_id = result_revision_id(
                    &plan.base_revision_id,
                    &request.idempotency_key,
                    &request.operation_digest,
                )?;
                let sealed_target =
                    StoredConfigurationProtectedOperationV1::from(&record.operation);
                let (commit, sealed_target_reference) = build_configuration_commit(
                    &transaction,
                    ConfigurationCommitDraft {
                        expected_base_revision_id: &plan.base_revision_id,
                        next_revision_id,
                        snapshot: target.snapshot,
                        actor_id: &authority.receipt.actor_id,
                        operation_kind: "rollback_apply",
                        operation_digest: request.operation_digest.clone(),
                        idempotency_key: request.idempotency_key.clone(),
                        change_plan: Some(plan.clone()),
                        event_kind: ConfigurationAuditEventKindV1::RollbackApplied,
                        created_at: authority.receipt.issued_at,
                        target: &sealed_target,
                    },
                )
                .await?;
                let receipt = commit_configuration_transaction(
                    &transaction,
                    &commit,
                    false,
                    Some(&sealed_target_reference),
                )
                .await
                .map_err(map_store_error)?;
                Ok(ConfigurationMutationReceipt {
                    receipt_id: receipt.receipt_id,
                    base_revision_id: receipt.base_revision_id,
                    result_revision_id: receipt.result_revision_id,
                    snapshot_id: commit.next_revision.snapshot.snapshot_id,
                    operation_digest: receipt.operation_digest,
                    created_at: receipt.created_at,
                })
            }
            .await;
            match outcome {
                Ok(receipt) => transaction
                    .commit()
                    .await
                    .map(|()| receipt)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }

    fn audit(
        &self,
        actor: &AuthorizedActor,
        query: &ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage> {
        let actor = actor.clone();
        let query = query.clone();
        Box::pin(async move {
            actor.validate()?;
            if query.limit == 0 || query.limit > CONFIGURATION_AUDIT_PAGE_LIMIT {
                return Err(ConfigurationError::validation_message(
                    "configuration audit limit must be between 1 and 1000",
                ));
            }
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let mut events =
                audit_from_transaction(&read, query.after_event_id.as_ref(), query.limit + 1)
                    .await
                    .map_err(map_store_error)?;
            let next_after_event_id = if events.len() > query.limit {
                events.pop();
                events.last().map(|event| event.event_id.clone())
            } else {
                None
            };
            Ok(ConfigurationAuditPage {
                events,
                next_after_event_id,
            })
        })
    }

    fn observed_state(
        &self,
        actor: &AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>> {
        let actor = actor.clone();
        Box::pin(async move {
            actor.validate()?;
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            latest_component_activation_states(&read)
                .await
                .map_err(map_store_error)
                .map(|states| {
                    states
                        .into_iter()
                        .map(|state| ComponentConfigurationState {
                            component: state.component,
                            desired_revision_id: state.desired_revision_id,
                            observed_revision_id: state
                                .observed_revision_id
                                .or(state.last_working_revision_id),
                            restart_required: state.restart_required,
                            activation_error_code: state.activation_error_code,
                        })
                        .collect()
                })
        })
    }
}

impl CredentialWritePort for GlobalDbConfigurationControlStore<'_> {
    fn write_reference(
        &self,
        authority: &ConfigurationMutationAuthority,
        write: &WriteOnlyCredentialMutation,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1> {
        let authority = authority.clone();
        let write = write.clone();
        let expected_revision = expected_revision.clone();
        Box::pin(async move {
            authority.validate_integrity()?;
            expected_revision
                .validate()
                .map_err(ConfigurationError::validation)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|_| ConfigurationError::Unavailable)?;
            let outcome = async {
                let current = current_state_from_transaction(&transaction).await?;
                if current.revision_id != expected_revision {
                    return Err(ConfigurationError::RevisionConflict);
                }
                let prior = match &write.expected_reference_id {
                    Some(reference_id) => {
                        let prior =
                            credential_reference_from_transaction(&transaction, reference_id)
                                .await?
                                .ok_or(ConfigurationError::PlanStale)?;
                        if prior.kind != write.kind {
                            return Err(ConfigurationError::IdempotencyConflict);
                        }
                        prior
                    }
                    None => CredentialReferenceMetadataV1::new(
                        CredentialReferenceId::new("credential.reference.none")
                            .map_err(ConfigurationError::validation)?,
                        write.kind.clone(),
                        canonical_sha256(&(
                            "tracedecay.configuration.empty-credential-reference.v1",
                        ))
                        .map_err(ConfigurationError::validation)?,
                        UtcMicros(0),
                        0,
                    )
                    .map_err(ConfigurationError::validation)?,
                };
                let rotation = if write.expected_reference_id.is_some() {
                    prior.rotation.checked_add(1).ok_or_else(|| {
                        ConfigurationError::validation_message("credential rotation overflow")
                    })?
                } else {
                    0
                };
                let reference_digest = canonical_sha256(&(
                    "tracedecay.configuration.credential-reference.v1",
                    &authority.receipt.receipt_id,
                    &write.kind,
                    write.write_handle.as_str(),
                    &write.expected_reference_id,
                    rotation,
                ))
                .map_err(ConfigurationError::validation)?;
                let operation_digest = canonical_sha256(&(
                    "tracedecay.configuration.credential-write.v1",
                    &authority.receipt.receipt_id,
                    &expected_revision,
                    &write.kind,
                    write.write_handle.as_str(),
                    &write.expected_reference_id,
                    rotation,
                ))
                .map_err(ConfigurationError::validation)?;
                let idempotency_key: ConfigurationIdempotencyKey = derived_identifier(
                    "configuration.idempotency.credential-write.v1",
                    &canonical_sha256(&(
                        "tracedecay.configuration.credential-write-idempotency.v1",
                        &authority.receipt.receipt_id,
                        &operation_digest,
                    ))
                    .map_err(ConfigurationError::validation)?,
                    "credential write idempotency key",
                )?;
                let reference_id: CredentialReferenceId = derived_identifier(
                    "credential.reference.v1",
                    &canonical_sha256(&(
                        "tracedecay.configuration.credential-reference-id.v1",
                        &authority.receipt.receipt_id,
                        &reference_digest,
                    ))
                    .map_err(ConfigurationError::validation)?,
                    "credential reference id",
                )?;
                let metadata = CredentialReferenceMetadataV1::new(
                    reference_id.clone(),
                    write.kind.clone(),
                    reference_digest,
                    authority.receipt.issued_at,
                    rotation,
                )
                .map_err(ConfigurationError::validation)?;
                match credential_reference_from_transaction(&transaction, &reference_id).await? {
                    Some(existing) if existing == metadata => Ok(existing),
                    Some(_) => Err(ConfigurationError::IdempotencyConflict),
                    None => {
                        transaction
                            .execute(
                                "INSERT INTO configuration_credential_references (
                                    reference_id, kind, reference_digest, created_at, rotation
                                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                                params![
                                    metadata.reference_id.as_str(),
                                    projection_encoding(&metadata.kind).map_err(map_store_error)?,
                                    metadata.reference_digest.as_str(),
                                    metadata.created_at.0,
                                    i64::try_from(metadata.rotation).map_err(|_| {
                                        ConfigurationError::validation_message(
                                            "credential rotation exceeds SQLite range",
                                        )
                                    })?,
                                ],
                            )
                            .await
                            .map_err(|_| ConfigurationError::Unavailable)?;
                        let event_id: ConfigurationAuditEventId = derived_identifier(
                            "configuration.audit.v1",
                            &canonical_sha256(&(
                                "tracedecay.configuration.credential-write-audit.v1",
                                &authority.receipt.actor_id,
                                &idempotency_key,
                                &operation_digest,
                            ))
                            .map_err(ConfigurationError::validation)?,
                            "configuration audit event id",
                        )?;
                        let (sealed_target_reference, target_commitment) = seal_audit_target(
                            &transaction,
                            &event_id,
                            &metadata,
                            authority.receipt.issued_at,
                        )
                        .await
                        .map_err(map_store_error)?;
                        let event = ConfigurationAuditEvent {
                            event_id,
                            event_kind: ConfigurationAuditEventKindV1::Applied,
                            actor_id: authority.receipt.actor_id.clone(),
                            idempotency_key: Some(idempotency_key),
                            base_revision_id: expected_revision.clone(),
                            result_revision_id: None,
                            operation_digest,
                            target_commitment,
                            receipt_id: None,
                            safe_reason_code: None,
                            occurred_at: authority.receipt.issued_at,
                        };
                        insert_audit_event_with_receipt_digest(
                            &transaction,
                            &event,
                            None,
                            Some(&sealed_target_reference),
                        )
                        .await
                        .map_err(map_store_error)?;
                        Ok(metadata)
                    }
                }
            }
            .await;
            match outcome {
                Ok(metadata) => transaction
                    .commit()
                    .await
                    .map(|()| metadata)
                    .map_err(|_| ConfigurationError::Unavailable),
                Err(error) => Err(error),
            }
        })
    }
}

impl ConfigurationRevisionStore for GlobalDbConfigurationControlStore<'_> {
    async fn current_revision(&self) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
        let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
        let revision_id = current_revision_id_from_executor(&read).await?;
        read_revision_from_executor(&read, &revision_id)
            .await?
            .ok_or_else(|| invalid_store_data("current configuration revision disappeared"))
    }

    fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>>> + Send
    {
        let revision_id = revision_id.clone();
        async move {
            revision_id
                .validate()
                .map_err(ConfigurationStoreError::from)?;
            let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
            read_revision_from_executor(&read, &revision_id).await
        }
    }

    fn save_change_plan(
        &self,
        plan: &ConfigurationProtectedPlanRecordV1,
    ) -> impl Future<Output = ConfigurationStoreResult<()>> + Send {
        let plan = plan.clone();
        async move {
            plan.validate().map_err(ConfigurationStoreError::from)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(unavailable_store)?;
            let outcome =
                match read_change_plan_from_executor(&transaction, &plan.plan.plan_id).await {
                    Ok(Some(existing)) if existing == plan => Ok(()),
                    Ok(Some(_)) => Err(ConfigurationStoreError::IdempotencyConflict),
                    Ok(None) => insert_change_plan(&transaction, &plan).await,
                    Err(error) => Err(error),
                };
            match outcome {
                Ok(()) => transaction.commit().await.map_err(unavailable_store),
                Err(error) => Err(error),
            }
        }
    }

    fn read_change_plan(
        &self,
        plan_id: &ChangePlanId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>>> + Send
    {
        let plan_id = plan_id.clone();
        async move {
            plan_id.validate().map_err(ConfigurationStoreError::from)?;
            let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
            read_change_plan_from_executor(&read, &plan_id).await
        }
    }

    async fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> ConfigurationStoreResult<ConfigurationMutationReceiptV1> {
        validate_commit_bindings(&commit)?;
        let transaction = self
            .db
            .begin_write_transaction()
            .await
            .map_err(unavailable_store)?;
        let outcome = commit_configuration_transaction(&transaction, &commit, false, None).await;
        match outcome {
            Ok(receipt) => transaction
                .commit()
                .await
                .map(|()| receipt)
                .map_err(unavailable_store),
            Err(error) => Err(error),
        }
    }

    fn audit(
        &self,
        after: Option<&ConfigurationAuditEventId>,
        limit: usize,
    ) -> impl Future<Output = ConfigurationStoreResult<Vec<ConfigurationAuditEvent>>> + Send {
        let after = after.cloned();
        async move {
            let read = self.db.read_snapshot().await.map_err(unavailable_store)?;
            audit_from_transaction(&read, after.as_ref(), limit).await
        }
    }
}

impl ConfigurationMigrationStore for GlobalDbConfigurationControlStore<'_> {
    fn receipt(
        &self,
        receipt_name: &'static str,
        source_snapshot_digest: &ManifestDigest,
    ) -> impl Future<
        Output = Result<Option<ConfigurationMigrationReceiptV1>, ConfigurationMigrationError>,
    > + Send {
        let source_snapshot_digest = source_snapshot_digest.clone();
        async move {
            let read = self
                .db
                .read_snapshot()
                .await
                .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
            migration_receipt_from_transaction(&read, receipt_name, &source_snapshot_digest).await
        }
    }

    fn commit_initial_migration(
        &self,
        receipt: &ConfigurationMigrationReceiptV1,
        resolution: &ConfigurationResolutionV1,
        quarantine: &[ConfigurationMigrationQuarantineEntryV1],
    ) -> impl Future<Output = Result<(), ConfigurationMigrationError>> + Send {
        let receipt = receipt.clone();
        let resolution = resolution.clone();
        let quarantine = quarantine.to_vec();
        async move {
            resolution
                .snapshot
                .validate()
                .map_err(ConfigurationMigrationError::Domain)?;
            let transaction = self
                .db
                .begin_write_transaction()
                .await
                .map_err(|error| ConfigurationMigrationError::Store(error.to_string()))?;
            let outcome = commit_initial_migration_transaction(
                &transaction,
                &receipt,
                &resolution,
                &quarantine,
                false,
            )
            .await;
            match outcome {
                Ok(()) => transaction
                    .commit()
                    .await
                    .map_err(|error| ConfigurationMigrationError::Store(error.to_string())),
                Err(error) => Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::registry::ConfigurationRegistry;
    use crate::configuration::resolver::resolve_configuration;
    use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
    use tracedecay_domain::configuration::{
        AccessRuleId, AuthorityRef, ConfigurationAuditEventKindV1, ConfigurationCandidateV1,
        ConfigurationGrantId, ConfigurationGrantReceiptId, ConfigurationLayerIdV1,
        ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
        ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationValueV1,
        CredentialKindV1, DIAGNOSTICS_PREWARM_SETTING_KEY, ProtectedApplyRequest, ProtectedChange,
        ProtectedChangePlan, RedactedConfigurationChangeV1, SOURCE_BINDINGS_SETTING_KEY,
        ScopeAccessRule, ScopeAccessSubjectV1, ScopeControlOperationV1, ScopeSourceBinding,
        SourceBindingId, SourceKindV1,
    };
    use tracedecay_domain::research::CapabilityId;
    use tracedecay_domain::{AccessPolicyDigest, ActorId, LocatorDigest, ProjectId, UtcMicros};

    async fn setup() -> (tempfile::TempDir, TestConnection) {
        let directory = tempfile::tempdir().unwrap();
        let connection = TestConnection::open(&directory.path().join("configuration.db"));
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();
        ensure_configuration_schema(&*connection).await.unwrap();
        (directory, connection)
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn forward_repair_materializes_defaults_added_after_a_stored_revision() {
        let registry = ConfigurationRegistry::core().unwrap();
        let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
        let missing_key = SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap();
        let mut effective_values = current.effective_values;
        let mut provenance = current.provenance;
        effective_values.remove(&missing_key);
        provenance.remove(&missing_key);
        let incomplete = ConfigurationSnapshotV1::new(effective_values, provenance).unwrap();
        assert!(validate_snapshot_registry_completeness(&incomplete).is_err());

        let repaired = complete_snapshot_for_current_registry(&incomplete).unwrap();

        validate_snapshot_registry_completeness(&repaired).unwrap();
        assert_eq!(
            repaired.effective_values.get(&missing_key),
            Some(&registry.definition(&missing_key).unwrap().default_value),
        );
    }

    #[test]
    fn forward_repair_disables_pre_digest_semantics_without_changing_install_intent() {
        let registry = ConfigurationRegistry::core().unwrap();
        let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
        let semantic_key =
            SettingKey::new(tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY)
                .unwrap();
        let legacy_artifact_path = std::env::temp_dir().join("tracedecay-semantic-legacy");
        let legacy = serde_json::json!({
            "selected_model": tracedecay_semantic::DEFAULT_FASTEMBED_MODEL_ID,
            "auto_download": true,
            "active_profile": {
                "profile_id": "profile.semantic.legacy.v1",
                "artifact_digest": "a".repeat(64),
                "artifact_path": legacy_artifact_path
            },
            "rollback_profile": null,
            "resources": tracedecay_semantic::SemanticResourceCeilings::default()
        });
        let mut effective_values = current.effective_values;
        let provenance = current.provenance;
        effective_values.insert(
            semantic_key.clone(),
            ConfigurationValueV1::Text(serde_json::to_string(&legacy).unwrap()),
        );
        let legacy_snapshot = ConfigurationSnapshotV1::new(effective_values, provenance).unwrap();
        let ConfigurationValueV1::Text(legacy_text) =
            legacy_snapshot.effective_values.get(&semantic_key).unwrap()
        else {
            panic!("semantic setting must remain typed text");
        };
        assert!(
            serde_json::from_str::<crate::configuration::semantic::SemanticConfig>(legacy_text)
                .is_err(),
            "fixture must reproduce the pre-accepted-profile-digest snapshot"
        );

        let repaired = complete_snapshot_for_current_registry(&legacy_snapshot).unwrap();

        let ConfigurationValueV1::Text(repaired_text) =
            repaired.effective_values.get(&semantic_key).unwrap()
        else {
            panic!("semantic setting must remain typed text");
        };
        let semantic =
            serde_json::from_str::<crate::configuration::semantic::SemanticConfig>(repaired_text)
                .unwrap();
        semantic.validate().unwrap();
        assert_eq!(
            semantic.selected_model.as_deref(),
            Some(tracedecay_semantic::DEFAULT_FASTEMBED_MODEL_ID)
        );
        assert!(semantic.auto_download);
        assert_eq!(
            semantic.resources,
            tracedecay_semantic::SemanticResourceCeilings::default()
        );
        assert!(semantic.active_profile.is_none());
        assert!(semantic.rollback_profile.is_none());
    }

    #[test]
    fn forward_repair_rejects_unrecognized_semantic_configuration() {
        let registry = ConfigurationRegistry::core().unwrap();
        let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
        let semantic_key =
            SettingKey::new(tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY)
                .unwrap();
        let legacy_artifact_path = std::env::temp_dir().join("tracedecay-semantic-legacy");
        let mut effective_values = current.effective_values;
        effective_values.insert(
            semantic_key,
            ConfigurationValueV1::Text(
                serde_json::json!({
                    "selected_model": tracedecay_semantic::DEFAULT_FASTEMBED_MODEL_ID,
                    "active_profile": {
                        "profile_id": "profile.semantic.legacy.v1",
                        "artifact_digest": "a".repeat(64),
                        "artifact_path": legacy_artifact_path
                    },
                    "rollback_profile": null,
                    "resources": tracedecay_semantic::SemanticResourceCeilings::default(),
                    "x": true
                })
                .to_string(),
            ),
        );
        let malformed = ConfigurationSnapshotV1::new(effective_values, current.provenance).unwrap();

        assert!(complete_snapshot_for_current_registry(&malformed).is_err());
    }

    fn migration_fixture() -> (
        ConfigurationMigrationReceiptV1,
        ConfigurationResolutionV1,
        Vec<ConfigurationMigrationQuarantineEntryV1>,
    ) {
        let resolution =
            resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[]).unwrap();
        let receipt = ConfigurationMigrationReceiptV1 {
            receipt_name: CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME,
            source_snapshot_digest: digest('a'),
            initial_revision_id: ConfigurationRevisionId::new("configuration.revision.initial")
                .unwrap(),
            initial_snapshot_id: resolution.snapshot.snapshot_id.clone(),
            created_at: UtcMicros(1),
        };
        let quarantine = vec![ConfigurationMigrationQuarantineEntryV1 {
            source_kind: LegacyConfigurationSourceKindV1::ConfigJson,
            source_key_digest: digest('b'),
            reason: super::super::migration::ConfigurationMigrationQuarantineReasonV1::UnknownKey,
            redacted_value_digest: digest('c'),
            quarantined_at: UtcMicros(1),
        }];
        (receipt, resolution, quarantine)
    }

    async fn count(connection: &Connection, table: &str) -> i64 {
        let mut rows = connection
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn root_revision() -> ConfigurationRevisionRecordV1 {
        let snapshot = resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[])
            .unwrap()
            .snapshot;
        ConfigurationRevisionRecordV1 {
            revision_id: id("configuration.revision.root"),
            parent_revision_id: None,
            snapshot,
            actor_id: id("actor.configuration.fixture"),
            operation_kind: "migration".to_owned(),
            created_at: UtcMicros(1),
        }
    }

    fn source_binding_snapshot(revision_id: &ConfigurationRevisionId) -> ConfigurationSnapshotV1 {
        let mut snapshot = resolve_configuration(&ConfigurationRegistry::core().unwrap(), &[])
            .unwrap()
            .snapshot;
        let key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap();
        let project_id: ProjectId = id("project.authoritative.fixture");
        let binding = ScopeSourceBinding::new(
            id::<SourceBindingId>("binding.authoritative.fixture"),
            SourceKindV1::Cursor,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            AuthorityRef::Project(project_id.clone()),
        )
        .unwrap();
        let candidate = ConfigurationCandidateV1 {
            layer: ConfigurationLayerIdV1::Project { project_id },
            revision_id: revision_id.clone(),
            disposition: CandidateDispositionV1::Winning,
            safe_reason: None,
        };
        snapshot.effective_values.insert(
            key.clone(),
            ConfigurationValueV1::SourceBindings(vec![binding]),
        );
        snapshot.provenance.insert(key, vec![candidate]);
        ConfigurationSnapshotV1::new(snapshot.effective_values, snapshot.provenance).unwrap()
    }

    fn protected_plan(
        base_revision_id: &ConfigurationRevisionId,
    ) -> ConfigurationProtectedPlanRecordV1 {
        let operation =
            ConfigurationProtectedOperationV1::Change(Box::new(ProtectedChange::BindSource(
                ScopeSourceBinding::new(
                    id::<SourceBindingId>("binding.plan.fixture"),
                    SourceKindV1::Cursor,
                    LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                    AuthorityRef::Project(id("project.authoritative.fixture")),
                )
                .unwrap(),
            )));
        let plan = ProtectedChangePlan {
            plan_id: id("configuration.plan.fixture"),
            actor_id: id("actor.configuration.fixture"),
            base_revision_id: base_revision_id.clone(),
            operation_digest: operation.operation_digest().unwrap(),
            resolved_scope_digest: digest('c'),
            membership_digest: Some(digest('d')),
            authorization_policy_digest: id::<AccessPolicyDigest>(&format!(
                "sha256:{}",
                "e".repeat(64)
            )),
            policy_epoch: 7,
            expires_at: UtcMicros(100),
            created_at: UtcMicros(10),
            redacted_changes: vec![RedactedConfigurationChangeV1 {
                setting_key: SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
                operation: ScopeControlOperationV1::SourceBind,
                before_digest: Some(digest('f')),
                after_digest: Some(digest('a')),
            }],
        };
        ConfigurationProtectedPlanRecordV1 { plan, operation }
    }

    fn protected_commit(
        root: &ConfigurationRevisionRecordV1,
    ) -> (ConfigurationProtectedPlanRecordV1, ConfigurationCommitV1) {
        let next_revision_id: ConfigurationRevisionId = id("configuration.revision.child");
        let next_revision = ConfigurationRevisionRecordV1 {
            revision_id: next_revision_id.clone(),
            parent_revision_id: Some(root.revision_id.clone()),
            snapshot: source_binding_snapshot(&next_revision_id),
            actor_id: root.actor_id.clone(),
            operation_kind: "protected_apply".to_owned(),
            created_at: UtcMicros(20),
        };
        let plan_record = protected_plan(&root.revision_id);
        let plan = plan_record.plan.clone();
        let receipt = ConfigurationMutationReceiptV1 {
            receipt_id: id("configuration.receipt.fixture"),
            actor_id: root.actor_id.clone(),
            idempotency_key: id("configuration.idempotency.fixture"),
            base_revision_id: root.revision_id.clone(),
            result_revision_id: next_revision_id.clone(),
            operation_digest: plan.operation_digest.clone(),
            receipt_digest: digest('9'),
            created_at: UtcMicros(21),
        };
        let audit_event = ConfigurationAuditEvent {
            event_id: id("configuration.audit.fixture"),
            event_kind: ConfigurationAuditEventKindV1::Applied,
            actor_id: root.actor_id.clone(),
            idempotency_key: Some(receipt.idempotency_key.clone()),
            base_revision_id: root.revision_id.clone(),
            result_revision_id: Some(next_revision_id),
            operation_digest: plan.operation_digest.clone(),
            target_commitment: digest('8'),
            receipt_id: Some(receipt.receipt_id.clone()),
            safe_reason_code: None,
            occurred_at: UtcMicros(22),
        };
        (
            plan_record,
            ConfigurationCommitV1 {
                expected_base_revision_id: root.revision_id.clone(),
                next_revision,
                receipt,
                change_plan: Some(plan),
                audit_event,
            },
        )
    }

    async fn seed_revision(connection: &Connection, revision: &ConfigurationRevisionRecordV1) {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        insert_revision(&transaction, revision).await.unwrap();
        transaction.commit().await.unwrap();
    }

    async fn global_setup() -> (
        tempfile::TempDir,
        HostAdmissionTestRuntimeV1,
        ConfigurationRevisionRecordV1,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let profile_root = directory.path().join("profile");
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let runtime = HostAdmissionTestRuntimeV1::project(
            &profile_root,
            &project_root,
            ProjectId::new("project.configuration-store.fixture").unwrap(),
        )
        .await
        .expect("registered configuration runtime");
        let db = runtime
            .registered_database(HostAdmissionScope::Project)
            .expect("registered project database");
        let root = root_revision();
        let transaction = db.begin_write_transaction().await.unwrap();
        insert_revision(&transaction, &root).await.unwrap();
        transaction.commit().await.unwrap();
        (directory, runtime, root)
    }

    fn policy_digest(byte: char) -> AccessPolicyDigest {
        AccessPolicyDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn direct_project_layer() -> ConfigurationLayerIdV1 {
        ConfigurationLayerIdV1::Project {
            project_id: id("project.configuration.fixture"),
        }
    }

    fn control_authority(
        operation: ConfigurationMutationOperationV1,
        expected_revision: &ConfigurationRevisionId,
    ) -> ConfigurationMutationAuthority {
        let (sink, effect) = match operation {
            ConfigurationMutationOperationV1::CredentialWrite => (
                ConfigurationMutationSinkV1::CredentialStore,
                ConfigurationMutationEffectV1::WriteCredentialReference,
            ),
            ConfigurationMutationOperationV1::ProtectedDryRun
            | ConfigurationMutationOperationV1::RollbackDryRun => (
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CreateProtectedChangePlan,
            ),
            ConfigurationMutationOperationV1::DirectMutation
            | ConfigurationMutationOperationV1::ProtectedApply
            | ConfigurationMutationOperationV1::RollbackApply => (
                ConfigurationMutationSinkV1::ConfigurationStore,
                ConfigurationMutationEffectV1::CommitConfigurationRevision,
            ),
        };
        let scope_digest = if operation == ConfigurationMutationOperationV1::DirectMutation {
            canonical_sha256(&(
                "tracedecay.configuration.direct-target-layer.v1",
                direct_project_layer(),
            ))
            .unwrap()
        } else {
            digest('a')
        };
        ConfigurationMutationAuthority {
            receipt: ConfigurationMutationGrantReceiptV1::issue(
                id::<ConfigurationGrantReceiptId>(
                    &format!("configuration.grant-receipt.{operation:?}").to_lowercase(),
                ),
                id::<ConfigurationGrantId>("configuration.grant.fixture"),
                id::<ActorId>("actor.configuration.fixture"),
                operation,
                scope_digest,
                expected_revision.clone(),
                7,
                policy_digest('b'),
                sink,
                effect,
                UtcMicros(10),
                UtcMicros(1_000),
            )
            .unwrap(),
        }
    }

    fn protected_plan_for(
        plan_id: &str,
        actor_id: &ActorId,
        base_revision_id: &ConfigurationRevisionId,
        change: &ProtectedChange,
    ) -> ProtectedChangePlan {
        ProtectedChangePlan {
            plan_id: id(plan_id),
            actor_id: actor_id.clone(),
            base_revision_id: base_revision_id.clone(),
            operation_digest: change.compute_digest().unwrap(),
            resolved_scope_digest: digest('a'),
            membership_digest: None,
            authorization_policy_digest: policy_digest('b'),
            policy_epoch: 7,
            created_at: UtcMicros(10),
            expires_at: UtcMicros(1_000),
            redacted_changes: vec![RedactedConfigurationChangeV1 {
                setting_key: match change {
                    ProtectedChange::BindSource(_)
                    | ProtectedChange::RebindSource(_)
                    | ProtectedChange::UnbindSource { .. } => {
                        SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap()
                    }
                    ProtectedChange::UpsertAccessRule(_)
                    | ProtectedChange::RemoveAccessRule { .. } => {
                        SettingKey::new(ACCESS_RULES_SETTING_KEY).unwrap()
                    }
                    ProtectedChange::ReplaceWorkTopologyPolicy(_) => {
                        SettingKey::new(WORK_TOPOLOGY_POLICY_SETTING_KEY).unwrap()
                    }
                },
                operation: change.operation_kind(),
                before_digest: Some(digest('c')),
                after_digest: Some(digest('d')),
            }],
        }
    }

    fn evidence_for(plan: &ProtectedChangePlan) -> ScopeRevalidationEvidenceV1 {
        ScopeRevalidationEvidenceV1 {
            resolved_scope_digest: plan.resolved_scope_digest.clone(),
            membership_digest: plan.membership_digest.clone(),
            authorization_policy_digest: plan.authorization_policy_digest.clone(),
            policy_epoch: plan.policy_epoch,
        }
    }

    #[test]
    fn direct_unset_restores_the_registry_default_and_its_provenance() {
        let registry = ConfigurationRegistry::core().unwrap();
        let current = resolve_configuration(&registry, &[]).unwrap().snapshot;
        let key = SettingKey::new(DIAGNOSTICS_PREWARM_SETTING_KEY).unwrap();
        let set_revision = id("configuration.revision.set");
        let set = apply_direct_mutation_to_snapshot(
            &current,
            &DirectConfigurationMutation::Set {
                layer: direct_project_layer(),
                key: key.clone(),
                value: ConfigurationValueV1::Boolean(true),
            },
            &set_revision,
            &registry,
        )
        .unwrap();
        let unset = apply_direct_mutation_to_snapshot(
            &set,
            &DirectConfigurationMutation::Unset {
                layer: direct_project_layer(),
                key: key.clone(),
            },
            &id("configuration.revision.unset"),
            &registry,
        )
        .unwrap();

        assert_eq!(
            unset.effective_values[&key],
            registry.definition(&key).unwrap().default_value
        );
        assert_eq!(
            unset.provenance[&key],
            vec![registry_default_candidate().unwrap()]
        );
    }

    #[test]
    fn audit_target_commitments_are_keyed_and_event_scoped() {
        let event_one = id("configuration.audit.one");
        let event_two = id("configuration.audit.two");
        let target = br#"{"binding_id":"binding.fixture"}"#;
        let first = audit_target_commitment(&[1; 32], &event_one, target).unwrap();

        assert_ne!(
            first,
            audit_target_commitment(&[2; 32], &event_one, target).unwrap()
        );
        assert_ne!(
            first,
            audit_target_commitment(&[1; 32], &event_two, target).unwrap()
        );
        assert_ne!(
            first,
            canonical_sha256(&(
                "tracedecay.configuration.audit-target-commitment.v1",
                &event_one,
                target,
            ))
            .unwrap(),
            "a public digest is not an HMAC commitment"
        );
    }

    #[tokio::test]
    async fn partial_rollback_is_typed_unavailable_until_selective_restore_exists() {
        let (_directory, runtime, root) = global_setup().await;
        let db = runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap();
        let store = GlobalDbConfigurationControlStore::new_registered(db);
        let authority = control_authority(
            ConfigurationMutationOperationV1::RollbackDryRun,
            &root.revision_id,
        );
        let result = store
            .dry_run_rollback(
                &authority,
                &ConfigurationRollbackRequest {
                    target_revision_id: root.revision_id,
                    mode: RollbackModeV1::Partial,
                },
                UtcMicros(20),
            )
            .await;
        assert_eq!(result, Err(ConfigurationError::Unavailable));
    }

    #[tokio::test]
    async fn global_control_adapter_enforces_direct_cas_and_exact_replay() {
        let (_directory, runtime, root) = global_setup().await;
        let db = runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap();
        let store = GlobalDbConfigurationControlStore::new_registered(db);
        let authority = control_authority(
            ConfigurationMutationOperationV1::DirectMutation,
            &root.revision_id,
        );
        let mutation = DirectConfigurationMutation::Set {
            layer: direct_project_layer(),
            key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
            value: ConfigurationValueV1::Boolean(true),
        };
        let foreign_target = DirectConfigurationMutation::Set {
            layer: ConfigurationLayerIdV1::Project {
                project_id: id("project.foreign.fixture"),
            },
            key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
            value: ConfigurationValueV1::Boolean(true),
        };
        assert_eq!(
            store
                .commit_direct(&authority, &foreign_target, &root.revision_id)
                .await,
            Err(ConfigurationError::MutationAuthorityRejected)
        );

        let receipt = store
            .commit_direct(&authority, &mutation, &root.revision_id)
            .await
            .unwrap();
        assert_eq!(
            store
                .commit_direct(&authority, &mutation, &root.revision_id)
                .await
                .unwrap(),
            receipt
        );

        let conflicting = DirectConfigurationMutation::Set {
            layer: direct_project_layer(),
            key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
            value: ConfigurationValueV1::Boolean(false),
        };
        assert_eq!(
            store
                .commit_direct(&authority, &conflicting, &root.revision_id)
                .await,
            Err(ConfigurationError::RevisionConflict)
        );
        assert_eq!(
            store
                .commit_direct(
                    &authority,
                    &DirectConfigurationMutation::Set {
                        layer: direct_project_layer(),
                        key: SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).unwrap(),
                        value: ConfigurationValueV1::SourceBindings(Vec::new()),
                    },
                    &root.revision_id,
                )
                .await,
            Err(ConfigurationError::PolicyWideningForbidden)
        );
        let credential_reference = CredentialReferenceMetadataV1::new(
            id("credential.reference.direct-rejection"),
            CredentialKindV1::ApiToken,
            digest('f'),
            UtcMicros(1),
            0,
        )
        .unwrap();
        assert_eq!(
            store
                .commit_direct(
                    &authority,
                    &DirectConfigurationMutation::Set {
                        layer: direct_project_layer(),
                        key: SettingKey::new("diagnostics.credential_reference.v1").unwrap(),
                        value: ConfigurationValueV1::CredentialReference(credential_reference),
                    },
                    &root.revision_id,
                )
                .await,
            Err(ConfigurationError::Validation(
                "credential references require the write-only credential operation".to_owned()
            ))
        );
    }

    fn assert_daemon_configuration_authority<T>()
    where
        T: ConfigurationControlStore + Clone + Send + Sync + 'static,
    {
    }

    #[test]
    fn owned_global_control_adapter_satisfies_daemon_registration_bounds() {
        assert_daemon_configuration_authority::<OwnedGlobalDbConfigurationControlStore>();
    }

    #[tokio::test]
    async fn owned_global_control_adapter_preserves_cas_while_daemon_scope_is_active() {
        let (_directory, runtime, root) = global_setup().await;
        let store = runtime
            .project_configuration_control_store_for_test()
            .unwrap();

        assert_eq!(store.current().await.unwrap().revision_id, root.revision_id);

        let authority = control_authority(
            ConfigurationMutationOperationV1::DirectMutation,
            &root.revision_id,
        );
        let mutation = DirectConfigurationMutation::Set {
            layer: direct_project_layer(),
            key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
            value: ConfigurationValueV1::Boolean(true),
        };
        let receipt = store
            .commit_direct(&authority, &mutation, &root.revision_id)
            .await
            .unwrap();

        assert_eq!(
            store
                .clone()
                .commit_direct(&authority, &mutation, &root.revision_id)
                .await
                .unwrap(),
            receipt
        );
        assert_eq!(
            store
                .commit_direct(
                    &authority,
                    &DirectConfigurationMutation::Set {
                        layer: direct_project_layer(),
                        key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
                        value: ConfigurationValueV1::Boolean(false),
                    },
                    &root.revision_id,
                )
                .await,
            Err(ConfigurationError::RevisionConflict)
        );
    }

    #[tokio::test]
    async fn owned_global_control_adapter_rejects_writes_after_daemon_scope_ends() {
        let (_directory, runtime, root) = global_setup().await;
        let store = runtime
            .project_configuration_control_store_for_test()
            .unwrap();
        drop(runtime);

        let authority = control_authority(
            ConfigurationMutationOperationV1::DirectMutation,
            &root.revision_id,
        );
        let mutation = DirectConfigurationMutation::Set {
            layer: direct_project_layer(),
            key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
            value: ConfigurationValueV1::Boolean(true),
        };

        assert_eq!(
            store
                .commit_direct(&authority, &mutation, &root.revision_id)
                .await,
            Err(ConfigurationError::Unavailable)
        );
    }

    #[tokio::test]
    async fn daemon_binding_repair_rejects_matching_locator_with_noncanonical_id() {
        let directory = tempfile::tempdir().unwrap();
        let profile_root = directory.path().join("profile");
        let project_root = directory.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let runtime = HostAdmissionTestRuntimeV1::project(
            &profile_root,
            &project_root,
            ProjectId::new("project.configuration-binding-repair").unwrap(),
        )
        .await
        .unwrap();
        let db = runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap();
        let mut root = root_revision();
        root.snapshot = source_binding_snapshot(&root.revision_id);
        let transaction = db.begin_write_transaction().await.unwrap();
        insert_revision(&transaction, &root).await.unwrap();
        transaction.commit().await.unwrap();
        let store = GlobalDbConfigurationControlStore::new_registered(db);
        let canonical = ScopeSourceBinding::new(
            id::<SourceBindingId>("binding.canonical.fixture"),
            SourceKindV1::Cursor,
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            AuthorityRef::Project(id("project.authoritative.fixture")),
        )
        .unwrap();

        assert_eq!(
            store
                .ensure_daemon_source_binding(canonical, UtcMicros(20))
                .await,
            Err(ConfigurationError::Validation(
                "daemon source binding registry repair found a non-canonical binding id".to_owned()
            ))
        );
    }

    #[tokio::test]
    async fn direct_audit_target_never_persists_sensitive_setting_values() {
        let (_directory, runtime, root) = global_setup().await;
        let db = runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap();
        let store = GlobalDbConfigurationControlStore::new_registered(db);
        let authority = control_authority(
            ConfigurationMutationOperationV1::DirectMutation,
            &root.revision_id,
        );
        let secret_path = "private-customer-source/**";
        let receipt = store
            .commit_direct(
                &authority,
                &DirectConfigurationMutation::Set {
                    layer: direct_project_layer(),
                    key: SettingKey::new("index.exclude.v1").unwrap(),
                    value: ConfigurationValueV1::StringList(vec![secret_path.to_owned()]),
                },
                &root.revision_id,
            )
            .await
            .unwrap();

        let read = db.read_snapshot().await.unwrap();
        let mut rows = read
            .query(
                "SELECT sealed_target_reference
                 FROM configuration_audit_events
                 WHERE result_revision_id = ?1",
                params![receipt.result_revision_id.as_str()],
            )
            .await
            .unwrap();
        let target = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<Vec<u8>>(0)
            .unwrap();
        assert!(!String::from_utf8_lossy(&target).contains(secret_path));
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn protected_operation_survives_adapter_rebuild_populates_projections_and_rolls_back() {
        let (_directory, runtime, root) = global_setup().await;
        let db = runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap();
        let actor_id: ActorId = id("actor.configuration.fixture");
        let source_change = ProtectedChange::BindSource(
            ScopeSourceBinding::new(
                id::<SourceBindingId>("binding.restart.fixture"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "e".repeat(64))).unwrap(),
                AuthorityRef::Project(id::<ProjectId>("project.restart.fixture")),
            )
            .unwrap(),
        );
        let source_plan = protected_plan_for(
            "configuration.plan.restart.source",
            &actor_id,
            &root.revision_id,
            &source_change,
        );
        {
            let store = GlobalDbConfigurationControlStore::new_registered(db);
            store.save_plan(&source_plan, &source_change).await.unwrap();
        }
        let store = GlobalDbConfigurationControlStore::new_registered(db);
        let apply_authority = control_authority(
            ConfigurationMutationOperationV1::ProtectedApply,
            &root.revision_id,
        );
        let source_request = ProtectedApplyRequest {
            plan_id: source_plan.plan_id.clone(),
            actor_id: actor_id.clone(),
            expected_base_revision_id: root.revision_id.clone(),
            operation_digest: source_plan.operation_digest.clone(),
            idempotency_key: id("configuration.idempotency.restart.source"),
        };
        let source_receipt = store
            .commit_protected(
                &apply_authority,
                &source_request,
                &source_plan,
                &evidence_for(&source_plan),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .commit_protected(
                    &apply_authority,
                    &source_request,
                    &source_plan,
                    &evidence_for(&source_plan),
                )
                .await
                .unwrap(),
            source_receipt
        );

        let read = db.read_snapshot().await.unwrap();
        let record = read_change_plan_from_executor(&read, &source_plan.plan_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.operation,
            ConfigurationProtectedOperationV1::Change(Box::new(source_change.clone()))
        );
        let mut rows = read
            .query(
                "SELECT COUNT(*), SUM(sealed_target_reference IS NOT NULL)
                 FROM configuration_source_bindings
                 CROSS JOIN configuration_audit_events",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 2);
        assert_eq!(row.get::<i64>(1).unwrap(), 2);
        drop(rows);
        drop(read);

        let access_change = ProtectedChange::UpsertAccessRule(
            ScopeAccessRule::new(
                id::<AccessRuleId>("access-rule.restart.fixture"),
                ScopeAccessSubjectV1 {
                    actor: Some(actor_id.clone()),
                    operation: Some(ScopeControlOperationV1::Read),
                    source_kind: Some(SourceKindV1::Cursor),
                },
                AuthorityRef::Project(id::<ProjectId>("project.restart.fixture")),
                BTreeSet::from([CapabilityId::new("capability.read.fixture").unwrap()]),
                RuleEffect::Deny,
                None,
            )
            .unwrap(),
        );
        let access_plan = protected_plan_for(
            "configuration.plan.restart.access",
            &actor_id,
            &source_receipt.result_revision_id,
            &access_change,
        );
        store.save_plan(&access_plan, &access_change).await.unwrap();
        let access_authority = control_authority(
            ConfigurationMutationOperationV1::ProtectedApply,
            &source_receipt.result_revision_id,
        );
        let access_request = ProtectedApplyRequest {
            plan_id: access_plan.plan_id.clone(),
            actor_id: actor_id.clone(),
            expected_base_revision_id: source_receipt.result_revision_id.clone(),
            operation_digest: access_plan.operation_digest.clone(),
            idempotency_key: id("configuration.idempotency.restart.access"),
        };
        let access_receipt = store
            .commit_protected(
                &access_authority,
                &access_request,
                &access_plan,
                &evidence_for(&access_plan),
            )
            .await
            .unwrap();

        let read = db.read_snapshot().await.unwrap();
        let mut rows = read
            .query("SELECT COUNT(*) FROM configuration_access_rules", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
        drop(rows);
        drop(read);

        let rollback_authority = control_authority(
            ConfigurationMutationOperationV1::RollbackDryRun,
            &access_receipt.result_revision_id,
        );
        let rollback = ConfigurationRollbackRequest {
            target_revision_id: root.revision_id.clone(),
            mode: RollbackModeV1::AllOrNothing,
        };
        let rollback_plan = store
            .dry_run_rollback(&rollback_authority, &rollback, UtcMicros(20))
            .await
            .unwrap();
        let rollback_apply_authority = control_authority(
            ConfigurationMutationOperationV1::RollbackApply,
            &access_receipt.result_revision_id,
        );
        let rollback_request = ProtectedApplyRequest {
            plan_id: rollback_plan.plan_id.clone(),
            actor_id,
            expected_base_revision_id: access_receipt.result_revision_id,
            operation_digest: rollback_plan.operation_digest.clone(),
            idempotency_key: id("configuration.idempotency.restart.rollback"),
        };
        let rollback_receipt = store
            .apply_rollback(
                &rollback_apply_authority,
                &rollback_request,
                &rollback_plan,
                &evidence_for(&rollback_plan),
            )
            .await
            .unwrap();
        assert_ne!(rollback_receipt.result_revision_id, root.revision_id);
        assert_eq!(store.current().await.unwrap().snapshot, root.snapshot);
    }

    #[tokio::test]
    async fn credential_references_are_opaque_and_activation_failure_preserves_last_working() {
        let (_directory, runtime, root) = global_setup().await;
        let db = runtime
            .registered_database(HostAdmissionScope::Project)
            .unwrap();
        let store = GlobalDbConfigurationControlStore::new_registered(db);
        store
            .record_component_activation(
                "gateway".to_owned(),
                Some(root.revision_id.clone()),
                None,
                UtcMicros(11),
            )
            .await
            .unwrap();

        let credential_authority = control_authority(
            ConfigurationMutationOperationV1::CredentialWrite,
            &root.revision_id,
        );
        let handle = "opaque-credential-write-handle";
        let metadata = store
            .write_reference(
                &credential_authority,
                &WriteOnlyCredentialMutation {
                    expected_reference_id: None,
                    kind: CredentialKindV1::ApiToken,
                    write_handle: crate::configuration::contracts::CredentialWriteHandleV1::new(
                        handle,
                    )
                    .unwrap(),
                },
                &root.revision_id,
            )
            .await
            .unwrap();
        let read = db.read_snapshot().await.unwrap();
        let mut rows = read
            .query(
                "SELECT reference_digest FROM configuration_credential_references WHERE reference_id = ?1",
                params![metadata.reference_id.as_str()],
            )
            .await
            .unwrap();
        let digest = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert!(!digest.contains(handle));
        drop(rows);
        let mut rows = read
            .query(
                "SELECT sealed_target_reference FROM configuration_audit_events",
                (),
            )
            .await
            .unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            let target = row.get::<Option<Vec<u8>>>(0).unwrap().unwrap_or_default();
            assert!(!String::from_utf8_lossy(&target).contains(handle));
        }
        drop(rows);
        drop(read);

        assert_eq!(
            store
                .write_reference(
                    &credential_authority,
                    &WriteOnlyCredentialMutation {
                        expected_reference_id: Some(metadata.reference_id.clone()),
                        kind: CredentialKindV1::AccessToken,
                        write_handle:
                            crate::configuration::contracts::CredentialWriteHandleV1::new(
                                "opaque-credential-kind-mismatch",
                            )
                            .unwrap(),
                    },
                    &root.revision_id,
                )
                .await,
            Err(ConfigurationError::IdempotencyConflict)
        );

        let direct_authority = control_authority(
            ConfigurationMutationOperationV1::DirectMutation,
            &root.revision_id,
        );
        let receipt = store
            .commit_direct(
                &direct_authority,
                &DirectConfigurationMutation::Set {
                    layer: direct_project_layer(),
                    key: SettingKey::new("diagnostics.prewarm.v1").unwrap(),
                    value: ConfigurationValueV1::Boolean(true),
                },
                &root.revision_id,
            )
            .await
            .unwrap();
        let actor = AuthorizedActor {
            actor_id: id("actor.configuration.fixture"),
        };
        let state = store.observed_state(&actor).await.unwrap().pop().unwrap();
        assert_eq!(state.desired_revision_id, receipt.result_revision_id);
        assert_eq!(state.observed_revision_id, Some(root.revision_id.clone()));
        assert!(state.restart_required);

        store
            .record_component_activation(
                "gateway".to_owned(),
                None,
                Some("gateway_activation_failed".to_owned()),
                UtcMicros(12),
            )
            .await
            .unwrap();
        let state = store.observed_state(&actor).await.unwrap().pop().unwrap();
        assert_eq!(state.observed_revision_id, Some(root.revision_id));
        assert!(state.restart_required);
        assert_eq!(
            state.activation_error_code.as_deref(),
            Some("gateway_activation_failed")
        );
    }

    #[tokio::test]
    async fn production_migration_store_commits_revision_quarantine_receipt_and_audit_atomically() {
        let (_directory, connection) = setup().await;
        let store = ConfigurationSqlStore::new(&connection);
        let (receipt, resolution, quarantine) = migration_fixture();

        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();

        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        let expected_entry_count = resolution
            .snapshot
            .effective_values
            .keys()
            .chain(resolution.snapshot.provenance.keys())
            .collect::<BTreeSet<_>>()
            .len();
        assert_eq!(
            count(&connection, "configuration_entries").await,
            i64::try_from(expected_entry_count).unwrap()
        );
        assert_eq!(
            count(&connection, "configuration_migration_quarantine").await,
            1
        );
        assert_eq!(
            count(&connection, "configuration_migration_receipts").await,
            1
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
        assert_eq!(
            count(&connection, "configuration_topology_policies").await,
            1
        );
        assert!(count(&connection, "configuration_topology_protected_refs").await > 0);
        assert_eq!(
            store.current_revision().await.unwrap().snapshot,
            resolution.snapshot
        );
        assert!(matches!(
            store.audit(None, 1).await.unwrap().as_slice(),
            [ConfigurationAuditEvent {
                event_kind: ConfigurationAuditEventKindV1::Recovered,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn production_migration_store_replays_exact_receipt_idempotently() {
        let (_directory, connection) = setup().await;
        let store = ConfigurationSqlStore::new(&connection);
        let (receipt, resolution, quarantine) = migration_fixture();

        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();
        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();

        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        assert_eq!(
            count(&connection, "configuration_migration_receipts").await,
            1
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
    }

    #[tokio::test]
    async fn production_migration_store_rejects_conflicting_replay() {
        let (_directory, connection) = setup().await;
        let store = ConfigurationSqlStore::new(&connection);
        let (receipt, resolution, quarantine) = migration_fixture();
        store
            .commit_initial_migration(&receipt, &resolution, &quarantine)
            .await
            .unwrap();

        let mut conflicting = receipt;
        conflicting.initial_revision_id =
            ConfigurationRevisionId::new("configuration.revision.conflict").unwrap();
        assert!(
            store
                .commit_initial_migration(&conflicting, &resolution, &quarantine)
                .await
                .is_err()
        );
        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
    }

    #[tokio::test]
    async fn injected_crash_rolls_back_every_migration_table() {
        let (directory, connection) = setup().await;
        let (receipt, resolution, quarantine) = migration_fixture();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();

        assert!(
            commit_initial_migration_transaction(
                &transaction,
                &receipt,
                &resolution,
                &quarantine,
                true,
            )
            .await
            .is_err()
        );
        drop(transaction);
        drop(connection);

        let connection = TestConnection::open(&directory.path().join("configuration.db"));
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();

        assert_eq!(count(&connection, "configuration_revisions").await, 0);
        assert_eq!(count(&connection, "configuration_entries").await, 0);
        assert_eq!(
            count(&connection, "configuration_migration_quarantine").await,
            0
        );
        assert_eq!(
            count(&connection, "configuration_migration_receipts").await,
            0
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 0);
    }

    #[tokio::test]
    async fn revision_store_round_trips_typed_snapshot_plan_receipt_and_audit() {
        let (_directory, connection) = setup().await;
        let root = root_revision();
        seed_revision(&connection, &root).await;
        let store = ConfigurationSqlStore::new(&connection);
        let (plan, commit) = protected_commit(&root);

        assert_eq!(store.current_revision().await.unwrap(), root);
        store.save_change_plan(&plan).await.unwrap();
        store.save_change_plan(&plan).await.unwrap();
        assert_eq!(
            store.read_change_plan(&plan.plan.plan_id).await.unwrap(),
            Some(plan.clone())
        );

        let receipt = store.commit(commit.clone()).await.unwrap();
        assert_eq!(receipt, commit.receipt);
        assert_eq!(
            store
                .read_revision(&commit.next_revision.revision_id)
                .await
                .unwrap(),
            Some(commit.next_revision.clone())
        );
        assert_eq!(
            store.current_revision().await.unwrap(),
            commit.next_revision.clone()
        );
        assert_eq!(store.commit(commit.clone()).await.unwrap(), receipt);

        let mut changed_input = commit.clone();
        changed_input.audit_event.safe_reason_code = Some("changed_input".to_owned());
        assert_eq!(
            store.commit(changed_input).await,
            Err(ConfigurationStoreError::IdempotencyConflict)
        );

        let mut stale = commit.clone();
        stale.change_plan = None;
        stale.next_revision.revision_id = id("configuration.revision.stale");
        stale.receipt.receipt_id = id("configuration.receipt.stale");
        stale.receipt.idempotency_key = id("configuration.idempotency.stale");
        stale.receipt.result_revision_id = stale.next_revision.revision_id.clone();
        stale.audit_event.event_id = id("configuration.audit.stale");
        stale.audit_event.idempotency_key = Some(stale.receipt.idempotency_key.clone());
        stale.audit_event.result_revision_id = Some(stale.next_revision.revision_id.clone());
        stale.audit_event.receipt_id = Some(stale.receipt.receipt_id.clone());
        assert_eq!(
            store.commit(stale).await,
            Err(ConfigurationStoreError::RevisionConflict)
        );

        assert_eq!(
            store.audit(None, 1).await.unwrap(),
            vec![commit.audit_event.clone()]
        );
        assert!(
            store
                .audit(Some(&commit.audit_event.event_id), 1)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(count(&connection, "configuration_revisions").await, 2);
        assert_eq!(
            count(&connection, "configuration_mutation_receipts").await,
            1
        );
        assert_eq!(
            count(&connection, "configuration_change_plan_events").await,
            2
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 1);
    }

    #[tokio::test]
    async fn rollback_terminal_event_is_persisted_and_visible_in_audit() {
        let (_directory, connection) = setup().await;
        let root = root_revision();
        seed_revision(&connection, &root).await;
        let store = ConfigurationSqlStore::new(&connection);
        let (plan, mut commit) = protected_commit(&root);
        commit.next_revision.operation_kind = "rollback_apply".to_owned();
        commit.audit_event.event_kind = ConfigurationAuditEventKindV1::RollbackApplied;

        store.save_change_plan(&plan).await.unwrap();
        store.commit(commit.clone()).await.unwrap();

        let mut rows = connection
            .query(
                "SELECT event_kind
                 FROM configuration_change_plan_events
                 WHERE plan_id = ?1 AND sequence = 1",
                params![plan.plan.plan_id.as_str()],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "rollback_applied"
        );
        assert_eq!(
            store.audit(None, 1).await.unwrap(),
            vec![commit.audit_event]
        );
    }

    #[tokio::test]
    async fn failed_configuration_commit_leaves_no_partial_revision_receipt_or_audit() {
        let (directory, connection) = setup().await;
        let root = root_revision();
        seed_revision(&connection, &root).await;
        let store = ConfigurationSqlStore::new(&connection);
        let (plan, commit) = protected_commit(&root);
        store.save_change_plan(&plan).await.unwrap();

        validate_commit_bindings(&commit).unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .unwrap();
        assert!(
            commit_configuration_transaction(&transaction, &commit, true, None)
                .await
                .is_err()
        );
        drop(transaction);
        drop(connection);

        let connection = TestConnection::open(&directory.path().join("configuration.db"));
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();
        assert_eq!(count(&connection, "configuration_revisions").await, 1);
        assert_eq!(
            count(&connection, "configuration_mutation_receipts").await,
            0
        );
        assert_eq!(count(&connection, "configuration_audit_events").await, 0);
        assert_eq!(
            count(&connection, "configuration_component_activation_events").await,
            0
        );
        assert_eq!(
            count(&connection, "configuration_change_plan_events").await,
            1
        );
    }
}

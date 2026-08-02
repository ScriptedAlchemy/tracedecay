//! Configuration migration storage and forward-repair boundaries.

use std::collections::BTreeSet;

use super::audit::insert_audit_event_with_receipt_digest;
use super::codec::{decode_id, insert_configuration_projections};
use super::read::validate_snapshot_registry_completeness;
use super::write::insert_snapshot_entries;
use super::{
    ConfigurationAuditEvent, ConfigurationAuditEventKindV1, ConfigurationError,
    ConfigurationRegistry, ConfigurationResolutionV1, ConfigurationRevisionId,
    ConfigurationSnapshotId, ConfigurationSnapshotV1, ConfigurationValueV1, Executor,
    GlobalDbConfigurationControlStore, ManifestDigest, QueryExecutor, SettingKey, UtcMicros,
    canonical_sha256, params, registry_default_candidate,
};
use crate::configuration::migration::{
    CONFIGURATION_CONTROL_PLANE_MIGRATION_RECEIPT_NAME, ConfigurationMigrationError,
    ConfigurationMigrationQuarantineEntryV1, ConfigurationMigrationReceiptV1,
    ConfigurationMigrationStore, LegacyConfigurationSourceKindV1,
};

pub(super) fn source_kind_name(source_kind: LegacyConfigurationSourceKindV1) -> &'static str {
    source_kind.as_str()
}

pub(super) async fn commit_initial_migration_transaction(
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

pub(super) async fn migration_receipt_exists(
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

pub(super) async fn migration_receipt_from_transaction(
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

/// Repairs only the exact profile shape stored before activation gained an
/// accepted-profile digest. The historical revision retains that selection;
/// the forward child keeps download/resource intent but disables semantic
/// influence until a newly evaluated profile can mint a current receipt.
pub(super) fn repair_pre_digest_semantic_configuration(
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

pub(super) fn is_pre_digest_semantic_profile(value: &serde_json::Value) -> bool {
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

pub(super) fn complete_snapshot_for_current_registry(
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

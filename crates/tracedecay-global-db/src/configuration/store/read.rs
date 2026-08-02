use std::collections::{BTreeMap, BTreeSet};

use super::codec::{
    CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION, StoredConfigurationSnapshotEntryV1,
    StoredRevisionMetadata, decode_id, decode_plan_row,
};
use super::{
    ActorId, ChangePlanId, ConfigurationProtectedPlanRecordV1, ConfigurationRegistry,
    ConfigurationRevisionId, ConfigurationRevisionRecordV1, ConfigurationSnapshotV1,
    ConfigurationStoreError, ConfigurationStoreResult, QueryExecutor, Row, SettingKey, UtcMicros,
    invalid_store_data, params, unavailable_store,
};
fn decode_snapshot_entry(
    value: &str,
) -> ConfigurationStoreResult<StoredConfigurationSnapshotEntryV1> {
    let entry =
        serde_json::from_str::<StoredConfigurationSnapshotEntryV1>(value).map_err(|error| {
            invalid_store_data(format!("decode configuration snapshot entry: {error}"))
        })?;
    if entry.schema_version != CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION {
        return Err(invalid_store_data(
            "unsupported configuration snapshot entry payload schema version",
        ));
    }
    Ok(entry)
}

fn snapshot_from_entries(
    entries: Vec<(String, i64, String)>,
    expected_snapshot_id: &str,
    expected_behavior_digest: &str,
    expected_provenance_digest: &str,
) -> ConfigurationStoreResult<ConfigurationSnapshotV1> {
    let mut effective_values = BTreeMap::new();
    let mut provenance = BTreeMap::new();

    for (stored_key, schema_revision, encoded_entry) in entries {
        if schema_revision != i64::from(CONFIGURATION_SNAPSHOT_ENTRY_PAYLOAD_SCHEMA_VERSION) {
            return Err(invalid_store_data(
                "unsupported configuration entry schema revision",
            ));
        }
        let key = SettingKey::new(stored_key).map_err(|error| {
            invalid_store_data(format!("invalid stored configuration key: {error}"))
        })?;
        let entry = decode_snapshot_entry(&encoded_entry)?;
        if entry.value.is_none() && entry.provenance.is_empty() {
            return Err(invalid_store_data(
                "configuration snapshot entry has neither value nor provenance",
            ));
        }
        if effective_values.contains_key(&key) || provenance.contains_key(&key) {
            return Err(invalid_store_data(
                "configuration snapshot contains duplicate setting entries",
            ));
        }
        if let Some(value) = entry.value {
            effective_values.insert(key.clone(), value);
        }
        if !entry.provenance.is_empty() {
            provenance.insert(key, entry.provenance);
        }
    }

    let snapshot = ConfigurationSnapshotV1::new(effective_values, provenance)
        .map_err(ConfigurationStoreError::from)?;
    if snapshot.snapshot_id.as_str() != expected_snapshot_id
        || snapshot.effective_behavior_digest.as_str() != expected_behavior_digest
        || snapshot.resolution_provenance_digest.as_str() != expected_provenance_digest
    {
        return Err(invalid_store_data(
            "stored configuration snapshot payload does not match revision metadata",
        ));
    }
    Ok(snapshot)
}

pub(super) fn validate_snapshot_registry_completeness(
    snapshot: &ConfigurationSnapshotV1,
) -> ConfigurationStoreResult<()> {
    let registry = ConfigurationRegistry::core()
        .map_err(|error| invalid_store_data(format!("load configuration registry: {error}")))?;
    let expected = registry
        .definitions()
        .map(|definition| definition.key.clone())
        .collect::<BTreeSet<_>>();
    let actual = snapshot
        .effective_values
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(invalid_store_data(
            "configuration snapshot does not contain the complete registry",
        ));
    }
    for (key, value) in &snapshot.effective_values {
        registry.validate_value(key, value).map_err(|error| {
            invalid_store_data(format!("validate configuration value: {error}"))
        })?;
    }
    Ok(())
}

pub(super) async fn snapshot_from_executor(
    executor: &impl QueryExecutor,
    revision_id: &ConfigurationRevisionId,
    expected_snapshot_id: &str,
    expected_behavior_digest: &str,
    expected_provenance_digest: &str,
) -> ConfigurationStoreResult<ConfigurationSnapshotV1> {
    let mut rows = executor
        .query(
            "SELECT key, schema_revision, typed_value
             FROM configuration_entries
             WHERE revision_id = ?1
             ORDER BY key ASC",
            params![revision_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        entries.push((
            row.get::<String>(0).map_err(|error| {
                invalid_store_data(format!("read configuration entry key: {error}"))
            })?,
            row.get::<i64>(1).map_err(|error| {
                invalid_store_data(format!("read configuration entry schema revision: {error}"))
            })?,
            row.get::<String>(2).map_err(|error| {
                invalid_store_data(format!("read configuration entry payload: {error}"))
            })?,
        ));
    }
    drop(rows);
    snapshot_from_entries(
        entries,
        expected_snapshot_id,
        expected_behavior_digest,
        expected_provenance_digest,
    )
}

fn decode_revision_metadata(row: &Row) -> ConfigurationStoreResult<StoredRevisionMetadata> {
    Ok(StoredRevisionMetadata {
        revision_id: row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read configuration revision id: {error}"))
        })?,
        parent_revision_id: row.get::<Option<String>>(1).map_err(|error| {
            invalid_store_data(format!("read configuration parent revision id: {error}"))
        })?,
        snapshot_id: row.get::<String>(2).map_err(|error| {
            invalid_store_data(format!("read configuration snapshot id: {error}"))
        })?,
        effective_behavior_digest: row.get::<String>(3).map_err(|error| {
            invalid_store_data(format!("read configuration behavior digest: {error}"))
        })?,
        resolution_provenance_digest: row.get::<String>(4).map_err(|error| {
            invalid_store_data(format!("read configuration provenance digest: {error}"))
        })?,
        actor_id: row
            .get::<String>(5)
            .map_err(|error| invalid_store_data(format!("read configuration actor id: {error}")))?,
        operation_kind: row.get::<String>(6).map_err(|error| {
            invalid_store_data(format!("read configuration operation kind: {error}"))
        })?,
        created_at: row.get::<i64>(7).map_err(|error| {
            invalid_store_data(format!("read configuration creation time: {error}"))
        })?,
    })
}

fn revision_from_metadata(
    metadata: StoredRevisionMetadata,
    snapshot: ConfigurationSnapshotV1,
) -> ConfigurationStoreResult<ConfigurationRevisionRecordV1> {
    let revision_id: ConfigurationRevisionId = decode_id(metadata.revision_id, "revision id")?;
    let parent_revision_id: Option<ConfigurationRevisionId> = metadata
        .parent_revision_id
        .map(|value| decode_id(value, "parent revision id"))
        .transpose()?;
    let actor_id: ActorId = decode_id(metadata.actor_id, "actor id")?;
    let record = ConfigurationRevisionRecordV1 {
        revision_id,
        parent_revision_id,
        snapshot,
        actor_id,
        operation_kind: metadata.operation_kind,
        created_at: UtcMicros(metadata.created_at),
    };
    record.validate().map_err(ConfigurationStoreError::from)?;
    Ok(record)
}

pub(super) async fn read_revision_from_executor(
    executor: &impl QueryExecutor,
    revision_id: &ConfigurationRevisionId,
) -> ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>> {
    let mut rows = executor
        .query(
            "SELECT revision_id, parent_revision_id, snapshot_id,
                    effective_behavior_digest, resolution_provenance_digest,
                    actor_id, operation_kind, created_at
             FROM configuration_revisions
             WHERE revision_id = ?1",
            params![revision_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Ok(None);
    };
    let metadata = decode_revision_metadata(&row)?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration revision id resolved to multiple rows",
        ));
    }
    drop(rows);
    let snapshot = snapshot_from_executor(
        executor,
        revision_id,
        &metadata.snapshot_id,
        &metadata.effective_behavior_digest,
        &metadata.resolution_provenance_digest,
    )
    .await?;
    Ok(Some(revision_from_metadata(metadata, snapshot)?))
}

pub(super) async fn read_change_plan_from_executor(
    executor: &impl QueryExecutor,
    plan_id: &ChangePlanId,
) -> ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>> {
    let mut rows = executor
        .query(
            "SELECT p.plan_id, p.actor_id, p.base_revision_id, p.operation_digest,
                    p.resolved_scope_digest, p.membership_digest,
                    p.authorization_policy_digest, p.policy_epoch, p.expires_at, p.created_at,
                    o.sequence, o.payload_schema_revision, o.sealed_typed_operation,
                    o.operation_digest
             FROM configuration_change_plans p
             LEFT JOIN configuration_change_plan_operations o ON o.plan_id = p.plan_id
             WHERE p.plan_id = ?1
             ORDER BY o.sequence ASC",
            params![plan_id.as_str()],
        )
        .await
        .map_err(unavailable_store)?;
    let mut plans = Vec::new();
    while let Some(row) = rows.next().await.map_err(unavailable_store)? {
        plans.push(decode_plan_row(&row)?);
    }
    if plans.len() > 1 {
        return Err(invalid_store_data(
            "configuration plan has multiple operation payloads unsupported by this contract",
        ));
    }
    Ok(plans.pop())
}

pub(super) async fn current_revision_id_from_executor(
    executor: &impl QueryExecutor,
) -> ConfigurationStoreResult<ConfigurationRevisionId> {
    let mut rows = executor
        .query(
            "SELECT revision_id
             FROM configuration_revisions AS candidate
             WHERE NOT EXISTS (
                 SELECT 1
                 FROM configuration_revisions AS child
                 WHERE child.parent_revision_id = candidate.revision_id
             )
             ORDER BY created_at ASC, revision_id ASC",
            (),
        )
        .await
        .map_err(unavailable_store)?;
    let Some(row) = rows.next().await.map_err(unavailable_store)? else {
        return Err(invalid_store_data(
            "configuration store has no current revision",
        ));
    };
    let revision_id: ConfigurationRevisionId = decode_id(
        row.get::<String>(0).map_err(|error| {
            invalid_store_data(format!("read current configuration revision: {error}"))
        })?,
        "current revision id",
    )?;
    if rows.next().await.map_err(unavailable_store)?.is_some() {
        return Err(invalid_store_data(
            "configuration revision history has multiple current leaves",
        ));
    }
    Ok(revision_id)
}

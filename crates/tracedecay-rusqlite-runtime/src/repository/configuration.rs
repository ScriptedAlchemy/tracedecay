use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{
    CandidateDispositionV1, ConfigurationAuditEventKindV1, ConfigurationCandidateV1,
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationSnapshotV1, ConfigurationValueV1,
    SettingKey,
};
use tracedecay_domain::{ActorId, UtcMicros};
use tracedecay_store::{
    ConfigurationCommitV1, ConfigurationRevisionRecordV1, ProfileReadOperationV1,
    ProfileReadResultV1,
};

use super::support::{conversion, encode, invalid};

const SNAPSHOT_ENTRY_SCHEMA_VERSION: u16 = 1;
const AUDIT_PAYLOAD_SCHEMA_VERSION: u16 = 1;
const AUTHORIZATION_NOT_RECORDED: &str = "not_recorded_by_configuration_store_v1";
const ACTIVATION_NOT_RECORDED: &str = "not_recorded_by_configuration_store_v1";

#[derive(Clone, Default)]
pub struct ConfigurationExecutor;

impl ConfigurationExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        commit: &ConfigurationCommitV1,
    ) -> rusqlite::Result<()> {
        commit.validate().map_err(invalid)?;
        if commit.next_revision.parent_revision_id.as_ref()
            != Some(&commit.expected_base_revision_id)
        {
            return Err(invalid(
                "configuration revision does not name the expected base revision",
            ));
        }
        let current = current_revision_id(savepoint)?;
        if current.as_deref() != Some(commit.expected_base_revision_id.as_str()) {
            return Err(invalid("configuration revision conflict"));
        }

        insert_revision(savepoint, &commit.next_revision)?;
        insert_receipt(savepoint, commit)?;
        if let Some(plan) = &commit.change_plan {
            let event_kind = match commit.audit_event.event_kind {
                ConfigurationAuditEventKindV1::Applied => "applied",
                ConfigurationAuditEventKindV1::RollbackApplied => "rollback_applied",
                _ => {
                    return Err(invalid(
                        "configuration plan commit requires a terminal audit event",
                    ));
                }
            };
            let next_sequence: i64 = savepoint.query_row(
                "SELECT COALESCE(MAX(sequence), -1) + 1
                 FROM configuration_change_plan_events
                 WHERE plan_id = ?1",
                [plan.plan_id.as_str()],
                |row| row.get(0),
            )?;
            savepoint.execute(
                "INSERT INTO configuration_change_plan_events (
                    plan_id, sequence, event_kind, safe_reason_code, occurred_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    plan.plan_id.as_str(),
                    next_sequence,
                    event_kind,
                    commit.audit_event.safe_reason_code.as_deref(),
                    commit.audit_event.occurred_at.0,
                ],
            )?;
        }
        insert_audit_event(savepoint, commit)?;
        Ok(())
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &ProfileReadOperationV1,
    ) -> rusqlite::Result<ProfileReadResultV1> {
        let revision_id = match operation {
            ProfileReadOperationV1::CurrentConfiguration => current_revision_id(snapshot)?
                .map(ConfigurationRevisionId::new)
                .transpose()
                .map_err(invalid)?,
            ProfileReadOperationV1::ConfigurationRevision(revision_id) => Some(revision_id.clone()),
        };
        let revision = revision_id
            .as_ref()
            .map(|revision_id| read_revision(snapshot, revision_id))
            .transpose()?
            .flatten();
        Ok(ProfileReadResultV1::ConfigurationRevision(
            revision.map(Box::new),
        ))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSnapshotEntryV1 {
    schema_version: u16,
    value: Option<ConfigurationValueV1>,
    provenance: Vec<ConfigurationCandidateV1>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StoredAuditPayloadV1<'a> {
    schema_version: u16,
    event: &'a tracedecay_domain::configuration::ConfigurationAuditEvent,
}

fn current_revision_id(connection: &rusqlite::Connection) -> rusqlite::Result<Option<String>> {
    let mut statement = connection.prepare(
        "SELECT revision_id
         FROM configuration_revisions AS candidate
         WHERE NOT EXISTS (
             SELECT 1 FROM configuration_revisions AS child
             WHERE child.parent_revision_id = candidate.revision_id
         )
         ORDER BY created_at, revision_id",
    )?;
    let revisions = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match revisions.as_slice() {
        [] => Ok(None),
        [revision] => Ok(Some(revision.clone())),
        _ => Err(conversion(
            "configuration revision history has multiple current leaves",
        )),
    }
}

fn insert_revision(
    savepoint: &Savepoint<'_>,
    revision: &ConfigurationRevisionRecordV1,
) -> rusqlite::Result<()> {
    revision.validate().map_err(invalid)?;
    savepoint.execute(
        "INSERT INTO configuration_revisions (
            revision_id, parent_revision_id, snapshot_id,
            effective_behavior_digest, resolution_provenance_digest,
            actor_id, operation_kind, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            revision.revision_id.as_str(),
            revision
                .parent_revision_id
                .as_ref()
                .map(ConfigurationRevisionId::as_str),
            revision.snapshot.snapshot_id.as_str(),
            revision.snapshot.effective_behavior_digest.as_str(),
            revision.snapshot.resolution_provenance_digest.as_str(),
            revision.actor_id.as_str(),
            revision.operation_kind,
            revision.created_at.0,
        ],
    )?;

    let keys = revision
        .snapshot
        .effective_values
        .keys()
        .chain(revision.snapshot.provenance.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in keys {
        let value = revision.snapshot.effective_values.get(&key).cloned();
        let provenance = revision
            .snapshot
            .provenance
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let (layer_kind, layer_id) = snapshot_layer(&provenance);
        let payload = encode(&StoredSnapshotEntryV1 {
            schema_version: SNAPSHOT_ENTRY_SCHEMA_VERSION,
            value,
            provenance,
        })?;
        savepoint.execute(
            "INSERT INTO configuration_entries (
                revision_id, key, layer_kind, layer_id, schema_revision, typed_value
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision.revision_id.as_str(),
                key.as_str(),
                layer_kind,
                layer_id.as_deref(),
                i64::from(SNAPSHOT_ENTRY_SCHEMA_VERSION),
                payload,
            ],
        )?;
    }
    Ok(())
}

fn snapshot_layer(provenance: &[ConfigurationCandidateV1]) -> (&'static str, Option<String>) {
    let layer = provenance
        .iter()
        .find(|candidate| {
            matches!(
                candidate.disposition,
                CandidateDispositionV1::Winning | CandidateDispositionV1::Defaulted
            )
        })
        .or_else(|| provenance.first())
        .map(|candidate| &candidate.layer);
    match layer {
        Some(ConfigurationLayerIdV1::UserProfile { profile_id }) => {
            ("user_profile", Some(profile_id.as_str().to_owned()))
        }
        Some(ConfigurationLayerIdV1::Project { project_id }) => {
            ("project", Some(project_id.as_str().to_owned()))
        }
        Some(ConfigurationLayerIdV1::Collection { collection_id }) => {
            ("collection", Some(collection_id.as_str().to_owned()))
        }
        Some(ConfigurationLayerIdV1::Default) | None => ("default", None),
    }
}

fn insert_receipt(
    savepoint: &Savepoint<'_>,
    commit: &ConfigurationCommitV1,
) -> rusqlite::Result<()> {
    let authorization = commit
        .change_plan
        .as_ref()
        .map(|plan| plan.authorization_policy_digest.as_str())
        .unwrap_or(AUTHORIZATION_NOT_RECORDED);
    savepoint.execute(
        "INSERT INTO configuration_mutation_receipts (
            receipt_id, plan_id, actor_id, idempotency_key,
            base_revision_id, result_revision_id, operation_digest,
            authorization_policy_digest, activation_status, receipt_digest, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            commit.receipt.receipt_id.as_str(),
            commit
                .change_plan
                .as_ref()
                .map(|plan| plan.plan_id.as_str()),
            commit.receipt.actor_id.as_str(),
            commit.receipt.idempotency_key.as_str(),
            commit.receipt.base_revision_id.as_str(),
            commit.receipt.result_revision_id.as_str(),
            commit.receipt.operation_digest.as_str(),
            authorization,
            ACTIVATION_NOT_RECORDED,
            commit.receipt.receipt_digest.as_str(),
            commit.receipt.created_at.0,
        ],
    )?;
    Ok(())
}

fn insert_audit_event(
    savepoint: &Savepoint<'_>,
    commit: &ConfigurationCommitV1,
) -> rusqlite::Result<()> {
    let event = &commit.audit_event;
    let payload = encode(&StoredAuditPayloadV1 {
        schema_version: AUDIT_PAYLOAD_SCHEMA_VERSION,
        event,
    })?;
    savepoint.execute(
        "INSERT INTO configuration_audit_events (
            event_id, actor_id, idempotency_key, operation_kind,
            base_revision_id, result_revision_id, sealed_target_reference,
            event_scoped_target_commitment, receipt_digest, correlation_id,
            safe_reason_code, occurred_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, NULL, ?9, ?10)",
        params![
            event.event_id.as_str(),
            event.actor_id.as_str(),
            event.idempotency_key.as_ref().map(|key| key.as_str()),
            payload,
            event.base_revision_id.as_str(),
            event
                .result_revision_id
                .as_ref()
                .map(|revision| revision.as_str()),
            event.target_commitment.as_str(),
            commit.receipt.receipt_digest.as_str(),
            event.safe_reason_code.as_deref(),
            event.occurred_at.0,
        ],
    )?;
    Ok(())
}

fn read_revision(
    connection: &rusqlite::Connection,
    revision_id: &ConfigurationRevisionId,
) -> rusqlite::Result<Option<ConfigurationRevisionRecordV1>> {
    let metadata = connection
        .query_row(
            "SELECT parent_revision_id, snapshot_id, effective_behavior_digest,
                    resolution_provenance_digest, actor_id, operation_kind, created_at
             FROM configuration_revisions WHERE revision_id = ?1",
            [revision_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((
        parent_revision_id,
        snapshot_id,
        behavior_digest,
        provenance_digest,
        actor_id,
        operation_kind,
        created_at,
    )) = metadata
    else {
        return Ok(None);
    };

    let mut statement = connection.prepare(
        "SELECT key, schema_revision, typed_value
         FROM configuration_entries
         WHERE revision_id = ?1
         ORDER BY key, layer_kind, layer_id",
    )?;
    let rows = statement
        .query_map([revision_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut effective_values = BTreeMap::new();
    let mut provenance = BTreeMap::new();
    for (key, schema_revision, payload) in rows {
        if schema_revision != i64::from(SNAPSHOT_ENTRY_SCHEMA_VERSION) {
            return Err(conversion(
                "unsupported configuration snapshot entry schema",
            ));
        }
        let key = SettingKey::new(key).map_err(conversion)?;
        let entry: StoredSnapshotEntryV1 = serde_json::from_str(&payload).map_err(conversion)?;
        if entry.schema_version != SNAPSHOT_ENTRY_SCHEMA_VERSION {
            return Err(conversion(
                "unsupported configuration snapshot payload schema",
            ));
        }
        if let Some(value) = entry.value {
            effective_values.insert(key.clone(), value);
        }
        if !entry.provenance.is_empty() {
            provenance.insert(key, entry.provenance);
        }
    }
    let snapshot =
        ConfigurationSnapshotV1::new(effective_values, provenance).map_err(conversion)?;
    if snapshot.snapshot_id.as_str() != snapshot_id
        || snapshot.effective_behavior_digest.as_str() != behavior_digest
        || snapshot.resolution_provenance_digest.as_str() != provenance_digest
    {
        return Err(conversion(
            "configuration snapshot projections do not match revision metadata",
        ));
    }
    Ok(Some(ConfigurationRevisionRecordV1 {
        revision_id: revision_id.clone(),
        parent_revision_id: parent_revision_id
            .map(ConfigurationRevisionId::new)
            .transpose()
            .map_err(conversion)?,
        snapshot,
        actor_id: ActorId::new(actor_id).map_err(conversion)?,
        operation_kind,
        created_at: UtcMicros(created_at),
    }))
}

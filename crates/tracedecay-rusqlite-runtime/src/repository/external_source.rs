//! Canonical SQLite projection for owner-bound external source state.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{SourceBindingIdentityV1, SourceBindingOwnerV1, SourceDeletionSemanticsV1};
use tracedecay_store::{
    ExternalSourceReadOperationV1, ExternalSourceReadResultV1, SourceAcquisitionQueueCasV1,
    SourceAcquisitionQueueStateV1, SourceAuthorityPublicationReceiptV1,
    SourceAuthorityPublicationV1, SourceCommitApplyOutcomeV1, SourceCommitReceiptV1,
    SourceCommitV1, SourceObjectMutationV1, SourcePendingProjectionV1,
    SourceProjectionApplyOutcomeV1, SourceProjectionCommitV1, SourceStoreStateV1,
    apply_source_authority_publication_owned, apply_source_commit_owned,
    apply_source_projection_owned, build_source_projection,
};

use super::support::{decode, encode, invalid};

// Immutable histories stay append-only until the canonical retention policy
// explicitly covers external-source receipts. Current-state reads and writes
// use only primary-key/index probes and normalized current rows.
pub const EXTERNAL_SOURCE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS external_source_states_v1 (
    binding_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('project', 'profile')),
    owner_id TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    definition_digest TEXT NOT NULL,
    binding_revision INTEGER NOT NULL CHECK (binding_revision > 0),
    binding_digest TEXT NOT NULL,
    source_frontier_digest TEXT NOT NULL,
    source_frontier_json TEXT NOT NULL,
    projection_frontier_digest TEXT,
    latest_source_receipt_digest TEXT NOT NULL,
    latest_projection_receipt_digest TEXT
);
CREATE INDEX IF NOT EXISTS idx_external_source_states_owner_v1
    ON external_source_states_v1(owner_kind, owner_id, source_id);
CREATE TABLE IF NOT EXISTS external_source_definition_revisions_v1 (
    source_id TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    definition_digest TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    PRIMARY KEY (source_id, definition_revision)
);
CREATE TABLE IF NOT EXISTS external_source_binding_revisions_v1 (
    binding_id TEXT NOT NULL,
    binding_revision INTEGER NOT NULL CHECK (binding_revision > 0),
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    binding_digest TEXT NOT NULL,
    binding_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, binding_revision)
);
CREATE TABLE IF NOT EXISTS external_source_authority_receipts_v1 (
    binding_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    definition_digest TEXT NOT NULL,
    binding_digest TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, idempotency_key)
);
CREATE TABLE IF NOT EXISTS external_source_commit_receipts_v1 (
    binding_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    binding_revision INTEGER NOT NULL CHECK (binding_revision > 0),
    predecessor_frontier_digest TEXT NOT NULL,
    successor_frontier_digest TEXT NOT NULL,
    receipt_digest TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, idempotency_key),
    UNIQUE (binding_id, receipt_digest),
    UNIQUE (binding_id, successor_frontier_digest)
);
CREATE TABLE IF NOT EXISTS external_source_mutations_v1 (
    binding_id TEXT NOT NULL,
    mutation_digest TEXT NOT NULL,
    native_object_digest TEXT NOT NULL,
    revision_digest TEXT NOT NULL,
    source_receipt_digest TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, mutation_digest),
    UNIQUE (binding_id, native_object_digest, revision_digest)
);
CREATE TABLE IF NOT EXISTS external_source_lineage_v1 (
    binding_id TEXT NOT NULL,
    lineage_digest TEXT NOT NULL,
    source_receipt_digest TEXT NOT NULL,
    lineage_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, lineage_digest)
);
CREATE TABLE IF NOT EXISTS external_source_objects_v1 (
    binding_id TEXT NOT NULL,
    native_object_digest TEXT NOT NULL,
    partition_digest TEXT NOT NULL,
    mutation_digest TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, native_object_digest)
);
CREATE TABLE IF NOT EXISTS external_source_pending_projections_v1 (
    binding_id TEXT NOT NULL,
    predecessor_frontier_digest TEXT NOT NULL,
    successor_frontier_digest TEXT NOT NULL,
    successor_sequence INTEGER NOT NULL CHECK (successor_sequence > 0),
    source_receipt_digest TEXT NOT NULL,
    PRIMARY KEY (binding_id, predecessor_frontier_digest),
    UNIQUE (binding_id, successor_frontier_digest),
    UNIQUE (binding_id, source_receipt_digest)
);
CREATE TABLE IF NOT EXISTS external_source_projection_publications_v1 (
    binding_id TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    source_receipt_digest TEXT NOT NULL,
    predecessor_frontier_digest TEXT NOT NULL,
    successor_frontier_digest TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, projection_digest),
    UNIQUE (binding_id, source_receipt_digest),
    UNIQUE (binding_id, successor_frontier_digest)
);
CREATE TABLE IF NOT EXISTS external_source_projection_effects_v1 (
    binding_id TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    effect_index INTEGER NOT NULL CHECK (effect_index >= 0),
    native_object_digest TEXT NOT NULL,
    effect_json TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, projection_digest, effect_index)
);
CREATE TABLE IF NOT EXISTS external_source_projection_lineage_v1 (
    binding_id TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    lineage_index INTEGER NOT NULL CHECK (lineage_index >= 0),
    lineage_digest TEXT NOT NULL,
    lineage_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, projection_digest, lineage_index)
);
CREATE TABLE IF NOT EXISTS external_source_projected_objects_v1 (
    binding_id TEXT NOT NULL,
    native_object_digest TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, native_object_digest)
);
CREATE TABLE IF NOT EXISTS external_source_acquisition_queue_v1 (
    binding_id TEXT PRIMARY KEY,
    state_digest TEXT NOT NULL,
    not_before_micros INTEGER,
    state_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_external_source_acquisition_ready_v1
    ON external_source_acquisition_queue_v1(not_before_micros, binding_id)
    WHERE not_before_micros IS NOT NULL;
";

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceStateMarker {
    data_version: i64,
    definition_revision: i64,
    definition_digest: String,
    binding_revision: i64,
    binding_digest: String,
    source_frontier_digest: String,
    projection_frontier_digest: Option<String>,
    latest_source_receipt_digest: String,
    latest_projection_receipt_digest: Option<String>,
}

struct CachedSourceState {
    marker: SourceStateMarker,
    state: SourceStoreStateV1,
}

#[derive(Default)]
pub struct ExternalSourceExecutor {
    verified_states: BTreeMap<String, CachedSourceState>,
}

impl Clone for ExternalSourceExecutor {
    /// Executor clones may be mounted on another SQLite connection. Verified
    /// state is connection-local provenance and must reload there.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl ExternalSourceExecutor {
    #[hotpath::measure(label = "rusqlite.external_source.execute_write")]
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        commit: &SourceCommitV1,
    ) -> rusqlite::Result<()> {
        commit.validate().map_err(invalid)?;
        let binding = commit.binding().immutable_identity().map_err(invalid)?;
        if let Some(receipt) =
            load_commit_receipt_by_idempotency(savepoint, &binding, commit.idempotency_key())?
        {
            return if receipt.request_digest() == commit.request_digest() {
                Ok(())
            } else {
                Err(invalid(
                    "external source idempotency key collides with another request",
                ))
            };
        }
        let current = self.take_verified_state(savepoint, &binding)?;
        let mutation_encodings = validate_revision_collisions(savepoint, &binding, commit)?;
        match apply_source_commit_owned(current, commit.clone()).map_err(invalid)? {
            SourceCommitApplyOutcomeV1::ExactDuplicate(_) => Ok(()),
            SourceCommitApplyOutcomeV1::Committed(state) => {
                let state = *state;
                persist_source_commit(savepoint, &state, state.receipt(), mutation_encodings)?;
                self.cache_verified_state(savepoint, state)
            }
        }
    }

    #[hotpath::measure(label = "rusqlite.external_source.execute_authority_publication")]
    pub fn execute_authority_publication(
        &mut self,
        savepoint: &Savepoint<'_>,
        publication: &SourceAuthorityPublicationV1,
    ) -> rusqlite::Result<()> {
        publication.validate().map_err(invalid)?;
        let binding = publication
            .binding()
            .immutable_identity()
            .map_err(invalid)?;
        if let Some(receipt) =
            load_authority_receipt(savepoint, &binding, publication.idempotency_key())?
        {
            return if receipt.request_digest() == publication.request_digest() {
                Ok(())
            } else {
                Err(invalid(
                    "external source authority idempotency key collision",
                ))
            };
        }
        let current = self
            .take_verified_state(savepoint, &binding)?
            .ok_or_else(|| invalid("external source authority publication has no source state"))?;
        let outcome = apply_source_authority_publication_owned(current, publication.clone())
            .map_err(invalid)?;
        let (revised, receipt) = outcome.into_parts();
        let revised = *revised;
        persist_authority_publication(savepoint, &revised, &receipt)?;
        self.cache_verified_state(savepoint, revised)
    }

    #[hotpath::measure(label = "rusqlite.external_source.execute_projection_write")]
    pub fn execute_projection_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        projection: &SourceProjectionCommitV1,
    ) -> rusqlite::Result<()> {
        projection.validate().map_err(invalid)?;
        let binding = projection.source_frontier().binding();
        if let Some(existing) =
            load_projection_receipt(savepoint, binding, projection.receipt_digest())?
        {
            return if &existing == projection {
                Ok(())
            } else {
                Err(invalid("external source projection digest collision"))
            };
        }
        let current = self
            .take_verified_state(savepoint, binding)?
            .ok_or_else(|| invalid("external source projection has no committed source state"))?;
        let pending = load_next_pending_projection(savepoint, binding)?.ok_or_else(|| {
            invalid("external source projection has no exact pending predecessor")
        })?;
        let expected =
            build_source_projection(&pending, projection.projector().clone()).map_err(invalid)?;
        if &expected != projection {
            return Err(invalid(
                "external source projection does not match the oldest pending receipt",
            ));
        }
        match apply_source_projection_owned(current, &pending, projection.clone())
            .map_err(invalid)?
        {
            SourceProjectionApplyOutcomeV1::ExactDuplicate(_) => Ok(()),
            SourceProjectionApplyOutcomeV1::Projected(state) => {
                let state = *state;
                persist_projection(savepoint, &state, pending.receipt(), projection)?;
                self.cache_verified_state(savepoint, state)
            }
        }
    }

    #[hotpath::measure(label = "rusqlite.external_source.execute_acquisition_cas")]
    pub fn execute_acquisition_state_cas(
        &mut self,
        savepoint: &Savepoint<'_>,
        command: &SourceAcquisitionQueueCasV1,
    ) -> rusqlite::Result<()> {
        command.validate().map_err(invalid)?;
        let current_digest = savepoint
            .prepare_cached(
                "SELECT state_digest
                 FROM external_source_acquisition_queue_v1
                 WHERE binding_id = ?1",
            )?
            .query_row(params![command.binding().binding_id.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        if current_digest.as_deref()
            != command
                .expected_state_digest()
                .map(tracedecay_domain::ManifestDigest::as_str)
        {
            return Err(invalid(
                "external source acquisition queue compare-and-swap conflict",
            ));
        }
        let not_before_micros = command
            .next()
            .active()
            .map(|scheduled| scheduled.not_before().0);
        savepoint.execute(
            "INSERT INTO external_source_acquisition_queue_v1 (
                 binding_id, state_digest, not_before_micros, state_json
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(binding_id) DO UPDATE SET
                 state_digest = excluded.state_digest,
                 not_before_micros = excluded.not_before_micros,
                 state_json = excluded.state_json",
            params![
                command.binding().binding_id.as_str(),
                command.next().state_digest().as_str(),
                not_before_micros,
                encode(command.next())?,
            ],
        )?;
        Ok(())
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &ExternalSourceReadOperationV1,
    ) -> rusqlite::Result<ExternalSourceReadResultV1> {
        match operation {
            ExternalSourceReadOperationV1::State { binding } => {
                binding.validate().map_err(invalid)?;
                load_state(snapshot, binding)
                    .map(|state| ExternalSourceReadResultV1::State(state.map(Box::new)))
            }
            ExternalSourceReadOperationV1::CommitReceipt {
                binding,
                idempotency_key,
            } => {
                binding.validate().map_err(invalid)?;
                idempotency_key.validate().map_err(invalid)?;
                load_commit_receipt_by_idempotency(snapshot, binding, idempotency_key)
                    .map(|receipt| ExternalSourceReadResultV1::CommitReceipt(receipt.map(Box::new)))
            }
            ExternalSourceReadOperationV1::NextPendingProjection { binding } => {
                let pending = match binding {
                    Some(binding) => {
                        binding.validate().map_err(invalid)?;
                        load_next_pending_projection(snapshot, binding)?
                    }
                    None => load_next_pending_projection_any(snapshot)?,
                };
                Ok(ExternalSourceReadResultV1::PendingProjection(
                    pending.map(Box::new),
                ))
            }
            ExternalSourceReadOperationV1::AcquisitionState { binding } => {
                binding.validate().map_err(invalid)?;
                load_acquisition_state(snapshot, binding)
                    .map(|state| ExternalSourceReadResultV1::AcquisitionState(state.map(Box::new)))
            }
            ExternalSourceReadOperationV1::NextReadyAcquisition { now } => {
                load_next_ready_acquisition(snapshot, *now)
                    .map(|state| ExternalSourceReadResultV1::AcquisitionState(state.map(Box::new)))
            }
            ExternalSourceReadOperationV1::AcquisitionPendingCount => snapshot
                .query_row(
                    "SELECT COUNT(*)
                     FROM external_source_acquisition_queue_v1
                    WHERE not_before_micros IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .and_then(|count| {
                    u64::try_from(count)
                        .map_err(|_| invalid("external source acquisition count is negative"))
                })
                .map(ExternalSourceReadResultV1::AcquisitionPendingCount),
        }
    }

    #[hotpath::measure(label = "rusqlite.external_source.cache.take_verified")]
    fn take_verified_state(
        &mut self,
        connection: &rusqlite::Connection,
        binding: &SourceBindingIdentityV1,
    ) -> rusqlite::Result<Option<SourceStoreStateV1>> {
        let marker = load_state_marker(connection, binding)?;
        match self.verified_states.remove(binding.binding_id.as_str()) {
            Some(cached) if marker.as_ref() == Some(&cached.marker) => Ok(Some(cached.state)),
            Some(_) | None => load_state(connection, binding),
        }
    }

    #[hotpath::measure(label = "rusqlite.external_source.cache.store_verified")]
    fn cache_verified_state(
        &mut self,
        connection: &rusqlite::Connection,
        state: SourceStoreStateV1,
    ) -> rusqlite::Result<()> {
        let binding = state.binding().immutable_identity().map_err(invalid)?;
        let marker = load_state_marker(connection, &binding)?
            .ok_or_else(|| invalid("external source persisted state marker is missing"))?;
        self.verified_states.insert(
            binding.binding_id.as_str().to_owned(),
            CachedSourceState { marker, state },
        );
        Ok(())
    }
}

fn load_acquisition_state(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
) -> rusqlite::Result<Option<SourceAcquisitionQueueStateV1>> {
    let state = connection
        .prepare_cached(
            "SELECT state_json
             FROM external_source_acquisition_queue_v1
             WHERE binding_id = ?1",
        )?
        .query_row(params![binding.binding_id.as_str()], |row| {
            decode::<SourceAcquisitionQueueStateV1>(row.get(0)?)
        })
        .optional()?;
    if state
        .as_ref()
        .is_some_and(|state| state.binding_identity().ok().as_ref() != Some(binding))
    {
        return Err(invalid(
            "external source acquisition queue binding identity mismatch",
        ));
    }
    state
        .as_ref()
        .map_or(Ok(()), SourceAcquisitionQueueStateV1::validate)
        .map_err(invalid)?;
    Ok(state)
}

fn load_next_ready_acquisition(
    connection: &rusqlite::Connection,
    now: tracedecay_domain::UtcMicros,
) -> rusqlite::Result<Option<SourceAcquisitionQueueStateV1>> {
    let state = connection
        .prepare_cached(
            "SELECT state_json
             FROM external_source_acquisition_queue_v1
             WHERE not_before_micros IS NOT NULL
               AND not_before_micros <= ?1
             ORDER BY not_before_micros, binding_id
             LIMIT 1",
        )?
        .query_row(params![now.0], |row| {
            decode::<SourceAcquisitionQueueStateV1>(row.get(0)?)
        })
        .optional()?;
    state
        .as_ref()
        .map_or(Ok(()), SourceAcquisitionQueueStateV1::validate)
        .map_err(invalid)?;
    Ok(state)
}

#[hotpath::measure(label = "rusqlite.external_source.load_state_marker")]
fn load_state_marker(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
) -> rusqlite::Result<Option<SourceStateMarker>> {
    let data_version =
        connection.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?;
    connection
        .prepare_cached(
            "SELECT definition_revision, definition_digest,
                    binding_revision, binding_digest,
                    source_frontier_digest, projection_frontier_digest,
                    latest_source_receipt_digest, latest_projection_receipt_digest
             FROM external_source_states_v1
             WHERE binding_id = ?1",
        )?
        .query_row(params![binding.binding_id.as_str()], |row| {
            Ok(SourceStateMarker {
                data_version,
                definition_revision: row.get(0)?,
                definition_digest: row.get(1)?,
                binding_revision: row.get(2)?,
                binding_digest: row.get(3)?,
                source_frontier_digest: row.get(4)?,
                projection_frontier_digest: row.get(5)?,
                latest_source_receipt_digest: row.get(6)?,
                latest_projection_receipt_digest: row.get(7)?,
            })
        })
        .optional()
}

#[hotpath::measure(label = "rusqlite.external_source.load_state")]
fn load_state(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
) -> rusqlite::Result<Option<SourceStoreStateV1>> {
    let row = connection
        .prepare_cached(
            "SELECT source_id, definition_revision, binding_revision,
                    source_frontier_json, latest_source_receipt_digest,
                    latest_projection_receipt_digest
             FROM external_source_states_v1
             WHERE binding_id = ?1",
        )?
        .query_row(params![binding.binding_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .optional()?;
    let Some((
        source_id,
        definition_revision,
        binding_revision,
        frontier_json,
        source_receipt_digest,
        projection_receipt_digest,
    )) = row
    else {
        return Ok(None);
    };
    let definition = load_definition(connection, &source_id, definition_revision)?;
    let stored_binding = load_binding(connection, binding.binding_id.as_str(), binding_revision)?;
    if stored_binding.immutable_identity().map_err(invalid)? != *binding {
        return Err(invalid(
            "stored external source state does not match its binding key",
        ));
    }
    let source_frontier = decode(frontier_json)?;
    let receipt = load_commit_receipt_by_digest(connection, binding, &source_receipt_digest)?
        .ok_or_else(|| invalid("external source current receipt is missing"))?;
    let projection = projection_receipt_digest
        .as_deref()
        .map(|digest| load_projection_receipt_by_digest(connection, binding, digest))
        .transpose()?
        .flatten();
    let observed = load_current_mutations(
        connection,
        "external_source_objects_v1",
        binding.binding_id.as_str(),
    )?;
    let projected = load_current_mutations(
        connection,
        "external_source_projected_objects_v1",
        binding.binding_id.as_str(),
    )?;
    let state = SourceStoreStateV1::restore(
        definition,
        stored_binding,
        source_frontier,
        projection,
        observed,
        projected,
        receipt,
    )
    .map_err(invalid)?;
    Ok(Some(state))
}

#[hotpath::measure(label = "rusqlite.external_source.load_definition")]
fn load_definition(
    connection: &rusqlite::Connection,
    source_id: &str,
    revision: i64,
) -> rusqlite::Result<tracedecay_domain::SourceDefinitionV1> {
    let encoded: String = connection.query_row(
        "SELECT definition_json
         FROM external_source_definition_revisions_v1
         WHERE source_id = ?1 AND definition_revision = ?2",
        params![source_id, revision],
        |row| row.get(0),
    )?;
    decode(encoded)
}

#[hotpath::measure(label = "rusqlite.external_source.load_binding")]
fn load_binding(
    connection: &rusqlite::Connection,
    binding_id: &str,
    revision: i64,
) -> rusqlite::Result<tracedecay_domain::SourceBindingV1> {
    let encoded: String = connection.query_row(
        "SELECT binding_json
         FROM external_source_binding_revisions_v1
         WHERE binding_id = ?1 AND binding_revision = ?2",
        params![binding_id, revision],
        |row| row.get(0),
    )?;
    decode(encoded)
}

#[hotpath::measure(label = "rusqlite.external_source.load_current_mutations")]
fn load_current_mutations(
    connection: &rusqlite::Connection,
    table: &str,
    binding_id: &str,
) -> rusqlite::Result<Vec<SourceObjectMutationV1>> {
    let sql = match table {
        "external_source_objects_v1" => {
            "SELECT mutation_json FROM external_source_objects_v1 WHERE binding_id = ?1"
        }
        "external_source_projected_objects_v1" => {
            "SELECT mutation_json FROM external_source_projected_objects_v1 WHERE binding_id = ?1"
        }
        _ => return Err(invalid("unknown external source current-object table")),
    };
    let mut statement = connection.prepare_cached(sql)?;
    statement
        .query_map([binding_id], |row| decode(row.get::<_, String>(0)?))?
        .collect()
}

const ROOT_PROJECTION_FRONTIER: &str = "root";

#[hotpath::measure(label = "rusqlite.external_source.persist_commit")]
fn persist_source_commit(
    savepoint: &Savepoint<'_>,
    state: &SourceStoreStateV1,
    receipt: &SourceCommitReceiptV1,
    mut mutation_encodings: BTreeMap<String, String>,
) -> rusqlite::Result<()> {
    state.validate().map_err(invalid)?;
    receipt.validate().map_err(invalid)?;
    let binding = state.binding().immutable_identity().map_err(invalid)?;
    persist_definition_and_binding(savepoint, state.definition(), state.binding())?;
    let predecessor = frontier_key(receipt.prior_source_frontier());
    let successor = receipt.source_frontier().digest().as_str();
    let receipt_json = encode(receipt)?;
    // `INSERT OR IGNORE` reports zero changed rows only when a conflict was
    // swallowed; only then can the stored row differ from this write, so the
    // read-back proof is needed only on that path.
    let changed = savepoint.execute(
        "INSERT OR IGNORE INTO external_source_commit_receipts_v1 (
            binding_id, idempotency_key, request_digest,
            definition_revision, binding_revision,
            predecessor_frontier_digest, successor_frontier_digest,
            receipt_digest, receipt_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            binding.binding_id.as_str(),
            receipt.idempotency_key().as_str(),
            receipt.request_digest().as_str(),
            i64::try_from(receipt.definition_revision()).map_err(|_| invalid(
                "external source definition revision exceeds SQLite INTEGER"
            ))?,
            i64::try_from(receipt.binding_revision())
                .map_err(|_| invalid("external source binding revision exceeds SQLite INTEGER"))?,
            predecessor,
            successor,
            receipt.receipt_digest().as_str(),
            receipt_json,
        ],
    )?;
    if changed == 0 {
        verify_encoded_row(
            savepoint,
            "SELECT receipt_json FROM external_source_commit_receipts_v1
             WHERE binding_id = ?1 AND idempotency_key = ?2",
            binding.binding_id.as_str(),
            receipt.idempotency_key().as_str(),
            &receipt_json,
            "external source commit receipt collision",
        )?;
    }
    for mutation in receipt.mutations() {
        // The collision validation already encoded every commit mutation; the
        // receipt carries those same mutations through, so a miss here only
        // means the encoding was not pre-computed and is re-derived.
        let mutation_json = match mutation_encodings.remove(mutation.mutation_digest().as_str()) {
            Some(encoded) => encoded,
            None => encode(mutation)?,
        };
        let native_object = mutation.observation().native_object();
        let changed = savepoint.execute(
            "INSERT OR IGNORE INTO external_source_mutations_v1 (
                binding_id, mutation_digest, native_object_digest,
                revision_digest, source_receipt_digest, mutation_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                binding.binding_id.as_str(),
                mutation.mutation_digest().as_str(),
                native_object.digest().as_str(),
                mutation.observation().revision().digest().as_str(),
                receipt.receipt_digest().as_str(),
                mutation_json,
            ],
        )?;
        if changed == 0 {
            verify_encoded_row(
                savepoint,
                "SELECT mutation_json FROM external_source_mutations_v1
                 WHERE binding_id = ?1 AND mutation_digest = ?2",
                binding.binding_id.as_str(),
                mutation.mutation_digest().as_str(),
                &mutation_json,
                "external source mutation collision",
            )?;
        }
        savepoint.execute(
            "INSERT INTO external_source_objects_v1 (
                binding_id, native_object_digest, partition_digest,
                mutation_digest, mutation_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(binding_id, native_object_digest) DO UPDATE SET
                partition_digest = excluded.partition_digest,
                mutation_digest = excluded.mutation_digest,
                mutation_json = excluded.mutation_json",
            params![
                binding.binding_id.as_str(),
                native_object.digest().as_str(),
                mutation.evidence().partition().digest().as_str(),
                mutation.mutation_digest().as_str(),
                mutation_json,
            ],
        )?;
    }
    for edge in receipt.lineage() {
        let encoded = encode(edge)?;
        let changed = savepoint.execute(
            "INSERT OR IGNORE INTO external_source_lineage_v1 (
                binding_id, lineage_digest, source_receipt_digest, lineage_json
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                binding.binding_id.as_str(),
                edge.lineage_digest().as_str(),
                receipt.receipt_digest().as_str(),
                encoded,
            ],
        )?;
        if changed == 0 {
            verify_encoded_row(
                savepoint,
                "SELECT lineage_json FROM external_source_lineage_v1
                 WHERE binding_id = ?1 AND lineage_digest = ?2",
                binding.binding_id.as_str(),
                edge.lineage_digest().as_str(),
                &encoded,
                "external source lineage collision",
            )?;
        }
    }
    let sequence = receipt
        .source_frontier()
        .partition(receipt.partition())
        .ok_or_else(|| invalid("external source receipt partition frontier is missing"))?
        .sequence();
    let pending_changed = savepoint.execute(
        "INSERT OR IGNORE INTO external_source_pending_projections_v1 (
            binding_id, predecessor_frontier_digest, successor_frontier_digest,
            successor_sequence, source_receipt_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            binding.binding_id.as_str(),
            predecessor,
            successor,
            i64::try_from(sequence)
                .map_err(|_| invalid("external source sequence exceeds SQLite INTEGER"))?,
            receipt.receipt_digest().as_str(),
        ],
    )?;
    if pending_changed == 0 {
        let pending: (String, String) = savepoint.query_row(
            "SELECT successor_frontier_digest, source_receipt_digest
             FROM external_source_pending_projections_v1
             WHERE binding_id = ?1 AND predecessor_frontier_digest = ?2",
            params![binding.binding_id.as_str(), predecessor],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if pending
            != (
                successor.to_owned(),
                receipt.receipt_digest().as_str().to_owned(),
            )
        {
            return Err(invalid("external source pending projection fork collision"));
        }
    }
    upsert_current_state(savepoint, state)
}

#[hotpath::measure(label = "rusqlite.external_source.persist_projection")]
fn persist_projection(
    savepoint: &Savepoint<'_>,
    state: &SourceStoreStateV1,
    source_receipt: &SourceCommitReceiptV1,
    projection: &SourceProjectionCommitV1,
) -> rusqlite::Result<()> {
    state.validate().map_err(invalid)?;
    projection.validate().map_err(invalid)?;
    let binding = projection.source_frontier().binding();
    let predecessor = frontier_key(projection.expected_projection_frontier());
    let encoded = encode(projection)?;
    savepoint.execute(
        "INSERT OR IGNORE INTO external_source_projection_publications_v1 (
            binding_id, projection_digest, source_receipt_digest,
            predecessor_frontier_digest, successor_frontier_digest, receipt_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            binding.binding_id.as_str(),
            projection.receipt_digest().as_str(),
            projection.source_receipt_digest().as_str(),
            predecessor,
            projection.source_frontier().digest().as_str(),
            encoded,
        ],
    )?;
    verify_encoded_row(
        savepoint,
        "SELECT receipt_json FROM external_source_projection_publications_v1
         WHERE binding_id = ?1 AND projection_digest = ?2",
        binding.binding_id.as_str(),
        projection.receipt_digest().as_str(),
        &encoded,
        "external source projection receipt collision",
    )?;
    for (index, (mutation, effect)) in projection
        .mutations()
        .iter()
        .zip(projection.effects())
        .enumerate()
    {
        let index = i64::try_from(index).map_err(|_| {
            invalid("external source projection effect index exceeds SQLite INTEGER")
        })?;
        let effect_json = encode(effect)?;
        let mutation_json = encode(mutation)?;
        savepoint.execute(
            "INSERT INTO external_source_projection_effects_v1 (
                binding_id, projection_digest, effect_index,
                native_object_digest, effect_json, mutation_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                binding.binding_id.as_str(),
                projection.receipt_digest().as_str(),
                index,
                mutation.observation().native_object().digest().as_str(),
                effect_json,
                mutation_json,
            ],
        )?;
        savepoint.execute(
            "INSERT INTO external_source_projected_objects_v1 (
                binding_id, native_object_digest, mutation_json
             ) VALUES (?1, ?2, ?3)
             ON CONFLICT(binding_id, native_object_digest) DO UPDATE SET
                mutation_json = excluded.mutation_json",
            params![
                binding.binding_id.as_str(),
                mutation.observation().native_object().digest().as_str(),
                mutation_json,
            ],
        )?;
    }
    for (index, edge) in projection.lineage().iter().enumerate() {
        savepoint.execute(
            "INSERT INTO external_source_projection_lineage_v1 (
                binding_id, projection_digest, lineage_index,
                lineage_digest, lineage_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                binding.binding_id.as_str(),
                projection.receipt_digest().as_str(),
                i64::try_from(index).map_err(|_| {
                    invalid("external source projection lineage index exceeds SQLite INTEGER")
                })?,
                edge.lineage_digest().as_str(),
                encode(edge)?,
            ],
        )?;
    }
    let deleted = savepoint.execute(
        "DELETE FROM external_source_pending_projections_v1
         WHERE binding_id = ?1
           AND predecessor_frontier_digest = ?2
           AND successor_frontier_digest = ?3
           AND source_receipt_digest = ?4",
        params![
            binding.binding_id.as_str(),
            predecessor,
            projection.source_frontier().digest().as_str(),
            source_receipt.receipt_digest().as_str(),
        ],
    )?;
    if deleted != 1 {
        return Err(invalid(
            "external source pending projection compare-and-set failed",
        ));
    }
    savepoint.execute(
        "UPDATE external_source_states_v1
         SET projection_frontier_digest = ?1,
             latest_projection_receipt_digest = ?2
         WHERE binding_id = ?3",
        params![
            projection.source_frontier().digest().as_str(),
            projection.receipt_digest().as_str(),
            binding.binding_id.as_str(),
        ],
    )?;
    Ok(())
}

#[hotpath::measure(label = "rusqlite.external_source.persist_authority")]
fn persist_authority_publication(
    savepoint: &Savepoint<'_>,
    state: &SourceStoreStateV1,
    receipt: &SourceAuthorityPublicationReceiptV1,
) -> rusqlite::Result<()> {
    state.validate().map_err(invalid)?;
    receipt.validate().map_err(invalid)?;
    let binding = state.binding().immutable_identity().map_err(invalid)?;
    persist_definition_and_binding(savepoint, state.definition(), state.binding())?;
    let encoded = encode(receipt)?;
    savepoint.execute(
        "INSERT OR IGNORE INTO external_source_authority_receipts_v1 (
            binding_id, idempotency_key, request_digest,
            definition_digest, binding_digest, receipt_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            binding.binding_id.as_str(),
            receipt.idempotency_key().as_str(),
            receipt.request_digest().as_str(),
            receipt.definition_digest().as_str(),
            receipt.binding_digest().as_str(),
            encoded,
        ],
    )?;
    verify_encoded_row(
        savepoint,
        "SELECT receipt_json FROM external_source_authority_receipts_v1
         WHERE binding_id = ?1 AND idempotency_key = ?2",
        binding.binding_id.as_str(),
        receipt.idempotency_key().as_str(),
        &encoded,
        "external source authority receipt collision",
    )?;
    let changed = savepoint.execute(
        "UPDATE external_source_states_v1
         SET definition_revision = ?1, definition_digest = ?2,
             binding_revision = ?3, binding_digest = ?4
         WHERE binding_id = ?5",
        params![
            i64::try_from(state.definition().revision).map_err(|_| invalid(
                "external source definition revision exceeds SQLite INTEGER"
            ))?,
            state.definition().definition_digest.as_str(),
            i64::try_from(state.binding().binding_revision)
                .map_err(|_| invalid("external source binding revision exceeds SQLite INTEGER"))?,
            state.binding().binding_digest.as_str(),
            binding.binding_id.as_str(),
        ],
    )?;
    if changed != 1 {
        return Err(invalid("external source authority state is missing"));
    }
    Ok(())
}

fn persist_definition_and_binding(
    savepoint: &Savepoint<'_>,
    definition: &tracedecay_domain::SourceDefinitionV1,
    binding: &tracedecay_domain::SourceBindingV1,
) -> rusqlite::Result<()> {
    let definition_revision = i64::try_from(definition.revision)
        .map_err(|_| invalid("external source definition revision exceeds SQLite INTEGER"))?;
    let definition_json = encode(definition)?;
    let changed = savepoint.execute(
        "INSERT OR IGNORE INTO external_source_definition_revisions_v1 (
            source_id, definition_revision, definition_digest, definition_json
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            definition.source_id.as_str(),
            definition_revision,
            definition.definition_digest.as_str(),
            definition_json,
        ],
    )?;
    if changed == 0 {
        verify_encoded_row(
            savepoint,
            "SELECT definition_json FROM external_source_definition_revisions_v1
             WHERE source_id = ?1 AND definition_revision = ?2",
            definition.source_id.as_str(),
            &definition_revision,
            &definition_json,
            "external source definition revision collision",
        )?;
    }
    let binding_revision = i64::try_from(binding.binding_revision)
        .map_err(|_| invalid("external source binding revision exceeds SQLite INTEGER"))?;
    let binding_json = encode(binding)?;
    let changed = savepoint.execute(
        "INSERT OR IGNORE INTO external_source_binding_revisions_v1 (
            binding_id, binding_revision, definition_revision,
            binding_digest, binding_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            binding.binding_id.as_str(),
            binding_revision,
            definition_revision,
            binding.binding_digest.as_str(),
            binding_json,
        ],
    )?;
    if changed == 0 {
        verify_encoded_row(
            savepoint,
            "SELECT binding_json FROM external_source_binding_revisions_v1
             WHERE binding_id = ?1 AND binding_revision = ?2",
            binding.binding_id.as_str(),
            &binding_revision,
            &binding_json,
            "external source binding revision collision",
        )?;
    }
    Ok(())
}

fn upsert_current_state(
    savepoint: &Savepoint<'_>,
    state: &SourceStoreStateV1,
) -> rusqlite::Result<()> {
    let binding = state.binding().immutable_identity().map_err(invalid)?;
    let (owner_kind, owner_id) = owner_key(&binding.owner);
    let projection_frontier = state
        .projection()
        .map(|projection| projection.source_frontier().digest().as_str());
    let projection_receipt = state
        .projection()
        .map(|projection| projection.receipt_digest().as_str());
    savepoint.execute(
        "INSERT INTO external_source_states_v1 (
            binding_id, source_id, owner_kind, owner_id,
            definition_revision, definition_digest,
            binding_revision, binding_digest,
            source_frontier_digest, source_frontier_json,
            projection_frontier_digest,
            latest_source_receipt_digest, latest_projection_receipt_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(binding_id) DO UPDATE SET
            source_id = excluded.source_id,
            owner_kind = excluded.owner_kind,
            owner_id = excluded.owner_id,
            definition_revision = excluded.definition_revision,
            definition_digest = excluded.definition_digest,
            binding_revision = excluded.binding_revision,
            binding_digest = excluded.binding_digest,
            source_frontier_digest = excluded.source_frontier_digest,
            source_frontier_json = excluded.source_frontier_json,
            latest_source_receipt_digest = excluded.latest_source_receipt_digest",
        params![
            binding.binding_id.as_str(),
            binding.source_id.as_str(),
            owner_kind,
            owner_id,
            i64::try_from(state.definition().revision).map_err(|_| invalid(
                "external source definition revision exceeds SQLite INTEGER"
            ))?,
            state.definition().definition_digest.as_str(),
            i64::try_from(state.binding().binding_revision)
                .map_err(|_| invalid("external source binding revision exceeds SQLite INTEGER"))?,
            state.binding().binding_digest.as_str(),
            state.source_frontier().digest().as_str(),
            encode(state.source_frontier())?,
            projection_frontier,
            state.receipt().receipt_digest().as_str(),
            projection_receipt,
        ],
    )?;
    Ok(())
}

const REVISION_COLLISION_PROBE_CHUNK: usize = 100;

/// Proves no stored mutation disagrees with this commit's encoding for the
/// same `(native object, revision)` identity, probing the unique
/// `(binding_id, native_object_digest, revision_digest)` index in batched
/// row-value `IN` chunks. Returns each mutation's encoding keyed by mutation
/// digest so the persist path reuses it instead of re-serializing.
#[hotpath::measure(label = "rusqlite.external_source.validate_revision_collisions")]
fn validate_revision_collisions(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
    commit: &SourceCommitV1,
) -> rusqlite::Result<BTreeMap<String, String>> {
    let mutations = commit.mutations();
    let mut encodings = BTreeMap::new();
    for mutation in mutations {
        encodings.insert(
            mutation.mutation_digest().as_str().to_owned(),
            encode(mutation)?,
        );
    }
    for chunk in mutations.chunks(REVISION_COLLISION_PROBE_CHUNK) {
        let mut sql = String::from(
            "SELECT native_object_digest, revision_digest, mutation_json
             FROM external_source_mutations_v1
             WHERE binding_id = ?1
               AND (native_object_digest, revision_digest) IN (VALUES ",
        );
        let mut probe_params: Vec<&str> = Vec::with_capacity(1 + chunk.len() * 2);
        probe_params.push(binding.binding_id.as_str());
        for (index, mutation) in chunk.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            let base = 2 + index * 2;
            sql.push_str(&format!("(?{base}, ?{})", base + 1));
            probe_params.push(mutation.observation().native_object().digest().as_str());
            probe_params.push(mutation.observation().revision().digest().as_str());
        }
        sql.push(')');
        let mut statement = connection.prepare(&sql)?;
        let mut stored = BTreeMap::new();
        let mut rows = statement.query(rusqlite::params_from_iter(probe_params))?;
        while let Some(row) = rows.next()? {
            stored.insert(
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            );
        }
        for mutation in chunk {
            let key = (
                mutation
                    .observation()
                    .native_object()
                    .digest()
                    .as_str()
                    .to_owned(),
                mutation
                    .observation()
                    .revision()
                    .digest()
                    .as_str()
                    .to_owned(),
            );
            let encoded = encodings.get(mutation.mutation_digest().as_str());
            if stored
                .get(&key)
                .is_some_and(|existing| Some(existing) != encoded)
            {
                return Err(invalid("external source object revision collision"));
            }
        }
    }
    Ok(encodings)
}

mod reads;
use reads::{
    load_authority_receipt, load_commit_receipt_by_digest, load_commit_receipt_by_idempotency,
    load_next_pending_projection, load_next_pending_projection_any, load_projection_receipt,
    load_projection_receipt_by_digest, verify_encoded_row,
};

fn frontier_key(frontier: Option<&tracedecay_domain::SourceAggregateFrontierV1>) -> &str {
    frontier.map_or(ROOT_PROJECTION_FRONTIER, |frontier| {
        frontier.digest().as_str()
    })
}

fn owner_key(owner: &SourceBindingOwnerV1) -> (&'static str, &str) {
    match owner {
        SourceBindingOwnerV1::Project(project_id) => ("project", project_id.as_str()),
        SourceBindingOwnerV1::Profile(profile_id) => ("profile", profile_id.as_str()),
    }
}

#[cfg(test)]
#[path = "external_source/tests.rs"]
mod tests;

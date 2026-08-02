//! Canonical SQLite projection for owner-bound external source state.

use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{SourceBindingIdentityV1, SourceBindingOwnerV1};
use tracedecay_store::{
    ExternalSourceReadOperationV1, ExternalSourceReadResultV1, SourceAuthorityPublicationV1,
    SourceCommitApplyOutcomeV1, SourceCommitV1, SourceStoreStateV1,
    apply_source_authority_publication, apply_source_commit,
};

use super::support::{decode, encode, invalid};

pub const EXTERNAL_SOURCE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS external_source_states_v1 (
    binding_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('project', 'profile')),
    owner_id TEXT NOT NULL,
    definition_digest TEXT NOT NULL,
    binding_digest TEXT NOT NULL,
    frontier_digest TEXT NOT NULL,
    receipt_idempotency_key TEXT NOT NULL,
    receipt_request_digest TEXT NOT NULL,
    state_json TEXT NOT NULL
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
CREATE TABLE IF NOT EXISTS external_source_projection_publications_v1 (
    binding_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    frontier_digest TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, idempotency_key)
);
";

#[derive(Clone, Default)]
pub struct ExternalSourceExecutor;

impl ExternalSourceExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        commit: &SourceCommitV1,
    ) -> rusqlite::Result<()> {
        commit.validate().map_err(invalid)?;
        let binding = commit.binding().immutable_identity().map_err(invalid)?;
        let current = load_state(savepoint, &binding)?;
        match apply_source_commit(current.as_ref(), commit.clone()).map_err(invalid)? {
            SourceCommitApplyOutcomeV1::ExactDuplicate(_) => Ok(()),
            SourceCommitApplyOutcomeV1::Committed(state) => {
                persist_state(savepoint, state.as_ref())
            }
        }
    }

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
        let current = load_state(savepoint, &binding)?
            .ok_or_else(|| invalid("external source authority publication has no source state"))?;
        let revised =
            apply_source_authority_publication(&current, publication.clone()).map_err(invalid)?;
        persist_state(savepoint, &revised)
    }

    pub fn rebuild_projection_publications(
        &mut self,
        savepoint: &Savepoint<'_>,
        binding: &SourceBindingIdentityV1,
    ) -> rusqlite::Result<()> {
        binding.validate().map_err(invalid)?;
        let state = load_state(savepoint, binding)?
            .ok_or_else(|| invalid("external source projection rebuild has no source state"))?;
        persist_state(savepoint, &state)
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
        }
    }
}

fn load_state(
    connection: &rusqlite::Connection,
    binding: &SourceBindingIdentityV1,
) -> rusqlite::Result<Option<SourceStoreStateV1>> {
    let encoded = connection
        .prepare(
            "SELECT state_json
             FROM external_source_states_v1
             WHERE binding_id = ?1",
        )?
        .query_row(params![binding.binding_id.as_str()], |row| {
            row.get::<_, String>(0)
        })
        .optional()?;
    let Some(encoded) = encoded else {
        return Ok(None);
    };
    let state: SourceStoreStateV1 = decode(encoded)?;
    state.validate().map_err(invalid)?;
    if state.binding().immutable_identity().map_err(invalid)? != *binding {
        return Err(invalid(
            "stored external source state does not match its binding key",
        ));
    }
    Ok(Some(state))
}

/// Write the whole source state, its authority history, and its receipt
/// history into the caller's savepoint.
///
/// Every row is compared against what is already stored so a replay can never
/// silently rewrite history. The comparison reuses the encoding that was just
/// bound as a parameter: `encode` is a pure `serde_json::to_string` of the same
/// value, so re-encoding produced identical bytes and only doubled the
/// serialization this loop performs for every historical revision and receipt
/// on every write.
fn persist_state(savepoint: &Savepoint<'_>, state: &SourceStoreStateV1) -> rusqlite::Result<()> {
    state.validate().map_err(invalid)?;
    let binding = state.binding().immutable_identity().map_err(invalid)?;
    let (owner_kind, owner_id) = owner_key(&binding.owner);
    for definition in state.definition_history().values() {
        let definition_revision = i64::try_from(definition.revision)
            .map_err(|_| invalid("external source definition revision exceeds SQLite INTEGER"))?;
        let encoded = encode(definition)?;
        savepoint.execute(
            "INSERT OR IGNORE INTO external_source_definition_revisions_v1 (
                source_id, definition_revision, definition_digest, definition_json
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                definition.source_id.as_str(),
                definition_revision,
                definition.definition_digest.as_str(),
                encoded,
            ],
        )?;
        let stored: String = savepoint.query_row(
            "SELECT definition_json
             FROM external_source_definition_revisions_v1
             WHERE source_id = ?1 AND definition_revision = ?2",
            params![definition.source_id.as_str(), definition_revision],
            |row| row.get(0),
        )?;
        if stored != encoded {
            return Err(invalid("external source definition revision collision"));
        }
    }
    for revision in state.binding_history().values() {
        let binding_revision = i64::try_from(revision.binding_revision)
            .map_err(|_| invalid("external source binding revision exceeds SQLite INTEGER"))?;
        let definition_revision = i64::try_from(revision.definition_revision)
            .map_err(|_| invalid("external source definition revision exceeds SQLite INTEGER"))?;
        let encoded = encode(revision)?;
        savepoint.execute(
            "INSERT OR IGNORE INTO external_source_binding_revisions_v1 (
                binding_id, binding_revision, definition_revision,
                binding_digest, binding_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                revision.binding_id.as_str(),
                binding_revision,
                definition_revision,
                revision.binding_digest.as_str(),
                encoded,
            ],
        )?;
        let stored: String = savepoint.query_row(
            "SELECT binding_json
             FROM external_source_binding_revisions_v1
             WHERE binding_id = ?1 AND binding_revision = ?2",
            params![revision.binding_id.as_str(), binding_revision],
            |row| row.get(0),
        )?;
        if stored != encoded {
            return Err(invalid("external source binding revision collision"));
        }
    }
    for receipt in state.authority_receipts().values() {
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
        let stored: String = savepoint.query_row(
            "SELECT receipt_json
             FROM external_source_authority_receipts_v1
             WHERE binding_id = ?1 AND idempotency_key = ?2",
            params![
                binding.binding_id.as_str(),
                receipt.idempotency_key().as_str()
            ],
            |row| row.get(0),
        )?;
        if stored != encoded {
            return Err(invalid("external source authority receipt collision"));
        }
    }
    for receipt in state.commit_receipts().values() {
        let encoded = encode(receipt)?;
        savepoint.execute(
            "INSERT OR IGNORE INTO external_source_projection_publications_v1 (
                binding_id, idempotency_key, request_digest,
                frontier_digest, projection_digest, receipt_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                binding.binding_id.as_str(),
                receipt.idempotency_key().as_str(),
                receipt.request_digest().as_str(),
                receipt.source_frontier().digest().as_str(),
                receipt.projection().receipt_digest().as_str(),
                encoded,
            ],
        )?;
        let stored: String = savepoint.query_row(
            "SELECT receipt_json
             FROM external_source_projection_publications_v1
             WHERE binding_id = ?1 AND idempotency_key = ?2",
            params![
                binding.binding_id.as_str(),
                receipt.idempotency_key().as_str()
            ],
            |row| row.get(0),
        )?;
        if stored != encoded {
            return Err(invalid("external source projection receipt collision"));
        }
    }
    savepoint.execute(
        "INSERT INTO external_source_states_v1 (
            binding_id, source_id, owner_kind, owner_id,
            definition_digest, binding_digest, frontier_digest,
            receipt_idempotency_key, receipt_request_digest, state_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(binding_id) DO UPDATE SET
            source_id = excluded.source_id,
            owner_kind = excluded.owner_kind,
            owner_id = excluded.owner_id,
            definition_digest = excluded.definition_digest,
            binding_digest = excluded.binding_digest,
            frontier_digest = excluded.frontier_digest,
            receipt_idempotency_key = excluded.receipt_idempotency_key,
            receipt_request_digest = excluded.receipt_request_digest,
            state_json = excluded.state_json",
        params![
            binding.binding_id.as_str(),
            binding.source_id.as_str(),
            owner_kind,
            owner_id,
            state.definition().definition_digest.as_str(),
            state.binding().binding_digest.as_str(),
            state.source_frontier().digest().as_str(),
            state.receipt().idempotency_key().as_str(),
            state.receipt().request_digest().as_str(),
            encode(state)?,
        ],
    )?;
    Ok(())
}

fn owner_key(owner: &SourceBindingOwnerV1) -> (&'static str, &str) {
    match owner {
        SourceBindingOwnerV1::Project(project_id) => ("project", project_id.as_str()),
        SourceBindingOwnerV1::Profile(profile_id) => ("profile", profile_id.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_domain::{
        AccessPolicyDigest, CapabilityId, ComponentVersion, LocatorDigest, ManifestDigest,
        PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProviderId,
        ResolutionAuthorizationV1, RetrievalAnchorId, SanitizationReceiptId,
        SanitizationReceiptRefV1, ScopeResolutionId, SourceAcquisitionCapabilitiesV1,
        SourceAcquisitionContractV1, SourceAggregateFrontierV1, SourceBindingOwnerV1,
        SourceBindingV1, SourceCaptureModeV1, SourceContentStateV1, SourceCoverageV1,
        SourceDefinitionV1, SourceDeletionSemanticsV1, SourceInstanceId, SourceNativeObjectIdV1,
        SourceObjectObservationV1, SourceObjectRevisionV1, SourcePartitionFrontierV1,
        SourcePartitionIdV1, SourceRefetchStrategyV1, SourceSnapshotCompletionV1,
        SourceSnapshotIdV1,
    };
    use tracedecay_store::{
        SourceAuthorityPublicationV1, SourceObjectMutationV1, SourceObjectTransitionV1,
        SourceObservationEvidenceV1,
    };

    use super::*;

    fn digest(seed: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
    }

    fn fixture() -> (SourceCommitV1, SourceBindingIdentityV1) {
        let definition = SourceDefinitionV1::new(
            SourceInstanceId::new("source.runtime-fixture").unwrap(),
            1,
            SourceAcquisitionContractV1::new(
                ProviderId::new("github").unwrap(),
                SourceAcquisitionCapabilitiesV1::new(
                    BTreeSet::from([SourceCaptureModeV1::Poll]),
                    BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
                    BTreeSet::from([SourceDeletionSemanticsV1::CompleteSnapshotAbsence]),
                )
                .unwrap(),
            )
            .unwrap(),
            SourceCaptureModeV1::Poll,
            SourceRefetchStrategyV1::WholeRoot,
            SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
            1,
        )
        .unwrap();
        let binding = SourceBindingV1::new(
            &definition,
            SourceBindingOwnerV1::Project(ProjectId::new("project.runtime-fixture").unwrap()),
            PrivacyDomainId::new("privacy.runtime-fixture").unwrap(),
            LocatorDigest::new(digest('a').as_str()).unwrap(),
            1,
        )
        .unwrap();
        let identity = binding.immutable_identity().unwrap();
        let partition = SourcePartitionIdV1::new(digest('b'));
        let snapshot = SourceSnapshotIdV1::new(digest('c'));
        let observation = SourceObjectObservationV1::new(
            SourceNativeObjectIdV1::new(digest('d')),
            SourceObjectRevisionV1::new(digest('e')),
            digest('f'),
            SourceContentStateV1::Live,
        )
        .unwrap();
        let frontier = SourcePartitionFrontierV1::new(
            identity.clone(),
            partition.clone(),
            None,
            Some(snapshot.clone()),
            None,
            SourceCoverageV1::Complete,
            1,
            None,
            digest('1'),
        )
        .unwrap();
        let aggregate =
            SourceAggregateFrontierV1::with_updated_partition(identity.clone(), None, frontier)
                .unwrap();
        let completion = SourceSnapshotCompletionV1::new(
            partition.clone(),
            snapshot,
            BTreeSet::from([observation.native_object().clone()]),
        )
        .unwrap();
        let evidence = SourceObservationEvidenceV1::new(
            identity.clone(),
            partition.clone(),
            &observation,
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.external-source.runtime-fixture").unwrap(),
                ComponentVersion::new("sanitizer.external-source.v1").unwrap(),
            )
            .unwrap(),
            RetrievalAnchorId::new("retrieval.external-source.runtime-fixture").unwrap(),
            ResolutionAuthorizationV1 {
                resolved_scope_id: ScopeResolutionId::new("scope.external-source.runtime-fixture")
                    .unwrap(),
                privacy_domain_id: identity.privacy_domain.clone(),
                access_policy_digest: AccessPolicyDigest::new(digest('4').as_str()).unwrap(),
                capability_id: CapabilityId::new("capability.external-source.runtime-fixture")
                    .unwrap(),
                canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(
                    digest('5').as_str(),
                )
                .unwrap(),
            },
            digest('6'),
        )
        .unwrap();
        let mutation = SourceObjectMutationV1::new(
            observation,
            None,
            SourceObjectTransitionV1::Initial,
            evidence,
        )
        .unwrap();
        let commit = SourceCommitV1::new(
            definition,
            binding,
            partition,
            ComponentVersion::new("external-source-projector-v1").unwrap(),
            digest('2'),
            digest('3'),
            None,
            aggregate,
            vec![mutation],
            Some(completion),
        )
        .unwrap();
        (commit, identity)
    }

    #[test]
    fn commit_replay_and_restart_read_share_one_durable_state() {
        let temporary = tempfile::tempdir().unwrap();
        let database_path = temporary.path().join("external-source.sqlite");
        let (commit, binding) = fixture();
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
            let mut interrupted = connection.transaction().unwrap();
            let savepoint = interrupted.savepoint().unwrap();
            ExternalSourceExecutor
                .execute_write(&savepoint, &commit)
                .unwrap();
            savepoint.commit().unwrap();
            drop(interrupted);
        }
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            let transaction = connection.transaction().unwrap();
            assert!(matches!(
                ExternalSourceExecutor
                    .execute_read(
                        &transaction,
                        &ExternalSourceReadOperationV1::State {
                            binding: binding.clone(),
                        },
                    )
                    .unwrap(),
                ExternalSourceReadResultV1::State(None)
            ));
        }
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            ExternalSourceExecutor
                .execute_write(&savepoint, &commit)
                .unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        let mut replay = connection.transaction().unwrap();
        let savepoint = replay.savepoint().unwrap();
        ExternalSourceExecutor
            .execute_write(&savepoint, &commit)
            .unwrap();
        savepoint.commit().unwrap();
        replay.commit().unwrap();
        let transaction = connection.transaction().unwrap();
        let state = match ExternalSourceExecutor
            .execute_read(
                &transaction,
                &ExternalSourceReadOperationV1::State {
                    binding: binding.clone(),
                },
            )
            .unwrap()
        {
            ExternalSourceReadResultV1::State(Some(state)) => state,
            other => panic!("expected durable external source state, got {other:?}"),
        };
        assert_eq!(state.receipt().idempotency_key(), commit.idempotency_key());
        assert_eq!(state.projected_objects().len(), 1);
        let state_json: String = transaction
            .query_row(
                "SELECT state_json FROM external_source_states_v1 WHERE binding_id = ?1",
                [binding.binding_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!state_json.contains("secret"));
        assert!(!state_json.contains("https://"));
    }

    #[test]
    fn authority_and_projection_histories_survive_restart_and_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let database_path = temporary.path().join("external-source-history.sqlite");
        let (commit, _) = fixture();
        let definition_v1 = commit.definition().clone();
        let binding_v1 = commit.binding().clone();
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            connection.execute_batch(EXTERNAL_SOURCE_SCHEMA_V1).unwrap();
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            ExternalSourceExecutor
                .execute_write(&savepoint, &commit)
                .unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
        let definition_v2 = SourceDefinitionV1::new(
            definition_v1.source_id.clone(),
            2,
            SourceAcquisitionContractV1::new(
                definition_v1.provider.clone(),
                definition_v1.acquisition_capabilities.clone(),
            )
            .unwrap(),
            definition_v1.capture_mode,
            definition_v1.refetch_strategy,
            definition_v1.deletion_semantics,
            definition_v1.max_partitions,
        )
        .unwrap();
        let binding_v2 = SourceBindingV1::new(
            &definition_v2,
            binding_v1.owner.clone(),
            binding_v1.privacy_domain.clone(),
            binding_v1.native_root.clone(),
            2,
        )
        .unwrap();
        let publication = SourceAuthorityPublicationV1::new(
            &definition_v2,
            &binding_v2,
            definition_v1.definition_digest.clone(),
            binding_v1.binding_digest.clone(),
            digest('7'),
            digest('8'),
        )
        .unwrap();
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            let mut interrupted = connection.transaction().unwrap();
            let savepoint = interrupted.savepoint().unwrap();
            ExternalSourceExecutor
                .execute_authority_publication(&savepoint, &publication)
                .unwrap();
            savepoint.commit().unwrap();
            drop(interrupted);
        }
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM external_source_definition_revisions_v1",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                1
            );
        }
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            ExternalSourceExecutor
                .execute_authority_publication(&savepoint, &publication)
                .unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            ExternalSourceExecutor
                .execute_authority_publication(&savepoint, &publication)
                .unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_source_definition_revisions_v1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_source_binding_revisions_v1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_source_authority_receipts_v1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_source_projection_publications_v1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        connection
            .execute("DELETE FROM external_source_projection_publications_v1", [])
            .unwrap();
        drop(connection);
        {
            let mut connection = rusqlite::Connection::open(&database_path).unwrap();
            let mut transaction = connection.transaction().unwrap();
            let savepoint = transaction.savepoint().unwrap();
            ExternalSourceExecutor
                .rebuild_projection_publications(
                    &savepoint,
                    &binding_v2.immutable_identity().unwrap(),
                )
                .unwrap();
            savepoint.commit().unwrap();
            transaction.commit().unwrap();
        }
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM external_source_projection_publications_v1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }
}

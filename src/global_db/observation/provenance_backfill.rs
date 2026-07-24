use tracedecay_domain::EvidenceAvailabilityV1;
use tracedecay_store::RepositoryProvenanceAttachmentV1;

use crate::db::engine::{Executor, params};

use super::super::{global_db_operation_error, global_db_operation_message};
use super::schema::OBSERVATION_SCHEMA_OPERATION;

const OBSERVATION_PROVENANCE_SCHEMA_MIGRATION: &str = "observation-repository-provenance-v1";

pub(super) async fn backfill_observation_repository_provenance(
    conn: &impl Executor,
) -> crate::errors::Result<()> {
    let availability_json = serde_json::to_string(
        RepositoryProvenanceAttachmentV1::new(EvidenceAvailabilityV1::Unknown, None)
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
            .availability(),
    )
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    conn.execute(
        "INSERT OR IGNORE INTO observation_repository_provenance (
            observation_id, availability_json, capture_json, retrieval_anchor_id, owner_json
         )
         SELECT observation_id, ?1, NULL, NULL, NULL FROM observations",
        params![availability_json],
    )
    .await
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let mut rows = conn
        .query(
            "SELECT observation.observation_id
             FROM observations AS observation
             LEFT JOIN observation_repository_provenance AS provenance
               ON provenance.observation_id = observation.observation_id
             WHERE provenance.observation_id IS NULL
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    if rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
        .is_some()
    {
        return Err(global_db_operation_message(
            OBSERVATION_SCHEMA_OPERATION,
            "repository provenance backfill left an observation without an attachment",
        ));
    }
    drop(rows);
    conn.execute(
        "INSERT OR REPLACE INTO global_schema_migrations(migration) VALUES (?1)",
        params![OBSERVATION_PROVENANCE_SCHEMA_MIGRATION],
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

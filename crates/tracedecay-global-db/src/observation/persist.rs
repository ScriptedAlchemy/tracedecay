use tracedecay_domain::{CanonicalObservationIdV1, ProjectionGenerationId};
use tracedecay_store::{ObservationCommitReceipt, ObservationStoreError, ObservationStoreResult};

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::codec::{decode, decode_repository_provenance_attachment, decode_sequence, storage};

async fn read_observation_row(
    conn: &impl QueryExecutor,
    sql: &'static str,
    value: &str,
    operation: &'static str,
) -> ObservationStoreResult<Option<ObservationCommitReceipt>> {
    let mut rows = conn
        .query(sql, params![value])
        .await
        .map_err(|error| storage(operation, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(operation, error))?
    else {
        return Ok(None);
    };
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let observation_json = row
        .get::<String>(1)
        .map_err(|error| storage(operation, error))?;
    let cursor_json = row
        .get::<String>(2)
        .map_err(|error| storage(operation, error))?;
    let anchor_json = row
        .get::<String>(3)
        .map_err(|error| storage(operation, error))?;
    let projection_generation = row
        .get::<String>(4)
        .map_err(|error| storage(operation, error))?;
    let repository_availability_json = row
        .get::<String>(5)
        .map_err(|error| storage(operation, error))?;
    let repository_capture_json = row
        .get::<Option<String>>(6)
        .map_err(|error| storage(operation, error))?;
    let repository_anchor_json = row
        .get::<Option<String>>(7)
        .map_err(|error| storage(operation, error))?;
    Ok(Some(
        ObservationCommitReceipt::new(
            sequence,
            decode(&observation_json, operation)?,
            decode(&cursor_json, operation)?,
            decode(&anchor_json, operation)?,
            ProjectionGenerationId::new(projection_generation)
                .map_err(ObservationStoreError::RetrievalAnchorContract)?,
        )?
        .with_repository_provenance_attachment(
            decode_repository_provenance_attachment(
                &repository_availability_json,
                repository_capture_json.as_deref(),
                repository_anchor_json.as_deref(),
                operation,
            )?,
        )?,
    ))
}

pub(super) async fn read_by_observation_id(
    conn: &impl QueryExecutor,
    observation_id: &CanonicalObservationIdV1,
) -> ObservationStoreResult<Option<ObservationCommitReceipt>> {
    read_observation_row(
        conn,
        "SELECT observation.sequence, observation.observation_json,
                observation.committed_cursor_json, anchor.anchor_json,
                anchor.projection_generation, repository.availability_json,
                repository.capture_json, repository_anchor.anchor_json
         FROM observations AS observation
         JOIN observation_retrieval_anchors AS binding
           ON binding.observation_id = observation.observation_id
         JOIN retrieval_anchors AS anchor ON anchor.anchor_id = binding.anchor_id
         JOIN observation_repository_provenance AS repository
           ON repository.observation_id = observation.observation_id
         LEFT JOIN retrieval_anchors AS repository_anchor
           ON repository_anchor.anchor_id = repository.retrieval_anchor_id
         WHERE observation.observation_id = ?1",
        observation_id.as_str(),
        "read observation",
    )
    .await
}

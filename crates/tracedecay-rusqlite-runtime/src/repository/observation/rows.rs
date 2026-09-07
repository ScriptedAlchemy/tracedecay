//! The one row projection every observation read decodes through.
//!
//! The projection constant and the two decode steps live together because they
//! are positionally coupled: the column list fixes the order, the tuple type
//! names it, and the decoder is the only place the joined halves are validated.

use tracedecay_domain::{
    EvidenceAvailabilityV1, GenerationBoundRepositoryProvenanceV1, ObservationSourceCursorV1,
    ProjectionGenerationId, RetrievalAnchorRecordV2,
};
use tracedecay_store::{
    ObservationCommitReceipt, RepositoryProvenanceAttachmentV1, StoredObservationRowV1,
};

use super::super::support::{decode, encode, invalid};

pub(super) fn decode_nonnegative(value: i64, message: &'static str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| invalid(message))
}

pub(super) type EncodedObservationRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

/// The single projection every observation read decodes through.
///
/// The outer joins are all optional by schema, so the missing halves are
/// rejected by [`decode_observation_row`] rather than by the query. Callers
/// append their own `WHERE`/`ORDER BY`/`LIMIT` clauses; the column list and its
/// order are fixed here because [`encoded_observation_row`] reads them
/// positionally.
pub(super) const OBSERVATION_ROW_PROJECTION: &str =
    "SELECT observation.sequence, observation.observation_json,
            observation.committed_cursor_json, anchor.anchor_json,
            anchor.projection_generation, repository.availability_json,
            repository.capture_json, repository_anchor.anchor_json,
            repository.owner_json,
            EXISTS(
                SELECT 1 FROM projection_queue
                WHERE projection_queue.observation_id =
                      observation.observation_id
            )
     FROM observations AS observation
     LEFT JOIN observation_retrieval_anchors AS binding
       ON binding.observation_id = observation.observation_id
     LEFT JOIN retrieval_anchors AS anchor
       ON anchor.anchor_id = binding.anchor_id
     LEFT JOIN observation_repository_provenance AS repository
       ON repository.observation_id = observation.observation_id
     LEFT JOIN retrieval_anchors AS repository_anchor
       ON repository_anchor.anchor_id = repository.retrieval_anchor_id";

/// Reads one [`OBSERVATION_ROW_PROJECTION`] row in column order.
pub(super) fn encoded_observation_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<EncodedObservationRow> {
    Ok((
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, Option<String>>(3)?,
        row.get::<_, Option<String>>(4)?,
        row.get::<_, Option<String>>(5)?,
        row.get::<_, Option<String>>(6)?,
        row.get::<_, Option<String>>(7)?,
        row.get::<_, Option<String>>(8)?,
        row.get::<_, i64>(9)?,
    ))
}

pub(super) fn decode_observation_row(
    (
        sequence,
        observation,
        cursor,
        retrieval_anchor,
        projection_generation,
        repository_availability,
        repository_capture,
        repository_anchor,
        repository_owner,
        projection_queued,
    ): EncodedObservationRow,
) -> rusqlite::Result<StoredObservationRowV1> {
    let repository_availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> =
        decode(
            repository_availability
                .ok_or_else(|| invalid("observation repository provenance is missing"))?,
        )?;
    let repository_capture = repository_capture
        .map(decode::<GenerationBoundRepositoryProvenanceV1>)
        .transpose()?;
    if repository_availability.value() != repository_capture.as_ref() {
        return Err(invalid("repository provenance binding mismatch"));
    }
    let sequence = u64::try_from(sequence).map_err(|_| invalid("negative observation sequence"))?;
    let observation: tracedecay_domain::DurableObservationV1 = decode(observation)?;
    let committed_cursor: ObservationSourceCursorV1 = decode(cursor)?;
    if observation.source() != committed_cursor.source()
        || observation.scope() != committed_cursor.scope()
        || observation.identity().generation() != committed_cursor.generation()
        || observation.identity().ordering_domain() != committed_cursor.ordering_domain()
        || observation.identity().position().end() != committed_cursor.position()
    {
        return Err(invalid("observation committed cursor binding mismatch"));
    }
    let retrieval_anchor: RetrievalAnchorRecordV2 = decode(
        retrieval_anchor.ok_or_else(|| invalid("observation retrieval anchor is missing"))?,
    )?;
    let projection_generation = ProjectionGenerationId::new(
        projection_generation
            .ok_or_else(|| invalid("observation projection generation is missing"))?,
    )
    .map_err(invalid)?;
    let repository_anchor = repository_anchor
        .map(decode::<RetrievalAnchorRecordV2>)
        .transpose()?;
    let expected_repository_owner = repository_anchor
        .as_ref()
        .map(|anchor| encode(anchor.owner()))
        .transpose()?;
    if repository_owner != expected_repository_owner {
        return Err(invalid("observation repository owner binding mismatch"));
    }
    let repository_provenance =
        RepositoryProvenanceAttachmentV1::new(repository_availability, repository_anchor)
            .map_err(invalid)?;
    ObservationCommitReceipt::new(
        sequence,
        observation.clone(),
        committed_cursor.clone(),
        retrieval_anchor.clone(),
        projection_generation.clone(),
    )
    .and_then(|receipt| {
        receipt.with_repository_provenance_attachment(repository_provenance.clone())
    })
    .map_err(invalid)?;
    Ok(StoredObservationRowV1 {
        sequence,
        observation,
        committed_cursor,
        retrieval_anchor,
        projection_generation,
        repository_provenance,
        projection_queued: projection_queued != 0,
    })
}

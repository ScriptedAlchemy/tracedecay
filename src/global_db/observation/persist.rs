use tracedecay_domain::{
    CanonicalObservationIdV1, NativeAliasV2, ObservationSourceCursorV1, ProjectionGenerationId,
    RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalAnchorTargetV2, SanitizationReceiptV1,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationCommitReceipt, ObservationStoreError, ObservationStoreResult,
    RepositoryProvenanceAttachmentV1,
};

use crate::db::engine::{Executor, QueryExecutor, params};

use super::codec::{
    decode, decode_repository_provenance_attachment, decode_sequence, encode, encode_json_string,
    storage, storage_message,
};

/// A native alias already bound to a different anchor than the one being
/// persisted. `persist_retrieval_anchor` always detects and reports these as
/// data rather than deciding what to do about them: live writes must not
/// commit a conflicting identity, so the caller maps this into
/// [`ObservationStoreError::RetrievalAnchorAliasCollision`] and fails the
/// transaction; the legacy anchor backfill instead logs it and continues —
/// stores that accumulated duplicate provider records (or anchors minted by
/// an earlier derivation) must still finish migrating, the first-bound alias
/// is never overwritten, and the later anchor simply stays reachable only by
/// id.
pub(super) struct AnchorAliasCollision {
    pub(super) alias: Box<NativeAliasV2>,
    pub(super) existing_anchor_id: Box<RetrievalAnchorId>,
    pub(super) candidate_anchor_id: Box<RetrievalAnchorId>,
}

/// Fails closed on the first reported alias collision, matching the fixed
/// live-write policy: a conflicting native alias must not commit.
fn fail_closed_on_alias_collision(
    collisions: Vec<AnchorAliasCollision>,
) -> ObservationStoreResult<()> {
    match collisions.into_iter().next() {
        Some(collision) => Err(ObservationStoreError::RetrievalAnchorAliasCollision {
            alias: collision.alias,
            existing_anchor_id: collision.existing_anchor_id,
            candidate_anchor_id: collision.candidate_anchor_id,
        }),
        None => Ok(()),
    }
}

async fn persist_retrieval_anchor(
    conn: &impl Executor,
    candidate: &RetrievalAnchorRecordV2,
) -> ObservationStoreResult<(
    RetrievalAnchorRecordV2,
    ProjectionGenerationId,
    Vec<AnchorAliasCollision>,
)> {
    let anchor_json = encode(candidate, "encode retrieval anchor")?;
    let owner_json = encode(candidate.owner(), "encode retrieval anchor owner")?;
    conn.execute(
        "INSERT OR IGNORE INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            candidate.anchor_id().as_str(),
            anchor_json.as_str(),
            owner_json.as_str(),
            candidate.projection_generation().as_str(),
        ],
    )
    .await
    .map_err(|error| storage("insert retrieval anchor", error))?;
    let mut rows = conn
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            params![candidate.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage("read retrieval anchor", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read retrieval anchor", error))?
        .ok_or_else(|| storage_message("read retrieval anchor", "anchor insert disappeared"))?;
    let stored_json = row
        .get::<String>(0)
        .map_err(|error| storage("read retrieval anchor", error))?;
    let stored_owner_json = row
        .get::<String>(1)
        .map_err(|error| storage("read retrieval anchor", error))?;
    let stored_projection_generation = row
        .get::<String>(2)
        .map_err(|error| storage("read retrieval anchor", error))?;
    drop(rows);
    let stored: RetrievalAnchorRecordV2 = decode(&stored_json, "decode retrieval anchor")?;
    let projection_generation = ProjectionGenerationId::new(stored_projection_generation)
        .map_err(ObservationStoreError::RetrievalAnchorContract)?;
    if stored != *candidate
        || stored.anchor_id() != candidate.anchor_id()
        || stored.owner() != candidate.owner()
        || stored_owner_json != encode(stored.owner(), "verify retrieval anchor owner")?
        || stored.projection_generation() != &projection_generation
    {
        return Err(ObservationStoreError::RetrievalAnchorCollision);
    }

    let mut alias_collisions = Vec::new();
    for alias in stored.aliases() {
        let alias_kind = encode_json_string(&alias.kind(), "encode retrieval anchor alias kind")?;
        let locator_digest = encode_json_string(
            alias.locator_digest(),
            "encode retrieval anchor alias digest",
        )?;
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO retrieval_anchor_aliases (
                    owner_json, alias_kind, locator_digest, anchor_id
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    stored_owner_json.as_str(),
                    alias_kind.as_str(),
                    locator_digest.as_str(),
                    stored.anchor_id().as_str(),
                ],
            )
            .await
            .map_err(|error| storage("insert retrieval anchor alias", error))?;
        if inserted == 0 {
            let mut rows = conn
                .query(
                    "SELECT anchor_id FROM retrieval_anchor_aliases
                     WHERE owner_json = ?1 AND alias_kind = ?2 AND locator_digest = ?3",
                    params![
                        stored_owner_json.as_str(),
                        alias_kind.as_str(),
                        locator_digest.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage("read retrieval anchor alias", error))?;
            let existing_anchor_id = rows
                .next()
                .await
                .map_err(|error| storage("read retrieval anchor alias", error))?
                .ok_or_else(|| {
                    storage_message(
                        "read retrieval anchor alias",
                        "alias conflict row disappeared",
                    )
                })?
                .get::<String>(0)
                .map_err(|error| storage("read retrieval anchor alias", error))?;
            if existing_anchor_id != stored.anchor_id().as_str() {
                alias_collisions.push(AnchorAliasCollision {
                    alias: Box::new(alias.clone()),
                    existing_anchor_id: Box::new(
                        RetrievalAnchorId::new(existing_anchor_id)
                            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
                    ),
                    candidate_anchor_id: Box::new(stored.anchor_id().clone()),
                });
            }
        }
    }
    Ok((stored, projection_generation, alias_collisions))
}

pub(super) async fn persist_observation_retrieval_anchor(
    conn: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
    candidate: &RetrievalAnchorRecordV2,
) -> ObservationStoreResult<(
    RetrievalAnchorRecordV2,
    ProjectionGenerationId,
    Vec<AnchorAliasCollision>,
)> {
    if !matches!(
        candidate.target(),
        RetrievalAnchorTargetV2::ExactObservation(target) if target == observation_id
    ) {
        return Err(ObservationStoreError::RetrievalAnchorObservationMismatch);
    }
    let (stored, projection_generation, alias_collisions) =
        persist_retrieval_anchor(conn, candidate).await?;
    conn.execute(
        "INSERT OR IGNORE INTO observation_retrieval_anchors (observation_id, anchor_id)
         VALUES (?1, ?2)",
        params![observation_id.as_str(), stored.anchor_id().as_str()],
    )
    .await
    .map_err(|error| storage("bind observation retrieval anchor", error))?;
    let mut rows = conn
        .query(
            "SELECT anchor_id FROM observation_retrieval_anchors WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("verify observation retrieval anchor", error))?;
    let bound_anchor_id = rows
        .next()
        .await
        .map_err(|error| storage("verify observation retrieval anchor", error))?
        .ok_or_else(|| {
            storage_message(
                "verify observation retrieval anchor",
                "observation anchor binding disappeared",
            )
        })?
        .get::<String>(0)
        .map_err(|error| storage("verify observation retrieval anchor", error))?;
    if bound_anchor_id != stored.anchor_id().as_str() {
        return Err(ObservationStoreError::RetrievalAnchorObservationMismatch);
    }
    Ok((stored, projection_generation, alias_collisions))
}

async fn persist_repository_provenance_attachment(
    conn: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
    candidate: &RepositoryProvenanceAttachmentV1,
) -> ObservationStoreResult<RepositoryProvenanceAttachmentV1> {
    let stored_anchor = match candidate.anchor() {
        Some(candidate_anchor) => {
            let (stored_anchor, _, alias_collisions) =
                persist_retrieval_anchor(conn, candidate_anchor).await?;
            fail_closed_on_alias_collision(alias_collisions)?;
            if &stored_anchor != candidate_anchor {
                return Err(ObservationStoreError::RetrievalAnchorCollision);
            }
            Some(stored_anchor)
        }
        None => None,
    };
    let stored =
        RepositoryProvenanceAttachmentV1::new(candidate.availability().clone(), stored_anchor)?;
    let availability_json = encode(
        stored.availability(),
        "encode repository provenance availability",
    )?;
    let capture_json = stored
        .provenance()
        .map(|capture| encode(capture, "encode repository provenance capture"))
        .transpose()?;
    let owner_json = stored
        .anchor()
        .map(|anchor| encode(anchor.owner(), "encode repository provenance owner"))
        .transpose()?;
    conn.execute(
        "INSERT INTO observation_repository_provenance (
            observation_id, availability_json, capture_json, retrieval_anchor_id, owner_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            observation_id.as_str(),
            availability_json.as_str(),
            capture_json.as_deref(),
            stored.anchor().map(|anchor| anchor.anchor_id().as_str()),
            owner_json.as_deref(),
        ],
    )
    .await
    .map_err(|error| storage("persist observation repository provenance", error))?;
    Ok(stored)
}

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

async fn read_cursor(
    conn: &impl QueryExecutor,
    source_json: &str,
    scope_json: &str,
) -> ObservationStoreResult<Option<ObservationSourceCursorV1>> {
    let mut rows = conn
        .query(
            "SELECT cursor_json FROM source_cursors
             WHERE source_json = ?1 AND scope_json = ?2",
            params![source_json, scope_json],
        )
        .await
        .map_err(|error| storage("read observation source cursor", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read observation source cursor", error))?
    else {
        return Ok(None);
    };
    let cursor_json = row
        .get::<String>(0)
        .map_err(|error| storage("read observation source cursor", error))?;
    decode(&cursor_json, "decode observation source cursor").map(Some)
}

async fn cursor_advance_receipt_matches(
    conn: &impl QueryExecutor,
    source_json: &str,
    scope_json: &str,
    advance: &ObservationCursorAdvance,
) -> ObservationStoreResult<bool> {
    let coverage_json = encode(&advance.coverage(), "encode observation coverage")?;
    let mut rows = conn
        .query(
            "SELECT reason, receipt_id FROM source_cursor_advances
             WHERE source_json = ?1 AND scope_json = ?2 AND coverage_json = ?3",
            params![source_json, scope_json, coverage_json],
        )
        .await
        .map_err(|error| storage("read source cursor advance receipt", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read source cursor advance receipt", error))?
    else {
        return Ok(false);
    };
    let reason = row
        .get::<String>(0)
        .map_err(|error| storage("read source cursor advance receipt", error))?;
    let receipt_id = row
        .get::<Option<String>>(1)
        .map_err(|error| storage("read source cursor advance receipt", error))?;
    Ok(reason == advance.reason().as_str()
        && receipt_id.as_deref()
            == advance
                .sanitization_receipt()
                .map(|receipt| receipt.receipt().receipt_id().as_str()))
}

async fn persist_sanitization_receipt(
    conn: &impl Executor,
    receipt: &SanitizationReceiptV1,
) -> ObservationStoreResult<()> {
    let receipt_json = encode(receipt, "encode sanitization receipt")?;
    let receipt_id = receipt.receipt().receipt_id().as_str();
    let sanitizer_version = receipt.receipt().sanitizer_version().as_str();
    let payload_digest = receipt
        .payload()
        .map_or("", |payload| payload.digest().as_str());
    conn.execute(
        "INSERT INTO sanitization_receipts
            (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(receipt_id) DO NOTHING",
        params![
            receipt_id,
            sanitizer_version,
            payload_digest,
            receipt_json.as_str()
        ],
    )
    .await
    .map_err(|error| storage("insert sanitization receipt", error))?;
    let mut rows = conn
        .query(
            "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
            params![receipt_id],
        )
        .await
        .map_err(|error| storage("verify sanitization receipt", error))?;
    let stored = rows
        .next()
        .await
        .map_err(|error| storage("verify sanitization receipt", error))?
        .ok_or_else(|| {
            storage_message("verify sanitization receipt", "receipt insert disappeared")
        })?
        .get::<String>(0)
        .map_err(|error| storage("verify sanitization receipt", error))?;
    if stored != receipt_json {
        return Err(ObservationStoreError::SanitizationReceiptCollision);
    }
    Ok(())
}

async fn write_cursor(
    conn: &impl Executor,
    source_json: &str,
    scope_json: &str,
    cursor_json: &str,
) -> ObservationStoreResult<()> {
    conn.execute(
        "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(source_json, scope_json) DO UPDATE SET
            cursor_json = excluded.cursor_json",
        params![source_json, scope_json, cursor_json],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("advance observation source cursor", error))
}

async fn apply_cursor_advance(
    conn: &impl Executor,
    advance: &ObservationCursorAdvance,
) -> ObservationStoreResult<CursorAdvanceOutcome> {
    let source_json = encode(advance.next_cursor().source(), "encode observation source")?;
    let scope_json = encode(advance.next_cursor().scope(), "encode observation scope")?;
    let actual_cursor = read_cursor(conn, &source_json, &scope_json).await?;
    if actual_cursor.as_ref() == Some(advance.next_cursor()) {
        return if cursor_advance_receipt_matches(conn, &source_json, &scope_json, advance).await? {
            Ok(CursorAdvanceOutcome::ExactDuplicate)
        } else {
            Err(ObservationStoreError::CursorAdvanceCollision)
        };
    }
    if actual_cursor.as_ref() != advance.expected_cursor() {
        return Err(ObservationStoreError::CursorConflict {
            expected: Box::new(advance.expected_cursor().cloned()),
            actual: Box::new(actual_cursor),
        });
    }
    if let Some(receipt) = advance.sanitization_receipt() {
        persist_sanitization_receipt(conn, receipt).await?;
    }
    let receipt_id = advance
        .sanitization_receipt()
        .map(|receipt| receipt.receipt().receipt_id().as_str());
    let coverage_json = encode(&advance.coverage(), "encode observation coverage")?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO source_cursor_advances(
                source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                coverage_json.as_str(),
                advance.reason().as_str(),
                receipt_id,
            ],
        )
        .await
        .map_err(|error| storage("persist cursor coverage receipt", error))?;
    if inserted == 0
        && !cursor_advance_receipt_matches(conn, &source_json, &scope_json, advance).await?
    {
        return Err(ObservationStoreError::CursorAdvanceCollision);
    }
    let cursor_json = encode(advance.next_cursor(), "encode committed observation cursor")?;
    write_cursor(conn, &source_json, &scope_json, &cursor_json).await?;
    Ok(CursorAdvanceOutcome::Committed)
}

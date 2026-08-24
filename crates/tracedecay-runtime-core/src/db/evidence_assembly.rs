use crate::db::engine::Executor;
use crate::errors::{Result, TraceDecayError};

pub(super) const EVIDENCE_ASSEMBLY_SCHEMA: &str = r"
    CREATE TABLE IF NOT EXISTS evidence_source_occurrences (
        occurrence_id TEXT PRIMARY KEY CHECK(length(occurrence_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        timeline_digest TEXT NOT NULL CHECK(length(timeline_digest) > 0),
        source_anchor_id TEXT NOT NULL CHECK(length(source_anchor_id) > 0),
        source_order INTEGER NOT NULL CHECK(source_order >= 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json))
    );
    CREATE INDEX IF NOT EXISTS idx_evidence_occurrences_anchor
        ON evidence_source_occurrences(owner_digest, source_anchor_id);
    CREATE INDEX IF NOT EXISTS idx_evidence_occurrences_timeline
        ON evidence_source_occurrences(owner_digest, timeline_digest, source_order);

    CREATE TABLE IF NOT EXISTS evidence_occurrence_sets (
        occurrence_set_id TEXT PRIMARY KEY CHECK(length(occurrence_set_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json))
    );
    CREATE TABLE IF NOT EXISTS evidence_occurrence_set_members (
        occurrence_set_id TEXT NOT NULL,
        canonical_ordinal INTEGER NOT NULL CHECK(canonical_ordinal >= 0),
        occurrence_id TEXT NOT NULL,
        PRIMARY KEY(occurrence_set_id, canonical_ordinal),
        UNIQUE(occurrence_set_id, occurrence_id),
        FOREIGN KEY(occurrence_set_id)
            REFERENCES evidence_occurrence_sets(occurrence_set_id),
        FOREIGN KEY(occurrence_id)
            REFERENCES evidence_source_occurrences(occurrence_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_spans (
        span_id TEXT PRIMARY KEY CHECK(length(span_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        occurrence_set_id TEXT NOT NULL,
        anchor_id TEXT NOT NULL UNIQUE CHECK(length(anchor_id) > 0),
        producer_kind TEXT NOT NULL CHECK(length(producer_kind) > 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        FOREIGN KEY(occurrence_set_id)
            REFERENCES evidence_occurrence_sets(occurrence_set_id)
    );
    CREATE TABLE IF NOT EXISTS evidence_span_members (
        span_id TEXT NOT NULL,
        assembly_ordinal INTEGER NOT NULL CHECK(assembly_ordinal >= 0),
        run_ordinal INTEGER NOT NULL CHECK(run_ordinal >= 0),
        run_member_ordinal INTEGER NOT NULL CHECK(run_member_ordinal >= 0),
        occurrence_id TEXT NOT NULL,
        PRIMARY KEY(span_id, assembly_ordinal),
        UNIQUE(span_id, occurrence_id),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id),
        FOREIGN KEY(occurrence_id)
            REFERENCES evidence_source_occurrences(occurrence_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_span_projection_receipts (
        projection_receipt_id TEXT PRIMARY KEY CHECK(length(projection_receipt_id) > 0),
        span_id TEXT NOT NULL,
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        UNIQUE(span_id, projection_receipt_id),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_retriever_contributions (
        contribution_id TEXT PRIMARY KEY CHECK(length(contribution_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        span_id TEXT NOT NULL,
        anchor_id TEXT NOT NULL UNIQUE CHECK(length(anchor_id) > 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_derived_anchors (
        anchor_id TEXT PRIMARY KEY CHECK(length(anchor_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        target_kind TEXT NOT NULL CHECK(
            target_kind IN ('source_occurrence', 'evidence_span', 'retriever_contribution')
        ),
        target_id TEXT NOT NULL CHECK(length(target_id) > 0),
        anchor_json TEXT NOT NULL CHECK(json_valid(anchor_json)),
        UNIQUE(owner_digest, target_kind, target_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_assembly_receipts (
        publication_receipt_id TEXT PRIMARY KEY CHECK(length(publication_receipt_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        privacy_domain_id TEXT NOT NULL CHECK(length(privacy_domain_id) > 0),
        key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
        idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) > 0),
        assembly_digest TEXT NOT NULL CHECK(length(assembly_digest) > 0),
        occurrence_set_id TEXT NOT NULL,
        span_id TEXT NOT NULL,
        contribution_id TEXT NOT NULL,
        projection_receipt_id TEXT NOT NULL,
        receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
        UNIQUE(owner_digest, privacy_domain_id, key_epoch, idempotency_key),
        FOREIGN KEY(occurrence_set_id)
            REFERENCES evidence_occurrence_sets(occurrence_set_id),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id),
        FOREIGN KEY(contribution_id)
            REFERENCES evidence_retriever_contributions(contribution_id),
        FOREIGN KEY(projection_receipt_id)
            REFERENCES evidence_span_projection_receipts(projection_receipt_id)
    );
";

pub(super) const EVIDENCE_ASSEMBLY_IMMUTABILITY: &str = r"
    CREATE TRIGGER IF NOT EXISTS evidence_source_occurrences_immutable_update
    BEFORE UPDATE ON evidence_source_occurrences BEGIN
        SELECT RAISE(ABORT, 'evidence source occurrences are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_source_occurrences_immutable_delete
    BEFORE DELETE ON evidence_source_occurrences BEGIN
        SELECT RAISE(ABORT, 'evidence source occurrences are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_sets_immutable_update
    BEFORE UPDATE ON evidence_occurrence_sets BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence sets are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_sets_immutable_delete
    BEFORE DELETE ON evidence_occurrence_sets BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence sets are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_set_members_immutable_update
    BEFORE UPDATE ON evidence_occurrence_set_members BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence set membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_set_members_immutable_delete
    BEFORE DELETE ON evidence_occurrence_set_members BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence set membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_spans_immutable_update
    BEFORE UPDATE ON evidence_spans BEGIN
        SELECT RAISE(ABORT, 'evidence spans are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_spans_immutable_delete
    BEFORE DELETE ON evidence_spans BEGIN
        SELECT RAISE(ABORT, 'evidence spans are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_members_immutable_update
    BEFORE UPDATE ON evidence_span_members BEGIN
        SELECT RAISE(ABORT, 'evidence span membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_members_immutable_delete
    BEFORE DELETE ON evidence_span_members BEGIN
        SELECT RAISE(ABORT, 'evidence span membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_projection_receipts_immutable_update
    BEFORE UPDATE ON evidence_span_projection_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence projection receipts are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_projection_receipts_immutable_delete
    BEFORE DELETE ON evidence_span_projection_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence projection receipts are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_retriever_contributions_immutable_update
    BEFORE UPDATE ON evidence_retriever_contributions BEGIN
        SELECT RAISE(ABORT, 'retriever contributions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_retriever_contributions_immutable_delete
    BEFORE DELETE ON evidence_retriever_contributions BEGIN
        SELECT RAISE(ABORT, 'retriever contributions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_derived_anchors_immutable_update
    BEFORE UPDATE ON evidence_derived_anchors BEGIN
        SELECT RAISE(ABORT, 'evidence derived anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_derived_anchors_immutable_delete
    BEFORE DELETE ON evidence_derived_anchors BEGIN
        SELECT RAISE(ABORT, 'evidence derived anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_assembly_receipts_immutable_update
    BEFORE UPDATE ON evidence_assembly_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence assembly receipts are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_assembly_receipts_immutable_delete
    BEFORE DELETE ON evidence_assembly_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence assembly receipts are immutable');
    END;
";

pub(crate) async fn install_evidence_assembly_schema(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
    super::retrieval_anchor_schema::install_retrieval_anchor_schema(conn, operation).await?;
    conn.execute_batch(EVIDENCE_ASSEMBLY_SCHEMA)
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("{operation}: failed to install evidence assembly schema: {error}"),
            operation: operation.to_owned(),
        })?;
    conn.execute_batch(EVIDENCE_ASSEMBLY_IMMUTABILITY)
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("{operation}: failed to install evidence immutability: {error}"),
            operation: operation.to_owned(),
        })?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
             source_anchor_id, owner_json, derivative_kind, derivative_id, direct_evidence
         )
         SELECT occurrence.source_anchor_id, anchor.owner_json,
                'span', span.span_id, 1
         FROM evidence_spans AS span
         JOIN evidence_span_members AS member
           ON member.span_id = span.span_id
         JOIN evidence_source_occurrences AS occurrence
           ON occurrence.occurrence_id = member.occurrence_id
         JOIN retrieval_anchors AS anchor
           ON anchor.anchor_id = occurrence.source_anchor_id;

         INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage (
             source_anchor_id, owner_json, derivative_kind, derivative_id, direct_evidence
         )
         SELECT occurrence.source_anchor_id, anchor.owner_json,
                'contribution', contribution.contribution_id, 1
         FROM evidence_retriever_contributions AS contribution
         JOIN evidence_span_members AS member
           ON member.span_id = contribution.span_id
         JOIN evidence_source_occurrences AS occurrence
           ON occurrence.occurrence_id = member.occurrence_id
         JOIN retrieval_anchors AS anchor
           ON anchor.anchor_id = occurrence.source_anchor_id;",
    )
    .await
    .map_err(|error| TraceDecayError::Database {
        message: format!("{operation}: failed to replay evidence anchor lineage: {error}"),
        operation: operation.to_owned(),
    })?;
    Ok(())
}

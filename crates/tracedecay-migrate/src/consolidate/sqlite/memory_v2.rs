use tracedecay_runtime_core::db::engine::Executor;
use tracedecay_runtime_core::db::{
    MemoryV2ArchiveDatabase, export_memory_v2_owner_archive, import_memory_v2_owner_archive,
    list_memory_v2_archive_owners, plan_memory_v2_owner_archive_import,
};

use serde::{Deserialize, Serialize};
use tracedecay_domain::FactOwnerV1;
use tracedecay_runtime_core::errors::Result;
use tracedecay_store::MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1;

use super::{db_error, db_message, query_i64};

#[derive(Clone, Copy)]
pub(super) enum LegacyMappingPolicy {
    PreserveSourceRows,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryV2ArchiveMergeProof {
    pub owner: FactOwnerV1,
    pub schema: String,
    pub source_digest: String,
    pub target_digest: String,
}

/// Transfers every owner-scoped Memory V2 closure through the typed archive
/// port. Planning observes the target before mutation, import preserves exact
/// stable identities, and readback verification occurs in the same target
/// transaction.
pub(super) async fn merge_memory_v2_owner_archives(
    conn: &(impl Executor + Sync),
) -> Result<Vec<MemoryV2ArchiveMergeProof>> {
    if !source_has_memory_v2(conn).await? {
        return Ok(Vec::new());
    }
    let owners = list_memory_v2_archive_owners(conn, MemoryV2ArchiveDatabase::Source).await?;
    let mut proofs = Vec::with_capacity(owners.len());
    for owner in owners {
        let archive =
            export_memory_v2_owner_archive(conn, MemoryV2ArchiveDatabase::Source, &owner).await?;
        let plan = plan_memory_v2_owner_archive_import(conn, &archive).await?;
        import_memory_v2_owner_archive(conn, &archive, &plan).await?;
        let target =
            export_memory_v2_owner_archive(conn, MemoryV2ArchiveDatabase::Main, &owner).await?;
        proofs.push(MemoryV2ArchiveMergeProof {
            owner,
            schema: MEMORY_V2_OWNER_ARCHIVE_SCHEMA_V1.to_owned(),
            source_digest: archive
                .digest()
                .map_err(|error| db_message("memory_v2_owner_archive", error.to_string()))?
                .as_str()
                .to_owned(),
            target_digest: target
                .digest()
                .map_err(|error| db_message("memory_v2_owner_archive", error.to_string()))?
                .as_str()
                .to_owned(),
        });
    }
    Ok(proofs)
}

/// Unions the source shard's `memory_v2` durable authority into the attached
/// target graph connection. Every carried table is owner-bound (fact ids embed
/// the owner digest), so rows are unioned by their identity columns with
/// `INSERT OR IGNORE` — never renumbered — mirroring the observation-authority
/// merge. The one exception is the derived `memory_v2_current_facts`
/// projection, which is reconciled with deletion terminality so a tombstoned
/// fact in either shard is never resurrected by an older live copy from the
/// other shard.
///
/// The connection must already have `source` attached and
/// `PRAGMA defer_foreign_keys = ON` set (as `merge_registered_graph_facts`
/// does), because the union crosses foreign keys between the durable tables.
pub(super) async fn merge_memory_v2_authority(
    conn: &impl Executor,
    legacy_mapping_policy: LegacyMappingPolicy,
) -> Result<()> {
    if !source_has_memory_v2(conn).await? {
        return Ok(());
    }
    reject_incompatible_authority(conn).await?;

    // Retrieval anchors back memory_v2 evidence and must land first. These are
    // the graph-side anchor tables (distinct from the sessions-db anchors the
    // observation-authority merge unions).
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchors(
             anchor_id, anchor_json, owner_json, projection_generation
         )
         SELECT anchor_id, anchor_json, owner_json, projection_generation
         FROM source.retrieval_anchors;

         INSERT OR IGNORE INTO retrieval_anchor_aliases(
             owner_json, alias_kind, locator_digest, anchor_id
         )
         SELECT owner_json, alias_kind, locator_digest, anchor_id
         FROM source.retrieval_anchor_aliases;

         INSERT OR IGNORE INTO retrieval_anchor_dispositions(
             disposition_id, anchor_id, owner_json, state, superseded_by,
             reason_class, effective_at, record_json
         )
         SELECT disposition_id, anchor_id, owner_json, state, superseded_by,
                reason_class, effective_at, record_json
         FROM source.retrieval_anchor_dispositions
         ORDER BY sequence;

         INSERT OR IGNORE INTO retrieval_anchor_reverse_lineage(
             source_anchor_id, owner_json, derivative_kind, derivative_id,
             direct_evidence
         )
         SELECT source_anchor_id, owner_json, derivative_kind, derivative_id,
                direct_evidence
         FROM source.retrieval_anchor_reverse_lineage;

         INSERT OR IGNORE INTO retrieval_anchor_derivative_tombstones(
             source_anchor_id, owner_json, derivative_kind, derivative_id,
             disposition_id, effective_at
         )
         SELECT source_anchor_id, owner_json, derivative_kind, derivative_id,
                disposition_id, effective_at
         FROM source.retrieval_anchor_derivative_tombstones;

         INSERT OR IGNORE INTO evidence_source_occurrences(
             occurrence_id, owner_digest, timeline_digest, source_anchor_id,
             source_order, record_digest, record_json
         )
         SELECT occurrence_id, owner_digest, timeline_digest, source_anchor_id,
                source_order, record_digest, record_json
         FROM source.evidence_source_occurrences;

         INSERT OR IGNORE INTO evidence_occurrence_sets(
             occurrence_set_id, owner_digest, record_digest, record_json
         )
         SELECT occurrence_set_id, owner_digest, record_digest, record_json
         FROM source.evidence_occurrence_sets;

         INSERT OR IGNORE INTO evidence_occurrence_set_members(
             occurrence_set_id, canonical_ordinal, occurrence_id
         )
         SELECT occurrence_set_id, canonical_ordinal, occurrence_id
         FROM source.evidence_occurrence_set_members;

         INSERT OR IGNORE INTO evidence_spans(
             span_id, owner_digest, occurrence_set_id, anchor_id, producer_kind,
             record_digest, record_json
         )
         SELECT span_id, owner_digest, occurrence_set_id, anchor_id, producer_kind,
                record_digest, record_json
         FROM source.evidence_spans;

         INSERT OR IGNORE INTO evidence_span_members(
             span_id, assembly_ordinal, run_ordinal, run_member_ordinal, occurrence_id
         )
         SELECT span_id, assembly_ordinal, run_ordinal, run_member_ordinal, occurrence_id
         FROM source.evidence_span_members;

         INSERT OR IGNORE INTO evidence_span_projection_receipts(
             projection_receipt_id, span_id, record_digest, record_json
         )
         SELECT projection_receipt_id, span_id, record_digest, record_json
         FROM source.evidence_span_projection_receipts;

         INSERT OR IGNORE INTO evidence_retriever_contributions(
             contribution_id, owner_digest, span_id, anchor_id, record_digest,
             record_json
         )
         SELECT contribution_id, owner_digest, span_id, anchor_id, record_digest,
                record_json
         FROM source.evidence_retriever_contributions;

         INSERT OR IGNORE INTO evidence_derived_anchors(
             anchor_id, owner_digest, target_kind, target_id, anchor_json
         )
         SELECT anchor_id, owner_digest, target_kind, target_id, anchor_json
         FROM source.evidence_derived_anchors;

         INSERT OR IGNORE INTO evidence_assembly_receipts(
             publication_receipt_id, owner_digest, privacy_domain_id, key_epoch,
             idempotency_key, assembly_digest, occurrence_set_id, span_id,
             contribution_id, projection_receipt_id, receipt_json
         )
         SELECT publication_receipt_id, owner_digest, privacy_domain_id, key_epoch,
                idempotency_key, assembly_digest, occurrence_set_id, span_id,
                contribution_id, projection_receipt_id, receipt_json
         FROM source.evidence_assembly_receipts;

         INSERT OR IGNORE INTO memory_v2_facts(
             fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         )
         SELECT fact_id, owner_kind, project_id, owner_json, identity_json, created_at
         FROM source.memory_v2_facts;

         INSERT OR IGNORE INTO memory_v2_lineage_events(
             event_id, fact_id, owner_kind, project_id, event_json,
             occurred_at, recorded_at
         )
         SELECT event_id, fact_id, owner_kind, project_id, event_json,
                occurred_at, recorded_at
         FROM source.memory_v2_lineage_events
         ORDER BY event_sequence;

         INSERT OR IGNORE INTO memory_v2_assertions(
             assertion_id, fact_id, owner_kind, project_id, owner_json,
             assertion_header_json, kind_json, payload_reference_json,
             receipt_json, asserted_at, actor_id
         )
         SELECT assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
         FROM source.memory_v2_assertions;

         INSERT OR IGNORE INTO memory_v2_assertion_supersession(
             assertion_id, fact_id, owner_kind, project_id,
             superseded_assertion_id, ordinal
         )
         SELECT assertion_id, fact_id, owner_kind, project_id,
                superseded_assertion_id, ordinal
         FROM source.memory_v2_assertion_supersession;

         INSERT OR IGNORE INTO memory_v2_assertion_payloads(
             assertion_id, fact_id, owner_kind, project_id, payload_json, content
         )
         SELECT assertion_id, fact_id, owner_kind, project_id, payload_json, content
         FROM source.memory_v2_assertion_payloads;

         INSERT OR IGNORE INTO memory_v2_assertion_vectors(
             assertion_id, fact_id, owner_kind, project_id, vector, algebra,
             dimensions, precision
         )
         SELECT assertion_id, fact_id, owner_kind, project_id, vector, algebra,
                dimensions, precision
         FROM source.memory_v2_assertion_vectors;

         INSERT OR IGNORE INTO memory_v2_evidence(
             evidence_id, fact_id, owner_kind, project_id, owner_json,
             anchor_id, evidence_json
         )
         SELECT evidence_id, fact_id, owner_kind, project_id, owner_json,
                anchor_id, evidence_json
         FROM source.memory_v2_evidence;

         INSERT OR IGNORE INTO memory_v2_assertion_evidence(
             assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
         )
         SELECT assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
         FROM source.memory_v2_assertion_evidence;

         INSERT OR IGNORE INTO memory_v2_feedback_history(
             owner_kind, project_id, fact_id, event_id, action, old_trust,
             new_trust, occurred_at, source, note, details_availability
         )
         SELECT owner_kind, project_id, fact_id, event_id, action, old_trust,
                new_trust, occurred_at, source, note, details_availability
         FROM source.memory_v2_feedback_history;

         INSERT OR IGNORE INTO memory_v2_fact_relations(
             owner_kind, project_id, source_fact_id, target_fact_id, relation,
             confidence, source_label, provenance_json, evidence_fact_ids_json,
             occurred_at, updated_at
         )
         SELECT owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, provenance_json, evidence_fact_ids_json,
                occurred_at, updated_at
         FROM source.memory_v2_fact_relations;

         INSERT OR IGNORE INTO memory_v2_proposals(
             proposal_id, owner_kind, project_id, owner_json, idempotency_key,
             request_digest, request_json, evidence_json, submitted_at
         )
         SELECT proposal_id, owner_kind, project_id, owner_json, idempotency_key,
                request_digest, request_json, evidence_json, submitted_at
         FROM source.memory_v2_proposals;

         INSERT OR IGNORE INTO memory_v2_proposal_transitions(
             transition_id, proposal_id, owner_kind, project_id, previous_state,
             current_state, reviewer_json, validation_json, origin,
             promoted_fact_id, promoted_assertion_id, promoted_event_id,
             transition_json, occurred_at
         )
         SELECT transition_id, proposal_id, owner_kind, project_id, previous_state,
                current_state, reviewer_json, validation_json, origin,
                promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
         FROM source.memory_v2_proposal_transitions
         ORDER BY transition_sequence;

         INSERT OR IGNORE INTO memory_v2_proposal_current(
             proposal_id, owner_kind, project_id, state, revision,
             last_transition_id, updated_at
         )
         SELECT proposal_id, owner_kind, project_id, state, revision,
                last_transition_id, updated_at
         FROM source.memory_v2_proposal_current;

         INSERT OR IGNORE INTO memory_v2_legacy_proposal_map(
             owner_kind, project_id, source_store_id, legacy_proposal_id,
             proposal_id, history_coverage, import_receipt_json, imported_at
         )
         SELECT owner_kind, project_id, source_store_id, legacy_proposal_id,
                proposal_id, history_coverage, import_receipt_json, imported_at
         FROM source.memory_v2_legacy_proposal_map;

         INSERT OR IGNORE INTO memory_v2_legacy_feedback_event_map(
             owner_kind, project_id, source_store_id, legacy_feedback_event_id,
             fact_id, event_id
         )
         SELECT owner_kind, project_id, source_store_id, legacy_feedback_event_id,
                fact_id, event_id
         FROM source.memory_v2_legacy_feedback_event_map;

         INSERT OR IGNORE INTO memory_v2_legacy_quarantine(
             owner_kind, project_id, source_store_id, source_table,
             source_row_id, reason_code, recorded_at
         )
         SELECT owner_kind, project_id, source_store_id, source_table,
                source_row_id, reason_code, recorded_at
         FROM source.memory_v2_legacy_quarantine;

         INSERT OR IGNORE INTO memory_v2_compatibility_operation_receipts(
             owner_kind, project_id, operation_id, operation_kind, request_digest,
             fact_id, event_id, receipt_json, recorded_at
         )
         SELECT owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
         FROM source.memory_v2_compatibility_operation_receipts;",
    )
    .await
    .map_err(|error| db_error("merge_memory_v2_authority", error))?;

    if matches!(
        legacy_mapping_policy,
        LegacyMappingPolicy::PreserveSourceRows
    ) {
        conn.execute_batch(
            "INSERT OR IGNORE INTO memory_v2_legacy_map(
                 owner_kind, project_id, owner_json, source_store_id,
                 legacy_fact_id, fact_id, mapping_json
             )
             SELECT owner_kind, project_id, owner_json, source_store_id,
                    legacy_fact_id, fact_id, mapping_json
             FROM source.memory_v2_legacy_map;",
        )
        .await
        .map_err(|error| db_error("merge_memory_v2_legacy_map", error))?;
    }

    merge_current_facts_with_deletion_terminality(conn).await?;
    reconcile_memory_v2_derived_state(conn).await?;
    verify_memory_v2_authority_coverage(conn).await
}

async fn reject_incompatible_authority(conn: &impl Executor) -> Result<()> {
    let conflicts = [
        (
            "fact identity",
            "SELECT COUNT(*)
             FROM source.memory_v2_facts AS source_row
             JOIN memory_v2_facts AS target_row
               ON target_row.fact_id=source_row.fact_id
              AND target_row.owner_kind=source_row.owner_kind
              AND target_row.project_id=source_row.project_id
             WHERE target_row.owner_json IS NOT source_row.owner_json
                OR target_row.identity_json IS NOT source_row.identity_json
                OR target_row.created_at IS NOT source_row.created_at",
        ),
        (
            "assertion identity",
            "SELECT COUNT(*)
             FROM source.memory_v2_assertions AS source_row
             JOIN memory_v2_assertions AS target_row
               ON target_row.assertion_id=source_row.assertion_id
              AND target_row.fact_id=source_row.fact_id
              AND target_row.owner_kind=source_row.owner_kind
              AND target_row.project_id=source_row.project_id
             WHERE target_row.owner_json IS NOT source_row.owner_json
                OR target_row.assertion_header_json IS NOT source_row.assertion_header_json
                OR target_row.kind_json IS NOT source_row.kind_json
                OR target_row.payload_reference_json IS NOT source_row.payload_reference_json
                OR target_row.receipt_json IS NOT source_row.receipt_json
                OR target_row.asserted_at IS NOT source_row.asserted_at
                OR target_row.actor_id IS NOT source_row.actor_id",
        ),
        (
            "assertion payload",
            "SELECT COUNT(*)
             FROM source.memory_v2_assertion_payloads AS source_row
             JOIN memory_v2_assertion_payloads AS target_row
               ON target_row.assertion_id=source_row.assertion_id
              AND target_row.fact_id=source_row.fact_id
              AND target_row.owner_kind=source_row.owner_kind
              AND target_row.project_id=source_row.project_id
             WHERE target_row.payload_json IS NOT source_row.payload_json
                OR target_row.content IS NOT source_row.content",
        ),
        (
            "lineage event",
            "SELECT COUNT(*)
             FROM source.memory_v2_lineage_events AS source_row
             JOIN memory_v2_lineage_events AS target_row
               ON target_row.event_id=source_row.event_id
              AND target_row.fact_id=source_row.fact_id
              AND target_row.owner_kind=source_row.owner_kind
              AND target_row.project_id=source_row.project_id
             WHERE target_row.event_json IS NOT source_row.event_json
                OR target_row.occurred_at IS NOT source_row.occurred_at
                OR target_row.recorded_at IS NOT source_row.recorded_at",
        ),
        (
            "evidence identity",
            "SELECT COUNT(*)
             FROM source.memory_v2_evidence AS source_row
             JOIN memory_v2_evidence AS target_row
               ON target_row.evidence_id=source_row.evidence_id
              AND target_row.fact_id=source_row.fact_id
              AND target_row.owner_kind=source_row.owner_kind
              AND target_row.project_id=source_row.project_id
             WHERE target_row.owner_json IS NOT source_row.owner_json
                OR target_row.anchor_id IS NOT source_row.anchor_id
                OR target_row.evidence_json IS NOT source_row.evidence_json",
        ),
    ];
    for (label, sql) in conflicts {
        if query_i64(conn, sql).await? != 0 {
            return Err(db_message(
                "merge_memory_v2_authority",
                format!("memory_v2 {label} conflict for an existing stable identity"),
            ));
        }
    }
    Ok(())
}

async fn verify_memory_v2_authority_coverage(conn: &impl Executor) -> Result<()> {
    let missing = [
        (
            "facts",
            "SELECT COUNT(*) FROM source.memory_v2_facts AS source_row
             WHERE NOT EXISTS(
                 SELECT 1 FROM memory_v2_facts AS target_row
                 WHERE target_row.fact_id=source_row.fact_id
                   AND target_row.owner_kind=source_row.owner_kind
                   AND target_row.project_id=source_row.project_id
             )",
        ),
        (
            "assertions",
            "SELECT COUNT(*) FROM source.memory_v2_assertions AS source_row
             WHERE NOT EXISTS(
                 SELECT 1 FROM memory_v2_assertions AS target_row
                 WHERE target_row.assertion_id=source_row.assertion_id
                   AND target_row.fact_id=source_row.fact_id
                   AND target_row.owner_kind=source_row.owner_kind
                   AND target_row.project_id=source_row.project_id
             )",
        ),
        (
            "lineage events",
            "SELECT COUNT(*) FROM source.memory_v2_lineage_events AS source_row
             WHERE NOT EXISTS(
                 SELECT 1 FROM memory_v2_lineage_events AS target_row
                 WHERE target_row.event_id=source_row.event_id
                   AND target_row.fact_id=source_row.fact_id
                   AND target_row.owner_kind=source_row.owner_kind
                   AND target_row.project_id=source_row.project_id
             )",
        ),
        (
            "evidence",
            "SELECT COUNT(*) FROM source.memory_v2_evidence AS source_row
             WHERE NOT EXISTS(
                 SELECT 1 FROM memory_v2_evidence AS target_row
                 WHERE target_row.evidence_id=source_row.evidence_id
                   AND target_row.fact_id=source_row.fact_id
                   AND target_row.owner_kind=source_row.owner_kind
                   AND target_row.project_id=source_row.project_id
             )",
        ),
        (
            "feedback history",
            "SELECT COUNT(*) FROM source.memory_v2_feedback_history AS source_row
             WHERE NOT EXISTS(
                 SELECT 1 FROM memory_v2_feedback_history AS target_row
                 WHERE target_row.event_id=source_row.event_id
                   AND target_row.fact_id=source_row.fact_id
                   AND target_row.owner_kind=source_row.owner_kind
                   AND target_row.project_id=source_row.project_id
             )",
        ),
        (
            "current facts",
            "SELECT COUNT(*) FROM source.memory_v2_current_facts AS source_row
             WHERE NOT EXISTS(
                 SELECT 1 FROM memory_v2_current_facts AS target_row
                 WHERE target_row.fact_id=source_row.fact_id
                   AND target_row.owner_kind=source_row.owner_kind
                   AND target_row.project_id=source_row.project_id
                   AND (
                       source_row.payload_access <> 'deleted'
                       OR target_row.payload_access = 'deleted'
                   )
             )",
        ),
    ];
    for (label, sql) in missing {
        let count = query_i64(conn, sql).await?;
        if count != 0 {
            return Err(db_message(
                "merge_memory_v2_authority",
                format!("{count} source memory_v2 {label} row(s) are absent from target"),
            ));
        }
    }
    Ok(())
}

/// Reconciles the derived `memory_v2_current_facts` projection across the two
/// shards. A `deleted` payload access is terminal: a tombstone from either
/// shard must win over a live copy from the other, and no payload, FTS, or
/// vector row is re-materialized for it. Among rows that agree on liveness the
/// most recently updated projection wins, matching single-store recovery.
async fn merge_current_facts_with_deletion_terminality(conn: &impl Executor) -> Result<()> {
    conn.execute_batch(
        "INSERT INTO memory_v2_current_facts(
             fact_id, owner_kind, project_id, payload_access, trust_score,
             active_assertion_id, last_event_id, updated_at, retrieval_count,
             access_count, helpful_count, unhelpful_count, last_retrieved_at,
             last_recalled_at, last_feedback_at, projection_state,
             vector_watermark_json
         )
         SELECT fact_id, owner_kind, project_id, payload_access, trust_score,
                active_assertion_id, last_event_id, updated_at, retrieval_count,
                access_count, helpful_count, unhelpful_count, last_retrieved_at,
                last_recalled_at, last_feedback_at, projection_state,
                vector_watermark_json
         FROM source.memory_v2_current_facts AS s
         WHERE true
         ON CONFLICT(fact_id, owner_kind, project_id) DO UPDATE SET
             payload_access = excluded.payload_access,
             trust_score = excluded.trust_score,
             active_assertion_id = excluded.active_assertion_id,
             last_event_id = excluded.last_event_id,
             updated_at = excluded.updated_at,
             retrieval_count = excluded.retrieval_count,
             access_count = excluded.access_count,
             helpful_count = excluded.helpful_count,
             unhelpful_count = excluded.unhelpful_count,
             last_retrieved_at = excluded.last_retrieved_at,
             last_recalled_at = excluded.last_recalled_at,
             last_feedback_at = excluded.last_feedback_at,
             projection_state = excluded.projection_state,
             vector_watermark_json = excluded.vector_watermark_json
         WHERE
             -- An incoming tombstone always wins; a live incoming row can never
             -- overwrite an existing tombstone (deletion is terminal).
             (excluded.payload_access = 'deleted'
                 AND memory_v2_current_facts.payload_access <> 'deleted')
             OR (
                 (excluded.payload_access = 'deleted')
                     = (memory_v2_current_facts.payload_access = 'deleted')
                 AND excluded.updated_at > memory_v2_current_facts.updated_at
             );",
    )
    .await
    .map_err(|error| db_error("merge_memory_v2_current_facts", error))?;
    Ok(())
}

/// Re-establishes the runtime's canonical payload-purge and projection-dirty
/// invariants after the durable authorities from both shards have been
/// unioned. Source compatibility-bank projections are intentionally not
/// copied: their fact set is no longer authoritative after consolidation.
async fn reconcile_memory_v2_derived_state(conn: &impl Executor) -> Result<()> {
    conn.execute_batch(
        "PRAGMA secure_delete = ON;

         DELETE FROM memory_v2_assertion_vectors
         WHERE EXISTS(
             SELECT 1 FROM memory_v2_current_facts AS current
             WHERE current.fact_id = memory_v2_assertion_vectors.fact_id
               AND current.owner_kind = memory_v2_assertion_vectors.owner_kind
               AND current.project_id = memory_v2_assertion_vectors.project_id
               AND current.payload_access = 'deleted'
         );

         DELETE FROM memory_v2_assertion_payloads
         WHERE EXISTS(
             SELECT 1 FROM memory_v2_current_facts AS current
             WHERE current.fact_id = memory_v2_assertion_payloads.fact_id
               AND current.owner_kind = memory_v2_assertion_payloads.owner_kind
               AND current.project_id = memory_v2_assertion_payloads.project_id
               AND current.payload_access = 'deleted'
         );

         UPDATE memory_v2_feedback_history
         SET source = NULL, note = NULL,
             details_availability = CASE
                 WHEN details_availability = 'available' THEN 'legacy_redacted'
                 ELSE details_availability
             END
         WHERE EXISTS(
             SELECT 1 FROM memory_v2_current_facts AS current
             WHERE current.fact_id = memory_v2_feedback_history.fact_id
               AND current.owner_kind = memory_v2_feedback_history.owner_kind
               AND current.project_id = memory_v2_feedback_history.project_id
               AND current.payload_access = 'deleted'
         );

         UPDATE memory_v2_current_facts
         SET active_assertion_id = NULL,
             projection_state = 'unavailable',
             vector_watermark_json = NULL
         WHERE payload_access = 'deleted';

         UPDATE memory_v2_current_facts
         SET projection_state = 'rebuilding',
             vector_watermark_json = NULL
         WHERE payload_access <> 'deleted'
           AND EXISTS(
               SELECT 1 FROM source.memory_v2_facts AS source_fact
               WHERE source_fact.owner_kind = memory_v2_current_facts.owner_kind
                 AND source_fact.project_id = memory_v2_current_facts.project_id
           );

         INSERT INTO memory_v2_compatibility_bank_dirty(
             owner_kind, project_id, source_store_id, owner_json, bank_name,
             updated_at
         )
         SELECT source_fact.owner_kind, source_fact.project_id,
                'legacy-memory-v1', source_fact.owner_json, bank.bank_name,
                MAX(source_fact.created_at)
         FROM source.memory_v2_facts AS source_fact
         CROSS JOIN (
             SELECT 'all' AS bank_name
             UNION ALL SELECT 'general'
             UNION ALL SELECT 'user_pref'
             UNION ALL SELECT 'project'
             UNION ALL SELECT 'tool'
             UNION ALL SELECT 'decision'
             UNION ALL SELECT 'code_area'
         ) AS bank
         GROUP BY source_fact.owner_kind, source_fact.project_id,
                  source_fact.owner_json, bank.bank_name
         ON CONFLICT(owner_kind, project_id, source_store_id, bank_name)
         DO UPDATE SET
             owner_json = excluded.owner_json,
             updated_at = MAX(
                 excluded.updated_at,
                 memory_v2_compatibility_bank_dirty.updated_at + 1
             );",
    )
    .await
    .map_err(|error| db_error("reconcile_memory_v2_derived_state", error))?;
    Ok(())
}

async fn source_has_memory_v2(conn: &impl Executor) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM source.sqlite_master
             WHERE type = 'table' AND name = 'memory_v2_facts'",
            (),
        )
        .await
        .map_err(|error| db_error("merge_memory_v2_probe", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| db_error("merge_memory_v2_probe", error))?
        .is_some())
}

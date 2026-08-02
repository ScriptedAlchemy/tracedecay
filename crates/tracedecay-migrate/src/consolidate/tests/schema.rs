//! Schema-disposition coverage tests for the consolidation table map.

use super::*;

const EXTERNAL_SOURCE_DISPOSITION: &str =
    "unioned by binding identity; divergent collisions rejected";

#[tokio::test]
async fn current_schema_tables_have_an_explicit_consolidation_disposition() {
    let fixture = fixture().await;
    let graph = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(tracedecay_runtime_core::config::DB_FILENAME);
    let sessions = fixture
        .profile
        .join("projects")
        .join(&fixture.source_id)
        .join(storage::SESSIONS_DB_FILENAME);

    let unknown_graph = unknown_tables(&graph, graph_table_disposition).await;
    let unknown_sessions = unknown_tables(&sessions, session_table_disposition).await;

    assert!(
        unknown_graph.is_empty(),
        "graph schema tables need an explicit consolidation disposition: {unknown_graph:?}"
    );
    assert!(
        unknown_sessions.is_empty(),
        "session schema tables need an explicit consolidation disposition: {unknown_sessions:?}"
    );

    for (store, disposition) in [
        (
            "graph",
            graph_table_disposition("external_source_states_v1"),
        ),
        (
            "session",
            session_table_disposition("external_source_states_v1"),
        ),
    ] {
        assert_eq!(
            disposition,
            Some(EXTERNAL_SOURCE_DISPOSITION),
            "{store} external_source_states_v1 disposition must select its executable \
             consolidation witness; a generic label does not substantiate merge SQL"
        );
    }
    external_source::assert_executable_union_witness().await;
}

async fn unknown_tables(path: &Path, classify: fn(&str) -> Option<&'static str>) -> Vec<String> {
    let (db, _) = test_open_read_only(path).await;
    let mut rows = db
        .conn()
        .query(
            "SELECT name FROM sqlite_schema
             WHERE type='table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            (),
        )
        .await
        .unwrap();
    let mut unknown = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        let name = row.get::<String>(0).unwrap();
        if classify(&name).is_none() {
            unknown.push(name);
        }
    }
    db.close();
    unknown
}

fn graph_table_disposition(table: &str) -> Option<&'static str> {
    match table {
        // Reducer state is copied by immutable binding identity. Equal rows
        // deduplicate; divergent histories fail closed because choosing either
        // state would discard durable receipts and projection effects.
        "external_source_states_v1" => Some(EXTERNAL_SOURCE_DISPOSITION),
        "memory_entities"
        | "memory_fact_entities"
        | "memory_fact_relations"
        | "memory_facts"
        | "memory_feedback_events"
        | "memory_oplog" => Some("merged"),
        "memory_bank_dirty" | "memory_banks" => Some("derived/rebuilt"),
        name if name == "memory_facts_fts" || name.starts_with("memory_facts_fts_") => {
            Some("derived/rebuilt")
        }
        // memory_v2 durable evidence set: unioned by owner-bound identity so
        // facts, assertions, evidence, lineage, proposals and their legacy
        // mappings survive consolidation (see merge_memory_v2_authority).
        "memory_v2_facts"
        | "memory_v2_assertions"
        | "memory_v2_assertion_evidence"
        | "memory_v2_assertion_payloads"
        | "memory_v2_assertion_vectors"
        | "memory_v2_assertion_supersession"
        | "memory_v2_evidence"
        | "memory_v2_fact_relations"
        | "memory_v2_lineage_events"
        | "memory_v2_feedback_history"
        | "memory_v2_proposals"
        | "memory_v2_proposal_transitions"
        | "memory_v2_proposal_current"
        | "memory_v2_legacy_map"
        | "memory_v2_legacy_proposal_map"
        | "memory_v2_legacy_feedback_event_map"
        | "memory_v2_legacy_quarantine"
        | "memory_v2_compatibility_operation_receipts"
        | "retrieval_anchors"
        | "retrieval_anchor_aliases"
        | "retrieval_anchor_derivative_tombstones"
        | "retrieval_anchor_dispositions"
        | "retrieval_anchor_reverse_lineage"
        // Plan 13 evidence-assembly ledger: payload-free membership/receipt
        // rows that consolidate with owner-bound retrieval evidence.
        | "evidence_assembly_receipts"
        | "evidence_derived_anchors"
        | "evidence_occurrence_set_members"
        | "evidence_occurrence_sets"
        | "evidence_retriever_contributions"
        | "evidence_source_occurrences"
        | "evidence_span_members"
        | "evidence_span_projection_receipts"
        | "evidence_spans" => Some("merged"),
        // Derived compatibility projections rebuilt from the merged lineage:
        // current_facts is re-derived with deletion terminality, banks are
        // marked dirty, and the assertion-payload FTS shadow follows the
        // canonical payload triggers. Owner-bound assertion vectors are copied
        // with their payload rows, then removed for terminal facts.
        "memory_v2_current_facts"
        | "memory_v2_compatibility_banks"
        | "memory_v2_compatibility_bank_dirty" => Some("derived/rebuilt"),
        name if name == "memory_v2_assertion_payloads_fts"
            || name.starts_with("memory_v2_assertion_payloads_fts_") =>
        {
            Some("derived/rebuilt")
        }
        // Per-store backfill/repair ledgers are target-local runtime cursors,
        // not durable evidence, so they are never carried across the merge.
        "memory_v2_backfill_progress" | "memory_v2_feedback_history_repair_progress" => {
            Some("target-local schema ledger")
        }
        // Code-graph tables are not flattened. Every source and target branch
        // database is copied intact into the destination branch topology.
        "edges" | "files" | "metadata" | "node_fingerprints" | "nodes" | "read_cache"
        | "redundancy_pairs" | "unresolved_refs" | "vectors" => Some("intentionally ignored"),
        name if name == "nodes_fts" || name.starts_with("nodes_fts_") => {
            Some("intentionally ignored")
        }
        _ => None,
    }
}

fn session_table_disposition(table: &str) -> Option<&'static str> {
    match table {
        "external_source_states_v1" => Some(EXTERNAL_SOURCE_DISPOSITION),
        "analytics_events"
        | "commit_sessions"
        | "configuration_access_rules"
        | "configuration_audit_events"
        | "configuration_audit_redaction_keys"
        | "configuration_change_plan_events"
        | "configuration_change_plan_operations"
        | "configuration_change_plans"
        | "configuration_component_activation_events"
        | "configuration_credential_references"
        | "configuration_entries"
        | "configuration_migration_quarantine"
        | "configuration_migration_receipts"
        | "configuration_mutation_receipts"
        | "configuration_revisions"
        | "configuration_source_bindings"
        | "configuration_topology_policies"
        | "configuration_topology_protected_refs"
        | "configuration_topology_roots"
        | "git_correlation_meta"
        | "git_index_preview_commitments"
        | "git_index_repository_quarantines"
        | "git_index_transaction_inputs"
        | "git_index_transaction_journals"
        | "git_index_transaction_receipts"
        | "lcm_external_payloads"
        | "lcm_gc_marks"
        | "lcm_gc_meta"
        | "lcm_lifecycle_state"
        | "lcm_maintenance_debt"
        | "lcm_raw_messages"
        | "lcm_summary_nodes"
        | "lcm_summary_sources"
        | "observations"
        | "observation_projection_aliases"
        | "observation_projection_dispositions"
        | "observation_projection_provenance"
        | "observation_repository_provenance"
        | "observation_retrieval_anchors"
        | "parse_offsets"
        | "projects"
        | "retrieval_anchor_aliases"
        | "retrieval_anchors"
        | "retrieval_anchor_derivative_tombstones"
        | "retrieval_anchor_dispositions"
        | "retrieval_anchor_reverse_lineage"
        | "sanitization_receipts"
        | "savings_ledger"
        | "session_backfill_meta"
        | "session_git_spans"
        | "session_messages"
        | "session_schema_migrations"
        | "sessions"
        | "source_cursor_advances"
        | "source_cursors"
        | "turns"
        | "workflow_agents"
        | "workflow_index_meta"
        | "workflow_runs"
        | "session_agents"
        | "session_agent_hierarchy_edges"
        | "session_assertions"
        | "session_assertion_supersession"
        | "session_current_entities"
        | "session_derived_evidence"
        | "session_derived_evidence_members"
        | "session_external_payload_manifests"
        | "session_logical_copy_edges"
        | "session_occurrences"
        | "session_query_cursor_keys"
        | "session_refresh_batch_bindings"
        | "session_refresh_bindings"
        | "session_refresh_operations"
        | "session_refresh_progress"
        | "session_refresh_receipts"
        | "session_summary_availability"
        | "session_summary_nodes"
        | "session_summary_sources"
        | "session_summary_successors"
        | "session_temporal_generations"
        | "session_temporal_migration_dispositions"
        | "session_temporal_migration_receipts"
        | "session_temporal_observation_effects"
        | "session_temporal_projection_receipts"
        | "session_threads"
        | "session_thread_hierarchy_edges"
        | "session_turn_members"
        | "session_turns" => Some("merged"),
        // Legacy dashboard token counts are a disposable derived cache. The
        // runtime no longer owns this table, so consolidation accepts old
        // inputs but deliberately does not materialize it in the destination.
        "dashboard_token_counts" => Some("legacy derived cache discarded"),
        "authority_audit_checkpoints"
        // Resumable foreign-key audit cursor is scoped to the source store's
        // table order. The consolidated target restarts its own audit.
        | "authority_foreign_key_audit_progress"
        | "global_schema_migrations"
        // Resumable-backfill watermarks scoped to one store's own sequences; a
        // consolidated target re-derives its own from the merged observations.
        | "observation_backfill_watermarks"
        | "session_temporal_schema_migrations" => Some("target-local schema ledger"),
        name if name == "session_occurrences_fts"
            || name.starts_with("session_occurrences_fts_")
            || name == "session_summary_nodes_fts"
            || name.starts_with("session_summary_nodes_fts_") =>
        {
            Some("derived/rebuilt")
        }
        "observation_projection_checkpoints"
        | "observation_projection_rebuild_aliases"
        | "observation_projection_rebuild_dispositions"
        | "observation_projection_rebuild_messages"
        | "observation_projection_rebuild_provenance"
        | "observation_projection_rebuild_sessions"
        | "observation_projection_rebuild_workflow_facts"
        | "observation_projection_rebuilds"
        | "observation_workflow_facts"
        | "projection_queue" => Some("derived/rebuilt"),
        "code_projects" | "graph_scopes" | "project_aliases" | "store_artifacts"
        | "store_instances" => Some("rejected registry-only"),
        name if name == "lcm_raw_messages_fts"
            || name.starts_with("lcm_raw_messages_fts_")
            || name == "lcm_summary_nodes_fts"
            || name.starts_with("lcm_summary_nodes_fts_")
            || name == "session_messages_fts"
            || name.starts_with("session_messages_fts_") =>
        {
            Some("derived/rebuilt")
        }
        _ => None,
    }
}

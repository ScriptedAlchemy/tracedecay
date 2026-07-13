//! Stable, content-free storage compatibility descriptors.
//!
//! This module deliberately inventories source-relative contracts. It never
//! opens a database, walks a profile, or serializes a discovered private path.

use super::model::{
    CompatibilityEntryV1, EntityDispositionV1, InventoryGatesV1, InventoryOwnersV1, RouteStatusV1,
};

mod source_families;

pub use source_families::{
    storage_source_family_appendix, validate_storage_source_family_appendix,
};

const STORE_READER: &str = "src/db/connection.rs::Database::open_read_only";
const STORE_WRITER: &str = "src/db/connection.rs::Database::open";
const SESSION_READER: &str = "src/global_db.rs::GlobalDb::open_read_only_at";
const SESSION_WRITER: &str = "src/global_db.rs::GlobalDb::open_at";
const STORAGE_TEST: &str = "src/migrate/consolidate/tests.rs::current_schema_tables_have_an_explicit_consolidation_disposition";
const CONSOLIDATION_TEST: &str =
    "src/migrate/consolidate/tests.rs::consolidation_restarts_after_every_durable_state";

const GRAPH_TABLES: &[(&str, &str)] = &[
    ("edges", "copied_per_branch"),
    ("files", "copied_per_branch"),
    ("memory_bank_dirty", "derived_rebuilt"),
    ("memory_banks", "derived_rebuilt"),
    ("memory_entities", "merged"),
    ("memory_fact_entities", "merged"),
    ("memory_fact_relations", "merged"),
    ("memory_facts", "merged"),
    ("memory_feedback_events", "merged"),
    ("memory_oplog", "merged"),
    ("metadata", "copied_per_branch"),
    ("node_fingerprints", "copied_per_branch"),
    ("nodes", "copied_per_branch"),
    ("read_cache", "copied_per_branch"),
    ("redundancy_pairs", "copied_per_branch"),
    ("unresolved_refs", "copied_per_branch"),
    ("vectors", "copied_per_branch"),
];

const SESSION_TABLES: &[(&str, &str)] = &[
    ("analytics_events", "merged"),
    ("code_projects", "rejected_registry_only"),
    ("commit_sessions", "merged"),
    ("dashboard_token_counts", "merged"),
    ("git_correlation_meta", "merged"),
    ("graph_scopes", "rejected_registry_only"),
    ("lcm_external_payloads", "merged"),
    ("lcm_gc_marks", "merged"),
    ("lcm_gc_meta", "merged"),
    ("lcm_lifecycle_state", "merged"),
    ("lcm_maintenance_debt", "merged"),
    ("lcm_raw_messages", "merged"),
    ("lcm_summary_nodes", "merged"),
    ("lcm_summary_sources", "merged_with_source_id_remap"),
    ("parse_offsets", "merged"),
    ("project_aliases", "rejected_registry_only"),
    ("projects", "merged"),
    ("savings_ledger", "merged"),
    ("session_backfill_meta", "merged"),
    ("session_git_spans", "merged"),
    ("session_messages", "merged"),
    ("session_schema_migrations", "merged"),
    ("sessions", "merged_with_variant_identity"),
    ("store_artifacts", "rejected_registry_only"),
    ("store_instances", "rejected_registry_only"),
    ("turns", "merged"),
    ("workflow_agents", "merged"),
    ("workflow_index_meta", "merged"),
    ("workflow_runs", "merged"),
];

const DERIVED_TABLES: &[&str] = &[
    "lcm_raw_messages_fts",
    "lcm_summary_nodes_fts",
    "memory_facts_fts",
    "nodes_fts",
    "session_messages_fts",
];

const INDEXES: &[&str] = &[
    "idx_analytics_events_kind",
    "idx_analytics_events_project_time",
    "idx_analytics_events_provider_project_session",
    "idx_analytics_events_timestamp",
    "idx_commit_sessions_branch",
    "idx_commit_sessions_session",
    "idx_edges_kind",
    "idx_edges_source",
    "idx_edges_source_kind",
    "idx_edges_target",
    "idx_edges_target_kind",
    "idx_edges_unique",
    "idx_graph_scopes_project_store",
    "idx_lcm_external_payloads_owner",
    "idx_lcm_maintenance_debt_kind",
    "idx_lcm_raw_session_id",
    "idx_lcm_raw_session_order",
    "idx_lcm_summary_nodes_session_depth_time",
    "idx_lcm_summary_sources_source",
    "idx_memory_banks_updated_at",
    "idx_memory_entities_type",
    "idx_memory_fact_entities_entity_id",
    "idx_memory_fact_relations_kind",
    "idx_memory_fact_relations_source",
    "idx_memory_fact_relations_target",
    "idx_memory_facts_category",
    "idx_memory_facts_source",
    "idx_memory_facts_trust_score",
    "idx_memory_facts_updated_at",
    "idx_memory_feedback_events_created_at",
    "idx_memory_feedback_events_fact_id",
    "idx_memory_oplog_ts",
    "idx_node_fingerprints_ast",
    "idx_node_fingerprints_size",
    "idx_nodes_file_path",
    "idx_nodes_file_path_start_line",
    "idx_nodes_kind",
    "idx_nodes_lower_name",
    "idx_nodes_name",
    "idx_nodes_parent_id",
    "idx_nodes_qualified_name",
    "idx_project_aliases_project_id",
    "idx_read_cache_session",
    "idx_redundancy_pairs_node_b",
    "idx_savings_ledger_project",
    "idx_savings_ledger_ts",
    "idx_session_git_spans_branch",
    "idx_session_git_spans_session",
    "idx_session_git_spans_worktree",
    "idx_session_messages_session",
    "idx_session_messages_source",
    "idx_session_messages_timestamp",
    "idx_sessions_parent",
    "idx_sessions_project",
    "idx_sessions_started_at",
    "idx_store_instances_project_id",
    "idx_turns_model",
    "idx_turns_project",
    "idx_turns_timestamp",
    "idx_unresolved_refs_file_path",
    "idx_unresolved_refs_from_node_id",
    "idx_unresolved_refs_reference_name",
    "idx_workflow_agents_run",
    "idx_workflow_runs_parent",
];

const TRIGGERS: &[&str] = &[
    "lcm_raw_messages_fts_delete",
    "lcm_raw_messages_fts_insert",
    "lcm_raw_messages_fts_update",
    "lcm_summary_nodes_fts_delete",
    "lcm_summary_nodes_fts_insert",
    "lcm_summary_nodes_fts_update",
    "memory_facts_fts_delete",
    "memory_facts_fts_insert",
    "memory_facts_fts_update",
    "nodes_fts_delete",
    "nodes_fts_insert",
    "nodes_fts_update",
    "session_messages_fts_delete",
    "session_messages_fts_insert",
    "session_messages_fts_update",
];

const LEDGER_STATES: &[&str] = &[
    "planned",
    "backups_ready",
    "destination_ready",
    "databases_merged",
    "artifacts_merged",
    "registered",
    "applied",
];

const CONSOLIDATION_OPERATIONS: &[(&str, &str)] = &[
    (
        "applied_manifest_retirement",
        "src/migrate/consolidate/mod.rs::retire_applied_input_manifests",
    ),
    (
        "collision_report",
        "src/migrate/consolidate/mod.rs::collision_summary",
    ),
    (
        "confirmation_fingerprint",
        "src/migrate/consolidate/mod.rs::confirmation_token",
    ),
    (
        "destination_verification",
        "src/migrate/consolidate/finalize.rs::verify_destination",
    ),
    (
        "doctor_recovery",
        "src/doctor.rs::database_recovery_guidance",
    ),
    ("holder_scan", "src/open_store_holders.rs::scan"),
    (
        "lcm_source_id_remap",
        "src/migrate/consolidate/sqlite.rs::build_consolidation_message_map",
    ),
    (
        "marker_cutover",
        "src/migrate/consolidate/finalize.rs::cut_over_markers",
    ),
    (
        "profile_offline_gate",
        "src/migrate/consolidate/preflight.rs::ensure_profile_offline",
    ),
    (
        "registry_publication",
        "src/migrate/consolidate/finalize.rs::register_destination",
    ),
    (
        "source_backup",
        "src/migrate/consolidate/mod.rs::backup_store",
    ),
    (
        "sqlite_write_reservations",
        "src/migrate/consolidate/sqlite/inspect.rs::acquire_offline_guards",
    ),
    (
        "store_write_locks",
        "src/migrate/consolidate/preflight.rs::acquire_store_locks",
    ),
    (
        "target_backup",
        "src/migrate/consolidate/mod.rs::backup_store",
    ),
];

const COLLISION_CLASSES: &[&str] = &[
    "artifact_path_overlap",
    "differing_artifact_path",
    "divergent_lcm_content_hash",
    "divergent_lcm_message",
    "divergent_lcm_payload_ref",
    "divergent_lcm_session_id",
    "divergent_lcm_storage_kind",
    "fact_content_overlap",
    "lcm_message_overlap",
    "message_overlap",
    "session_overlap",
];

const STAGING_STATES: &[&str] = &[
    "branch_meta_write",
    "publish",
    "source_branch",
    "target_copy",
];

const STORAGE_ARTIFACTS: &[(&str, &str)] = &[
    ("branch_add_lock", "projects/<project-id>/.branch-add.lock"),
    ("branch_metadata", "projects/<project-id>/branch-meta.json"),
    ("dashboard_artifacts", "projects/<project-id>/dashboard/**"),
    ("dirty_sentinel", "projects/<project-id>/dirty"),
    (
        "enrollment_marker",
        "<repository>/.tracedecay/enrollment.json",
    ),
    ("lcm_payloads", "projects/<project-id>/lcm-payloads/**"),
    (
        "repository_identity_marker",
        "<git-common-dir>/tracedecay-project.json",
    ),
    (
        "response_handles",
        "projects/<project-id>/response-handles/**",
    ),
    ("store_config", "projects/<project-id>/config.json"),
    (
        "store_manifest",
        "projects/<project-id>/store_manifest.json",
    ),
    ("sync_lock", "projects/<project-id>/sync.lock"),
];

const EXTERNAL_SOURCE_FAMILIES: &[(&str, &str, &str, &str)] = &[
    (
        "automation_skills_curation",
        "src/automation",
        "root/automation",
        "tracedecay-activity",
    ),
    (
        "hermes_kanban_task_graph",
        "external/hermes-kanban",
        "external/hermes-kanban",
        "tracedecay-task-graph",
    ),
    (
        "hermes_legacy",
        "src/migrate/hermes.rs",
        "root/migrate/hermes",
        "tracedecay-activity",
    ),
    (
        "hooks_hints_outcomes",
        "src/hooks",
        "root/hooks",
        "tracedecay-capture",
    ),
    (
        "lifecycle_lease_service_state",
        "src/lifecycle_lease.rs",
        "root/lifecycle",
        "root-composition",
    ),
    (
        "provider_transcripts",
        "src/sessions",
        "root/sessions",
        "tracedecay-capture",
    ),
    (
        "runtime_daemon_logs_crash_telemetry",
        "src/runtime_telemetry.rs",
        "root/runtime-telemetry",
        "tracedecay-observability",
    ),
];

/// Returns the canonical, deterministic storage slice of the V1 inventory.
///
/// The records are derived from checked-in runtime ownership points rather
/// than live stores, so output cannot contain user data or machine-local paths.
pub fn storage_entries() -> Vec<CompatibilityEntryV1> {
    let mut entries = Vec::new();

    for (name, source, readers, writers) in [
        (
            "branch_graph_database",
            "src/storage.rs::StoreLayout::graph_db_path",
            &[STORE_READER][..],
            &[STORE_WRITER][..],
        ),
        (
            "profile_global_database",
            "src/global_db.rs::global_db_path",
            &[SESSION_READER][..],
            &[SESSION_WRITER][..],
        ),
        (
            "project_graph_database",
            "src/storage.rs::StoreLayout::graph_db_path",
            &[STORE_READER][..],
            &[STORE_WRITER, "src/memory/store.rs::MemoryStore"][..],
        ),
        (
            "project_sessions_database",
            "src/storage.rs::StoreLayout::sessions_db_path",
            &[SESSION_READER][..],
            &[SESSION_WRITER][..],
        ),
        (
            "user_memory_database",
            "src/memory/user.rs::user_memory_db_path",
            &[STORE_READER][..],
            &[
                "src/memory/user.rs::open_user_memory_db",
                "src/memory/store.rs::MemoryStore",
            ][..],
        ),
        (
            "user_sessions_database",
            "src/sessions/mod.rs::user_sessions_db_path",
            &[SESSION_READER][..],
            &["src/sessions/mod.rs::open_user_session_db", SESSION_WRITER][..],
        ),
    ] {
        entries.push(entry(
            "store",
            name,
            &[source],
            RouteStatusV1::V1Only,
            readers,
            writers,
            &[STORAGE_TEST],
            "preserve the complete SQLite family and reopen through the canonical layout",
            "PR 37",
        ));
    }

    for &(name, disposition) in GRAPH_TABLES {
        entries.push(schema_entry(
            "table",
            name,
            "src/db/migrations.rs::create_schema",
            disposition,
            STORE_READER,
            STORE_WRITER,
        ));
    }
    for &(name, disposition) in SESSION_TABLES {
        entries.push(schema_entry(
            "table",
            name,
            session_schema_owner(name),
            disposition,
            SESSION_READER,
            SESSION_WRITER,
        ));
    }
    for &name in DERIVED_TABLES {
        let (source, reader, writer) = if name.starts_with("memory_") || name.starts_with("nodes_")
        {
            (
                "src/db/migrations.rs::create_schema",
                STORE_READER,
                STORE_WRITER,
            )
        } else {
            (session_schema_owner(name), SESSION_READER, SESSION_WRITER)
        };
        entries.push(schema_entry(
            "table",
            name,
            source,
            "derived_rebuilt",
            reader,
            writer,
        ));
    }
    for &name in INDEXES {
        let source = index_owner(name);
        let (reader, writer) = if source.starts_with("src/db/") {
            (STORE_READER, STORE_WRITER)
        } else {
            (SESSION_READER, SESSION_WRITER)
        };
        entries.push(schema_entry(
            "index",
            name,
            source,
            "derived_rebuilt",
            reader,
            writer,
        ));
    }
    for &name in TRIGGERS {
        let source = trigger_owner(name);
        let (reader, writer) = if source.starts_with("src/db/") {
            (STORE_READER, STORE_WRITER)
        } else {
            (SESSION_READER, SESSION_WRITER)
        };
        entries.push(schema_entry(
            "trigger",
            name,
            source,
            "derived_rebuilt",
            reader,
            writer,
        ));
    }

    for name in ["sqlite_shm", "sqlite_wal"] {
        entries.push(entry(
            "sidecar",
            name,
            &["src/migrate/consolidate/files.rs::sqlite_sidecar"],
            RouteStatusV1::MigrationOnly,
            &["src/sqlite_read_snapshot.rs::family_paths"],
            &["src/db/connection.rs::Database"],
            &["src/migrate/consolidate/tests.rs::sqlite_family_backup_includes_wal_and_shm"],
            "preserve main, WAL, and SHM as one recovery set",
            "PR 35",
        ));
    }
    for &(name, path) in STORAGE_ARTIFACTS {
        entries.push(entry(
            "storage_artifact",
            name,
            &["src/storage.rs::StoreLayout", path],
            RouteStatusV1::V1Only,
            &["src/storage.rs::StoreLayout"],
            &["src/storage.rs::PrivateStoreIo"],
            &["tests/storage_suite/storage_resolver_test.rs"],
            "preserve the artifact with its owning store family and validate its relative path",
            "PR 37",
        ));
    }
    for &(name, source, v1_owner, v2_owner) in EXTERNAL_SOURCE_FAMILIES {
        entries.push(source_family_entry(name, source, v1_owner, v2_owner));
    }

    for &(name, source) in CONSOLIDATION_OPERATIONS {
        entries.push(entry(
            "migration_operation",
            name,
            &[source],
            RouteStatusV1::MigrationOnly,
            &[source],
            &[source],
            &[CONSOLIDATION_TEST],
            operation_recovery(name),
            "PR 35",
        ));
    }
    for &name in LEDGER_STATES {
        entries.push(entry(
            "ledger_state",
            name,
            &["src/migrate/consolidate/mod.rs::ConsolidationState"],
            RouteStatusV1::MigrationOnly,
            &["src/migrate/consolidate/mod.rs::load_ledger"],
            &["src/migrate/consolidate/mod.rs::save_ledger"],
            &[CONSOLIDATION_TEST],
            "resume from the last durable state after revalidating frozen inputs",
            "PR 35",
        ));
    }
    for &name in STAGING_STATES {
        entries.push(entry(
            "staging_state",
            name,
            &["src/migrate/consolidate/prepare.rs::PrepareStop"],
            RouteStatusV1::MigrationOnly,
            &["src/migrate/consolidate/prepare.rs::validate_prepared_root"],
            &["src/migrate/consolidate/prepare.rs::prepare_destination_with_stop"],
            &["src/migrate/consolidate/tests.rs::destination_preparation_restarts_after_every_publish_boundary"],
            "discard only the private staging tree and retry from immutable inputs",
            "PR 35",
        ));
    }
    for &name in COLLISION_CLASSES {
        entries.push(entry(
            "collision_class",
            name,
            &["src/migrate/consolidate/mod.rs::CollisionSummary"],
            RouteStatusV1::MigrationOnly,
            &["src/migrate/consolidate/sqlite/inspect.rs::inspect_collisions"],
            &["src/migrate/consolidate/sqlite.rs::merge_sessions"],
            &["src/migrate/consolidate/tests.rs::divergent_projection_and_raw_content_preserve_a_linked_source_variant"],
            "preserve divergent variants or fail closed before publication",
            "PR 35",
        ));
    }

    entries.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
    entries
}

fn source_family_entry(
    name: &str,
    source: &str,
    v1_owner: &str,
    v2_owner: &str,
) -> CompatibilityEntryV1 {
    CompatibilityEntryV1 {
        stable_id: format!("storage:source_family_entry:{name}"),
        kind: "source_family".to_owned(),
        canonical_name: name.to_owned(),
        source_refs: vec![source.to_owned()],
        platform: "all".to_owned(),
        route_status: RouteStatusV1::V1Only,
        entity_disposition: EntityDispositionV1::Retained,
        platform_disposition: None,
        owners: InventoryOwnersV1 {
            v1_owner: v1_owner.to_owned(),
            v2_owner: v2_owner.to_owned(),
        },
        readers: vec![source.to_owned()],
        writers: Vec::new(),
        tests: vec![
            "src/compatibility_inventory/storage.rs::tests::source_family_appendix_covers_every_section_eight_family"
                .to_owned(),
        ],
        gates: InventoryGatesV1 {
            parity_gate: "PR 3R source-family inventory parity".to_owned(),
            cutover_gate: "PR 35 source-family migration cutover".to_owned(),
        },
        recovery: "retain the bounded V1 source read-only until import parity is proven".to_owned(),
        delete_by_pr: "PR 37".to_owned(),
    }
}

fn schema_entry(
    kind: &str,
    name: &str,
    source: &str,
    consolidation_disposition: &str,
    reader: &str,
    writer: &str,
) -> CompatibilityEntryV1 {
    let disposition = format!("consolidation_disposition:{consolidation_disposition}");
    entry(
        kind,
        name,
        &[source, disposition.as_str()],
        RouteStatusV1::V1Only,
        &[reader],
        &[writer],
        &[STORAGE_TEST],
        "restore the verified SQLite family; rebuild only explicitly derived objects",
        "PR 37",
    )
}

fn entry(
    kind: &str,
    name: &str,
    source_refs: &[&str],
    route_status: RouteStatusV1,
    readers: &[&str],
    writers: &[&str],
    tests: &[&str],
    recovery: &str,
    delete_by_pr: &str,
) -> CompatibilityEntryV1 {
    CompatibilityEntryV1 {
        stable_id: format!("storage:{kind}:{name}"),
        kind: kind.to_owned(),
        canonical_name: name.to_owned(),
        source_refs: names(source_refs.iter().copied()),
        platform: "all".to_owned(),
        route_status,
        entity_disposition: EntityDispositionV1::Retained,
        platform_disposition: None,
        owners: InventoryOwnersV1 {
            v1_owner: if route_status == RouteStatusV1::MigrationOnly {
                "root/migrate/consolidate"
            } else {
                "root/storage"
            }
            .to_owned(),
            v2_owner: "tracedecay-store".to_owned(),
        },
        readers: names(readers.iter().copied()),
        writers: names(writers.iter().copied()),
        tests: names(tests.iter().copied()),
        gates: InventoryGatesV1 {
            parity_gate: "PR 3R storage inventory parity".to_owned(),
            cutover_gate: "PR 35 storage migration cutover".to_owned(),
        },
        recovery: recovery.to_owned(),
        delete_by_pr: delete_by_pr.to_owned(),
    }
}

fn names<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut values = values.into_iter().map(str::to_owned).collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn session_schema_owner(name: &str) -> &'static str {
    if name.starts_with("lcm_") {
        "src/sessions/lcm/schema.rs::ensure_lcm_schema"
    } else if name.starts_with("workflow_") {
        "src/sessions/workflow_index.rs::ensure_workflow_index_schema"
    } else if matches!(
        name,
        "commit_sessions" | "git_correlation_meta" | "session_git_spans"
    ) {
        "src/sessions/git_correlation.rs::ensure_git_correlation_schema"
    } else if name == "session_backfill_meta" {
        "src/sessions/transcript_backfill.rs::ensure_backfill_meta_table"
    } else {
        "src/global_db.rs::GlobalDb::open_at_unsynchronized"
    }
}

fn index_owner(name: &str) -> &'static str {
    if name.starts_with("idx_lcm_") {
        "src/sessions/lcm/schema.rs::ensure_lcm_schema"
    } else if name.starts_with("idx_workflow_") {
        "src/sessions/workflow_index.rs::ensure_workflow_index_schema"
    } else if name.starts_with("idx_commit_sessions_") || name.starts_with("idx_session_git_spans_")
    {
        "src/sessions/git_correlation.rs::ensure_git_correlation_schema"
    } else if name.starts_with("idx_analytics_")
        || name.starts_with("idx_graph_scopes_")
        || name.starts_with("idx_project_aliases_")
        || name.starts_with("idx_savings_")
        || name.starts_with("idx_session_messages_")
        || name.starts_with("idx_sessions_")
        || name.starts_with("idx_store_instances_")
        || name.starts_with("idx_turns_")
    {
        "src/global_db.rs::GlobalDb::open_at_unsynchronized"
    } else {
        "src/db/migrations.rs::create_schema"
    }
}

fn trigger_owner(name: &str) -> &'static str {
    if name.starts_with("lcm_") {
        "src/sessions/lcm/schema.rs::ensure_lcm_schema"
    } else if name.starts_with("session_messages_") {
        "src/global_db.rs::GlobalDb::open_at_unsynchronized"
    } else {
        "src/db/migrations.rs::create_schema"
    }
}

fn operation_recovery(name: &str) -> &'static str {
    match name {
        "doctor_recovery" => {
            "preserve DB, WAL, SHM, and dirty sentinel together before offline repair"
        }
        "source_backup" | "target_backup" => {
            "retry without modifying either source; publication requires both verified backups"
        }
        "registry_publication" | "marker_cutover" => {
            "resume under the applied ledger and publish only the proven destination identity"
        }
        _ => "reacquire lifecycle, holder, reservation, and confirmation evidence before resuming",
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{
        COMPATIBILITY_INVENTORY_SCHEMA_V1, CompatibilityInventoryV1, InventorySummariesV1,
    };
    use super::*;

    #[test]
    fn descriptors_are_sorted_and_unique() {
        for (label, names) in [
            ("derived tables", DERIVED_TABLES),
            ("indexes", INDEXES),
            ("triggers", TRIGGERS),
            ("collision classes", COLLISION_CLASSES),
            ("staging states", STAGING_STATES),
        ] {
            assert!(
                names.windows(2).all(|pair| pair[0] < pair[1]),
                "{label} are not sorted"
            );
        }
        assert_eq!(
            LEDGER_STATES
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            LEDGER_STATES.len(),
            "ordered ledger states must remain unique"
        );
        for rows in [
            GRAPH_TABLES,
            SESSION_TABLES,
            CONSOLIDATION_OPERATIONS,
            STORAGE_ARTIFACTS,
        ] {
            assert!(rows.windows(2).all(|pair| pair[0].0 < pair[1].0));
        }
    }

    #[test]
    fn split_store_contract_has_every_durable_state_and_boundary() {
        assert_eq!(LEDGER_STATES.len(), 7);
        for required in [
            "holder_scan",
            "sqlite_write_reservations",
            "source_backup",
            "target_backup",
            "collision_report",
            "lcm_source_id_remap",
            "registry_publication",
            "marker_cutover",
            "doctor_recovery",
        ] {
            assert!(CONSOLIDATION_OPERATIONS.iter().any(|row| row.0 == required));
        }
    }

    #[test]
    fn storage_descriptors_never_contain_private_absolute_paths() {
        let values = GRAPH_TABLES
            .iter()
            .chain(SESSION_TABLES)
            .map(|row| row.0)
            .chain(DERIVED_TABLES.iter().copied())
            .chain(INDEXES.iter().copied())
            .chain(TRIGGERS.iter().copied())
            .chain(LEDGER_STATES.iter().copied())
            .chain(
                CONSOLIDATION_OPERATIONS
                    .iter()
                    .flat_map(|row| [row.0, row.1]),
            );
        assert!(values.into_iter().all(|value| !value.starts_with('/')));
    }

    #[test]
    fn generated_storage_slice_validates_and_uses_only_relative_descriptors() {
        let entries = storage_entries();
        let source_family_appendix = storage_source_family_appendix(&entries);
        let summaries = InventorySummariesV1::from_entries(&entries);
        let inventory = CompatibilityInventoryV1 {
            schema: COMPATIBILITY_INVENTORY_SCHEMA_V1.to_owned(),
            entries,
            source_family_appendix,
            summaries,
        };
        inventory.validate().unwrap();
        assert!(inventory.source_family_appendix.iter().all(|family| {
            family
                .relative_paths_or_globs
                .iter()
                .all(|path| !path.starts_with('/'))
        }));
    }
}

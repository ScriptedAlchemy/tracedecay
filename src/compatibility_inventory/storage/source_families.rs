use super::{
    DERIVED_TABLES, GRAPH_TABLES, INDEXES, SESSION_TABLES, TRIGGERS, index_owner, names,
    trigger_owner,
};
use crate::compatibility_inventory::model::{CompatibilityEntryV1, SourceFamilyAppendixEntryV1};

/// Returns source-family views whose paths are fixed placeholders, never
/// discovered paths. `entries` must be the result of `storage_entries()`.
pub fn storage_source_family_appendix(
    entries: &[CompatibilityEntryV1],
) -> Vec<SourceFamilyAppendixEntryV1> {
    let graph_tables = owned_graph_tables();
    let session_tables = owned_session_tables();
    let graph_indexes = owned_graph_indexes();
    let session_indexes = owned_session_indexes();
    let graph_triggers = owned_graph_triggers();
    let session_triggers = owned_session_triggers();
    let all_refs = |kind: &str| {
        names(
            entries
                .iter()
                .filter(|entry| entry.kind == kind)
                .map(|entry| entry.stable_id.as_str()),
        )
    };
    let mut families = vec![
        family(
            "consolidation_ledger",
            &["migration-inventory/consolidate_<id>.json"],
            &[],
            &[],
            &[],
            &[],
            all_refs("ledger_state"),
        ),
        family(
            "consolidation_source_backup",
            &["migration-backups/consolidate_<id>/<source-project-id>/**"],
            &[],
            &[],
            &[],
            &["*-shm", "*-wal"],
            vec!["storage:migration_operation:source_backup".to_owned()],
        ),
        family(
            "consolidation_staging",
            &["projects/.consolidate_<id>.staging/**"],
            &[],
            &[],
            &[],
            &["*-shm", "*-wal"],
            all_refs("staging_state"),
        ),
        family(
            "consolidation_source_input",
            &["projects/<source-project-id>/**"],
            &[],
            &[],
            &[],
            &["*-shm", "*-wal"],
            all_refs("ledger_state"),
        ),
        family(
            "consolidation_target_backup",
            &["migration-backups/consolidate_<id>/<target-project-id>/**"],
            &[],
            &[],
            &[],
            &["*-shm", "*-wal"],
            vec!["storage:migration_operation:target_backup".to_owned()],
        ),
        family(
            "consolidation_target_input",
            &["projects/<target-project-id>/**"],
            &[],
            &[],
            &[],
            &["*-shm", "*-wal"],
            vec![
                "storage:migration_operation:confirmation_fingerprint".to_owned(),
                "storage:migration_operation:holder_scan".to_owned(),
                "storage:migration_operation:sqlite_write_reservations".to_owned(),
            ],
        ),
        family(
            "profile_global_database",
            &["global.db"],
            &session_tables,
            &session_indexes,
            &session_triggers,
            &["global.db-shm", "global.db-wal"],
            vec!["storage:store:profile_global_database".to_owned()],
        ),
        family(
            "project_graph_database",
            &[
                "projects/<project-id>/*.db",
                "projects/<project-id>/tracedecay.db",
            ],
            &graph_tables,
            &graph_indexes,
            &graph_triggers,
            &[
                "*.db-shm",
                "*.db-wal",
                "tracedecay.db-shm",
                "tracedecay.db-wal",
            ],
            vec![
                "storage:store:branch_graph_database".to_owned(),
                "storage:store:project_graph_database".to_owned(),
            ],
        ),
        family(
            "project_sessions_database",
            &["projects/<project-id>/sessions.db"],
            &session_tables,
            &session_indexes,
            &session_triggers,
            &["sessions.db-shm", "sessions.db-wal"],
            vec!["storage:store:project_sessions_database".to_owned()],
        ),
        family(
            "project_store_artifacts",
            &[
                "<git-common-dir>/tracedecay-project.json",
                "<repository>/.tracedecay/enrollment.json",
                "projects/<project-id>/.branch-add.lock",
                "projects/<project-id>/branch-meta.json",
                "projects/<project-id>/config.json",
                "projects/<project-id>/dashboard/**",
                "projects/<project-id>/dirty",
                "projects/<project-id>/lcm-payloads/**",
                "projects/<project-id>/response-handles/**",
                "projects/<project-id>/store_manifest.json",
                "projects/<project-id>/sync.lock",
            ],
            &[],
            &[],
            &[],
            &[],
            all_refs("storage_artifact"),
        ),
        family(
            "user_memory_database",
            &["user-memory.db"],
            &graph_tables,
            &graph_indexes,
            &graph_triggers,
            &["user-memory.db-shm", "user-memory.db-wal"],
            vec!["storage:store:user_memory_database".to_owned()],
        ),
        family(
            "user_sessions_database",
            &["user-sessions.db"],
            &session_tables,
            &session_indexes,
            &session_triggers,
            &["user-sessions.db-shm", "user-sessions.db-wal"],
            vec!["storage:store:user_sessions_database".to_owned()],
        ),
    ];
    families.extend([
        family_owned(
            "repository_profile_identity",
            &[
                "<git-common-dir>/tracedecay-project.json",
                "<repository>/.tracedecay/enrollment.json",
                "global.db",
                "projects/<project-id>/store_manifest.json",
            ],
            &[],
            &[],
            &[],
            &[],
            "tracedecay-store",
            vec![
                "storage:storage_artifact:enrollment_marker".to_owned(),
                "storage:storage_artifact:repository_identity_marker".to_owned(),
                "storage:storage_artifact:store_manifest".to_owned(),
                "storage:store:profile_global_database".to_owned(),
            ],
        ),
        family_owned(
            "provider_transcripts",
            &[
                "<claude-home>/projects/**/*.jsonl",
                "<codex-home>/sessions/**/*.jsonl",
                "<cursor-home>/projects/**/*.jsonl",
                "<provider-home>/**/*.jsonl",
            ],
            &[],
            &[],
            &[],
            &[],
            "tracedecay-capture",
            vec!["storage:source_family_entry:provider_transcripts".to_owned()],
        ),
        family_owned(
            "lcm_raw_summary_payload",
            &[
                "projects/<project-id>/lcm-payloads/**",
                "projects/<project-id>/sessions.db",
            ],
            &names(
                SESSION_TABLES
                    .iter()
                    .map(|row| row.0)
                    .filter(|name| name.starts_with("lcm_")),
            ),
            &[],
            &[],
            &["sessions.db-shm", "sessions.db-wal"],
            "tracedecay-activity",
            vec![
                "storage:storage_artifact:lcm_payloads".to_owned(),
                "storage:store:project_sessions_database".to_owned(),
            ],
        ),
        family_owned(
            "hooks_hints_outcomes",
            &["analytics/hook-events*.jsonl", "hook-logs/**/*.jsonl"],
            &[],
            &[],
            &[],
            &[],
            "tracedecay-capture",
            vec!["storage:source_family_entry:hooks_hints_outcomes".to_owned()],
        ),
        family_owned(
            "code_index_diagnostics_tests",
            &[
                "projects/<project-id>/*.db",
                "projects/<project-id>/tracedecay.db",
            ],
            &graph_tables,
            &graph_indexes,
            &graph_triggers,
            &["*.db-shm", "*.db-wal"],
            "tracedecay-code-index",
            vec![
                "storage:store:branch_graph_database".to_owned(),
                "storage:store:project_graph_database".to_owned(),
            ],
        ),
        family_owned(
            "git_delivery",
            &[
                "<git-common-dir>/tracedecay-project.json",
                "projects/<project-id>/branch-meta.json",
                "projects/<project-id>/sessions.db",
            ],
            &names(SESSION_TABLES.iter().map(|row| row.0).filter(|name| {
                matches!(
                    *name,
                    "commit_sessions" | "git_correlation_meta" | "session_git_spans"
                )
            })),
            &[],
            &[],
            &["sessions.db-shm", "sessions.db-wal"],
            "tracedecay-projectors",
            vec![
                "storage:storage_artifact:branch_metadata".to_owned(),
                "storage:storage_artifact:repository_identity_marker".to_owned(),
                "storage:store:project_sessions_database".to_owned(),
            ],
        ),
        family_owned(
            "memory_facts",
            &["projects/<project-id>/*.db", "user-memory.db"],
            &names(
                GRAPH_TABLES
                    .iter()
                    .map(|row| row.0)
                    .chain(DERIVED_TABLES.iter().copied())
                    .filter(|name| name.starts_with("memory_")),
            ),
            &graph_indexes,
            &graph_triggers,
            &[
                "*.db-shm",
                "*.db-wal",
                "user-memory.db-shm",
                "user-memory.db-wal",
            ],
            "tracedecay-knowledge",
            vec![
                "storage:store:project_graph_database".to_owned(),
                "storage:store:user_memory_database".to_owned(),
            ],
        ),
        family_owned(
            "automation_skills_curation",
            &[
                "agent-managed/**",
                "automation/**/*.json",
                "automation/**/*.jsonl",
            ],
            &[],
            &[],
            &[],
            &[],
            "tracedecay-activity",
            vec!["storage:source_family_entry:automation_skills_curation".to_owned()],
        ),
        family_owned(
            "hermes_legacy",
            &[
                "<hermes-home>/**/*.db",
                "<hermes-home>/**/*.json",
                "<hermes-home>/**/*.jsonl",
            ],
            &[],
            &[],
            &[],
            &["*.db-shm", "*.db-wal"],
            "tracedecay-activity",
            vec!["storage:source_family_entry:hermes_legacy".to_owned()],
        ),
        family_owned(
            "transitional_user_memory",
            &["user-memory.db"],
            &graph_tables,
            &graph_indexes,
            &graph_triggers,
            &["user-memory.db-shm", "user-memory.db-wal"],
            "tracedecay-knowledge",
            vec!["storage:store:user_memory_database".to_owned()],
        ),
        family_owned(
            "hermes_kanban_task_graph",
            &[
                "<hermes-root>/kanban/boards/*/kanban.db",
                "<hermes-root>/kanban/kanban.db",
            ],
            &names([
                "kanban_notify_subs",
                "task_attachments",
                "task_comments",
                "task_events",
                "task_links",
                "task_runs",
                "tasks",
            ]),
            &[],
            &[],
            &["kanban.db-shm", "kanban.db-wal"],
            "tracedecay-task-graph",
            vec!["storage:source_family_entry:hermes_kanban_task_graph".to_owned()],
        ),
        family_owned(
            "analytics_accounting",
            &[
                "analytics/**/*.jsonl",
                "global.db",
                "projects/<project-id>/sessions.db",
            ],
            &names(
                SESSION_TABLES
                    .iter()
                    .map(|row| row.0)
                    .filter(|name| matches!(*name, "analytics_events" | "savings_ledger")),
            ),
            &session_indexes,
            &[],
            &[
                "global.db-shm",
                "global.db-wal",
                "sessions.db-shm",
                "sessions.db-wal",
            ],
            "tracedecay-accounting",
            vec![
                "storage:store:profile_global_database".to_owned(),
                "storage:store:project_sessions_database".to_owned(),
            ],
        ),
        family_owned(
            "dashboard_settings_provider_manifests",
            &[
                "<provider-home>/**/settings.json",
                "<provider-home>/**/settings.jsonc",
                "<provider-home>/**/settings.toml",
                "projects/<project-id>/config.json",
                "projects/<project-id>/dashboard/**",
            ],
            &[],
            &[],
            &[],
            &[],
            "root-host-config",
            vec![
                "storage:storage_artifact:dashboard_artifacts".to_owned(),
                "storage:storage_artifact:store_config".to_owned(),
            ],
        ),
        family_owned(
            "response_handles_artifacts_backups",
            &[
                "migration-backups/**",
                "projects/<project-id>/lcm-payloads/**",
                "projects/<project-id>/response-handles/**",
            ],
            &[],
            &[],
            &[],
            &[],
            "tracedecay-store",
            vec![
                "storage:storage_artifact:lcm_payloads".to_owned(),
                "storage:storage_artifact:response_handles".to_owned(),
            ],
        ),
        family_owned(
            "retention_gc_bookkeeping",
            &[
                "projects/<project-id>/dirty",
                "projects/<project-id>/store_manifest.json",
            ],
            &names(
                SESSION_TABLES
                    .iter()
                    .map(|row| row.0)
                    .filter(|name| matches!(*name, "lcm_gc_marks" | "lcm_gc_meta")),
            ),
            &[],
            &[],
            &[],
            "tracedecay-store",
            vec![
                "storage:storage_artifact:dirty_sentinel".to_owned(),
                "storage:storage_artifact:store_manifest".to_owned(),
            ],
        ),
        family_owned(
            "runtime_daemon_logs_crash_telemetry",
            &["crash-reports/**", "logs/**", "runtime-telemetry/**"],
            &[],
            &[],
            &[],
            &[],
            "tracedecay-observability",
            vec!["storage:source_family_entry:runtime_daemon_logs_crash_telemetry".to_owned()],
        ),
        family_owned(
            "lifecycle_lease_service_state",
            &[
                "<launchd-home>/com.tracedecay.daemon.plist",
                "<runtime-dir>/tracedecay*.lock",
                "<systemd-user-home>/tracedecay.service",
            ],
            &[],
            &[],
            &[],
            &[],
            "root-composition",
            vec!["storage:source_family_entry:lifecycle_lease_service_state".to_owned()],
        ),
    ]);
    families.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
    validate_storage_source_family_appendix(&families)
        .expect("checked storage source-family descriptors must remain in parity");
    families
}

/// Checks the generated appendix against the bounded schema descriptors owned
/// by this module. External source schemas remain explicitly scoped to their
/// family and cannot silently enlarge TraceDecay-owned SQLite schemas.
pub fn validate_storage_source_family_appendix(
    families: &[SourceFamilyAppendixEntryV1],
) -> Result<(), String> {
    if families
        .windows(2)
        .any(|pair| pair[0].stable_id >= pair[1].stable_id)
    {
        return Err("source families must be strictly stable-ID sorted".to_owned());
    }

    let graph = required_family(families, "project_graph_database")?;
    require_schema_parity("graph tables", &graph.tables, &owned_graph_tables())?;
    require_schema_parity("graph indexes", &graph.indexes, &owned_graph_indexes())?;
    require_schema_parity("graph triggers", &graph.triggers, &owned_graph_triggers())?;

    let sessions = required_family(families, "project_sessions_database")?;
    require_schema_parity("session tables", &sessions.tables, &owned_session_tables())?;
    require_schema_parity(
        "session indexes",
        &sessions.indexes,
        &owned_session_indexes(),
    )?;
    require_schema_parity(
        "session triggers",
        &sessions.triggers,
        &owned_session_triggers(),
    )?;

    let mut allowed_tables = owned_graph_tables();
    allowed_tables.extend(owned_session_tables());
    allowed_tables.extend(names([
        "kanban_notify_subs",
        "task_attachments",
        "task_comments",
        "task_events",
        "task_links",
        "task_runs",
        "tasks",
    ]));
    allowed_tables.sort();
    allowed_tables.dedup();
    let mut allowed_indexes = owned_graph_indexes();
    allowed_indexes.extend(owned_session_indexes());
    allowed_indexes.sort();
    allowed_indexes.dedup();
    let mut allowed_triggers = owned_graph_triggers();
    allowed_triggers.extend(owned_session_triggers());
    allowed_triggers.sort();
    allowed_triggers.dedup();

    for family in families {
        require_known_schema_names("table", &family.tables, &allowed_tables)?;
        require_known_schema_names("index", &family.indexes, &allowed_indexes)?;
        require_known_schema_names("trigger", &family.triggers, &allowed_triggers)?;
    }
    Ok(())
}

fn required_family<'a>(
    families: &'a [SourceFamilyAppendixEntryV1],
    name: &str,
) -> Result<&'a SourceFamilyAppendixEntryV1, String> {
    families
        .iter()
        .find(|family| family.source_family == name)
        .ok_or_else(|| format!("missing required source family {name}"))
}

fn require_schema_parity(kind: &str, actual: &[String], expected: &[String]) -> Result<(), String> {
    if actual != expected {
        return Err(format!("{kind} drifted from owned schema descriptors"));
    }
    Ok(())
}

fn require_known_schema_names(
    kind: &str,
    actual: &[String],
    allowed: &[String],
) -> Result<(), String> {
    if let Some(name) = actual
        .iter()
        .find(|name| allowed.binary_search(name).is_err())
    {
        return Err(format!("unregistered {kind} descriptor {name}"));
    }
    Ok(())
}

fn owned_graph_tables() -> Vec<String> {
    names(
        GRAPH_TABLES.iter().map(|row| row.0).chain(
            DERIVED_TABLES
                .iter()
                .copied()
                .filter(|name| name.starts_with("memory_") || name.starts_with("nodes_")),
        ),
    )
}

fn owned_session_tables() -> Vec<String> {
    names(
        SESSION_TABLES.iter().map(|row| row.0).chain(
            DERIVED_TABLES
                .iter()
                .copied()
                .filter(|name| !name.starts_with("memory_") && !name.starts_with("nodes_")),
        ),
    )
}

fn owned_graph_indexes() -> Vec<String> {
    names(
        INDEXES
            .iter()
            .copied()
            .filter(|name| index_owner(name).starts_with("src/db/")),
    )
}

fn owned_session_indexes() -> Vec<String> {
    names(
        INDEXES
            .iter()
            .copied()
            .filter(|name| !index_owner(name).starts_with("src/db/")),
    )
}

fn owned_graph_triggers() -> Vec<String> {
    names(
        TRIGGERS
            .iter()
            .copied()
            .filter(|name| trigger_owner(name).starts_with("src/db/")),
    )
}

fn owned_session_triggers() -> Vec<String> {
    names(
        TRIGGERS
            .iter()
            .copied()
            .filter(|name| !trigger_owner(name).starts_with("src/db/")),
    )
}

fn family(
    name: &str,
    paths: &[&str],
    tables: &[String],
    indexes: &[String],
    triggers: &[String],
    sidecars: &[&str],
    entry_refs: Vec<String>,
) -> SourceFamilyAppendixEntryV1 {
    family_owned(
        name,
        paths,
        tables,
        indexes,
        triggers,
        sidecars,
        "tracedecay-store",
        entry_refs,
    )
}

#[allow(clippy::too_many_arguments)]
fn family_owned(
    name: &str,
    paths: &[&str],
    tables: &[String],
    indexes: &[String],
    triggers: &[String],
    sidecars: &[&str],
    owner: &str,
    mut entry_refs: Vec<String>,
) -> SourceFamilyAppendixEntryV1 {
    entry_refs.sort();
    entry_refs.dedup();
    SourceFamilyAppendixEntryV1 {
        stable_id: format!("storage:source_family:{name}"),
        source_family: name.to_owned(),
        relative_paths_or_globs: names(paths.iter().copied()),
        tables: tables.to_vec(),
        indexes: indexes.to_vec(),
        triggers: triggers.to_vec(),
        sidecars: names(sidecars.iter().copied()),
        owner: owner.to_owned(),
        entry_refs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compatibility_inventory::storage::storage_entries;

    #[test]
    fn source_family_appendix_covers_every_section_eight_family() {
        let families = storage_source_family_appendix(&storage_entries());
        let actual = families
            .iter()
            .map(|family| family.source_family.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for required in [
            "repository_profile_identity",
            "provider_transcripts",
            "lcm_raw_summary_payload",
            "hooks_hints_outcomes",
            "code_index_diagnostics_tests",
            "git_delivery",
            "memory_facts",
            "automation_skills_curation",
            "hermes_legacy",
            "transitional_user_memory",
            "hermes_kanban_task_graph",
            "analytics_accounting",
            "dashboard_settings_provider_manifests",
            "response_handles_artifacts_backups",
            "retention_gc_bookkeeping",
            "runtime_daemon_logs_crash_telemetry",
            "lifecycle_lease_service_state",
        ] {
            assert!(
                actual.contains(required),
                "missing source family {required}"
            );
        }

        let kanban = required_family(&families, "hermes_kanban_task_graph").unwrap();
        assert_eq!(
            kanban.relative_paths_or_globs,
            names([
                "<hermes-root>/kanban/boards/*/kanban.db",
                "<hermes-root>/kanban/kanban.db",
            ])
        );
        assert_eq!(
            kanban.tables,
            names([
                "kanban_notify_subs",
                "task_attachments",
                "task_comments",
                "task_events",
                "task_links",
                "task_runs",
                "tasks",
            ])
        );
        assert_eq!(kanban.sidecars, names(["kanban.db-shm", "kanban.db-wal"]));
    }

    #[test]
    fn owned_schema_descriptor_drift_fails_closed() {
        let entries = storage_entries();

        let mut removed_table = storage_source_family_appendix(&entries);
        required_family_mut(&mut removed_table, "project_graph_database")
            .tables
            .pop();
        assert!(validate_storage_source_family_appendix(&removed_table).is_err());

        let mut added_index = storage_source_family_appendix(&entries);
        required_family_mut(&mut added_index, "project_graph_database")
            .indexes
            .push("unregistered_index".to_owned());
        assert!(validate_storage_source_family_appendix(&added_index).is_err());

        let mut removed_trigger = storage_source_family_appendix(&entries);
        required_family_mut(&mut removed_trigger, "project_sessions_database")
            .triggers
            .pop();
        assert!(validate_storage_source_family_appendix(&removed_trigger).is_err());
    }

    #[test]
    fn every_source_family_references_a_canonical_inventory_entry() {
        let families = storage_source_family_appendix(&storage_entries());
        assert!(
            families.iter().all(|family| !family.entry_refs.is_empty()),
            "source-family appendix rows must never be orphaned"
        );
    }

    fn required_family_mut<'a>(
        families: &'a mut [SourceFamilyAppendixEntryV1],
        name: &str,
    ) -> &'a mut SourceFamilyAppendixEntryV1 {
        families
            .iter_mut()
            .find(|family| family.source_family == name)
            .unwrap()
    }
}

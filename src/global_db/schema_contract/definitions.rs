#[derive(Clone, Copy)]
pub(super) struct Column {
    pub(super) name: &'static str,
    pub(super) declared_type: &'static str,
    pub(super) not_null: bool,
    pub(super) default_value: Option<&'static str>,
    pub(super) primary_key_ordinal: i64,
}

const fn column(
    name: &'static str,
    declared_type: &'static str,
    not_null: bool,
    default_value: Option<&'static str>,
    primary_key_ordinal: i64,
) -> Column {
    Column {
        name,
        declared_type,
        not_null,
        default_value,
        primary_key_ordinal,
    }
}

#[derive(Clone, Copy)]
pub(super) struct ForeignKey {
    pub(super) sequence: i64,
    pub(super) from: &'static str,
    pub(super) target_table: &'static str,
    pub(super) target_column: &'static str,
    pub(super) on_delete: &'static str,
}

const fn foreign_key(
    from: &'static str,
    target_table: &'static str,
    target_column: &'static str,
    on_delete: &'static str,
) -> ForeignKey {
    foreign_key_sequence(from, target_table, target_column, on_delete, 0)
}

const fn foreign_key_sequence(
    from: &'static str,
    target_table: &'static str,
    target_column: &'static str,
    on_delete: &'static str,
    sequence: i64,
) -> ForeignKey {
    ForeignKey {
        sequence,
        from,
        target_table,
        target_column,
        on_delete,
    }
}

#[derive(Clone, Copy)]
pub(super) struct Table {
    pub(super) name: &'static str,
    pub(super) columns: &'static [Column],
    pub(super) foreign_keys: &'static [ForeignKey],
}

macro_rules! table {
    ($name:literal, [$($column:expr),* $(,)?], [$($foreign_key:expr),* $(,)?]) => {
        Table {
            name: $name,
            columns: &[$($column),*],
            foreign_keys: &[$($foreign_key),*],
        }
    };
}

pub(super) const TABLES: &[Table] = &[
    table!(
        "projects",
        [
            column("path", "TEXT", false, None, 1),
            column("tokens_saved", "INTEGER", true, Some("0"), 0),
        ],
        []
    ),
    table!(
        "code_projects",
        [
            column("project_id", "TEXT", false, None, 1),
            column("canonical_root", "TEXT", true, None, 0),
            column("display_root", "TEXT", true, None, 0),
            column("primary_root_platform", "TEXT", false, None, 0),
            column("primary_root_bytes", "BLOB", false, None, 0),
            column("primary_root_last_seen_at", "INTEGER", false, None, 0),
            column("git_common_dir", "TEXT", false, None, 0),
            column("git_remote_url", "TEXT", false, None, 0),
            column("default_branch", "TEXT", false, None, 0),
            column("created_at", "INTEGER", true, None, 0),
            column("last_seen_at", "INTEGER", true, None, 0),
        ],
        []
    ),
    table!(
        "project_aliases",
        [
            column("alias_path", "TEXT", false, None, 1),
            column("project_id", "TEXT", true, None, 0),
            column("last_seen_at", "INTEGER", true, None, 0),
        ],
        [foreign_key(
            "project_id",
            "code_projects",
            "project_id",
            "CASCADE"
        )]
    ),
    table!(
        "store_instances",
        [
            column("store_id", "TEXT", false, None, 1),
            column("project_id", "TEXT", true, None, 0),
            column("store_kind", "TEXT", true, None, 0),
            column("storage_mode", "TEXT", true, None, 0),
            column("store_relpath", "TEXT", true, None, 0),
            column("manifest_relpath", "TEXT", false, None, 0),
            column("created_at", "INTEGER", true, None, 0),
            column("last_verified_at", "INTEGER", false, None, 0),
            column("last_write_at", "INTEGER", false, None, 0),
        ],
        [foreign_key(
            "project_id",
            "code_projects",
            "project_id",
            "CASCADE"
        )]
    ),
    table!(
        "graph_scopes",
        [
            column("graph_scope_id", "TEXT", false, None, 1),
            column("project_id", "TEXT", true, None, 0),
            column("store_id", "TEXT", true, None, 0),
            column("branch_name", "TEXT", true, None, 0),
            column("db_relpath", "TEXT", true, None, 0),
            column("parent_scope_id", "TEXT", false, None, 0),
            column("last_synced_at", "INTEGER", false, None, 0),
            column("writable", "INTEGER", true, Some("1"), 0),
        ],
        [
            foreign_key("project_id", "code_projects", "project_id", "CASCADE"),
            foreign_key("store_id", "store_instances", "store_id", "CASCADE"),
        ]
    ),
    table!(
        "store_artifacts",
        [
            column("store_id", "TEXT", true, None, 1),
            column("artifact_kind", "TEXT", true, None, 2),
            column("relpath", "TEXT", true, None, 3),
            column("size_bytes", "INTEGER", false, None, 0),
            column("schema_version", "TEXT", false, None, 0),
            column("updated_at", "INTEGER", false, None, 0),
        ],
        [foreign_key(
            "store_id",
            "store_instances",
            "store_id",
            "CASCADE"
        )]
    ),
    table!(
        "sanitization_receipts",
        [
            column("receipt_id", "TEXT", false, None, 1),
            column("sanitizer_version", "TEXT", true, None, 0),
            column("payload_digest", "TEXT", true, None, 0),
            column("receipt_json", "TEXT", true, None, 0),
        ],
        []
    ),
    table!(
        "observations",
        [
            column("sequence", "INTEGER", false, None, 1),
            column("observation_id", "TEXT", true, None, 0),
            column("payload_digest", "TEXT", true, None, 0),
            column("receipt_id", "TEXT", true, None, 0),
            column("observation_json", "TEXT", true, None, 0),
            column("committed_cursor_json", "TEXT", true, None, 0),
        ],
        [foreign_key(
            "receipt_id",
            "sanitization_receipts",
            "receipt_id",
            "NO ACTION"
        )]
    ),
    table!(
        "source_cursors",
        [
            column("source_json", "TEXT", true, None, 1),
            column("scope_json", "TEXT", true, None, 2),
            column("cursor_json", "TEXT", true, None, 0),
        ],
        []
    ),
    table!(
        "source_cursor_advances",
        [
            column("source_json", "TEXT", true, None, 1),
            column("scope_json", "TEXT", true, None, 2),
            column("coverage_json", "TEXT", true, None, 3),
            column("reason", "TEXT", true, None, 0),
            column("receipt_id", "TEXT", false, None, 0),
        ],
        [foreign_key(
            "receipt_id",
            "sanitization_receipts",
            "receipt_id",
            "NO ACTION"
        )]
    ),
    table!(
        "authority_audit_checkpoints",
        [
            column("audit_name", "TEXT", false, None, 1),
            column("audit_version", "INTEGER", true, None, 0),
            column("receipt_rowid", "INTEGER", true, None, 0),
            column("observation_sequence", "INTEGER", true, None, 0),
            column("provenance_rowid", "INTEGER", true, None, 0),
            column("disposition_rowid", "INTEGER", true, None, 0),
            column("alias_rowid", "INTEGER", true, None, 0),
            column("projection_checkpoint", "INTEGER", true, None, 0),
            column("last_receipts_audited", "INTEGER", true, None, 0),
            column("last_observations_audited", "INTEGER", true, None, 0),
            column("last_provenance_audited", "INTEGER", true, None, 0),
            column("last_dispositions_audited", "INTEGER", true, None, 0),
            column("last_aliases_audited", "INTEGER", true, None, 0),
            column(
                "bounded_passes_since_exhaustive",
                "INTEGER",
                true,
                Some("0"),
                0
            ),
        ],
        []
    ),
    table!(
        "projection_queue",
        [
            column("observation_id", "TEXT", false, None, 1),
            column("observation_sequence", "INTEGER", true, None, 0),
        ],
        [foreign_key(
            "observation_id",
            "observations",
            "observation_id",
            "NO ACTION"
        )]
    ),
    table!(
        "observation_projection_provenance",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("observation_id", "TEXT", true, None, 2),
            column("output_ordinal", "INTEGER", true, Some("0"), 3),
            column("receipt_id", "TEXT", true, None, 0),
            column("output_provider", "TEXT", true, None, 0),
            column("output_message_id", "TEXT", true, None, 0),
            column("output_digest", "TEXT", true, None, 0),
            column("message_created", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "observation_projection_checkpoints",
        [
            column("projector_version", "TEXT", false, None, 1),
            column("last_sequence", "INTEGER", true, None, 0),
        ],
        []
    ),
    table!(
        "observation_projection_migrations",
        [
            column("source_projector_version", "TEXT", true, None, 1),
            column("target_projector_version", "TEXT", true, None, 2),
            column("source_frontier", "INTEGER", true, None, 0),
            column("migrated_through", "INTEGER", true, None, 0),
            column("completed", "INTEGER", true, None, 0),
        ],
        []
    ),
    table!(
        "observation_projection_aliases",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("observation_id", "TEXT", true, None, 2),
            column("output_provider", "TEXT", true, None, 0),
            column("output_message_id", "TEXT", true, None, 0),
        ],
        [foreign_key(
            "observation_id",
            "observations",
            "observation_id",
            "NO ACTION"
        )]
    ),
    table!(
        "observation_projection_dispositions",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("observation_id", "TEXT", true, None, 2),
            column("receipt_id", "TEXT", true, None, 0),
            column("reason", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "observation_workflow_facts",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("observation_id", "TEXT", true, None, 2),
            column("fact_ordinal", "INTEGER", true, None, 3),
            column("receipt_id", "TEXT", true, None, 0),
            column("observation_sequence", "INTEGER", true, None, 0),
            column("provider", "TEXT", true, None, 0),
            column("session_id", "TEXT", true, None, 0),
            column("semantic_kind", "TEXT", true, None, 0),
            column("provider_reference", "TEXT", false, None, 0),
            column("item_id", "TEXT", false, None, 0),
            column("parent_reference", "TEXT", false, None, 0),
            column("list_reference", "TEXT", false, None, 0),
            column("state", "TEXT", false, None, 0),
            column("status", "TEXT", false, None, 0),
            column("item_order", "INTEGER", false, None, 0),
            column("native_revision", "TEXT", false, None, 0),
            column("event_sequence", "INTEGER", false, None, 0),
            column("source_sequence", "INTEGER", false, None, 0),
            column("native_timestamp", "INTEGER", false, None, 0),
            column("ordering_domain", "TEXT", true, None, 0),
            column("content_json", "TEXT", false, None, 0),
            column("content_text", "TEXT", true, None, 0),
            column("output_digest", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "observation_projection_rebuilds",
        [
            column("projector_version", "TEXT", false, None, 1),
            column("generation", "TEXT", true, None, 0),
            column("frontier_sequence", "INTEGER", true, None, 0),
            column("aliases_staged_through", "INTEGER", true, Some("0"), 0),
            column("staged_through", "INTEGER", true, Some("0"), 0),
            column("projected_rows", "INTEGER", true, Some("0"), 0),
            column("skipped_observations", "INTEGER", true, Some("0"), 0),
            column("state", "TEXT", true, None, 0),
        ],
        []
    ),
    table!(
        "observation_projection_rebuild_aliases",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("generation", "TEXT", true, None, 2),
            column("observation_id", "TEXT", true, None, 3),
            column("output_provider", "TEXT", true, None, 0),
            column("output_message_id", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "projector_version",
                "observation_projection_rebuilds",
                "projector_version",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "observation_projection_rebuilds",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "observation_projection_rebuild_sessions",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("generation", "TEXT", true, None, 2),
            column("provider", "TEXT", true, None, 3),
            column("session_id", "TEXT", true, None, 4),
            column("session_json", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "projector_version",
                "observation_projection_rebuilds",
                "projector_version",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "observation_projection_rebuilds",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "observation_projection_rebuild_messages",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("generation", "TEXT", true, None, 2),
            column("output_provider", "TEXT", true, None, 3),
            column("output_message_id", "TEXT", true, None, 4),
            column("message_json", "TEXT", true, None, 0),
            column("content_hash", "TEXT", true, None, 0),
            column("snippet_text", "TEXT", true, None, 0),
            column("index_text", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "projector_version",
                "observation_projection_rebuilds",
                "projector_version",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "observation_projection_rebuilds",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "observation_projection_rebuild_provenance",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("generation", "TEXT", true, None, 2),
            column("observation_id", "TEXT", true, None, 3),
            column("output_ordinal", "INTEGER", true, None, 4),
            column("receipt_id", "TEXT", true, None, 0),
            column("output_provider", "TEXT", true, None, 0),
            column("output_message_id", "TEXT", true, None, 0),
            column("output_digest", "TEXT", true, None, 0),
            column("message_created", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "projector_version",
                "observation_projection_rebuilds",
                "projector_version",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "observation_projection_rebuilds",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "observation_projection_rebuild_dispositions",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("generation", "TEXT", true, None, 2),
            column("observation_id", "TEXT", true, None, 3),
            column("receipt_id", "TEXT", true, None, 0),
            column("reason", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "projector_version",
                "observation_projection_rebuilds",
                "projector_version",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "observation_projection_rebuilds",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "observation_projection_rebuild_workflow_facts",
        [
            column("projector_version", "TEXT", true, None, 1),
            column("generation", "TEXT", true, None, 2),
            column("observation_id", "TEXT", true, None, 3),
            column("fact_ordinal", "INTEGER", true, None, 4),
            column("receipt_id", "TEXT", true, None, 0),
            column("observation_sequence", "INTEGER", true, None, 0),
            column("provider", "TEXT", true, None, 0),
            column("session_id", "TEXT", true, None, 0),
            column("semantic_kind", "TEXT", true, None, 0),
            column("provider_reference", "TEXT", false, None, 0),
            column("item_id", "TEXT", false, None, 0),
            column("parent_reference", "TEXT", false, None, 0),
            column("list_reference", "TEXT", false, None, 0),
            column("state", "TEXT", false, None, 0),
            column("status", "TEXT", false, None, 0),
            column("item_order", "INTEGER", false, None, 0),
            column("native_revision", "TEXT", false, None, 0),
            column("event_sequence", "INTEGER", false, None, 0),
            column("source_sequence", "INTEGER", false, None, 0),
            column("native_timestamp", "INTEGER", false, None, 0),
            column("ordering_domain", "TEXT", true, None, 0),
            column("content_json", "TEXT", false, None, 0),
            column("content_text", "TEXT", true, None, 0),
            column("output_digest", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "projector_version",
                "observation_projection_rebuilds",
                "projector_version",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "observation_projection_rebuilds",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "receipt_id",
                "sanitization_receipts",
                "receipt_id",
                "NO ACTION"
            ),
        ]
    ),
];

pub(super) const REGISTRY_TABLE_NAMES: &[&str] = &[
    "projects",
    "code_projects",
    "project_aliases",
    "store_instances",
    "graph_scopes",
    "store_artifacts",
];

pub(super) const OBSERVATIONS_TABLE_NAME: &str = "observations";

#[derive(Clone, Copy)]
pub(super) struct Index {
    pub(super) table: &'static str,
    pub(super) name: Option<&'static str>,
    pub(super) unique: bool,
    pub(super) origin: &'static str,
    pub(super) columns: &'static [&'static str],
}

pub(super) const INDEXES: &[Index] = &[
    Index {
        table: "project_aliases",
        name: Some("idx_project_aliases_project_id"),
        unique: false,
        origin: "c",
        columns: &["project_id"],
    },
    Index {
        table: "store_instances",
        name: Some("idx_store_instances_project_id"),
        unique: false,
        origin: "c",
        columns: &["project_id"],
    },
    Index {
        table: "graph_scopes",
        name: Some("idx_graph_scopes_project_store"),
        unique: false,
        origin: "c",
        columns: &["project_id", "store_id"],
    },
    Index {
        table: "observations",
        name: None,
        unique: true,
        origin: "u",
        columns: &["observation_id"],
    },
    Index {
        table: "projection_queue",
        name: None,
        unique: true,
        origin: "u",
        columns: &["observation_sequence"],
    },
    Index {
        table: "observation_projection_provenance",
        name: Some("idx_observation_projection_provenance_global_output"),
        unique: false,
        origin: "c",
        columns: &["output_provider", "output_message_id", "projector_version"],
    },
    Index {
        table: "observation_workflow_facts",
        name: Some("idx_observation_workflow_facts_query"),
        unique: false,
        origin: "c",
        columns: &[
            "provider",
            "session_id",
            "semantic_kind",
            "status",
            "observation_sequence",
        ],
    },
    Index {
        table: "observation_workflow_facts",
        name: Some("idx_observation_workflow_facts_item"),
        unique: false,
        origin: "c",
        columns: &[
            "provider",
            "session_id",
            "semantic_kind",
            "item_id",
            "provider_reference",
            "event_sequence",
            "source_sequence",
            "observation_sequence",
        ],
    },
    Index {
        table: "observation_projection_rebuilds",
        name: None,
        unique: true,
        origin: "u",
        columns: &["projector_version", "generation"],
    },
    Index {
        table: "observation_projection_rebuild_provenance",
        name: Some("idx_projection_rebuild_provenance_output"),
        unique: false,
        origin: "c",
        columns: &[
            "projector_version",
            "generation",
            "output_provider",
            "output_message_id",
        ],
    },
    Index {
        table: "observation_projection_rebuild_workflow_facts",
        name: Some("idx_projection_rebuild_workflow_goal"),
        unique: false,
        origin: "c",
        columns: &[
            "projector_version",
            "generation",
            "provider",
            "session_id",
            "semantic_kind",
            "provider_reference",
            "observation_sequence",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::{INDEXES, TABLES};

    const REBUILD_TABLES: &[&str] = &[
        "observation_projection_rebuilds",
        "observation_projection_rebuild_aliases",
        "observation_projection_rebuild_sessions",
        "observation_projection_rebuild_messages",
        "observation_projection_rebuild_provenance",
        "observation_projection_rebuild_dispositions",
        "observation_projection_rebuild_workflow_facts",
    ];

    #[test]
    fn rebuild_schema_contract_registration_is_complete() {
        let tables = TABLES
            .iter()
            .map(|table| table.name)
            .filter(|name| name.starts_with("observation_projection_rebuild"))
            .collect::<Vec<_>>();
        assert_eq!(tables, REBUILD_TABLES);

        let indexes = INDEXES
            .iter()
            .filter(|index| index.table.starts_with("observation_projection_rebuild"))
            .map(|index| (index.table, index.name, index.unique, index.columns))
            .collect::<Vec<_>>();
        assert_eq!(
            indexes,
            vec![
                (
                    "observation_projection_rebuilds",
                    None,
                    true,
                    &["projector_version", "generation"] as &[_],
                ),
                (
                    "observation_projection_rebuild_provenance",
                    Some("idx_projection_rebuild_provenance_output"),
                    false,
                    &[
                        "projector_version",
                        "generation",
                        "output_provider",
                        "output_message_id",
                    ],
                ),
                (
                    "observation_projection_rebuild_workflow_facts",
                    Some("idx_projection_rebuild_workflow_goal"),
                    false,
                    &[
                        "projector_version",
                        "generation",
                        "provider",
                        "session_id",
                        "semantic_kind",
                        "provider_reference",
                        "observation_sequence",
                    ],
                ),
            ]
        );
    }
}

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
    ForeignKey {
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
];

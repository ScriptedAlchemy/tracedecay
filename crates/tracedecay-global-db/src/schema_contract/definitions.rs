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
        "retrieval_anchors",
        [
            column("anchor_id", "TEXT", false, None, 1),
            column("anchor_json", "TEXT", true, None, 0),
            column("owner_json", "TEXT", true, None, 0),
            column("projection_generation", "TEXT", true, None, 0),
        ],
        []
    ),
    table!(
        "observation_retrieval_anchors",
        [
            column("observation_id", "TEXT", false, None, 1),
            column("anchor_id", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key("anchor_id", "retrieval_anchors", "anchor_id", "NO ACTION"),
        ]
    ),
    table!(
        "observation_repository_provenance",
        [
            column("observation_id", "TEXT", false, None, 1),
            column("availability_json", "TEXT", true, None, 0),
            column("capture_json", "TEXT", false, None, 0),
            column("retrieval_anchor_id", "TEXT", false, None, 0),
            column("owner_json", "TEXT", false, None, 0),
        ],
        [
            foreign_key(
                "observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "retrieval_anchor_id",
                "retrieval_anchors",
                "anchor_id",
                "NO ACTION"
            ),
            foreign_key_sequence(
                "owner_json",
                "retrieval_anchors",
                "owner_json",
                "NO ACTION",
                1
            ),
        ]
    ),
    table!(
        "retrieval_anchor_aliases",
        [
            column("owner_json", "TEXT", true, None, 1),
            column("alias_kind", "TEXT", true, None, 2),
            column("locator_digest", "TEXT", true, None, 3),
            column("anchor_id", "TEXT", true, None, 0),
        ],
        [
            foreign_key("anchor_id", "retrieval_anchors", "anchor_id", "NO ACTION"),
            foreign_key_sequence(
                "owner_json",
                "retrieval_anchors",
                "owner_json",
                "NO ACTION",
                1
            ),
        ]
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
            column("source_cursor_rowid", "INTEGER", true, Some("0"), 0),
            column("source_advance_rowid", "INTEGER", true, Some("0"), 0),
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
        "observation_backfill_watermarks",
        [
            column("migration", "TEXT", true, None, 1),
            column("backfilled_through", "INTEGER", true, None, 0),
        ],
        []
    ),
    table!(
        "projection_queue",
        [
            column("observation_id", "TEXT", false, None, 1),
            column("observation_sequence", "INTEGER", true, None, 0),
            column("attempt_count", "INTEGER", true, Some("0"), 0),
            column("next_retry_at_micros", "INTEGER", true, Some("0"), 0),
            column("last_error_code", "TEXT", false, None, 0),
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
            column("retrieval_anchor_id", "TEXT", false, None, 0),
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
            foreign_key(
                "retrieval_anchor_id",
                "retrieval_anchors",
                "anchor_id",
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
            column("retrieval_anchor_id", "TEXT", false, None, 0),
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
            foreign_key(
                "retrieval_anchor_id",
                "retrieval_anchors",
                "anchor_id",
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
            column("retrieval_anchor_id", "TEXT", false, None, 0),
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
            foreign_key(
                "retrieval_anchor_id",
                "retrieval_anchors",
                "anchor_id",
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
            column("retrieval_anchor_id", "TEXT", false, None, 0),
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
            foreign_key(
                "retrieval_anchor_id",
                "retrieval_anchors",
                "anchor_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "session_temporal_schema_migrations",
        [
            column("name", "TEXT", false, None, 1),
            column("version", "INTEGER", true, None, 0),
            column("applied_at", "INTEGER", true, None, 0),
        ],
        []
    ),
    table!(
        "session_summary_nodes",
        [
            column("summary_id", "TEXT", false, None, 1),
            column("session_id", "TEXT", true, None, 0),
            column("summary_anchor_id", "TEXT", true, None, 0),
            column("summary_text", "TEXT", true, None, 0),
            column("index_text", "TEXT", true, None, 0),
            column("source_horizon_json", "TEXT", true, None, 0),
            column("publication_json", "TEXT", false, None, 0),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [foreign_key(
            "summary_anchor_id",
            "retrieval_anchors",
            "anchor_id",
            "NO ACTION"
        )]
    ),
    table!(
        "session_summary_sources",
        [
            column("summary_id", "TEXT", true, None, 1),
            column("source_ordinal", "INTEGER", true, None, 2),
            column("source_kind", "TEXT", true, None, 0),
            column("source_anchor_id", "TEXT", false, None, 0),
            column("source_summary_id", "TEXT", false, None, 0),
        ],
        [
            foreign_key(
                "summary_id",
                "session_summary_nodes",
                "summary_id",
                "CASCADE"
            ),
            foreign_key(
                "source_anchor_id",
                "retrieval_anchors",
                "anchor_id",
                "NO ACTION"
            ),
            foreign_key(
                "source_summary_id",
                "session_summary_nodes",
                "summary_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "session_summary_successors",
        [
            column("predecessor_summary_id", "TEXT", true, None, 1),
            column("successor_summary_id", "TEXT", true, None, 2),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "predecessor_summary_id",
                "session_summary_nodes",
                "summary_id",
                "NO ACTION"
            ),
            foreign_key(
                "successor_summary_id",
                "session_summary_nodes",
                "summary_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "session_external_payload_manifests",
        [
            column("payload_ref", "TEXT", false, None, 1),
            column("session_id", "TEXT", true, None, 0),
            column("payload_digest", "TEXT", true, None, 0),
            column("manifest_json", "TEXT", true, None, 0),
            column("receipt_id", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [foreign_key(
            "receipt_id",
            "sanitization_receipts",
            "receipt_id",
            "NO ACTION"
        ),]
    ),
    table!(
        "session_refresh_operations",
        [
            column("session_id", "TEXT", true, None, 1),
            column("operation_id", "TEXT", true, None, 2),
            column("request_digest", "TEXT", true, None, 0),
            column("target_frontier_json", "TEXT", true, None, 0),
            column("state", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
            column("updated_at", "INTEGER", true, None, 0),
            column("terminal_at", "INTEGER", false, None, 0),
            column("failure_code", "TEXT", false, None, 0),
        ],
        []
    ),
    table!(
        "session_refresh_bindings",
        [
            column("session_id", "TEXT", true, None, 1),
            column("operation_id", "TEXT", true, None, 2),
            column("scope_kind", "TEXT", true, None, 0),
            column("source_frontier", "INTEGER", true, None, 0),
            column("target_frontier", "INTEGER", true, None, 0),
            column("projector_version", "TEXT", true, None, 0),
            column("config_digest", "TEXT", true, None, 0),
            column("generation", "INTEGER", true, None, 0),
            column("frozen_watermarks_json", "TEXT", true, None, 0),
            column("binding_digest", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_refresh_operations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "operation_id",
                "session_refresh_operations",
                "operation_id",
                "CASCADE",
                1
            ),
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_refresh_progress",
        [
            column("session_id", "TEXT", true, None, 1),
            column("operation_id", "TEXT", true, None, 2),
            column("progress_ordinal", "INTEGER", true, None, 3),
            column("frontier_json", "TEXT", true, None, 0),
            column("coverage_json", "TEXT", true, None, 0),
            column("committed_batches", "INTEGER", true, None, 0),
            column("committed_records", "INTEGER", true, None, 0),
            column("recorded_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_refresh_operations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "operation_id",
                "session_refresh_operations",
                "operation_id",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_refresh_batch_bindings",
        [
            column("session_id", "TEXT", true, None, 1),
            column("operation_id", "TEXT", true, None, 2),
            column("progress_ordinal", "INTEGER", true, None, 3),
            column("generation", "INTEGER", true, None, 0),
            column("batch_ordinal", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_refresh_progress",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "operation_id",
                "session_refresh_progress",
                "operation_id",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "progress_ordinal",
                "session_refresh_progress",
                "progress_ordinal",
                "CASCADE",
                2
            ),
            foreign_key(
                "session_id",
                "session_temporal_projection_receipts",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_projection_receipts",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "batch_ordinal",
                "session_temporal_projection_receipts",
                "batch_ordinal",
                "CASCADE",
                2
            ),
        ]
    ),
    table!(
        "session_refresh_receipts",
        [
            column("session_id", "TEXT", true, None, 1),
            column("operation_id", "TEXT", true, None, 2),
            column("terminal_state", "TEXT", true, None, 0),
            column("frontier_json", "TEXT", true, None, 0),
            column("coverage_json", "TEXT", true, None, 0),
            column("failure_code", "TEXT", false, None, 0),
            column("terminal_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_refresh_operations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "operation_id",
                "session_refresh_operations",
                "operation_id",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_query_cursor_keys",
        [
            column("key_id", "TEXT", false, None, 1),
            column("key_version", "INTEGER", true, None, 0),
            column("key_material", "BLOB", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
            column("retired_at", "INTEGER", false, None, 0),
        ],
        []
    ),
    table!(
        "session_temporal_generations",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("state", "TEXT", true, None, 0),
            column("frozen_watermarks_json", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
            column("ready_at", "INTEGER", false, None, 0),
            column("activated_at", "INTEGER", false, None, 0),
            column("completed_at", "INTEGER", false, None, 0),
        ],
        []
    ),
    table!(
        "session_temporal_projection_receipts",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("batch_ordinal", "INTEGER", true, None, 3),
            column("batch_digest", "TEXT", true, None, 0),
            column("frozen_watermarks_json", "TEXT", true, None, 0),
            column("source_through", "INTEGER", true, None, 0),
            column("projection_through", "INTEGER", true, None, 0),
            column("occurrence_count", "INTEGER", true, None, 0),
            column("occurrence_digest", "TEXT", true, None, 0),
            column("dimension_count", "INTEGER", true, None, 0),
            column("dimension_digest", "TEXT", true, None, 0),
            column("copy_count", "INTEGER", true, None, 0),
            column("copy_digest", "TEXT", true, None, 0),
            column("assertion_count", "INTEGER", true, None, 0),
            column("assertion_digest", "TEXT", true, None, 0),
            column("supersession_count", "INTEGER", true, None, 0),
            column("supersession_digest", "TEXT", true, None, 0),
            column("current_count", "INTEGER", true, None, 0),
            column("current_digest", "TEXT", true, None, 0),
            column("fts_count", "INTEGER", true, None, 0),
            column("fts_digest", "TEXT", true, None, 0),
            column("committed_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_temporal_observation_effects",
        [
            column("observation_id", "TEXT", false, None, 1),
            column("observation_sequence", "INTEGER", true, None, 0),
            column("session_id", "TEXT", true, None, 0),
            column("receipt_id", "TEXT", true, None, 0),
            column("effect_digest", "TEXT", true, None, 0),
            column("output_count", "INTEGER", true, None, 0),
            column("recorded_at", "INTEGER", true, None, 0),
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
        "session_turns",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("turn_id", "TEXT", true, None, 3),
            column("ordinal", "INTEGER", true, None, 0),
            column("grouping_provenance", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_threads",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("thread_id", "TEXT", true, None, 3),
            column("grouping_provenance", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_agents",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("agent_id", "TEXT", true, None, 3),
            column("agent_json", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_occurrences",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("occurrence_id", "TEXT", true, None, 3),
            column("source_observation_id", "TEXT", true, None, 0),
            column("projection_output_ordinal", "INTEGER", true, None, 0),
            column("retrieval_anchor_id", "TEXT", true, None, 0),
            column("thread_id", "TEXT", false, None, 0),
            column("thread_grouping_json", "TEXT", false, None, 0),
            column("turn_id", "TEXT", false, None, 0),
            column("turn_grouping_json", "TEXT", false, None, 0),
            column("message_id", "TEXT", false, None, 0),
            column("agent_id", "TEXT", false, None, 0),
            column("role", "TEXT", true, None, 0),
            column("knowledge_at", "INTEGER", true, None, 0),
            column("valid_time_json", "TEXT", true, None, 0),
            column("evidence_json", "TEXT", true, None, 0),
            column("snippet_text", "TEXT", true, None, 0),
            column("index_text", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "source_observation_id",
                "observations",
                "observation_id",
                "NO ACTION"
            ),
            foreign_key(
                "retrieval_anchor_id",
                "retrieval_anchors",
                "anchor_id",
                "NO ACTION"
            ),
            foreign_key("session_id", "session_threads", "session_id", "NO ACTION"),
            foreign_key_sequence(
                "generation",
                "session_threads",
                "generation",
                "NO ACTION",
                1
            ),
            foreign_key_sequence("thread_id", "session_threads", "thread_id", "NO ACTION", 2),
            foreign_key("session_id", "session_turns", "session_id", "NO ACTION"),
            foreign_key_sequence("generation", "session_turns", "generation", "NO ACTION", 1),
            foreign_key_sequence("turn_id", "session_turns", "turn_id", "NO ACTION", 2),
            foreign_key("session_id", "session_agents", "session_id", "NO ACTION"),
            foreign_key_sequence("generation", "session_agents", "generation", "NO ACTION", 1),
            foreign_key_sequence("agent_id", "session_agents", "agent_id", "NO ACTION", 2),
        ]
    ),
    table!(
        "session_logical_copy_edges",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("occurrence_id", "TEXT", true, None, 3),
            column("copied_from_occurrence_id", "TEXT", true, None, 4),
            column("proof_json", "TEXT", true, None, 0),
            column("knowledge_at", "INTEGER", true, None, 0),
            column("valid_time_json", "TEXT", true, None, 0),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key("session_id", "session_occurrences", "session_id", "CASCADE"),
            foreign_key_sequence(
                "generation",
                "session_occurrences",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "occurrence_id",
                "session_occurrences",
                "occurrence_id",
                "CASCADE",
                2
            ),
            foreign_key("session_id", "session_occurrences", "session_id", "CASCADE"),
            foreign_key_sequence(
                "generation",
                "session_occurrences",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "copied_from_occurrence_id",
                "session_occurrences",
                "occurrence_id",
                "CASCADE",
                2
            ),
        ]
    ),
    table!(
        "session_turn_members",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("turn_id", "TEXT", true, None, 3),
            column("occurrence_id", "TEXT", true, None, 4),
            column("ordinal", "INTEGER", true, None, 0),
        ],
        [
            foreign_key("session_id", "session_turns", "session_id", "CASCADE"),
            foreign_key_sequence("generation", "session_turns", "generation", "CASCADE", 1),
            foreign_key_sequence("turn_id", "session_turns", "turn_id", "CASCADE", 2),
            foreign_key("session_id", "session_occurrences", "session_id", "CASCADE"),
            foreign_key_sequence(
                "generation",
                "session_occurrences",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "occurrence_id",
                "session_occurrences",
                "occurrence_id",
                "CASCADE",
                2
            ),
        ]
    ),
    table!(
        "session_thread_hierarchy_edges",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("parent_thread_id", "TEXT", true, None, 3),
            column("child_thread_id", "TEXT", true, None, 4),
            column("ordinal", "INTEGER", true, None, 0),
        ],
        [
            foreign_key("session_id", "session_threads", "session_id", "CASCADE"),
            foreign_key_sequence("generation", "session_threads", "generation", "CASCADE", 1),
            foreign_key_sequence(
                "parent_thread_id",
                "session_threads",
                "thread_id",
                "CASCADE",
                2
            ),
            foreign_key("session_id", "session_threads", "session_id", "CASCADE"),
            foreign_key_sequence("generation", "session_threads", "generation", "CASCADE", 1),
            foreign_key_sequence(
                "child_thread_id",
                "session_threads",
                "thread_id",
                "CASCADE",
                2
            ),
        ]
    ),
    table!(
        "session_agent_hierarchy_edges",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("parent_agent_id", "TEXT", true, None, 3),
            column("child_agent_id", "TEXT", true, None, 4),
            column("ordinal", "INTEGER", true, None, 0),
        ],
        [
            foreign_key("session_id", "session_agents", "session_id", "CASCADE"),
            foreign_key_sequence("generation", "session_agents", "generation", "CASCADE", 1),
            foreign_key_sequence(
                "parent_agent_id",
                "session_agents",
                "agent_id",
                "CASCADE",
                2
            ),
            foreign_key("session_id", "session_agents", "session_id", "CASCADE"),
            foreign_key_sequence("generation", "session_agents", "generation", "CASCADE", 1),
            foreign_key_sequence("child_agent_id", "session_agents", "agent_id", "CASCADE", 2),
        ]
    ),
    table!(
        "session_assertions",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("assertion_id", "TEXT", true, None, 3),
            column("assertion_kind", "TEXT", true, None, 0),
            column("subject_anchor_id", "TEXT", true, None, 0),
            column("object_anchor_id", "TEXT", true, None, 0),
            column("knowledge_at", "INTEGER", true, None, 0),
            column("valid_time_json", "TEXT", true, None, 0),
            column("evidence_json", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "subject_anchor_id",
                "retrieval_anchors",
                "anchor_id",
                "NO ACTION"
            ),
            foreign_key(
                "object_anchor_id",
                "retrieval_anchors",
                "anchor_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "session_assertion_supersession",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("superseded_assertion_id", "TEXT", true, None, 3),
            column("superseding_assertion_id", "TEXT", true, None, 4),
            column("created_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key("session_id", "session_assertions", "session_id", "CASCADE"),
            foreign_key_sequence(
                "generation",
                "session_assertions",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "superseded_assertion_id",
                "session_assertions",
                "assertion_id",
                "CASCADE",
                2
            ),
            foreign_key("session_id", "session_assertions", "session_id", "CASCADE"),
            foreign_key_sequence(
                "generation",
                "session_assertions",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "superseding_assertion_id",
                "session_assertions",
                "assertion_id",
                "CASCADE",
                2
            ),
        ]
    ),
    table!(
        "session_current_entities",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("entity_kind", "TEXT", true, None, 3),
            column("entity_id", "TEXT", true, None, 4),
            column("current_assertion_id", "TEXT", false, None, 0),
            column("current_occurrence_id", "TEXT", false, None, 0),
            column("coverage_json", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "session_id",
                "session_assertions",
                "session_id",
                "NO ACTION"
            ),
            foreign_key_sequence(
                "generation",
                "session_assertions",
                "generation",
                "NO ACTION",
                1
            ),
            foreign_key_sequence(
                "current_assertion_id",
                "session_assertions",
                "assertion_id",
                "NO ACTION",
                2
            ),
            foreign_key(
                "session_id",
                "session_occurrences",
                "session_id",
                "NO ACTION"
            ),
            foreign_key_sequence(
                "generation",
                "session_occurrences",
                "generation",
                "NO ACTION",
                1
            ),
            foreign_key_sequence(
                "current_occurrence_id",
                "session_occurrences",
                "occurrence_id",
                "NO ACTION",
                2
            ),
        ]
    ),
    table!(
        "session_derived_evidence",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("evidence_kind", "TEXT", true, None, 3),
            column("evidence_id", "TEXT", true, None, 4),
            column("retrieval_anchor_id", "TEXT", true, None, 0),
            column("thread_id", "TEXT", false, None, 0),
            column("first_occurrence_id", "TEXT", true, None, 0),
            column("last_occurrence_id", "TEXT", true, None, 0),
            column("algorithm_version", "TEXT", true, None, 0),
            column("configuration_digest", "TEXT", true, None, 0),
            column("member_count", "INTEGER", true, None, 0),
            column("member_digest", "TEXT", true, None, 0),
            column("evidence_json", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "retrieval_anchor_id",
                "retrieval_anchors",
                "anchor_id",
                "NO ACTION"
            ),
            foreign_key(
                "session_id",
                "session_occurrences",
                "session_id",
                "NO ACTION"
            ),
            foreign_key_sequence(
                "generation",
                "session_occurrences",
                "generation",
                "NO ACTION",
                1
            ),
            foreign_key_sequence(
                "first_occurrence_id",
                "session_occurrences",
                "occurrence_id",
                "NO ACTION",
                2
            ),
            foreign_key(
                "session_id",
                "session_occurrences",
                "session_id",
                "NO ACTION"
            ),
            foreign_key_sequence(
                "generation",
                "session_occurrences",
                "generation",
                "NO ACTION",
                1
            ),
            foreign_key_sequence(
                "last_occurrence_id",
                "session_occurrences",
                "occurrence_id",
                "NO ACTION",
                2
            ),
        ]
    ),
    table!(
        "session_derived_evidence_members",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("evidence_kind", "TEXT", true, None, 3),
            column("evidence_id", "TEXT", true, None, 4),
            column("ordinal", "INTEGER", true, None, 5),
            column("occurrence_id", "TEXT", true, None, 0),
            column("member_role", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_derived_evidence",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_derived_evidence",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key_sequence(
                "evidence_kind",
                "session_derived_evidence",
                "evidence_kind",
                "CASCADE",
                2
            ),
            foreign_key_sequence(
                "evidence_id",
                "session_derived_evidence",
                "evidence_id",
                "CASCADE",
                3
            ),
            foreign_key(
                "session_id",
                "session_occurrences",
                "session_id",
                "NO ACTION"
            ),
            foreign_key_sequence(
                "generation",
                "session_occurrences",
                "generation",
                "NO ACTION",
                1
            ),
            foreign_key_sequence(
                "occurrence_id",
                "session_occurrences",
                "occurrence_id",
                "NO ACTION",
                2
            ),
        ]
    ),
    table!(
        "session_summary_availability",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("summary_id", "TEXT", true, None, 3),
            column("availability", "TEXT", true, None, 0),
            column("source_horizon_json", "TEXT", true, None, 0),
            column("reason", "TEXT", false, None, 0),
            column("checked_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
            foreign_key(
                "summary_id",
                "session_summary_nodes",
                "summary_id",
                "NO ACTION"
            ),
        ]
    ),
    table!(
        "session_temporal_migration_dispositions",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("batch_ordinal", "INTEGER", true, None, 3),
            column("disposition_ordinal", "INTEGER", true, None, 4),
            column("provider", "TEXT", true, None, 0),
            column("message_id", "TEXT", true, None, 0),
            column("output_ordinal", "INTEGER", true, None, 0),
            column("observation_id", "TEXT", false, None, 0),
            column("retrieval_anchor_id", "TEXT", false, None, 0),
            column("disposition", "TEXT", true, None, 0),
            column("reason", "TEXT", true, None, 0),
            column("row_digest", "TEXT", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
            ),
        ]
    ),
    table!(
        "session_temporal_migration_receipts",
        [
            column("session_id", "TEXT", true, None, 1),
            column("generation", "INTEGER", true, None, 2),
            column("batch_ordinal", "INTEGER", true, None, 3),
            column("source_digest", "TEXT", true, None, 0),
            column("frozen_watermarks_json", "TEXT", true, None, 0),
            column("imported_items", "INTEGER", true, None, 0),
            column("committed_at", "INTEGER", true, None, 0),
        ],
        [
            foreign_key(
                "session_id",
                "session_temporal_generations",
                "session_id",
                "CASCADE"
            ),
            foreign_key_sequence(
                "generation",
                "session_temporal_generations",
                "generation",
                "CASCADE",
                1
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
        table: "retrieval_anchors",
        name: Some("idx_retrieval_anchors_owner"),
        unique: true,
        origin: "c",
        columns: &["anchor_id", "owner_json"],
    },
    Index {
        table: "observation_retrieval_anchors",
        name: None,
        unique: true,
        origin: "u",
        columns: &["anchor_id"],
    },
    Index {
        table: "observation_repository_provenance",
        name: None,
        unique: true,
        origin: "u",
        columns: &["retrieval_anchor_id"],
    },
    Index {
        table: "retrieval_anchor_aliases",
        name: None,
        unique: true,
        origin: "u",
        columns: &["anchor_id", "alias_kind", "locator_digest"],
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
    Index {
        table: "session_summary_nodes",
        name: Some("idx_session_summary_nodes_session_created"),
        unique: false,
        origin: "c",
        columns: &["session_id", "created_at"],
    },
    Index {
        table: "session_summary_nodes",
        name: Some("idx_session_summary_nodes_root_created_order"),
        unique: false,
        origin: "c",
        columns: &["created_at", "session_id", "summary_id"],
    },
    Index {
        table: "session_summary_sources",
        name: Some("idx_session_summary_sources_anchor"),
        unique: false,
        origin: "c",
        columns: &["source_anchor_id"],
    },
    Index {
        table: "session_summary_sources",
        name: Some("idx_session_summary_sources_summary"),
        unique: false,
        origin: "c",
        columns: &["source_summary_id", "summary_id"],
    },
    Index {
        table: "session_summary_successors",
        name: Some("idx_session_summary_successors_successor"),
        unique: false,
        origin: "c",
        columns: &[
            "successor_summary_id",
            "created_at",
            "predecessor_summary_id",
        ],
    },
    Index {
        table: "session_external_payload_manifests",
        name: Some("idx_session_external_payload_manifests_session"),
        unique: false,
        origin: "c",
        columns: &["session_id"],
    },
    Index {
        table: "session_refresh_operations",
        name: Some("idx_session_refresh_operations_join"),
        unique: false,
        origin: "c",
        columns: &["session_id", "request_digest", "state"],
    },
    Index {
        table: "session_refresh_operations",
        name: Some("idx_session_refresh_operations_state"),
        unique: false,
        origin: "c",
        columns: &["state", "updated_at"],
    },
    Index {
        table: "session_refresh_operations",
        name: Some("idx_session_refresh_operations_one_running"),
        unique: true,
        origin: "c",
        columns: &["session_id"],
    },
    Index {
        table: "session_refresh_bindings",
        name: None,
        unique: true,
        origin: "u",
        columns: &["session_id", "generation"],
    },
    Index {
        table: "session_refresh_batch_bindings",
        name: None,
        unique: true,
        origin: "u",
        columns: &["session_id", "generation", "batch_ordinal"],
    },
    Index {
        table: "session_refresh_receipts",
        name: Some("idx_session_refresh_receipts_session"),
        unique: false,
        origin: "c",
        columns: &["session_id", "terminal_at"],
    },
    Index {
        table: "session_query_cursor_keys",
        name: Some("idx_session_query_cursor_keys_active"),
        unique: false,
        origin: "c",
        columns: &["retired_at", "key_version"],
    },
    Index {
        table: "session_query_cursor_keys",
        name: None,
        unique: true,
        origin: "u",
        columns: &["key_version"],
    },
    Index {
        table: "session_temporal_generations",
        name: Some("idx_session_temporal_generations_session_state"),
        unique: false,
        origin: "c",
        columns: &["session_id", "state"],
    },
    Index {
        table: "session_temporal_generations",
        name: Some("idx_session_temporal_generations_one_active"),
        unique: true,
        origin: "c",
        columns: &["session_id"],
    },
    Index {
        table: "session_temporal_projection_receipts",
        name: None,
        unique: true,
        origin: "u",
        columns: &["session_id", "generation", "batch_digest"],
    },
    Index {
        table: "session_temporal_observation_effects",
        name: None,
        unique: true,
        origin: "u",
        columns: &["observation_sequence"],
    },
    Index {
        table: "session_temporal_observation_effects",
        name: Some("idx_session_temporal_observation_effects_session"),
        unique: false,
        origin: "c",
        columns: &["session_id", "observation_sequence"],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_generation_order"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "knowledge_at", "occurrence_id"],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_root_generation_order"),
        unique: false,
        origin: "c",
        columns: &["knowledge_at", "session_id", "occurrence_id", "generation"],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_session_time"),
        unique: false,
        origin: "c",
        columns: &["session_id", "knowledge_at"],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_anchor_order"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "retrieval_anchor_id",
            "knowledge_at",
            "occurrence_id",
        ],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_message"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "message_id",
            "knowledge_at",
            "occurrence_id",
        ],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_thread"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "thread_id",
            "knowledge_at",
            "occurrence_id",
        ],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_turn"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "turn_id",
            "knowledge_at",
            "occurrence_id",
        ],
    },
    Index {
        table: "session_occurrences",
        name: Some("idx_session_occurrences_agent"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "agent_id",
            "knowledge_at",
            "occurrence_id",
        ],
    },
    Index {
        table: "session_logical_copy_edges",
        name: Some("idx_session_logical_copy_edges_target"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "copied_from_occurrence_id"],
    },
    Index {
        table: "session_turn_members",
        name: Some("idx_session_turn_members_occurrence"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "occurrence_id"],
    },
    Index {
        table: "session_thread_hierarchy_edges",
        name: Some("idx_session_thread_hierarchy_edges_child"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "child_thread_id"],
    },
    Index {
        table: "session_agent_hierarchy_edges",
        name: Some("idx_session_agent_hierarchy_edges_child"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "child_agent_id"],
    },
    Index {
        table: "session_assertions",
        name: Some("idx_session_assertions_subject"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "subject_anchor_id"],
    },
    Index {
        table: "session_assertions",
        name: Some("idx_session_assertions_object_order"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "object_anchor_id",
            "knowledge_at",
            "assertion_id",
        ],
    },
    Index {
        table: "session_assertions",
        name: Some("idx_session_assertions_kind_order"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "assertion_kind",
            "knowledge_at",
            "assertion_id",
        ],
    },
    Index {
        table: "session_assertions",
        name: Some("idx_session_assertions_generation_order"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "knowledge_at", "assertion_id"],
    },
    Index {
        table: "session_assertion_supersession",
        name: Some("idx_session_assertion_supersession_successor"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "superseding_assertion_id"],
    },
    Index {
        table: "session_current_entities",
        name: Some("idx_session_current_entities_assertion"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "current_assertion_id"],
    },
    Index {
        table: "session_current_entities",
        name: Some("idx_session_current_entities_occurrence"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "current_occurrence_id"],
    },
    Index {
        table: "session_derived_evidence",
        name: Some("idx_session_derived_evidence_scope_order"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "evidence_kind",
            "first_occurrence_id",
            "evidence_id",
        ],
    },
    Index {
        table: "session_derived_evidence",
        name: Some("idx_session_derived_evidence_anchor"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "retrieval_anchor_id",
            "evidence_kind",
            "evidence_id",
        ],
    },
    Index {
        table: "session_derived_evidence",
        name: Some("idx_session_derived_evidence_thread_order"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "thread_id",
            "evidence_kind",
            "first_occurrence_id",
            "evidence_id",
        ],
    },
    Index {
        table: "session_derived_evidence_members",
        name: None,
        unique: true,
        origin: "u",
        columns: &[
            "session_id",
            "generation",
            "evidence_kind",
            "evidence_id",
            "occurrence_id",
        ],
    },
    Index {
        table: "session_derived_evidence_members",
        name: Some("idx_session_derived_evidence_members_occurrence"),
        unique: false,
        origin: "c",
        columns: &[
            "session_id",
            "generation",
            "occurrence_id",
            "evidence_kind",
            "evidence_id",
            "ordinal",
        ],
    },
    Index {
        table: "session_summary_availability",
        name: Some("idx_session_summary_availability_generation"),
        unique: false,
        origin: "c",
        columns: &["session_id", "generation", "availability"],
    },
    Index {
        table: "session_temporal_migration_receipts",
        name: Some("idx_session_temporal_migration_receipts_source"),
        unique: false,
        origin: "c",
        columns: &["session_id", "source_digest", "generation"],
    },
    Index {
        table: "session_temporal_migration_dispositions",
        name: Some("idx_session_temporal_migration_dispositions_row"),
        unique: false,
        origin: "c",
        columns: &["session_id", "provider", "message_id", "output_ordinal"],
    },
    Index {
        table: "session_temporal_migration_dispositions",
        name: Some("idx_session_temporal_migration_dispositions_kind"),
        unique: false,
        origin: "c",
        columns: &["session_id", "disposition", "generation"],
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

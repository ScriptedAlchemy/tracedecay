#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterParityFixtureV1 {
    pub family: &'static str,
    pub write_payloads: &'static [&'static str],
    pub read_operations: &'static [&'static str],
    pub canonical_tables: &'static [&'static str],
}

/// Static parity inventory consumed by later registry-mount acceptance tests.
///
/// These fixtures name only existing migration-owned tables. They do not
/// install schemas, open stores, or create another storage authority.
pub const PRE_CUTOVER_ADAPTER_PARITY_FIXTURES_V1: &[AdapterParityFixtureV1] = &[
    AdapterParityFixtureV1 {
        family: "profile",
        write_payloads: &["configuration"],
        read_operations: &["current_configuration", "configuration_revision"],
        canonical_tables: &[
            "configuration_revisions",
            "configuration_entries",
            "configuration_mutation_receipts",
            "configuration_audit_events",
        ],
    },
    AdapterParityFixtureV1 {
        family: "project",
        write_payloads: &["fact", "observation", "diagnostics", "diagnostic_supersession"],
        read_operations: &[
            "fact_current",
            "fact_lineage",
            "observation_source_cursor",
            "observation",
            "diagnostic_current_generation",
            "diagnostic_generation",
            "diagnostic_current_for_file",
            "diagnostic_by_anchor",
            "diagnostic_stale",
            "diagnostic_supersession_chain",
        ],
        canonical_tables: &[
            "memory_v2_facts",
            "memory_v2_assertions",
            "memory_v2_lineage_events",
            "memory_v2_current_facts",
            "retrieval_anchors",
            "observations",
            "source_cursors",
            "generation_diagnostics",
            "diagnostic_generation_publications",
        ],
    },
    AdapterParityFixtureV1 {
        family: "external_source",
        write_payloads: &["external_source"],
        read_operations: &["external_source_state"],
        canonical_tables: &["external_source_states_v1"],
    },
    AdapterParityFixtureV1 {
        family: "sessions",
        write_payloads: &["session_projection", "session_summary"],
        read_operations: &["projection_batch", "summary"],
        canonical_tables: &[
            "session_temporal_generations",
            "session_temporal_projection_receipts",
            "session_occurrences",
            "session_logical_copy_edges",
            "session_assertions",
            "session_summary_nodes",
            "session_summary_sources",
            "session_summary_successors",
        ],
    },
];

mod definitions;
mod invariants;
mod pragma;
mod validation;

pub(crate) fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

fn normalize_trigger_sql(sql: &str) -> String {
    sql.trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub(crate) use definitions::SESSION_RELATION_RECEIPT_RECOVERY_COLUMNS;
pub(crate) use invariants::{
    authority_invariant_triggers_intact, released_v3_invariant_triggers_intact,
    validate_authority_rows_exhaustive,
};
pub use invariants::{
    ensure_authority_audit_checkpoint_schema, ensure_authority_invariant_schema,
    require_foreign_key_audit,
};
pub(crate) use invariants::{
    ensure_authority_invariants, ensure_fresh_authority_invariants,
    invariant_trigger_names_for_tables, invariant_trigger_sql_for_tables,
};
pub use validation::validate_registry_schema_contract;
pub(crate) use validation::{
    validate_authority_schema_contract, validate_released_v3_temporal_projection_receipt_contract,
    validate_remote_deletion_schema_contract, validate_session_graph_publication_schema_contract,
    validate_session_relation_receipts_without_recovery_contract,
    validate_session_temporal_schema_contract,
};

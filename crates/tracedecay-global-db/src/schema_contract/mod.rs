mod definitions;
mod invariants;
mod pragma;
mod validation;

fn normalize_trigger_sql(sql: &str) -> String {
    sql.trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub(super) use invariants::{
    authority_invariant_triggers_intact, restore_immutability_after_canonical_repair,
    suspend_immutability_for_canonical_repair, suspend_session_invariants_for_schema_upgrade,
    validate_authority_rows_exhaustive,
};
pub use invariants::{
    ensure_authority_audit_checkpoint_schema, ensure_authority_invariant_schema,
    ensure_authority_invariants, require_foreign_key_audit,
};
pub use validation::validate_registry_schema_contract;
pub(super) use validation::{
    validate_authority_schema_contract, validate_observation_migration_source,
};

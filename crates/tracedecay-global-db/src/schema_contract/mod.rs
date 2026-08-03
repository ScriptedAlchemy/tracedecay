mod definitions;
mod final_schema;
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

pub use final_schema::{final_registered_schema_contract, install_final_registered_schema};
pub use invariants::{
    ensure_authority_audit_checkpoint_schema, ensure_authority_invariant_schema,
    ensure_authority_invariants, require_foreign_key_audit,
};
pub(super) use invariants::{
    restore_immutability_after_canonical_repair, suspend_immutability_for_canonical_repair,
    validate_authority_rows_exhaustive,
};
pub(super) use validation::validate_authority_schema_contract;
pub use validation::validate_registry_schema_contract;

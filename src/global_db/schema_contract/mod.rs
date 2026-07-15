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
    ensure_authority_invariants, restore_immutability_after_canonical_repair,
    suspend_immutability_for_canonical_repair, validate_authority_rows_exhaustive,
};
pub(super) use validation::{
    validate_authority_schema_contract, validate_observation_migration_source,
    validate_registry_schema_contract,
};

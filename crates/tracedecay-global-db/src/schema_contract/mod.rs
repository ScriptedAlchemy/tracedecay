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

pub use invariants::ensure_authority_invariant_schema;
pub(super) use invariants::validate_authority_rows_exhaustive;
pub(super) use validation::validate_authority_schema_contract;
pub use validation::validate_registry_schema_contract;

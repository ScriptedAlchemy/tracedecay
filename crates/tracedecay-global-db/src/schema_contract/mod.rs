mod definitions;
pub(crate) mod invariants;
mod pragma;
mod validation;

use tracedecay_domain::canonical_text::canonical_framed_sha256;
use tracedecay_lcm::schema::LCM_SCHEMA_VERSION;
use tracedecay_runtime_core::storage::STORE_MANIFEST_SCHEMA_VERSION;
use tracedecay_rusqlite_runtime::workflow::{
    WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_SCHEMA_VERSION_V1, WORKFLOW_TABLE_CONTRACTS_V1,
};
use tracedecay_session_temporal_store::{SESSION_TEMPORAL_SCHEMA_VERSION, TEMPORAL_TABLE_COLUMNS};
use tracedecay_sessions::runtime::git_correlation::GIT_CORRELATION_SCHEMA_VERSION;

use crate::configuration::{CONFIGURATION_FORMAT_REVISION, TOPOLOGY_POLICY_SCHEMA_VERSION};
use crate::observation::OBSERVATION_ADMISSION_MARKERS;

/// Domain tag for [`expected_admitted_schema_fingerprint`]. Distinct from the
/// graph-database final-shape domain so the two identities are not interchangeable.
const ADMITTED_SCHEMA_FINGERPRINT_DOMAIN: &[u8] = b"tracedecay.admitted-store-schema.v1";

/// Fingerprint of the schema constants a seeded store is admitted against.
///
/// Walks the authority table, index, and trigger contracts, the session-temporal
/// column contracts, and the format revisions stored beside them. Changing any
/// of those constants changes this digest; callers do not keep a second list.
pub fn expected_admitted_schema_fingerprint() -> String {
    hash_admitted_schema(None).0
}

/// Domain tag for [`registered_schema_admission_digest`].
const REGISTERED_SCHEMA_ADMISSION_DIGEST_DOMAIN: &[u8] =
    b"tracedecay.registered-schema-admission.v1";

/// Digest of every contract registered-schema admission can refuse a store
/// on: the admitted authority fingerprint plus the LCM, session-temporal,
/// git-correlation, workflow, and observation identities admission compares a
/// store against. A store admitted under this digest is admitted again by any
/// binary that computes the same one; additive install stages never refuse.
pub fn registered_schema_admission_digest() -> String {
    let mut parts = vec![
        format!("admitted:{}", expected_admitted_schema_fingerprint()),
        format!("lcm:{LCM_SCHEMA_VERSION}"),
        format!("session_temporal:{SESSION_TEMPORAL_SCHEMA_VERSION}"),
        format!("git_correlation:{GIT_CORRELATION_SCHEMA_VERSION}"),
        format!("workflow:{WORKFLOW_SCHEMA_VERSION_V1}:{WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}"),
    ];
    for table in WORKFLOW_TABLE_CONTRACTS_V1 {
        parts.push(format!("workflow_table:{}:{}", table.name, table.sql));
    }
    for marker in OBSERVATION_ADMISSION_MARKERS {
        parts.push(format!("observation_marker:{marker}"));
    }
    let bytes: Vec<&[u8]> = parts.iter().map(String::as_bytes).collect();
    canonical_framed_sha256(REGISTERED_SCHEMA_ADMISSION_DIGEST_DOMAIN, &bytes)
}

/// Fingerprint of the admitted contract with one column constant removed.
///
/// `None` when `table`/`column` is not part of the contract, so a removed
/// column cannot be mistaken for a different current shape.
pub fn authority_schema_fingerprint_omitting_column(table: &str, column: &str) -> Option<String> {
    let (fingerprint, omitted) = hash_admitted_schema(Some((table, column)));
    omitted.then_some(fingerprint)
}

fn hash_admitted_schema(omit: Option<(&str, &str)>) -> (String, bool) {
    let mut parts = Vec::new();
    let mut omitted = false;
    parts.push(format!(
        "configuration_format:{CONFIGURATION_FORMAT_REVISION}"
    ));
    parts.push(format!("topology_policy:{TOPOLOGY_POLICY_SCHEMA_VERSION}"));
    parts.push(format!("store_manifest:{STORE_MANIFEST_SCHEMA_VERSION}"));
    for table in definitions::TABLES {
        parts.push(format!("table:{}", table.name));
        for column in table.columns {
            if omit == Some((table.name, column.name)) {
                omitted = true;
                continue;
            }
            let default_value = match column.default_value {
                Some(value) => format!("some:{value}"),
                None => "none".to_owned(),
            };
            parts.push(format!(
                "col:{}:{}:{}:{}:{default_value}:{}:{}",
                table.name,
                column.name,
                column.declared_type,
                u8::from(column.not_null),
                column.primary_key_ordinal,
                column.hidden,
            ));
        }
        for foreign_key in table.foreign_keys {
            parts.push(format!(
                "fk:{}:{}:{}:{}:{}:{}",
                table.name,
                foreign_key.sequence,
                foreign_key.from,
                foreign_key.target_table,
                foreign_key.target_column,
                foreign_key.on_delete,
            ));
        }
    }
    for index in definitions::INDEXES {
        parts.push(format!(
            "index:{}:{}:{}:{}:{}",
            index.table,
            index.name.unwrap_or(""),
            u8::from(index.unique),
            index.origin,
            index.columns.join(","),
        ));
    }
    for (index_name, columns) in definitions::INDEX_DESCENDING_COLUMNS {
        parts.push(format!("index_desc:{index_name}:{}", columns.join(",")));
    }
    parts.push(format!(
        "index_expression:{}",
        definitions::INDEX_EXPRESSION_COLUMN
    ));
    for invariant in invariants::INVARIANTS {
        for trigger in invariant.triggers {
            parts.push(format!(
                "trigger:{}:{}:{}",
                trigger.name, trigger.table, trigger.create_sql
            ));
        }
    }
    for (table, columns) in TEMPORAL_TABLE_COLUMNS {
        parts.push(format!("temporal:{table}"));
        for column in *columns {
            if omit == Some((*table, *column)) {
                omitted = true;
                continue;
            }
            parts.push(format!("temporal_col:{table}:{column}"));
        }
    }
    let bytes: Vec<&[u8]> = parts.iter().map(String::as_bytes).collect();
    (
        canonical_framed_sha256(ADMITTED_SCHEMA_FINGERPRINT_DOMAIN, &bytes),
        omitted,
    )
}

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

pub(crate) use invariants::{
    authority_invariant_triggers_intact, validate_authority_rows_exhaustive,
};
pub use invariants::{
    ensure_authority_audit_checkpoint_schema, ensure_authority_invariant_schema,
    require_foreign_key_audit,
};
pub(crate) use invariants::{ensure_authority_invariants, ensure_fresh_authority_invariants};
pub use validation::validate_registry_schema_contract;
pub(crate) use validation::{
    validate_authority_schema_contract, validate_remote_deletion_schema_contract,
    validate_session_graph_publication_schema_contract, validate_session_temporal_schema_contract,
};

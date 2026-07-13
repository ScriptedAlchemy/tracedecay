pub mod baseline;
pub mod footprint;
pub mod model;
pub mod platform;
pub mod render;
pub mod storage;
pub mod surfaces;
pub mod validate;

use footprint::{CheckedFootprintDescriptors, FootprintError};
use model::{
    COMPATIBILITY_INVENTORY_SCHEMA_V1, CompatibilityInventoryV1, InventorySummariesV1,
    InventoryValidationError,
};

// Advance this only when the inventory baseline moves to the next migration PR.
const CURRENT_MIGRATION_PR: u32 = 3;

pub struct GenerateInventoryOptions<'a> {
    pub architecture_toml: &'a str,
    pub cargo_metadata_json: &'a str,
    pub footprint_descriptors: CheckedFootprintDescriptors,
}

#[derive(Debug, thiserror::Error)]
pub enum GenerateInventoryError {
    #[error("footprint inventory failed: {0}")]
    Footprint(#[source] FootprintError),
    #[error("platform inventory failed: {0}")]
    Platform(String),
    #[error("compatibility inventory failed: {0}")]
    Validation(#[source] InventoryValidationError),
}

pub fn generate_inventory(
    options: GenerateInventoryOptions<'_>,
) -> Result<CompatibilityInventoryV1, GenerateInventoryError> {
    let storage_entries = storage::storage_entries();
    let source_family_appendix = storage::storage_source_family_appendix(&storage_entries);
    let platform_entries =
        platform::platform_entries().map_err(GenerateInventoryError::Platform)?;
    platform::validate_platform_entries(&platform_entries)
        .map_err(GenerateInventoryError::Platform)?;

    let mut entries = surfaces::discover_surfaces();
    entries.extend(storage_entries);
    entries.extend(platform_entries);
    entries.extend(
        footprint::collect_footprint_entries(
            options.architecture_toml,
            options.cargo_metadata_json,
            options.footprint_descriptors,
        )
        .map_err(GenerateInventoryError::Footprint)?,
    );
    entries.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));

    let inventory = CompatibilityInventoryV1 {
        schema: COMPATIBILITY_INVENTORY_SCHEMA_V1.to_owned(),
        summaries: InventorySummariesV1::from_entries(&entries),
        entries,
        source_family_appendix,
    };
    validate::validate_inventory(&inventory).map_err(GenerateInventoryError::Validation)?;
    inventory
        .validate_adapter_deadlines(CURRENT_MIGRATION_PR)
        .map_err(GenerateInventoryError::Validation)?;
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_summary_is_canonical() {
        assert_eq!(
            InventorySummariesV1::from_entries(&[]),
            InventorySummariesV1::default()
        );
    }

    #[test]
    fn production_generation_rejects_an_adapter_expired_at_the_current_pr() {
        const ARCHITECTURE: &str = r#"
package_ceiling = 12

[budgets]
definite_duplicate_body_lines = 0
default_binary_ratio_max = 1.0
idle_rss_ratio_max = 1.0
hot_build_ratio_max = 1.0
clean_build_ratio_max = 1.0
parity_replacement = "smaller"
generated_accounting = "separate"

[[adapter_contracts]]
id = "expired"
owner = "root"
delete_by_pr = "PR 3"
new_callers_forbidden = true
policy_forbidden = true
required_fields = ["adapter_id", "owner"]
"#;
        const CARGO_METADATA: &str =
            r#"{"packages":[],"workspace_members":[],"workspace_root":"/repo"}"#;

        let error = generate_inventory(GenerateInventoryOptions {
            architecture_toml: ARCHITECTURE,
            cargo_metadata_json: CARGO_METADATA,
            footprint_descriptors: CheckedFootprintDescriptors::default(),
        })
        .unwrap_err();

        let GenerateInventoryError::Validation(error) = error else {
            panic!("expected adapter deadline validation, got {error}");
        };
        assert!(error.field.ends_with("delete_by_pr"));
        assert!(error.message.contains("expired at PR 3"));
    }
}

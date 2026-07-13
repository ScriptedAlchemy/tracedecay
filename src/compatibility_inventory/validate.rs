//! Release-gate validation for the generated compatibility inventory.

use super::model::{
    CompatibilityEntryV1, CompatibilityInventoryV1, InventoryValidationError,
    PlatformDispositionV1, RouteStatusV1,
};

/// Validates both the schema contract and the release-blocking inventory rules.
pub fn validate_inventory(
    inventory: &CompatibilityInventoryV1,
) -> Result<(), InventoryValidationError> {
    inventory.validate()?;

    for (index, entry) in inventory.entries.iter().enumerate() {
        let field = format!("entries[{index}]");
        validate_ownership(entry, &field)?;
        validate_adapter(entry, &field)?;
        validate_platform_lane(entry, &field)?;
        validate_storage_mapping(entry, &field)?;
    }
    for (index, appendix) in inventory.source_family_appendix.iter().enumerate() {
        if is_unowned(&appendix.owner) {
            return Err(failure(
                format!("source_family_appendix[{index}].owner"),
                "must name a concrete owner",
            ));
        }
        if appendix.entry_refs.is_empty() {
            return Err(failure(
                format!("source_family_appendix[{index}].entry_refs"),
                "storage source family must map at least one tested entry",
            ));
        }
    }

    Ok(())
}

fn validate_ownership(
    entry: &CompatibilityEntryV1,
    field: &str,
) -> Result<(), InventoryValidationError> {
    for (name, owner) in [
        ("v1_owner", entry.owners.v1_owner.as_str()),
        ("v2_owner", entry.owners.v2_owner.as_str()),
    ] {
        if is_unowned(owner) {
            return Err(failure(
                format!("{field}.owners.{name}"),
                "must name a concrete owner",
            ));
        }
    }
    Ok(())
}

fn validate_adapter(
    entry: &CompatibilityEntryV1,
    field: &str,
) -> Result<(), InventoryValidationError> {
    if entry.kind != "adapter" {
        return Ok(());
    }
    if matches!(
        entry.route_status,
        RouteStatusV1::V2Default | RouteStatusV1::Retired
    ) {
        return Err(failure(
            format!("{field}.route_status"),
            "adapter is expired after V2 default or retirement",
        ));
    }
    if is_placeholder(&entry.delete_by_pr) {
        return Err(failure(
            format!("{field}.delete_by_pr"),
            "adapter must carry an active deletion PR",
        ));
    }
    Ok(())
}

fn validate_platform_lane(
    entry: &CompatibilityEntryV1,
    field: &str,
) -> Result<(), InventoryValidationError> {
    if entry.kind != "platform_operation" {
        return Ok(());
    }
    let Some(disposition) = entry.platform_disposition else {
        return Err(failure(
            format!("{field}.platform_disposition"),
            "platform operation must declare a disposition",
        ));
    };
    if disposition == PlatformDispositionV1::Untested {
        return Err(failure(
            format!("{field}.platform_disposition"),
            "untested platform lanes are forbidden",
        ));
    }
    if matches!(
        disposition,
        PlatformDispositionV1::Supported | PlatformDispositionV1::Alternative
    ) && !(has_evidence(&entry.tests, "test:")
        && has_nonzero_evidence(&entry.tests, "lane:", "lane:none")
        && has_nonzero_evidence(&entry.tests, "denominator:", "denominator:0"))
    {
        return Err(failure(
            format!("{field}.tests"),
            "supported platform lane requires test, lane, and denominator evidence",
        ));
    }
    Ok(())
}

fn validate_storage_mapping(
    entry: &CompatibilityEntryV1,
    field: &str,
) -> Result<(), InventoryValidationError> {
    if (entry.stable_id.starts_with("storage:") || entry.kind == "storage_artifact")
        && entry.tests.is_empty()
    {
        return Err(failure(
            format!("{field}.tests"),
            "storage mappings require test evidence",
        ));
    }
    Ok(())
}

fn has_evidence(values: &[String], prefix: &str) -> bool {
    values.iter().any(|value| value.starts_with(prefix))
}

fn has_nonzero_evidence(values: &[String], prefix: &str, zero: &str) -> bool {
    values
        .iter()
        .any(|value| value.starts_with(prefix) && value != zero)
}

fn is_unowned(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "none" | "tbd" | "unknown" | "unowned"
    )
}

fn is_placeholder(value: &str) -> bool {
    is_unowned(value)
        || matches!(
            value.to_ascii_lowercase().as_str(),
            "expired" | "not-applicable" | "n/a"
        )
}

fn failure(field: String, message: &str) -> InventoryValidationError {
    InventoryValidationError {
        field,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compatibility_inventory::model::{
        COMPATIBILITY_INVENTORY_SCHEMA_V1, EntityDispositionV1, InventoryGatesV1,
        InventoryOwnersV1, InventorySummariesV1,
    };

    fn entry() -> CompatibilityEntryV1 {
        CompatibilityEntryV1 {
            stable_id: "storage:store:activity".to_owned(),
            kind: "store".to_owned(),
            canonical_name: "activity".to_owned(),
            source_refs: vec!["src/global_db.rs".to_owned()],
            platform: "all".to_owned(),
            route_status: RouteStatusV1::V1Only,
            entity_disposition: EntityDispositionV1::Retained,
            platform_disposition: None,
            owners: InventoryOwnersV1 {
                v1_owner: "root/storage".to_owned(),
                v2_owner: "tracedecay-store".to_owned(),
            },
            readers: Vec::new(),
            writers: Vec::new(),
            tests: vec!["test:storage_inventory".to_owned()],
            gates: InventoryGatesV1 {
                parity_gate: "PR3-PARITY".to_owned(),
                cutover_gate: "PR37-CUTOVER".to_owned(),
            },
            recovery: "restore archive".to_owned(),
            delete_by_pr: "PR 37".to_owned(),
        }
    }

    fn inventory(entry: CompatibilityEntryV1) -> CompatibilityInventoryV1 {
        let summaries = InventorySummariesV1::from_entries(std::slice::from_ref(&entry));
        CompatibilityInventoryV1 {
            schema: COMPATIBILITY_INVENTORY_SCHEMA_V1.to_owned(),
            entries: vec![entry],
            source_family_appendix: Vec::new(),
            summaries,
        }
    }

    #[test]
    fn complete_storage_mapping_passes() {
        validate_inventory(&inventory(entry())).unwrap();
    }

    #[test]
    fn duplicate_ids_and_invalid_summaries_fail_through_model_gate() {
        let mut duplicate = inventory(entry());
        duplicate.entries.push(duplicate.entries[0].clone());
        assert_eq!(validate_inventory(&duplicate).unwrap_err().field, "entries");

        let mut bad_summary = inventory(entry());
        bad_summary.summaries.entries_by_kind.clear();
        assert_eq!(
            validate_inventory(&bad_summary).unwrap_err().field,
            "summaries"
        );
    }

    #[test]
    fn unowned_and_expired_adapter_entries_fail() {
        let mut unowned = entry();
        unowned.owners.v2_owner = "unowned".to_owned();
        assert!(
            validate_inventory(&inventory(unowned))
                .unwrap_err()
                .field
                .ends_with("v2_owner")
        );

        let mut expired = entry();
        expired.stable_id = "footprint:adapter:legacy".to_owned();
        expired.kind = "adapter".to_owned();
        expired.route_status = RouteStatusV1::Retired;
        assert!(
            validate_inventory(&inventory(expired))
                .unwrap_err()
                .message
                .contains("expired")
        );
    }

    #[test]
    fn untested_platform_and_storage_entries_fail() {
        let mut platform = entry();
        platform.stable_id = "platform.watch.linux".to_owned();
        platform.kind = "platform_operation".to_owned();
        platform.platform_disposition = Some(PlatformDispositionV1::Untested);
        assert!(
            validate_inventory(&inventory(platform))
                .unwrap_err()
                .message
                .contains("untested")
        );

        let mut storage = entry();
        storage.tests.clear();
        let error = validate_inventory(&inventory(storage)).unwrap_err();
        assert!(error.field.ends_with(".tests"));
        assert!(error.message.contains("test"));
    }
}

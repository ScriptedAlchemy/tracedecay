//! Checked `(operation, platform)` compatibility rows.
//!
//! The rows are curated runtime descriptors. They intentionally do not parse
//! Rust source or plans. Source references make every target gate and test
//! substitution reviewable, while CI-lane and denominator markers make a
//! support claim mechanically distinguishable from code that merely compiles.

use super::model::{
    CompatibilityEntryV1, EntityDispositionV1, InventoryGatesV1, InventoryOwnersV1,
    PlatformDispositionV1, RouteStatusV1,
};

mod descriptors;

use descriptors::{
    CFG_EXCLUSIONS, LaneReceipt, OPERATIONS, OperationSpec, PLATFORM_COUNT, PLATFORMS, PlatformSpec,
};

/// Returns stable-ID-sorted platform rows.
pub fn platform_entries() -> Result<Vec<CompatibilityEntryV1>, String> {
    validate_specs(OPERATIONS)?;
    let mut entries = Vec::with_capacity(OPERATIONS.len() * PLATFORM_COUNT);
    for operation in OPERATIONS {
        for (index, platform) in PLATFORMS.iter().enumerate() {
            let tests = evidence_tests(operation.tests[index], platform.receipt);
            let mut source_refs = vec![operation.production_owner.to_owned()];
            if !operation.substitute_refs[index].is_empty() {
                source_refs.push(operation.substitute_refs[index].to_owned());
            }
            if cfg_exclusion_applies(operation.broad_cfg_exclusion, platform.name) {
                source_refs.push(operation.broad_cfg_exclusion.to_owned());
            }
            source_refs.sort();
            source_refs.dedup();
            entries.push(CompatibilityEntryV1 {
                stable_id: format!("platform.{}.{}", operation.id, platform.name),
                kind: "platform_operation".to_owned(),
                canonical_name: operation.id.to_owned(),
                source_refs,
                platform: platform.name.to_owned(),
                route_status: RouteStatusV1::V1Only,
                entity_disposition: EntityDispositionV1::Retained,
                platform_disposition: Some(operation.dispositions[index]),
                owners: InventoryOwnersV1 {
                    v1_owner: operation.production_owner.to_owned(),
                    v2_owner: "root-composition".to_owned(),
                },
                readers: Vec::new(),
                writers: Vec::new(),
                tests,
                gates: InventoryGatesV1 {
                    parity_gate: operation.parity_gate.to_owned(),
                    cutover_gate: operation.cutover_gate[index].to_owned(),
                },
                recovery: operation.recovery[index].to_owned(),
                delete_by_pr: "PR 37A".to_owned(),
            });
        }
    }
    entries.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
    validate_platform_entries(&entries)?;
    Ok(entries)
}

/// Rejects claims not backed by an executed lane and exact denominator.
pub fn validate_platform_entries(entries: &[CompatibilityEntryV1]) -> Result<(), String> {
    for pair in entries.windows(2) {
        if pair[0].stable_id >= pair[1].stable_id {
            return Err("platform rows must be strictly stable-ID sorted".to_owned());
        }
    }
    for (index, entry) in entries.iter().enumerate() {
        let platform = PLATFORMS
            .iter()
            .find(|platform| platform.name == entry.platform)
            .ok_or_else(|| {
                format!(
                    "platform row {index} names unknown platform {}",
                    entry.platform
                )
            })?;
        let operation = OPERATIONS
            .iter()
            .find(|operation| operation.id == entry.canonical_name)
            .ok_or_else(|| {
                format!(
                    "platform row {index} names unknown operation {}",
                    entry.canonical_name
                )
            })?;
        let platform_index = PLATFORMS
            .iter()
            .position(|candidate| candidate.name == platform.name)
            .expect("known platform has an index");
        let Some(disposition) = entry.platform_disposition else {
            return Err(format!("platform row {index} lacks a disposition"));
        };
        if disposition != operation.dispositions[platform_index] {
            return Err(format!(
                "platform row {index} disposition disagrees with its checked descriptor"
            ));
        }
        if disposition == PlatformDispositionV1::Untested {
            return Err(format!(
                "platform row {index} has forbidden untested disposition"
            ));
        }
        if matches!(
            disposition,
            PlatformDispositionV1::Supported | PlatformDispositionV1::Alternative
        ) {
            let expected_tests = operation.tests[platform_index];
            if expected_tests.is_empty()
                || !expected_tests
                    .iter()
                    .all(|test| entry.tests.iter().any(|value| value == test))
            {
                return Err(format!(
                    "supported platform row {index} lacks its checked test denominator"
                ));
            }
        }
        validate_receipt_markers(entry, platform.receipt, index)?;
        for substitution in entry
            .source_refs
            .iter()
            .filter(|value| value.starts_with("substitute:"))
        {
            if substitution.ends_with("production_reachable=true") {
                return Err(format!(
                    "platform row {index} has a production-reachable substitution"
                ));
            }
            if !platform
                .receipt
                .substitutions
                .contains(&substitution.as_str())
            {
                return Err(format!(
                    "platform row {index} has an unknown lane substitution"
                ));
            }
        }
        for exclusion in entry
            .source_refs
            .iter()
            .filter(|value| value.starts_with("broad-cfg-exclusion:"))
        {
            if !cfg_exclusion_applies(exclusion, platform.name)
                || !entry.gates.parity_gate.contains("NO-BROAD-TEST-EXCLUSION")
            {
                return Err(format!(
                    "platform row {index} hides an unknown cfg exclusion"
                ));
            }
        }
    }
    Ok(())
}

fn evidence_tests(tests: &[&str], receipt: LaneReceipt) -> Vec<String> {
    let mut evidence = tests
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    evidence.push(format!("denominator:{}", tests.len()));
    evidence.push(format!("cfg:{}", receipt.cfg));
    evidence.push(format!("lane:{}", receipt.id));
    evidence.push(format!(
        "receipt-denominator:{}",
        receipt.executed_tests.len()
    ));
    evidence.push(format!("receipt-ignored:{}", receipt.ignored_tests.len()));
    evidence.push(format!("target:{}", receipt.target));
    evidence.sort();
    evidence.dedup();
    evidence
}

fn validate_specs(operations: &[OperationSpec]) -> Result<(), String> {
    for platform in PLATFORMS {
        validate_lane_receipt(platform)?;
    }
    for pair in operations.windows(2) {
        if pair[0].id >= pair[1].id {
            return Err("operation descriptors must be strictly sorted".to_owned());
        }
    }
    for operation in operations {
        for index in 0..PLATFORM_COUNT {
            let disposition = operation.dispositions[index];
            if disposition == PlatformDispositionV1::Untested {
                return Err(format!(
                    "{} {} is untested",
                    operation.id, PLATFORMS[index].name
                ));
            }
            if matches!(
                disposition,
                PlatformDispositionV1::Supported | PlatformDispositionV1::Alternative
            ) && operation.tests[index].is_empty()
            {
                return Err(format!(
                    "{} {} lacks an executed denominator",
                    operation.id, PLATFORMS[index].name
                ));
            }
            if !operation.tests[index]
                .iter()
                .all(|test| PLATFORMS[index].receipt.executed_tests.contains(test))
            {
                return Err(format!(
                    "{} {} cites a test absent from the checked lane receipt",
                    operation.id, PLATFORMS[index].name
                ));
            }
            let substitution = operation.substitute_refs[index];
            if substitution.ends_with("production_reachable=true") {
                return Err(format!(
                    "{} {} has a production-reachable substitution",
                    operation.id, PLATFORMS[index].name
                ));
            }
            if !substitution.is_empty()
                && !PLATFORMS[index]
                    .receipt
                    .substitutions
                    .contains(&substitution)
            {
                return Err(format!(
                    "{} {} has an unknown test substitution",
                    operation.id, PLATFORMS[index].name
                ));
            }
        }
        if !operation.broad_cfg_exclusion.is_empty() {
            let Some(exclusion) = CFG_EXCLUSIONS
                .iter()
                .find(|value| value.reference == operation.broad_cfg_exclusion)
            else {
                return Err(format!("{} has an unknown cfg exclusion", operation.id));
            };
            if !operation.parity_gate.contains("NO-BROAD-TEST-EXCLUSION") {
                return Err(format!("{} hides a broad cfg exclusion", operation.id));
            }
            for platform in exclusion.ignored_platforms {
                let index = PLATFORMS
                    .iter()
                    .position(|candidate| candidate.name == *platform)
                    .ok_or_else(|| format!("{} names unknown ignored platform", operation.id))?;
                if operation.dispositions[index] != PlatformDispositionV1::Unavailable {
                    return Err(format!(
                        "{} claims support on cfg-excluded platform {}",
                        operation.id, platform
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_lane_receipt(platform: PlatformSpec) -> Result<(), String> {
    let receipt = platform.receipt;
    if receipt.id.is_empty() || receipt.target.is_empty() || receipt.cfg.is_empty() {
        return Err(format!("{} lane receipt is incomplete", platform.name));
    }
    if platform.name == "other" {
        if !receipt.executed_tests.is_empty() || receipt.id != "none" {
            return Err("unsupported target must not carry an executed lane".to_owned());
        }
    } else if receipt.id == "none" || receipt.executed_tests.is_empty() {
        return Err(format!("{} lacks an executed lane receipt", platform.name));
    }
    for values in [
        receipt.executed_tests,
        receipt.ignored_tests,
        receipt.substitutions,
    ] {
        let mut unique = std::collections::BTreeSet::new();
        if !values
            .iter()
            .all(|value| !value.is_empty() && unique.insert(*value))
        {
            return Err(format!(
                "{} lane receipt has duplicate or empty evidence",
                platform.name
            ));
        }
    }
    if receipt
        .ignored_tests
        .iter()
        .any(|test| receipt.executed_tests.contains(test))
    {
        return Err(format!("{} executes a test marked ignored", platform.name));
    }
    if receipt
        .substitutions
        .iter()
        .any(|value| !value.ends_with("production_reachable=false"))
    {
        return Err(format!(
            "{} lane receipt has a production-reachable substitution",
            platform.name
        ));
    }
    Ok(())
}

fn validate_receipt_markers(
    entry: &CompatibilityEntryV1,
    receipt: LaneReceipt,
    index: usize,
) -> Result<(), String> {
    for marker in [
        format!("cfg:{}", receipt.cfg),
        format!("lane:{}", receipt.id),
        format!("receipt-denominator:{}", receipt.executed_tests.len()),
        format!("receipt-ignored:{}", receipt.ignored_tests.len()),
        format!("target:{}", receipt.target),
    ] {
        if !entry.tests.contains(&marker) {
            return Err(format!(
                "platform row {index} lacks checked lane receipt marker {marker}"
            ));
        }
    }
    Ok(())
}

fn cfg_exclusion_applies(reference: &str, platform: &str) -> bool {
    !reference.is_empty()
        && CFG_EXCLUSIONS.iter().any(|exclusion| {
            exclusion.reference == reference && exclusion.ignored_platforms.contains(&platform)
        })
}

#[cfg(test)]
mod tests {
    use super::descriptors::NONE;
    use super::*;

    #[test]
    fn checked_platform_rows_are_complete_and_deterministic() {
        let entries = platform_entries().unwrap();
        assert_eq!(entries.len(), OPERATIONS.len() * PLATFORM_COUNT);
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].stable_id < pair[1].stable_id)
        );
        for entry in entries {
            let platform = PLATFORMS
                .iter()
                .find(|platform| platform.name == entry.platform)
                .unwrap();
            assert!(entry.tests.contains(&format!(
                "receipt-denominator:{}",
                platform.receipt.executed_tests.len()
            )));
            assert!(entry.tests.contains(&format!(
                "receipt-ignored:{}",
                platform.receipt.ignored_tests.len()
            )));
        }
    }

    #[test]
    fn rejects_untested_and_supported_without_an_executed_denominator() {
        let mut operations = OPERATIONS.to_vec();
        operations[0].dispositions[0] = PlatformDispositionV1::Untested;
        assert!(
            validate_specs(&operations)
                .unwrap_err()
                .contains("untested")
        );
        operations[0].dispositions[0] = PlatformDispositionV1::Supported;
        operations[0].tests[0] = NONE;
        assert!(
            validate_specs(&operations)
                .unwrap_err()
                .contains("denominator")
        );
    }

    #[test]
    fn rejects_production_reachable_test_substitution() {
        let mut operations = OPERATIONS.to_vec();
        operations[0].substitute_refs[0] = "substitute:test:production_reachable=true";
        assert!(
            validate_specs(&operations)
                .unwrap_err()
                .contains("production-reachable")
        );
    }

    #[test]
    fn records_broad_cfg_gaps_and_known_test_substitutions() {
        let entries = platform_entries().unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry
                    .source_refs
                    .iter()
                    .any(|value| value.starts_with("broad-cfg-exclusion:")))
                .count(),
            4
        );
        assert!(
            entries
                .iter()
                .filter(|entry| entry
                    .source_refs
                    .iter()
                    .any(|value| value.starts_with("broad-cfg-exclusion:")))
                .all(|entry| matches!(entry.platform.as_str(), "windows" | "other"))
        );
        assert!(entries.iter().any(|entry| {
            entry.source_refs.iter().any(|value| {
                value.starts_with("substitute:") && value.contains("open_store_holders.rs#scan")
            })
        }));
        assert!(entries.iter().any(|entry| {
            entry.source_refs.iter().any(|value| {
                value.starts_with("substitute:")
                    && value.contains("sqlite/inspect.rs#acquire_offline_guards")
            })
        }));
    }

    #[test]
    fn rejects_unknown_platform_operation_and_lane_receipt_evidence() {
        let mut entries = platform_entries().unwrap();

        entries[0].platform = "solaris".to_owned();
        assert!(
            validate_platform_entries(&entries)
                .unwrap_err()
                .contains("unknown platform")
        );

        let mut entries = platform_entries().unwrap();
        entries[0].canonical_name = "unknown-operation".to_owned();
        assert!(
            validate_platform_entries(&entries)
                .unwrap_err()
                .contains("unknown operation")
        );

        let mut entries = platform_entries().unwrap();
        entries[0]
            .tests
            .retain(|value| !value.starts_with("receipt-denominator:"));
        assert!(
            validate_platform_entries(&entries)
                .unwrap_err()
                .contains("receipt marker")
        );
    }

    #[test]
    fn checked_receipts_bound_targets_cfg_ignored_tests_and_substitutions() {
        for platform in PLATFORMS {
            validate_lane_receipt(platform).unwrap();
            assert!(!platform.receipt.target.is_empty());
            assert!(!platform.receipt.cfg.is_empty());
        }

        let mut platform = PLATFORMS[0];
        platform.receipt.substitutions = &["substitute:test:production_reachable=true"];
        assert!(
            validate_lane_receipt(platform)
                .unwrap_err()
                .contains("production-reachable")
        );

        platform = PLATFORMS[0];
        platform.receipt.ignored_tests = platform.receipt.executed_tests;
        assert!(
            validate_lane_receipt(platform)
                .unwrap_err()
                .contains("marked ignored")
        );
    }
}

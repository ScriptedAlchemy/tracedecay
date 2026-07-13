use tracedecay::compatibility_inventory::footprint::CheckedFootprintDescriptors;
use tracedecay::compatibility_inventory::model::{
    COMPATIBILITY_INVENTORY_SCHEMA_V1, CompatibilityEntryV1, CompatibilityInventoryV1,
    EntityDispositionV1, InventoryGatesV1, InventoryOwnersV1, InventorySummariesV1, RouteStatusV1,
};
use tracedecay::compatibility_inventory::render::{
    canonical_semantic_json_bytes, render_compact_markdown, semantic_snapshot_digest,
};
use tracedecay::compatibility_inventory::validate::validate_inventory;
use tracedecay::compatibility_inventory::{GenerateInventoryOptions, generate_inventory};

const ARCHITECTURE: &str = r#"
package_ceiling = 12

[budgets]
definite_duplicate_body_lines = 0
default_binary_ratio_max = 1.25
idle_rss_ratio_max = 1.25
hot_build_ratio_max = 1.25
clean_build_ratio_max = 1.5
parity_replacement = "smaller"
generated_accounting = "separate"
"#;

const CARGO_METADATA: &str = r#"{
  "packages": [{
    "id": "path+file:///repo#tracedecay@1.0.0",
    "name": "tracedecay",
    "manifest_path": "/repo/Cargo.toml",
    "dependencies": [],
    "targets": [{
      "name": "tracedecay",
      "kind": ["lib"],
      "src_path": "/repo/src/lib.rs"
    }],
    "features": {}
  }],
  "workspace_members": ["path+file:///repo#tracedecay@1.0.0"],
  "workspace_root": "/repo"
}"#;

#[test]
fn generated_inventory_is_byte_deterministic() {
    let first = generated_inventory();
    let second = generated_inventory();

    validate_inventory(&first).unwrap();
    assert_eq!(
        canonical_semantic_json_bytes(&first).unwrap(),
        canonical_semantic_json_bytes(&second).unwrap()
    );
    assert_eq!(
        semantic_snapshot_digest(&first).unwrap(),
        semantic_snapshot_digest(&second).unwrap()
    );
}

#[test]
fn canonical_json_and_compact_report_have_entry_parity() {
    let inventory = generated_inventory();
    let bytes = canonical_semantic_json_bytes(&inventory).unwrap();
    let decoded: CompatibilityInventoryV1 = serde_json::from_slice(&bytes).unwrap();
    let report = render_compact_markdown(&inventory).unwrap();

    assert_eq!(decoded, inventory);
    assert!(report.contains(&format!("Semantic entries: {}", inventory.entries.len())));
    for entry in &inventory.entries {
        assert_eq!(
            report
                .lines()
                .filter(|line| line.starts_with(&format!("| {} |", entry.stable_id)))
                .count(),
            1,
            "report row parity failed for {}",
            entry.stable_id
        );
        assert!(report.contains(&entry.gates.parity_gate));
        assert!(report.contains(&entry.gates.cutover_gate));
    }
    for appendix in &inventory.source_family_appendix {
        assert_eq!(
            report
                .lines()
                .filter(|line| line.starts_with(&format!("| {} |", appendix.stable_id)))
                .count(),
            1,
            "appendix row parity failed for {}",
            appendix.stable_id
        );
    }
}

#[test]
fn duplicate_stable_ids_fail_closed() {
    let mut inventory = one_entry_inventory();
    inventory.entries.push(inventory.entries[0].clone());
    inventory
        .summaries
        .entries_by_kind
        .insert("store".into(), 2);
    inventory
        .summaries
        .entries_by_route_status
        .insert(RouteStatusV1::V1Only, 2);
    inventory
        .summaries
        .entries_by_entity_disposition
        .insert(EntityDispositionV1::Retained, 2);

    let error = validate_inventory(&inventory).unwrap_err();
    assert_eq!(error.field, "entries");
    assert!(error.message.contains("duplicate-free"));
}

#[test]
fn unowned_entries_fail_closed() {
    for owner in [OwnerField::V1, OwnerField::V2] {
        let mut inventory = one_entry_inventory();
        match owner {
            OwnerField::V1 => inventory.entries[0].owners.v1_owner = "unowned".to_owned(),
            OwnerField::V2 => inventory.entries[0].owners.v2_owner = "unowned".to_owned(),
        }

        let error = validate_inventory(&inventory).unwrap_err();
        assert!(error.field.ends_with("_owner"));
        assert!(error.message.contains("concrete owner"));
    }
}

#[test]
fn expired_adapter_fails_closed() {
    let mut inventory = one_entry_inventory();
    let entry = &mut inventory.entries[0];
    entry.stable_id = "footprint:adapter:legacy".to_owned();
    entry.kind = "adapter".to_owned();
    entry.delete_by_pr = "PR 37".to_owned();
    inventory.summaries = InventorySummariesV1::from_entries(std::slice::from_ref(entry));

    let error = inventory.validate_adapter_deadlines(37).unwrap_err();
    assert!(error.field.ends_with("delete_by_pr"));
    assert!(error.message.contains("expired"));
}

fn generated_inventory() -> CompatibilityInventoryV1 {
    let options = GenerateInventoryOptions {
        architecture_toml: ARCHITECTURE,
        cargo_metadata_json: CARGO_METADATA,
        footprint_descriptors: CheckedFootprintDescriptors::default(),
    };
    generate_inventory(options).unwrap()
}

enum OwnerField {
    V1,
    V2,
}

fn one_entry_inventory() -> CompatibilityInventoryV1 {
    let entry = CompatibilityEntryV1 {
        stable_id: "store:activity".to_owned(),
        kind: "store".to_owned(),
        canonical_name: "activity".to_owned(),
        source_refs: vec!["src/global_db.rs".to_owned()],
        platform: "all".to_owned(),
        route_status: RouteStatusV1::V1Only,
        entity_disposition: EntityDispositionV1::Retained,
        platform_disposition: None,
        owners: InventoryOwnersV1 {
            v1_owner: "root".to_owned(),
            v2_owner: "tracedecay-store".to_owned(),
        },
        readers: vec!["reader".to_owned()],
        writers: vec!["writer".to_owned()],
        tests: vec!["test:inventory".to_owned()],
        gates: InventoryGatesV1 {
            parity_gate: "PR3-PARITY".to_owned(),
            cutover_gate: "PR37-CUTOVER".to_owned(),
        },
        recovery: "restore archive".to_owned(),
        delete_by_pr: "PR 37".to_owned(),
    };
    let summaries = InventorySummariesV1::from_entries(std::slice::from_ref(&entry));
    CompatibilityInventoryV1 {
        schema: COMPATIBILITY_INVENTORY_SCHEMA_V1.to_owned(),
        entries: vec![entry],
        source_family_appendix: Vec::new(),
        summaries,
    }
}

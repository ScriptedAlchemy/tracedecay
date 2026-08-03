use tracedecay::migrate::inventory::StoreStatus as RootStoreStatus;
use tracedecay_migrate::inventory::StoreStatus as CrateStoreStatus;

#[test]
fn root_inventory_facade_preserves_the_migrate_crate_contract() {
    let status: CrateStoreStatus = RootStoreStatus::NeedsManualReview;

    assert_eq!(
        serde_json::to_value(status).expect("inventory status serializes"),
        serde_json::json!("needs_manual_review")
    );
}

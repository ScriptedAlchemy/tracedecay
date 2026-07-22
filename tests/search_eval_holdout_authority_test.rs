#![allow(clippy::option_env_unwrap)]

use tempfile::TempDir;
use tracedecay::search_eval::holdout::{
    AgentDelegationPayloadV1, HoldoutAuthorityStoreV1,
};
use tracedecay_domain::{DecisionOwnerId, FixtureContentDigest};

fn store() -> (TempDir, HoldoutAuthorityStoreV1) {
    let profile = TempDir::new().unwrap();
    let opened = HoldoutAuthorityStoreV1::open_at(profile.path()).unwrap();
    (profile, opened)
}

#[test]
fn private_store_rejects_world_readable_roots_and_keeps_labels_content_addressed() {
    let (profile, store) = store();
    let bytes = br#"{"schema_revision":1,"label_authority":"deterministic","judgments":[]}"#;
    let record = store.import_sealed_labels(bytes, 40).unwrap();
    assert!(record.locator.starts_with("authorized-store://"));
    assert!(record.locator.contains("sealed-labels/"));

    // No signing/trust-root machinery is created for local owner acceptance.
    assert!(!profile.path().join("search-quality-holdout-v1/keys").exists());
    assert!(
        !profile
            .path()
            .join("search-quality-holdout-v1/decision-owner-keys-v1.jsonl")
            .exists()
    );
    assert!(
        !profile
            .path()
            .join("search-quality-holdout-v1/trust-events-v1.jsonl")
            .exists()
    );
}

#[test]
fn owner_delegation_is_content_addressed_without_signatures() {
    let (_profile, store) = store();
    let packet = store
        .import_blinded_packet(b"blinded-packet-bytes", 50)
        .unwrap();
    let record = store
        .import_owner_delegation(AgentDelegationPayloadV1 {
            schema_revision: 1,
            delegated_by: DecisionOwnerId::new("owner-search-quality-lead").unwrap(),
            blinded_packet_digest: packet.content_digest.clone(),
            recorded_at_unix: 51,
        })
        .unwrap();
    assert!(record.locator.contains("owner-delegation/"));
    assert_ne!(record.content_digest, FixtureContentDigest::new(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
    ).unwrap());
}

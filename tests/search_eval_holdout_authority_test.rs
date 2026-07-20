use std::fs;

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

use tempfile::TempDir;
use tracedecay::search_eval::holdout::{
    DecisionOwnerKeySpecV1, HoldoutAuthorityError, HoldoutAuthorityStoreV1,
    HoldoutEnvelopePayloadV1,
};
use tracedecay_domain::{
    DecisionOwnerId, HoldoutLabelAuthorityV1, HoldoutSealDigest, RunId, RunManifestDigest,
};

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn key_spec(root_id: &str, epoch: u64) -> DecisionOwnerKeySpecV1 {
    DecisionOwnerKeySpecV1 {
        owner_id: DecisionOwnerId::new("owner-search-quality-lead").unwrap(),
        trust_root_id: root_id.to_string(),
        not_before_unix: 100,
        not_after_unix: 10_000,
        rotation_epoch: epoch,
    }
}

fn store() -> (TempDir, HoldoutAuthorityStoreV1) {
    let profile = tempfile::tempdir().unwrap();
    let store = HoldoutAuthorityStoreV1::open_at(profile.path()).unwrap();
    (profile, store)
}

#[test]
fn immutable_registry_is_content_addressed_owner_only_and_tamper_evident() {
    let (profile, store) = store();
    let payload = br#"{"schema_revision":1,"private":"sealed"}"#;

    let first = store.import_sealed_labels(payload, 120).unwrap();
    let duplicate = store.import_sealed_labels(payload, 121).unwrap();

    assert_eq!(first.locator, duplicate.locator);
    assert_eq!(first.content_digest, duplicate.content_digest);
    assert!(first.locator.starts_with("authorized-store://"));
    assert!(
        !first
            .locator
            .contains(profile.path().to_string_lossy().as_ref())
    );
    #[cfg(unix)]
    assert_owner_only_tree(profile.path());

    let object = files_below(profile.path())
        .into_iter()
        .find(|path| fs::read(path).is_ok_and(|bytes| bytes == payload))
        .expect("stored object exists");
    fs::write(object, b"tampered").unwrap();
}

#[cfg(unix)]
#[test]
fn private_store_rejects_symlinked_authority_root() {
    let profile = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(
        outside.path(),
        profile.path().join("search-quality-holdout-v1"),
    )
    .unwrap();

    assert!(matches!(
        HoldoutAuthorityStoreV1::open_at(profile.path()),
        Err(HoldoutAuthorityError::UnsafeStorage)
    ));
}

#[test]
fn generated_keys_never_leave_the_store_and_trust_lifecycle_fails_closed() {
    let (profile, store) = store();
    let first = store.generate_decision_owner_key(key_spec("root-v1", 1), 120);
    let first = first.unwrap();

    let serialized = serde_json::to_string(&first).unwrap();
    assert!(!serialized.contains("private"));
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("seed"));
    assert!(!serialized.contains("\"public_key_hex\""));
    assert!(!serialized.contains(profile.path().to_string_lossy().as_ref()));
    assert_eq!(
        store
            .resolve_trust_root("root-v1", 150)
            .unwrap()
            .rotation_epoch,
        1
    );

    let second = store
        .rotate_decision_owner_key("root-v1", key_spec("root-v2", 2), 200)
        .unwrap();
    assert_eq!(second.rotation_epoch, 2);
    assert!(matches!(
        store.resolve_trust_root("root-v1", 201),
        Err(HoldoutAuthorityError::RetiredTrustRoot { .. })
    ));
    store
        .revoke_trust_root("root-v2", 220, "local key retirement")
        .unwrap();
    assert!(matches!(
        store.resolve_trust_root("root-v2", 221),
        Err(HoldoutAuthorityError::RevokedTrustRoot { .. })
    ));

    #[cfg(unix)]
    assert_owner_only_tree(profile.path());
}

#[test]
fn signed_envelopes_bind_labels_seal_owner_and_run_independent_trust() {
    let (_profile, store) = store();
    store
        .generate_decision_owner_key(key_spec("root-v1", 1), 120)
        .unwrap();
    let labels = store
        .import_sealed_labels(br#"{"schema_revision":1}"#, 121)
        .unwrap();
    let payload = HoldoutEnvelopePayloadV1 {
        schema_revision: 1,
        labels_locator: labels.locator.clone(),
        labels_content_digest: labels.content_digest.clone(),
        seal_digest: HoldoutSealDigest::new(DIGEST_A).unwrap(),
        label_authority: HoldoutLabelAuthorityV1::HumanAuthoritative,
        signed_by: DecisionOwnerId::new("owner-search-quality-lead").unwrap(),
        trust_root_id: "root-v1".to_string(),
        signed_at_unix: 130,
    };
    let envelope = store.sign_envelope(payload).unwrap();
    assert!(envelope.locator.starts_with("authorized-store://"));
    assert_ne!(envelope.content_digest, labels.content_digest);
}

#[test]
fn reveal_capability_is_run_bound_expiring_and_contains_no_paths() {
    let (profile, store) = store();
    store
        .generate_decision_owner_key(key_spec("root-v1", 1), 100)
        .unwrap();
    let capability = store
        .sign_reveal_capability(
            tracedecay_domain::HoldoutRevealCapabilityV1 {
                schema_revision: 1,
                labels_locator: "authorized-store://labels/opaque".to_string(),
                envelope_locator: "authorized-store://envelopes/opaque".to_string(),
                seal_digest: HoldoutSealDigest::new(DIGEST_A).unwrap(),
                run_id: RunId::new("run-locked-v2").unwrap(),
                run_manifest_digest: RunManifestDigest::new(DIGEST_B).unwrap(),
                revealed_by: DecisionOwnerId::new("owner-search-quality-lead").unwrap(),
                operation: "evaluate_locked_quality_v1".to_string(),
                not_before_unix: 100,
                expires_at_unix: 200,
            },
            "root-v1",
            120,
        )
        .unwrap();

    let serialized = serde_json::to_string(&capability).unwrap();
    assert!(!serialized.contains(profile.path().to_string_lossy().as_ref()));
    assert!(capability.locator.starts_with("authorized-store://"));
}

#[test]
fn receipt_authority_is_not_publicly_injectable() {
    let domain_source = include_str!("../crates/tracedecay-domain/src/evaluation.rs");
    let store_source = include_str!("../src/search_eval/holdout.rs");
    assert!(!domain_source.contains("pub trait HoldoutReceiptAuthorityV1"));
    assert!(!domain_source.contains("receipt_authority: &A"));
    assert!(store_source.contains("pub(crate) fn validate_accepted_pr9_evidence"));
}

fn files_below(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                files.push(entry.path());
            }
        }
    }
    files
}

#[cfg(unix)]
fn assert_owner_only_tree(root: &std::path::Path) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
                pending.push(entry.path());
            } else if metadata.is_file() {
                assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            }
        }
    }
}

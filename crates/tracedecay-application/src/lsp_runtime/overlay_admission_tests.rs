use super::overlay_admission::admit_overlay;
use tracedecay_domain::ContentDigest;
use tracedecay_lsp::{AdmittedRoot, OverlaySnapshot, OverlayStore};

fn overlay(content_digest: ContentDigest) -> (OverlayStore, OverlaySnapshot) {
    let mut store = OverlayStore::default();
    let mut snapshot = store
        .open(
            &AdmittedRoot::new("file:///root"),
            "file:///root/src/lib.rs",
            "rust",
            7,
            "fn current() {}",
        )
        .expect("admitted overlay");
    snapshot.content_digest = content_digest;
    (store, snapshot)
}

#[test]
fn overlay_admission_rejects_a_stale_content_digest() {
    let stale = ContentDigest::of_bytes(b"fn stale() {}");
    let (_store, snapshot) = overlay(stale);
    let error = admit_overlay(snapshot, "file:///root/src/lib.rs")
        .expect_err("stale digest must not authorize overlay bytes");

    assert_eq!(error.class(), "document-overlay-content-digest-mismatch");
}

#[test]
fn overlay_admission_preserves_exact_version_digest_and_shared_text() {
    let digest = ContentDigest::of_bytes(b"fn current() {}");
    let (_store, snapshot) = overlay(digest.clone());
    let admitted = admit_overlay(snapshot, "file:///root/src/lib.rs").expect("exact overlay");

    assert_eq!(admitted.version, 7);
    assert_eq!(admitted.content_digest, digest);
    assert_eq!(&*admitted.text, "fn current() {}");
}

#[test]
fn overlay_admission_rejects_work_retained_after_session_close() {
    let (store, snapshot) = overlay(ContentDigest::of_bytes(b"fn current() {}"));
    drop(store);
    let error = admit_overlay(snapshot, "file:///root/src/lib.rs")
        .expect_err("queued work cannot keep the overlay admission alive");
    assert_eq!(error.class(), "document-overlay-closed");
}

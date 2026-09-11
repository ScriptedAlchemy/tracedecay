use std::sync::Arc;

use tracedecay_domain::ContentDigest;
use tracedecay_lsp::{
    OverlayExtractionState, OverlayParseState, OverlayParseUnavailable, OverlaySnapshot,
};

use super::admit_overlay;

fn overlay(content_digest: ContentDigest) -> OverlaySnapshot {
    OverlaySnapshot {
        uri: "file:///root/src/lib.rs".to_owned(),
        language_id: "rust".to_owned(),
        version: 7,
        content_digest,
        text: Arc::from("fn current() {}"),
        ephemeral: true,
        parse_state: OverlayParseState::Unavailable(OverlayParseUnavailable::StaleReport),
        extraction_state: OverlayExtractionState::Unavailable(OverlayParseUnavailable::StaleReport),
    }
}

#[test]
fn overlay_admission_rejects_a_stale_content_digest() {
    let stale = ContentDigest::of_bytes(b"fn stale() {}");
    let error = admit_overlay(overlay(stale), "file:///root/src/lib.rs")
        .expect_err("stale digest must not authorize overlay bytes");

    assert_eq!(error.class(), "document-overlay-content-digest-mismatch");
}

#[test]
fn overlay_admission_preserves_exact_version_digest_and_shared_text() {
    let digest = ContentDigest::of_bytes(b"fn current() {}");
    let admitted =
        admit_overlay(overlay(digest.clone()), "file:///root/src/lib.rs").expect("exact overlay");

    assert_eq!(admitted.version, 7);
    assert_eq!(admitted.content_digest, digest);
    assert_eq!(&*admitted.text, "fn current() {}");
}

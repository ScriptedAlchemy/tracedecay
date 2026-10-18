use tracedecay_domain::ContentDigest;
use tracedecay_lsp::{LspRuntimeFailure, OverlaySnapshot};

pub(super) fn admit_overlay(
    overlay: OverlaySnapshot,
    expected_document_uri: &str,
) -> Result<OverlaySnapshot, LspRuntimeFailure> {
    if !overlay.ephemeral || overlay.uri != expected_document_uri || overlay.language_id.is_empty()
    {
        return Err(LspRuntimeFailure::new("document-overlay-invalid"));
    }
    if ContentDigest::of_bytes(overlay.text.as_bytes()) != overlay.content_digest {
        return Err(LspRuntimeFailure::new(
            "document-overlay-content-digest-mismatch",
        ));
    }
    Ok(overlay)
}

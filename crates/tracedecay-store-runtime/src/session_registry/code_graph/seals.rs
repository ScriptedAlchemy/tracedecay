use tracedecay_graph_db::{GraphDbError, SealedGraphStateDigest};

/// The logical manifest locator remains byte-identical to the codec identity;
/// it no longer names a loose file or replay-pool copy.
pub(super) fn sealed_digest_from_generation_file(
    generation_file: &str,
) -> Result<SealedGraphStateDigest, GraphDbError> {
    let digest = generation_file
        .strip_prefix("generation-")
        .and_then(|value| value.strip_suffix(".json"))
        .ok_or_else(|| GraphDbError::invalid("sealed generation locator is invalid"))?;
    SealedGraphStateDigest::try_from(format!("sha256:{digest}"))
}

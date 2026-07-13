use super::model::{
    CompatibilityInventoryEnvelopeV1, CompatibilityInventoryV1, InventoryValidationError,
};
use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum BaselineLoadError {
    #[error("failed to read compatibility baseline: {0}")]
    Io(#[source] std::io::Error),
    #[error("failed to parse compatibility baseline: {0}")]
    Json(#[source] serde_json::Error),
    #[error("invalid compatibility baseline: {0}")]
    Validation(#[source] InventoryValidationError),
}

pub fn load_baseline_bytes(bytes: &[u8]) -> Result<CompatibilityInventoryV1, BaselineLoadError> {
    let baseline = serde_json::from_slice::<CompatibilityInventoryV1>(bytes)
        .map_err(BaselineLoadError::Json)?;
    baseline.validate().map_err(BaselineLoadError::Validation)?;
    Ok(baseline)
}

pub fn load_baseline_path(
    path: impl AsRef<Path>,
) -> Result<CompatibilityInventoryV1, BaselineLoadError> {
    let bytes = fs::read(path).map_err(BaselineLoadError::Io)?;
    load_baseline_bytes(&bytes)
}

pub fn load_envelope_bytes(
    bytes: &[u8],
) -> Result<CompatibilityInventoryEnvelopeV1, BaselineLoadError> {
    let envelope = serde_json::from_slice::<CompatibilityInventoryEnvelopeV1>(bytes)
        .map_err(BaselineLoadError::Json)?;
    envelope.validate().map_err(BaselineLoadError::Validation)?;
    Ok(envelope)
}

pub fn load_envelope_path(
    path: impl AsRef<Path>,
) -> Result<CompatibilityInventoryEnvelopeV1, BaselineLoadError> {
    let bytes = fs::read(path).map_err(BaselineLoadError::Io)?;
    load_envelope_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_loader_rejects_unknown_fields() {
        let error = load_baseline_bytes(
            br#"{"schema":"tracedecay.v2.compatibility-inventory/v1","entries":[],"source_family_appendix":[],"summaries":{"entries_by_kind":{},"entries_by_route_status":{},"entries_by_entity_disposition":{},"entries_by_platform_disposition":{}},"unexpected":true}"#,
        )
        .unwrap_err();
        assert!(matches!(error, BaselineLoadError::Json(_)));
    }

    #[test]
    fn path_loader_rejects_an_empty_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1-compatibility.json");
        fs::write(
            &path,
            br#"{"schema":"tracedecay.v2.compatibility-inventory/v1","entries":[],"source_family_appendix":[],"summaries":{"entries_by_kind":{},"entries_by_route_status":{},"entries_by_entity_disposition":{},"entries_by_platform_disposition":{}}}"#,
        )
        .unwrap();
        assert!(matches!(
            load_baseline_path(path),
            Err(BaselineLoadError::Validation(_))
        ));
    }

    #[test]
    fn envelope_loader_rejects_a_stale_semantic_digest() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "metadata": {
                "binary": "compatibility-inventory",
                "commit": "abc123",
                "generated_at": "2026-07-13T00:00:00Z",
                "watermark": "fixture",
                "source_set_digest": format!("sha256:{}", "a".repeat(64)),
                "semantic_snapshot_digest": format!("sha256:{}", "b".repeat(64)),
            },
            "inventory": {
                "schema": "tracedecay.v2.compatibility-inventory/v1",
                "entries": [{
                    "stable_id": "store:activity",
                    "kind": "store",
                    "canonical_name": "activity",
                    "source_refs": ["src/global_db.rs"],
                    "platform": "all",
                    "route_status": "v1_only",
                    "entity_disposition": "retained",
                    "platform_disposition": null,
                    "owners": {"v1_owner": "root", "v2_owner": "tracedecay-store"},
                    "readers": [],
                    "writers": [],
                    "tests": ["test:compatibility_inventory"],
                    "gates": {"parity_gate": "PR3-PARITY", "cutover_gate": "PR37-CUTOVER"},
                    "recovery": "restore archive",
                    "delete_by_pr": "PR 37"
                }],
                "source_family_appendix": [],
                "summaries": {
                    "entries_by_kind": {"store": 1},
                    "entries_by_route_status": {"v1_only": 1},
                    "entries_by_entity_disposition": {"retained": 1},
                    "entries_by_platform_disposition": {}
                }
            }
        }))
        .unwrap();

        let error = load_envelope_bytes(&bytes).unwrap_err();
        assert!(matches!(error, BaselineLoadError::Validation(_)));
    }
}

//! Sidecar persistence for the dashboard's last dry-run curation preview.
//!
//! The preview lives in memory (`DashboardState::curate_preview`) and is
//! mirrored to `.tracedecay/dashboard/curation_preview.json` so a server
//! restart does not lose it (the original `holographic_plus` backend also
//! persisted previews to a JSON file). If only the legacy `.tokensave/`
//! directory exists, the sidecar stays there for backward compatibility.
//! The sidecar is a best-effort cache:
//! load/save/clear failures are logged and never fail an API request, and
//! the API shape of `GET /curation/preview` is unchanged — staleness is
//! still recomputed against the live fact count on every read.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::CuratePreviewEntry;

pub(crate) fn sidecar_path(project_root: &Path) -> PathBuf {
    let preferred = project_root.join(".tracedecay");
    let legacy = project_root.join(".tokensave");
    // Mirror the repo-wide ".tracedecay primary, .tokensave fallback"
    // convention; later this should delegate to the central data-dir helper.
    let data_dir = if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    };
    data_dir.join("dashboard").join("curation_preview.json")
}

/// Loads the persisted preview, or `None` when absent/unreadable/malformed.
pub(crate) async fn load(project_root: &Path) -> Option<CuratePreviewEntry> {
    let path = sidecar_path(project_root);
    let bytes = tokio::fs::read(&path).await.ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let report = value.get("report")?.clone();
    if report.is_null() {
        return None;
    }
    Some(CuratePreviewEntry {
        report,
        saved_at: value.get("saved_at")?.as_str()?.to_string(),
        active_facts_at_save: value.get("active_facts_at_save")?.as_i64()?,
    })
}

pub(crate) async fn save(project_root: &Path, entry: &CuratePreviewEntry) {
    let path = sidecar_path(project_root);
    let payload = json!({
        "report": entry.report,
        "saved_at": entry.saved_at,
        "active_facts_at_save": entry.active_facts_at_save,
    });
    let result = async {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = serde_json::to_vec_pretty(&payload).unwrap_or_default();
        tokio::fs::write(&path, bytes).await
    }
    .await;
    if let Err(e) = result {
        eprintln!(
            "Warning: could not persist curation preview to {}: {e}",
            path.display()
        );
    }
}

pub(crate) async fn clear(project_root: &Path) {
    let path = sidecar_path(project_root);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "Warning: could not clear persisted curation preview {}: {e}",
            path.display()
        ),
    }
}

//! Text-artifact scan positions owned by the existing maintenance registry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tracedecay_code_index_retention::code_index_generations::CodeGenerationRetentionScanV1;

use super::StoreTelemetrySamplingRegistry;

pub(super) struct TextArtifactScanEntryV1 {
    pub(super) data_root: PathBuf,
    scan: Arc<Mutex<CodeGenerationRetentionScanV1>>,
}

pub(super) type TextArtifactScansV1 = Arc<Mutex<HashMap<PathBuf, TextArtifactScanEntryV1>>>;

impl StoreTelemetrySamplingRegistry {
    /// Mounted and registered scopes share this owner across short ticks.
    /// The map lock protects only lookup; verification holds a per-scope lock
    /// on the blocking worker, never the daemon-wide telemetry lock.
    pub(crate) fn text_artifact_scan(
        &self,
        data_root: &Path,
        store_root: &Path,
    ) -> Arc<Mutex<CodeGenerationRetentionScanV1>> {
        let mut scans = self
            .text_artifact_scans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = scans.entry(store_root.to_path_buf()).or_insert_with(|| {
            TextArtifactScanEntryV1 {
                data_root: data_root.to_path_buf(),
                scan: Arc::new(Mutex::new(CodeGenerationRetentionScanV1::default())),
            }
        });
        Arc::clone(&entry.scan)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::Arc;

    use super::StoreTelemetrySamplingRegistry;

    #[test]
    fn scope_scans_share_the_owner_and_retire_with_project_state() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let clone = registry.clone();
        let project = Path::new("retained-project");
        let other = Path::new("retired-project");
        let scope = Path::new("retained-scope");
        let retired_scope = Path::new("retired-scope");
        let first = registry.text_artifact_scan(project, scope);
        // An unmounted registered project is absent from mounted graph
        // maintenance but must retain scan progress across short ticks.
        assert!(Arc::ptr_eq(&first, &clone.text_artifact_scan(project, scope)));
        let second = registry.text_artifact_scan(project, Path::new("other-scope"));
        assert!(!Arc::ptr_eq(&first, &second));
        let retired = registry.text_artifact_scan(other, retired_scope);
        let retired_weak = Arc::downgrade(&retired);
        drop(retired);
        registry.retain_project_maintenance_state(&BTreeSet::new());
        registry.retain_text_artifact_scan_projects(&BTreeSet::from([project.to_path_buf()]));
        assert!(retired_weak.upgrade().is_none());
        assert!(Arc::ptr_eq(&first, &clone.text_artifact_scan(project, scope)));
        let first_weak = Arc::downgrade(&first);
        let second_weak = Arc::downgrade(&second);
        drop(first);
        drop(second);
        registry.release_retained_handles_for_shutdown();
        assert!(first_weak.upgrade().is_none());
        assert!(second_weak.upgrade().is_none());
    }
}

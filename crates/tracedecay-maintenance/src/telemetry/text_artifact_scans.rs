//! Text-artifact scan positions owned by the maintenance registry.

use std::collections::{BTreeSet, HashMap};
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
        let entry =
            scans
                .entry(store_root.to_path_buf())
                .or_insert_with(|| TextArtifactScanEntryV1 {
                    data_root: data_root.to_path_buf(),
                    scan: Arc::new(Mutex::new(CodeGenerationRetentionScanV1::default())),
                });
        Arc::clone(&entry.scan)
    }

    /// Release directory streams for scopes absent from a successfully read
    /// project scope inventory. A registered project can outlive many retired
    /// worktrees; pruning by project alone would retain their open ReadDir
    /// handles indefinitely.
    pub(crate) fn retain_text_artifact_scan_scopes(
        &self,
        data_root: &Path,
        live_store_roots: &BTreeSet<PathBuf>,
    ) {
        self.text_artifact_scans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|store_root, entry| {
                entry.data_root != data_root || live_store_roots.contains(store_root)
            });
    }

    /// A scope selected for quarantine must not be pinned by an unfinished
    /// staging-directory scan. Reset even if a blocking caller still holds an
    /// Arc to the scan, so its ReadDir handles are closed before the rename.
    /// The maintenance writer serializes this with scan preparation.
    pub(crate) fn release_text_artifact_scans_before_scope_collection(
        &self,
        collectable_store_roots: &BTreeSet<PathBuf>,
    ) {
        let mut scans = self
            .text_artifact_scans
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for root in collectable_store_roots {
            if let Some(entry) = scans.remove(root) {
                *entry.scan.lock().unwrap_or_else(PoisonError::into_inner) =
                    CodeGenerationRetentionScanV1::default();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::Arc;
    use tracedecay_code_index_retention::code_index_generations::code_text_artifact_staging_root;

    use super::StoreTelemetrySamplingRegistry;

    #[test]
    fn retired_scope_releases_scan_while_registered_project_stays_live() {
        let registry = StoreTelemetrySamplingRegistry::default();
        let project = Path::new("registered-project");
        let survivor = Path::new("registered-project/code-index-v1/survivor");
        let retired = Path::new("registered-project/code-index-v1/retired");
        let other = Path::new("other-project/code-index-v1/scope");

        let live = registry.text_artifact_scan(project, survivor);
        let removed = registry.text_artifact_scan(project, retired);
        let removed_weak = Arc::downgrade(&removed);
        drop(removed);

        let other_data_root = Path::new("other-project");
        let elsewhere = registry.text_artifact_scan(other_data_root, other);
        let elsewhere_weak = Arc::downgrade(&elsewhere);
        drop(elsewhere);

        // The registered project is still present. Only its *scope* inventory
        // changed, so the retired scan must not keep its ReadDir handles.
        registry
            .retain_text_artifact_scan_scopes(project, &BTreeSet::from([survivor.to_path_buf()]));
        assert!(removed_weak.upgrade().is_none());
        assert!(Arc::ptr_eq(
            &live,
            &registry.text_artifact_scan(project, survivor)
        ));
        assert!(elsewhere_weak.upgrade().is_some());

        // A failed/incomplete project inventory does not invoke pruning.
        // After a later successful empty inventory, all its old streams close.
        let live_weak = Arc::downgrade(&live);
        drop(live);
        registry.retain_text_artifact_scan_scopes(project, &BTreeSet::new());
        assert!(live_weak.upgrade().is_none());
        assert!(elsewhere_weak.upgrade().is_some());
    }

    #[test]
    fn scope_quarantine_releases_live_staging_directory_streams() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("project");
        let scope_root = data_root.join("code-index-v1").join("retired");
        let staging = code_text_artifact_staging_root(&scope_root);
        std::fs::create_dir_all(&staging).unwrap();
        // More than one bounded page keeps a real ReadDir open after prepare.
        for i in 0..96 {
            std::fs::write(
                staging.join(format!(".text-artifact-{i:064x}.staging")),
                b"unpublished staging",
            )
            .unwrap();
        }
        let registry = StoreTelemetrySamplingRegistry::default();
        let retained = registry.text_artifact_scan(&data_root, &scope_root);
        let planned = retained
            .lock()
            .unwrap()
            .prepare(&scope_root, &BTreeSet::new(), &|| false, None)
            .unwrap();
        assert!(planned.has_pending_text_artifact_inventory());

        let another_root = data_root.join("code-index-v1").join("live");
        let live = registry.text_artifact_scan(&data_root, &another_root);
        registry.release_text_artifact_scans_before_scope_collection(&BTreeSet::from([
            scope_root.clone()
        ]));
        assert!(Arc::ptr_eq(
            &live,
            &registry.text_artifact_scan(&data_root, &another_root)
        ));
        assert!(
            !Arc::ptr_eq(
                &retained,
                &registry.text_artifact_scan(&data_root, &scope_root)
            ),
            "a retired scope must not keep a reusable scan owner"
        );
        // On Windows a live ReadDir inside the source prevents this rename.
        // Keep the prior Arc alive: removal alone is insufficient unless its
        // open directory streams were also reset.
        let quarantined = data_root.join("quarantined-scope");
        std::fs::rename(&scope_root, &quarantined).unwrap();
        assert!(quarantined.join("code-text-artifact-staging-v1").is_dir());
    }

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
        assert!(Arc::ptr_eq(
            &first,
            &clone.text_artifact_scan(project, scope)
        ));
        let second = registry.text_artifact_scan(project, Path::new("other-scope"));
        assert!(!Arc::ptr_eq(&first, &second));
        let retired = registry.text_artifact_scan(other, retired_scope);
        let retired_weak = Arc::downgrade(&retired);
        drop(retired);
        registry.retain_project_maintenance_state(&BTreeSet::new());
        registry.retain_text_artifact_scan_projects(&BTreeSet::from([project.to_path_buf()]));
        assert!(retired_weak.upgrade().is_none());
        assert!(Arc::ptr_eq(
            &first,
            &clone.text_artifact_scan(project, scope)
        ));
        let first_weak = Arc::downgrade(&first);
        let second_weak = Arc::downgrade(&second);
        drop(first);
        drop(second);
        registry.release_retained_handles_for_shutdown();
        assert!(first_weak.upgrade().is_none());
        assert!(second_weak.upgrade().is_none());
    }
}

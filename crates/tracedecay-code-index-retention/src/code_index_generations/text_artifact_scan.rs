//! Process-retained directory progress for the maintenance owner.
//!
//! Listed entries are only candidates. They do not decide liveness or permit an
//! unlink. Each preparation re-reads current publication pointers and verifies
//! the selected bytes, and apply revalidates under the store and project locks.

use std::collections::BTreeSet;
use std::fs::ReadDir;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_domain::CodeGenerationId;

use super::text_artifacts::private_directory_exists;
use super::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionPlanV1,
    MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1, code_text_artifact_staging_root,
    code_text_artifacts_root, prepare_next_code_generation_retention_page_cancellable, storage,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TextArtifactPageV1 {
    pub(super) entries: Vec<(bool, PathBuf)>,
    pub(super) more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TextArtifactPageProgressV1 {
    pub(super) page: Arc<TextArtifactPageV1>,
    pub(super) consumed: usize,
}

/// A maintenance owner's unfinished text-artifact enumeration for one scope.
///
/// The scan keeps two directory streams and at most one bounded page between
/// ticks. A restart loses only the scan position, because each preparation
/// repeats the safety checks. A completed scan closes both streams, and the
/// next tick opens a new inventory. The scan keeps no process-global cache or
/// durable state.
#[derive(Debug, Default)]
pub struct CodeGenerationRetentionScanV1 {
    root: Option<PathBuf>,
    completed: Option<ReadDir>,
    staging: Option<ReadDir>,
    scan_staging: bool,
    pending: Vec<(bool, PathBuf)>,
    page: Option<Arc<TextArtifactPageV1>>,
}

impl CodeGenerationRetentionScanV1 {
    /// Prepare one page through the same planner used by one-off censuses.
    /// Metadata and full verification consume exactly the same bounded names.
    /// Cancellation, a planning refusal, or a failed apply retains the page
    /// for revalidation rather than advancing past unprocessed candidates.
    pub fn prepare(
        &mut self,
        store_root: &Path,
        protected_sources: &BTreeSet<CodeGenerationId>,
        is_cancelled: &dyn Fn() -> bool,
        graph_replay_pool_root: Option<&Path>,
    ) -> Result<CodeGenerationRetentionPlanV1, CodeGenerationRetentionErrorV1> {
        let page = self.next_page(store_root, is_cancelled)?;
        let mut plan = prepare_next_code_generation_retention_page_cancellable(
            store_root,
            protected_sources,
            is_cancelled,
            graph_replay_pool_root,
            Some(&page),
        )?;
        // An absent, never-published store returns before the census. Its
        // empty page still needs acknowledging so a later mount is rescanned.
        if plan.text_artifact_page_progress.is_none() {
            plan.text_artifact_page_progress = Some(TextArtifactPageProgressV1 {
                consumed: page.entries.len(),
                page,
            });
        }
        Ok(plan)
    }

    /// Acknowledge a successful apply, or a successfully verified no-op page.
    /// A stale/double acknowledgement cannot advance a newer page. When the
    /// deletion cap stopped classification early, keep its unvisited tail.
    pub fn complete(&mut self, plan: &CodeGenerationRetentionPlanV1) {
        let Some(progress) = plan.text_artifact_page_progress.as_ref() else {
            return;
        };
        let Some(page) = self.page.as_ref() else {
            return;
        };
        if !Arc::ptr_eq(page, &progress.page) {
            return;
        }
        if progress.consumed < page.entries.len() {
            self.page = Some(Arc::new(TextArtifactPageV1 {
                entries: page.entries[progress.consumed..].to_vec(),
                more: page.more,
            }));
        } else {
            self.page = None;
            if self.completed.is_none() && self.staging.is_none() {
                *self = Self::default();
            }
        }
    }

    fn next_page(
        &mut self,
        store_root: &Path,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<TextArtifactPageV1>, CodeGenerationRetentionErrorV1> {
        if is_cancelled() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        if self.root.as_deref() != Some(store_root) {
            *self = Self::default();
            self.completed = open_inventory(&code_text_artifacts_root(store_root))?;
            self.staging = open_inventory(&code_text_artifact_staging_root(store_root))?;
            self.root = Some(store_root.to_path_buf());
        }
        if let Some(page) = self.page.as_ref() {
            return Ok(Arc::clone(page));
        }
        while self.pending.len() < MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1 {
            if is_cancelled() {
                return Err(CodeGenerationRetentionErrorV1::Cancelled);
            }
            if self.completed.is_none() && self.staging.is_none() {
                break;
            }
            let shared = !self.scan_staging;
            self.scan_staging = !self.scan_staging;
            let listing = if shared {
                &mut self.completed
            } else {
                &mut self.staging
            };
            match listing.as_mut().and_then(Iterator::next) {
                Some(entry) => self.pending.push((shared, entry.map_err(storage)?.path())),
                None => *listing = None,
            }
        }
        let page = Arc::new(TextArtifactPageV1 {
            entries: std::mem::take(&mut self.pending),
            more: self.completed.is_some() || self.staging.is_some(),
        });
        self.page = Some(Arc::clone(&page));
        Ok(page)
    }
}

fn open_inventory(path: &Path) -> Result<Option<ReadDir>, CodeGenerationRetentionErrorV1> {
    if private_directory_exists(path)? {
        std::fs::read_dir(path).map(Some).map_err(storage)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use tracedecay_domain::UtcMicros;
    use tracedecay_domain::canonical_text::sha256_hex;

    use super::CodeGenerationRetentionScanV1;
    use crate::code_index_generations::{
        CodeGenerationRetentionErrorV1, CodeGenerationRetentionModeV1,
        MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1, MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1,
        code_text_artifact_staging_root, code_text_artifacts_root,
        execute_code_generation_retention, prepare_next_code_generation_retention_cancellable,
    };

    fn write_orphan(root: &Path, index: usize) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let bytes = format!("verified artifact {index}");
        let path = root.join(format!(
            "text-artifact-{}.bin",
            sha256_hex(bytes.as_bytes())
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn preserved_same_directory_prefix_does_not_hide_later_verified_artifacts() {
        let store = tempfile::tempdir().unwrap();
        let root = code_text_artifacts_root(store.path());
        let count = MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1 * 3;
        let mut original = BTreeMap::new();
        for index in 0..count {
            let path = write_orphan(&root, index);
            original.insert(path.clone(), std::fs::read(&path).unwrap());
            std::fs::write(path, b"untrusted content").unwrap();
        }
        // Choose a late name in real filesystem order. Do not rely on
        // a sorted directory listing.
        let last = std::fs::read_dir(&root)
            .unwrap()
            .last()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(&last, &original[&last]).unwrap();
        for _ in 0..2 {
            let stateless = prepare_next_code_generation_retention_cancellable(
                store.path(),
                &BTreeSet::new(),
                &|| false,
                None,
            )
            .unwrap();
            assert_eq!(stateless.collectable_text_artifact_count(), 0);
        }

        let mut scan = CodeGenerationRetentionScanV1::default();
        let mut empty_continuations = 0;
        let mut finished = false;
        for tick in 0..(count + 4) {
            let plan = scan
                .prepare(store.path(), &BTreeSet::new(), &|| false, None)
                .unwrap();
            let progress = plan.text_artifact_page_progress.as_ref().unwrap();
            assert!(progress.page.entries.len() <= MAX_CODE_TEXT_ARTIFACT_INVENTORY_ENTRIES_V1);
            let more = plan.has_pending_text_artifact_inventory();
            if plan.has_collectable_work() {
                let report = execute_code_generation_retention(
                    store.path(),
                    plan,
                    CodeGenerationRetentionModeV1::Apply,
                    UtcMicros(tick as i64),
                    None,
                )
                .unwrap();
                scan.complete(&report.plan);
            } else {
                empty_continuations += usize::from(more);
                scan.complete(&plan);
            }
            if !more {
                finished = true;
                break;
            }
        }
        assert!(finished);
        assert!(
            empty_continuations > 0,
            "an all-preserved page is not completion"
        );
        assert!(!last.exists());
        for path in original.keys().filter(|path| **path != last) {
            assert_eq!(std::fs::read(path).unwrap(), b"untrusted content");
        }
    }

    #[test]
    fn successful_batches_keep_unvisited_page_tails_and_reject_stale_acknowledgements() {
        let store = tempfile::tempdir().unwrap();
        let root = code_text_artifacts_root(store.path());
        let paths = (0..100)
            .map(|index| write_orphan(&root, index))
            .collect::<Vec<_>>();
        let mut scan = CodeGenerationRetentionScanV1::default();
        let mut stale = None;
        for tick in 0..20 {
            let plan = scan
                .prepare(store.path(), &BTreeSet::new(), &|| false, None)
                .unwrap();
            assert!(
                plan.collectable_text_artifact_count() <= MAX_CODE_TEXT_ARTIFACT_RETENTION_BATCH_V1
            );
            let more = plan.has_pending_text_artifact_inventory();
            if let Some(stale) = stale.as_ref() {
                let current = Arc::clone(scan.page.as_ref().unwrap());
                scan.complete(stale);
                assert!(Arc::ptr_eq(scan.page.as_ref().unwrap(), &current));
            }
            if plan.has_collectable_work() {
                let report = execute_code_generation_retention(
                    store.path(),
                    plan,
                    CodeGenerationRetentionModeV1::Apply,
                    UtcMicros(tick),
                    None,
                )
                .unwrap();
                scan.complete(&report.plan);
                stale = Some(report.plan);
            } else {
                scan.complete(&plan);
            }
            if !more {
                break;
            }
        }
        assert!(paths.iter().all(|path| !path.exists()));
        assert!(scan.completed.is_none() && scan.staging.is_none());
        let newly_published = write_orphan(&root, 101);
        let next = scan
            .prepare(store.path(), &BTreeSet::new(), &|| false, None)
            .unwrap();
        assert_eq!(next.collectable_text_artifact_count(), 1);
        assert!(newly_published.exists(), "preparing never unlinks");
    }

    #[test]
    fn cancelled_enumeration_and_failed_apply_keep_the_pending_page() {
        let store = tempfile::tempdir().unwrap();
        let root = code_text_artifacts_root(store.path());
        for index in 0..100 {
            write_orphan(&root, index);
        }
        let mut scan = CodeGenerationRetentionScanV1::default();
        let polls = Cell::new(0);
        let cancelled = || {
            polls.set(polls.get() + 1);
            polls.get() > 10
        };
        assert!(matches!(
            scan.prepare(store.path(), &BTreeSet::new(), &cancelled, None),
            Err(CodeGenerationRetentionErrorV1::Cancelled)
        ));
        assert!(!scan.pending.is_empty());
        let plan = scan
            .prepare(store.path(), &BTreeSet::new(), &|| false, None)
            .unwrap();
        let page = Arc::clone(&plan.text_artifact_page_progress.as_ref().unwrap().page);
        // Without an acknowledgement, the next preparation must revalidate the
        // same page, as it does after an apply conflict or cancellation.
        let retry = scan
            .prepare(store.path(), &BTreeSet::new(), &|| false, None)
            .unwrap();
        assert!(Arc::ptr_eq(
            &page,
            &retry.text_artifact_page_progress.as_ref().unwrap().page
        ));
        assert_eq!(
            plan.collectable_text_artifact_count(),
            retry.collectable_text_artifact_count()
        );
    }

    #[test]
    fn empty_scans_restart_and_scope_changes_do_not_reuse_foreign_entries() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut scan = CodeGenerationRetentionScanV1::default();
        let empty = scan
            .prepare(first.path(), &BTreeSet::new(), &|| false, None)
            .unwrap();
        assert!(!empty.has_pending_text_artifact_inventory());
        scan.complete(&empty);
        let live = write_orphan(&code_text_artifacts_root(first.path()), 1);
        let first_plan = scan
            .prepare(first.path(), &BTreeSet::new(), &|| false, None)
            .unwrap();
        assert_eq!(first_plan.collectable_text_artifact_count(), 1);
        let second_plan = scan
            .prepare(second.path(), &BTreeSet::new(), &|| false, None)
            .unwrap();
        scan.complete(&first_plan);
        assert_eq!(second_plan.collectable_text_artifact_count(), 0);
        assert!(live.exists());
        scan.complete(&second_plan);
        let staging = code_text_artifact_staging_root(second.path());
        std::fs::create_dir_all(&staging).unwrap();
        let debris = staging.join(format!(".text-artifact-{}.staging", "a".repeat(64)));
        std::fs::write(&debris, b"abandoned staging").unwrap();
        let next = scan
            .prepare(second.path(), &BTreeSet::new(), &|| false, None)
            .unwrap();
        assert_eq!(next.collectable_text_artifact_count(), 1);
        assert!(debris.exists());
    }
}

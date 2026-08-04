//! Changed-path-only capture for warm code-index reconciliation.
//!
//! The retained candidate map is the last successfully published sanitized
//! snapshot. Warm passes replace only paths proven changed by an exact hook,
//! an old-to-new HEAD tree diff, or the bounded full-status backstop. Unchanged
//! source bytes are never reopened.

use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::UNIX_EPOCH,
};

use gix::bstr::ByteSlice;
use tracedecay_application::{is_canonical_repository_relative_path, now_micros};
use tracedecay_domain::{
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerDispositionV1,
    SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1,
};

use super::{
    CODE_SOURCE_SANITIZER_VERSION_V1, CodeIndexCapturedFileV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, GitStateMayHaveChanged, LanguageRegistry, PendingHintsV1,
    StaticLanguageRegistry, file_occurrence_id, id, sha256_hex, snapshot_content_identity,
};
use crate::privacy::{CodeSourceSanitizationV1, sanitize_code_source_bytes};

#[derive(Clone)]
struct CapturedCandidateV1 {
    file: SanitizedCodeFileV1,
    receipt_id: SanitizationReceiptId,
    retained: Arc<[u8]>,
    sensitivity_level: SensitivityLevelV1,
    source_len: u64,
    modified_at_nanos: u128,
}

#[derive(Clone, Default)]
pub(super) struct WarmDeltaStateV1 {
    initialized: bool,
    candidates: BTreeMap<String, CapturedCandidateV1>,
    reconciled_watcher_epoch: Option<u64>,
}

impl WarmDeltaStateV1 {
    pub(super) const fn reconciled_watcher_epoch(&self) -> Option<u64> {
        self.reconciled_watcher_epoch
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct WarmDeltaMeasurementsV1 {
    pub source_files_read: usize,
    pub full_status_scans: usize,
    pub head_tree_diffs: usize,
}

pub(super) struct CapturedSnapshotV1 {
    pub snapshot: SanitizedCodeSnapshotV1,
    pub captured_files: Vec<CodeIndexCapturedFileV1>,
    pub changed_paths: BTreeSet<String>,
    pub next_state: WarmDeltaStateV1,
    pub measurements: WarmDeltaMeasurementsV1,
    pub stat_signature: String,
}

impl CodeIndexWorktreeSchedulerV1 {
    fn capture_changed_candidate(
        &self,
        registry: &StaticLanguageRegistry,
        logical_path: &str,
    ) -> Result<Option<CapturedCandidateV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(super::cancelled_code_index_reconcile());
        }
        let absolute = self.project_root.join(logical_path);
        if !absolute.is_file() {
            return Ok(None);
        }
        let Some(extension) = absolute.extension().and_then(|value| value.to_str()) else {
            return Ok(None);
        };
        let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
            return Ok(None);
        };
        let raw_bytes = std::fs::read(&absolute)?;
        let metadata = std::fs::metadata(&absolute)?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(super::cancelled_code_index_reconcile());
        }
        let sanitized: CodeSourceSanitizationV1 = sanitize_code_source_bytes(&raw_bytes)
            .map_err(|error| CodeIndexSchedulerErrorV1::Privacy(error.to_string()))?;
        let sensitivity_level = match sanitized.receipt().disposition() {
            SanitizerDispositionV1::Accepted => SensitivityLevelV1::Public,
            SanitizerDispositionV1::Redacted => SensitivityLevelV1::Redacted,
            SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
                return Err(CodeIndexSchedulerErrorV1::Privacy(
                    "durable code source carried a non-durable sanitizer disposition".to_owned(),
                ));
            }
        };
        let receipt_id = sanitized.receipt().receipt().receipt_id().clone();
        let (sanitized_bytes, _) = sanitized.into_parts();
        let (digest, shared) = self.byte_pool.intern(sanitized_bytes);
        let occurrence = file_occurrence_id(
            &self.repository_id,
            &self.worktree_id,
            logical_path,
            &digest,
            &receipt_id,
        )?;
        Ok(Some(CapturedCandidateV1 {
            file: SanitizedCodeFileV1 {
                file_occurrence_id: occurrence.clone(),
                logical_path: logical_path.to_owned(),
                language: Some(descriptor.language.clone()),
                content_digest: digest,
                disposition: SnapshotFileDispositionV1::Present,
            },
            receipt_id,
            retained: shared,
            sensitivity_level,
            source_len: metadata.len(),
            modified_at_nanos: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |elapsed| elapsed.as_nanos()),
        }))
    }

    pub(super) fn capture_authoritative_snapshot(
        &self,
        prior_identity: &super::identity::IndexingIdentityV1,
        hints: &PendingHintsV1,
        force_full_status: bool,
    ) -> Result<CapturedSnapshotV1, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(super::cancelled_code_index_reconcile());
        }
        let repository = gix::open(&self.project_root)
            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
        let mut measurements = WarmDeltaMeasurementsV1::default();
        let mut next_state = self.warm_delta.clone();
        let mut changed_paths = BTreeSet::new();
        let paths_to_capture;

        let watcher_epoch = watcher_epoch_to_reconcile(
            hints.git_state.as_ref(),
            &self.identity,
            next_state.reconciled_watcher_epoch,
        )?;
        if !next_state.initialized {
            let classification =
                super::classification::WorktreeChangeClassificationV1::classify(&repository)
                    .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
            measurements.full_status_scans = 1;
            paths_to_capture = classification.candidate_paths();
            changed_paths = paths_to_capture.clone();
            next_state.candidates.clear();
        } else {
            let exact_paths = normalize_hints(&self.project_root, &hints.paths);
            if prior_identity.head_tree() != self.identity.head_tree() {
                match (prior_identity.head_tree(), self.identity.head_tree()) {
                    (Some(old_tree), Some(new_tree)) => {
                        changed_paths.extend(changed_paths_between(
                            &repository,
                            old_tree.as_str(),
                            new_tree.as_str(),
                        )?);
                        measurements.head_tree_diffs = 1;
                    }
                    _ => {
                        let classification =
                            super::classification::WorktreeChangeClassificationV1::classify(
                                &repository,
                            )
                            .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
                        measurements.full_status_scans = 1;
                        changed_paths.extend(classification.changed_paths());
                    }
                }
            }
            if !exact_paths.is_empty() {
                changed_paths.extend(
                    super::classification::WorktreeChangeClassificationV1::changed_paths_for(
                        &repository,
                        &exact_paths,
                    )
                    .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?,
                );
            }
            let duplicate_watcher_only = hints.git_state.is_some()
                && watcher_epoch.is_none()
                && exact_paths.is_empty()
                && !hints.overflow
                && prior_identity.head_tree() == self.identity.head_tree();
            let needs_backstop = force_full_status
                || hints.overflow
                || (changed_paths.is_empty() && exact_paths.is_empty() && !duplicate_watcher_only);
            if needs_backstop && measurements.full_status_scans == 0 {
                let classification =
                    super::classification::WorktreeChangeClassificationV1::classify(&repository)
                        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
                measurements.full_status_scans = 1;
                changed_paths.extend(classification.changed_paths());
            }
            paths_to_capture = changed_paths.clone();
        }

        if self.shutting_down.load(Ordering::Acquire) {
            return Err(super::cancelled_code_index_reconcile());
        }
        let registry = StaticLanguageRegistry::new();
        let ordered_paths = paths_to_capture.into_iter().collect::<Vec<_>>();
        let outcomes = crate::code_index::parallelism::install(|| {
            use rayon::prelude::*;
            ordered_paths
                .par_iter()
                .map(|logical_path| {
                    (
                        logical_path.clone(),
                        self.capture_changed_candidate(&registry, logical_path),
                    )
                })
                .collect::<Vec<_>>()
        });

        let mut captured_files = Vec::new();
        for (logical_path, outcome) in outcomes {
            match outcome? {
                Some(candidate) => {
                    measurements.source_files_read += 1;
                    captured_files.push(CodeIndexCapturedFileV1 {
                        file_occurrence_id: candidate.file.file_occurrence_id.clone(),
                        sanitized_bytes: candidate.retained.to_vec(),
                        sensitivity_level: candidate.sensitivity_level,
                    });
                    next_state.candidates.insert(logical_path, candidate);
                }
                None => {
                    next_state.candidates.remove(&logical_path);
                }
            }
        }
        next_state.initialized = true;
        if let Some(epoch) = watcher_epoch {
            next_state.reconciled_watcher_epoch = Some(epoch);
        }

        let mut files = next_state
            .candidates
            .values()
            .map(|candidate| candidate.file.clone())
            .collect::<Vec<_>>();
        files.sort_by(|left, right| {
            (&left.logical_path, &left.file_occurrence_id)
                .cmp(&(&right.logical_path, &right.file_occurrence_id))
        });
        captured_files
            .sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
        let sanitization_receipts = next_state
            .candidates
            .values()
            .map(|candidate| candidate.receipt_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let content_identity = snapshot_content_identity(&files, &sanitization_receipts);
        let stat_signature = stat_signature(&next_state.candidates);
        Ok(CapturedSnapshotV1 {
            snapshot: SanitizedCodeSnapshotV1 {
                repository: self.repository_id.clone(),
                worktree: Some(self.worktree_id.clone()),
                reference: self.identity.head_ref().cloned(),
                source_revision: self.identity.head_commit().cloned(),
                sanitizer_revision: id::<SanitizerRevision>(CODE_SOURCE_SANITIZER_VERSION_V1)?,
                sanitization_receipts,
                content_identity,
                captured_at: now_micros(),
                files,
            },
            captured_files,
            changed_paths,
            next_state,
            measurements,
            stat_signature,
        })
    }
}

fn stat_signature(candidates: &BTreeMap<String, CapturedCandidateV1>) -> String {
    let mut buffer = Vec::new();
    for (logical_path, candidate) in candidates {
        buffer.extend_from_slice(logical_path.as_bytes());
        buffer.push(0);
        buffer.extend_from_slice(&candidate.source_len.to_le_bytes());
        buffer.extend_from_slice(&candidate.modified_at_nanos.to_le_bytes());
        buffer.push(0xff);
    }
    format!("sha256:{}", sha256_hex(&buffer))
}

fn watcher_epoch_to_reconcile(
    event: Option<&GitStateMayHaveChanged>,
    identity: &super::identity::IndexingIdentityV1,
    reconciled_epoch: Option<u64>,
) -> Result<Option<u64>, CodeIndexSchedulerErrorV1> {
    let Some(event) = event else {
        return Ok(None);
    };
    if !identity.authorizes_reuse_of(&event.identity) {
        return Err(CodeIndexSchedulerErrorV1::Identity(
            "git watcher event belongs to a different repository/worktree identity".to_owned(),
        ));
    }
    Ok(
        (reconciled_epoch.is_none_or(|epoch| event.watcher_epoch > epoch))
            .then_some(event.watcher_epoch),
    )
}

fn normalize_hints(project_root: &Path, hints: &BTreeSet<PathBuf>) -> BTreeSet<String> {
    hints
        .iter()
        .filter_map(|hint| {
            let relative = if hint.is_absolute() {
                hint.strip_prefix(project_root).ok()?
            } else {
                hint.as_path()
            };
            let path = relative.to_str()?.replace('\\', "/");
            is_canonical_repository_relative_path(&path).then_some(path)
        })
        .collect()
}

fn changed_paths_between(
    repository: &gix::Repository,
    old_tree: &str,
    new_tree: &str,
) -> Result<BTreeSet<String>, CodeIndexSchedulerErrorV1> {
    let old_id = gix::ObjectId::from_hex(old_tree.as_bytes())
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let new_id = gix::ObjectId::from_hex(new_tree.as_bytes())
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let old_tree = repository
        .find_tree(old_id)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let new_tree = repository
        .find_tree(new_id)
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    let mut paths = BTreeSet::new();
    let mut changes = old_tree
        .changes()
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    changes.options(|options| {
        options.track_path();
        options.track_rewrites(None);
    });
    changes
        .for_each_to_obtain_tree(&new_tree, |change| {
            use gix::object::tree::diff::Change;
            match change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                }
                | Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } if !entry_mode.is_tree() => {
                    paths.insert(location.to_str_lossy().into_owned());
                }
                Change::Modification {
                    location,
                    entry_mode,
                    ..
                } if !entry_mode.is_tree() => {
                    paths.insert(location.to_str_lossy().into_owned());
                }
                Change::Rewrite {
                    source_location,
                    source_entry_mode,
                    location,
                    entry_mode,
                    ..
                } => {
                    if !source_entry_mode.is_tree() {
                        paths.insert(source_location.to_str_lossy().into_owned());
                    }
                    if !entry_mode.is_tree() {
                        paths.insert(location.to_str_lossy().into_owned());
                    }
                }
                _ => {}
            }
            Ok::<_, Infallible>(std::ops::ControlFlow::Continue(()))
        })
        .map_err(|error| CodeIndexSchedulerErrorV1::Git(error.to_string()))?;
    Ok(paths)
}

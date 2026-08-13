//! Immutable Git-tree capture for exact branch generation reads.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use gix::bstr::ByteSlice;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1;

use super::*;

pub(super) struct ExactGitTreeSourceV1 {
    pub reference: tracedecay_domain::RefId,
    pub revision: tracedecay_domain::CommitId,
    pub tree: tracedecay_domain::TreeId,
}

/// One source path the privacy boundary refused to hand on.
///
/// A refused file produces no sanitization receipt and no sanitized bytes, so
/// there is nothing a snapshot entry could truthfully carry for it: it is
/// withheld from the generation and named here instead. The distinction that
/// matters is scope — a refusal is evidence about *one file*, never about the
/// tree, so it must not be allowed to cost every other file its index.
#[derive(Debug)]
pub(super) struct WithheldSourceV1 {
    pub logical_path: String,
    pub reason: String,
}

/// Classifies a per-file capture failure as either a privacy refusal that the
/// capture degrades around, or a genuine capture fault that still terminates
/// it. Only the sanitizer's own refusal is survivable: an I/O, identity, or
/// production failure says the capture itself is unsound.
pub(super) fn classify_capture_failure(
    logical_path: &str,
    error: CodeIndexSchedulerErrorV1,
) -> Result<WithheldSourceV1, CodeIndexSchedulerErrorV1> {
    match error {
        CodeIndexSchedulerErrorV1::Privacy(reason) => Ok(WithheldSourceV1 {
            logical_path: logical_path.to_owned(),
            reason,
        }),
        other => Err(other),
    }
}

/// Largest number of withheld paths named in the single summary record. The
/// summary is one line per capture rather than one per file precisely because
/// an unbounded per-file warning is what turned a handful of refusals into a
/// log flood.
const MAX_REPORTED_WITHHELD_SOURCES: usize = 16;

pub(super) fn report_withheld_sources(withheld: &[WithheldSourceV1]) {
    if withheld.is_empty() {
        return;
    }
    let named = withheld
        .iter()
        .take(MAX_REPORTED_WITHHELD_SOURCES)
        .map(|source| format!("{}: {}", source.logical_path, source.reason))
        .collect::<Vec<_>>()
        .join("; ");
    tracing::warn!(
        withheld = withheld.len(),
        named = %named,
        "code_index_sources_withheld_by_privacy"
    );
}

impl CodeIndexExecutionControlV1 for branch_generations::BranchGenerationReadControlV1 {
    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
    }

    fn is_deadline_exceeded(&self) -> bool {
        self.deadline.as_ref().is_some_and(|deadline| {
            deadline.is_elapsed_at(tracedecay_application::clock::now_micros())
        })
    }
}

impl DaemonCodeIndexPublicationStoreV1 {
    pub(super) fn exact_git_evidence(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<Option<(String, String, String)>, CodeIndexPublicationStoreErrorV1> {
        let Some(source_revision) = generation.snapshot().source_revision.as_ref() else {
            return Ok(None);
        };
        let Some(reference) = generation.snapshot().reference.as_ref() else {
            return Ok(None);
        };
        let repository = gix::open(&self.project_root).map_err(Self::unavailable)?;
        let identity =
            identity::IndexingIdentityV1::resolve(&self.project_root).map_err(Self::unavailable)?;
        if generation.snapshot().repository != *identity.repository_id()
            || generation.snapshot().worktree.as_ref() != Some(identity.worktree_id())
        {
            return Ok(None);
        }
        let mut git_reference = repository
            .try_find_reference(reference.as_str())
            .map_err(Self::unavailable)?
            .ok_or_else(|| Self::unavailable("exact code-generation reference is missing"))?;
        let commit = git_reference.peel_to_commit().map_err(Self::unavailable)?;
        if commit.id().to_string() != source_revision.as_str() {
            return Ok(None);
        }
        let tree = commit.tree_id().map_err(Self::unavailable)?;
        Ok(Some((
            reference.as_str().to_owned(),
            source_revision.as_str().to_owned(),
            tree.to_string(),
        )))
    }

    pub(super) fn validate_exact_git_evidence(
        &self,
        revision: &str,
        expected_tree: &str,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let repository = gix::open(&self.project_root).map_err(Self::unavailable)?;
        let object_id =
            gix::hash::ObjectId::from_hex(revision.as_bytes()).map_err(Self::unavailable)?;
        let commit = repository
            .find_object(object_id)
            .map_err(Self::unavailable)?
            .try_into_commit()
            .map_err(Self::unavailable)?;
        let actual_tree = commit.tree_id().map_err(Self::unavailable)?;
        if actual_tree.to_string() != expected_tree {
            return Err(Self::unavailable(
                "durable code-generation index commit tree does not match Git",
            ));
        }
        Ok(())
    }
}

impl CodeIndexWorktreeSchedulerV1 {
    pub(super) fn capture_candidate_bytes(
        &self,
        registry: &StaticLanguageRegistry,
        logical_path: &str,
        raw_bytes: &[u8],
    ) -> Result<Option<CapturedCandidateV1>, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let Some(extension) = Path::new(logical_path)
            .extension()
            .and_then(|value| value.to_str())
        else {
            return Ok(None);
        };
        let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
            return Ok(None);
        };
        let (sanitized_bytes, sensitivity_level, receipt_id) =
            privacy::sanitize_code_file(raw_bytes)?;
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
            captured: CodeIndexCapturedFileV1 {
                file_occurrence_id: occurrence,
                sanitized_bytes: shared.to_vec(),
                sensitivity_level,
            },
            receipt_id,
            retained: shared,
        }))
    }

    pub(super) fn capture_exact_git_tree_snapshot(
        &self,
        source: &ExactGitTreeSourceV1,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<CapturedSnapshotV1, CodeIndexSearchUnavailableReasonV1> {
        control.termination().map_or(Ok(()), Err)?;
        let repository = gix::open(&self.project_root)
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        let mut reference = repository
            .try_find_reference(source.reference.as_str())
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?
            .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        let commit = reference
            .peel_to_commit()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        if commit.id().to_string() != source.revision.as_str() {
            return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
        }
        let tree = commit
            .tree()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        if tree.id().to_string() != source.tree.as_str() {
            return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
        }
        let mut entries = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        entries.sort_by(|left, right| left.filepath.cmp(&right.filepath));

        let registry = StaticLanguageRegistry::new();
        let mut files = Vec::new();
        let mut captured_files = Vec::new();
        let mut sanitization_receipts = BTreeSet::new();
        let mut retained_bytes: Vec<Arc<[u8]>> = Vec::new();
        let mut changed_paths = BTreeSet::new();
        let mut withheld_sources = Vec::new();
        for entry in entries {
            control.termination().map_or(Ok(()), Err)?;
            if entry.mode.is_tree() || entry.mode.is_commit() {
                continue;
            }
            let logical_path = entry.filepath.to_str_lossy().into_owned();
            if crate::config::is_generated_path_segment(&logical_path) {
                continue;
            }
            let blob = repository
                .find_blob(entry.oid)
                .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
            let candidate = match self.capture_candidate_bytes(&registry, &logical_path, &blob.data)
            {
                Ok(Some(candidate)) => candidate,
                Ok(None) => continue,
                Err(error) => {
                    if self.shutting_down.load(Ordering::Acquire) {
                        return Err(CodeIndexSearchUnavailableReasonV1::Cancelled);
                    }
                    match classify_capture_failure(&logical_path, error) {
                        Ok(withheld) => {
                            withheld_sources.push(withheld);
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                path = %logical_path,
                                "exact_git_tree_capture_failed"
                            );
                            return Err(CodeIndexSearchUnavailableReasonV1::Internal);
                        }
                    }
                }
            };
            changed_paths.insert(logical_path);
            sanitization_receipts.insert(candidate.receipt_id);
            retained_bytes.push(candidate.retained);
            files.push(candidate.file);
            captured_files.push(candidate.captured);
        }
        report_withheld_sources(&withheld_sources);
        // Withholding every indexable file is not a degraded generation, it is
        // no generation: publishing an empty index over a tree that has sources
        // would answer later queries with a confident, false "nothing here".
        if files.is_empty() && !withheld_sources.is_empty() {
            return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
        }
        if let Some(active) = self
            .publication
            .load_active_shared()
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
        {
            changed_paths.extend(
                active
                    .snapshot()
                    .files
                    .iter()
                    .map(|file| file.logical_path.clone()),
            );
        }
        files.sort_by(|left, right| {
            (&left.logical_path, &left.file_occurrence_id)
                .cmp(&(&right.logical_path, &right.file_occurrence_id))
        });
        captured_files
            .sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
        let sanitization_receipts = sanitization_receipts.into_iter().collect::<Vec<_>>();
        let content_identity = snapshot_content_identity(&files, &sanitization_receipts);
        Ok(CapturedSnapshotV1 {
            // An exact sealed Git tree is immutable committed state: the parse
            // identity is the tree itself and can never be dirty.
            repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
                tree: Some(source.tree.clone()),
                dirty: tracedecay_domain::RepositoryDirtyStateV1::Clean,
            },
            snapshot: SanitizedCodeSnapshotV1 {
                repository: self.repository_id.clone(),
                worktree: Some(self.worktree_id.clone()),
                reference: Some(source.reference.clone()),
                source_revision: Some(source.revision.clone()),
                sanitizer_revision: id::<SanitizerRevision>(CODE_SOURCE_SANITIZER_VERSION_V1)
                    .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?,
                sanitization_receipts,
                content_identity,
                captured_at: now_micros(),
                files,
            },
            captured_files,
            changed_paths,
            retained_bytes,
        })
    }

    pub(super) fn publish_exact_git_tree_generation(
        &mut self,
        source: &ExactGitTreeSourceV1,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<LatestCompleteCodeIndexV1, CodeIndexSearchUnavailableReasonV1> {
        let captured = self.capture_exact_git_tree_snapshot(source, control)?;
        let CapturedSnapshotV1 {
            snapshot,
            repository_parse_identity,
            captured_files,
            changed_paths,
            retained_bytes: _retained_bytes,
        } = captured;
        let mut owner = open_production_code_index_owner_v1(
            self.production_config.clone(),
            self.publication.retained_history(),
            DaemonProjectionSinkV1,
        )
        .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?
        .with_physical_artifact_pool(self.byte_pool.physical_artifacts.clone());
        let generation = owner
            .build_and_publish(
                CodeIndexBuildRequestV1 {
                    snapshot,
                    captured_files,
                    changed_files: changed_paths,
                    invalidations: BTreeSet::new(),
                    repository_parse_identity,
                    ignored_source_admissions: Vec::new(),
                    sealed_at: now_micros(),
                    target_projection_key: projection_key()
                        .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?,
                },
                control,
            )
            .map_err(|error| match error {
                CodeIndexProductionErrorV1::Interrupted(
                    crate::code_index::production::CodeIndexInterruptionV1::Cancelled,
                ) => CodeIndexSearchUnavailableReasonV1::Cancelled,
                CodeIndexProductionErrorV1::Interrupted(
                    crate::code_index::production::CodeIndexInterruptionV1::DeadlineExceeded,
                ) => CodeIndexSearchUnavailableReasonV1::TimedOut,
                CodeIndexProductionErrorV1::Publication(error) => {
                    DaemonCodeIndexPublicationStoreV1::exact_read_error(error)
                }
                _ => CodeIndexSearchUnavailableReasonV1::Internal,
            })?;
        Ok(self.bind_latest_complete(Arc::new(generation)))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use tempfile::TempDir;
    use tracedecay_code_index::production::CodeIndexIgnoredSourceAdmissionV1;
    use tracedecay_domain::ProjectId;

    use super::{
        CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
        classify_capture_failure,
    };

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new(crate::git::git_program())
            .current_dir(root)
            .args(arguments)
            .status()
            .expect("run git fixture command");
        assert!(
            status.success(),
            "git fixture command failed: {arguments:?}"
        );
    }

    fn generated_source_fixture() -> (TempDir, TempDir, CodeIndexWorktreeSchedulerV1) {
        let project = TempDir::new().expect("project root");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::create_dir_all(project.path().join("dist")).expect("generated directory");
        std::fs::write(project.path().join("src/lib.rs"), "pub fn kept() {}\n")
            .expect("ordinary source");
        std::fs::write(
            project.path().join("dist/generated.js"),
            "export function generatedOnly() {}\n",
        )
        .expect("generated source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "fixture"]);

        let store = TempDir::new().expect("code-index store");
        let scheduler = CodeIndexWorktreeSchedulerV1::open(
            ProjectId::new("project.generated-source-policy").expect("project id"),
            project.path(),
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open code-index scheduler");
        (project, store, scheduler)
    }

    fn captured_paths(scheduler: &CodeIndexWorktreeSchedulerV1) -> Vec<String> {
        scheduler
            .capture_authoritative_snapshot(None)
            .expect("capture authoritative snapshot")
            .snapshot
            .files
            .into_iter()
            .map(|file| file.logical_path)
            .collect()
    }

    #[test]
    fn committed_generated_directory_source_is_excluded_from_exact_tree_capture() {
        let (_project, _store, scheduler) = generated_source_fixture();

        assert_eq!(captured_paths(&scheduler), vec!["src/lib.rs"]);
    }

    #[test]
    fn dirty_generated_candidate_is_excluded_while_ordinary_source_remains() {
        let (project, _store, scheduler) = generated_source_fixture();
        std::fs::write(
            project.path().join("dist/generated.js"),
            "export function changedGeneratedOnly() {}\n",
        )
        .expect("modify generated source");

        assert_eq!(captured_paths(&scheduler), vec!["src/lib.rs"]);
    }

    #[test]
    fn explicit_ignored_source_admission_can_include_generated_path() {
        let (project, _store, mut scheduler) = generated_source_fixture();
        std::fs::write(
            project.path().join("dist/generated.js"),
            "export function explicitlyAdmitted() {}\n",
        )
        .expect("modify generated source");
        scheduler.ignored_source_admissions = vec![CodeIndexIgnoredSourceAdmissionV1 {
            logical_path: "dist/generated.js".to_owned(),
        }];

        assert_eq!(
            captured_paths(&scheduler),
            vec!["dist/generated.js", "src/lib.rs"]
        );
    }

    /// A sanitizer refusal is evidence about one file. Before this, the first
    /// refused path failed the whole tree capture, so a single file the privacy
    /// boundary would not hand on left the project with no code index at all.
    #[test]
    fn a_privacy_refusal_withholds_only_its_own_path() {
        let withheld = classify_capture_failure(
            "src/fixtures/malformed.json",
            CodeIndexSchedulerErrorV1::Privacy("structured document is malformed".to_owned()),
        )
        .expect("a privacy refusal is survivable");

        assert_eq!(withheld.logical_path, "src/fixtures/malformed.json");
        assert_eq!(withheld.reason, "structured document is malformed");
    }

    /// Everything that is not the sanitizer's own refusal says the capture
    /// itself is unsound, and must still terminate it rather than quietly
    /// dropping files out of a generation that claims to be complete.
    #[test]
    fn a_capture_fault_still_terminates_the_capture() {
        let error = classify_capture_failure(
            "src/main.rs",
            CodeIndexSchedulerErrorV1::Identity("occurrence identity failed".to_owned()),
        )
        .expect_err("a capture fault is not survivable");

        assert!(matches!(error, CodeIndexSchedulerErrorV1::Identity(_)));
    }
}

//! Immutable Git-tree capture for exact branch generation reads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use gix::bstr::ByteSlice;
use tracedecay_application::code_index::open_production_code_index_owner_v1;
use tracedecay_code_index::production::CodeIndexExecutionControlV1;
use tracedecay_contracts::now_micros;
use tracedecay_domain::{
    CodeSourceOmissionReasonV1, OmittedCodeSourceV1, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
    validate_code_logical_path,
};
use tracedecay_privacy::CODE_SOURCE_SANITIZER_VERSION_V1;
use tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1;
use tracedecay_runtime_core::resident_memory::ResidentMemoryReservationV1;

use super::{
    CapturedCandidateV1, CapturedSnapshotV1, CodeIndexSchedulerErrorV1,
    CodeIndexWorktreeSchedulerV1, DaemonCodeIndexPublicationStoreV1, DaemonProjectionSinkV1,
    LatestCompleteCodeIndexV1, branch_generations, cancelled_code_index_reconcile,
    file_occurrence_id, id, identity, omitted_file_occurrence_id, privacy, projection_key,
    snapshot_content_identity,
};
use crate::code_index::chunks::content_digest;
use crate::code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use crate::code_index::production::{
    CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexGenerationScopeV1,
    CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexRepositoryParseIdentityV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactGitTreeSourceV1 {
    pub reference: tracedecay_domain::RefId,
    pub revision: tracedecay_domain::CommitId,
    pub tree: tracedecay_domain::TreeId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCandidateGenerationSourcesV1 {
    pub merge_base: ExactGitTreeSourceV1,
    pub source: ExactGitTreeSourceV1,
    pub destination: ExactGitTreeSourceV1,
    pub candidate_reference: tracedecay_domain::RefId,
    pub candidate_tree: tracedecay_domain::TreeId,
}

pub struct NativeCandidateGenerationBindingsV1 {
    pub merge_base: LatestCompleteCodeIndexV1,
    pub source: LatestCompleteCodeIndexV1,
    pub destination: LatestCompleteCodeIndexV1,
    pub candidate: LatestCompleteCodeIndexV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCandidateGenerationIdentityV1 {
    pub generation_id: tracedecay_domain::CodeGenerationId,
    pub project_id: tracedecay_domain::ProjectId,
    pub repository_id: tracedecay_domain::RepositoryId,
    pub worktree_id: Option<tracedecay_domain::WorktreeId>,
    pub reference: Option<tracedecay_domain::RefId>,
    pub snapshot_digest: tracedecay_domain::ManifestDigest,
    pub content_identity: tracedecay_domain::ContentDigest,
    pub source_revision: Option<tracedecay_domain::GitOidV1>,
    pub source_tree: tracedecay_domain::GitOidV1,
    pub seal_digest: tracedecay_domain::ManifestDigest,
}

/// Omitted files still belong to the source roster; only absent paths disappear.
pub(super) enum CapturedFileOutcomeV1 {
    Present(CapturedCandidateV1),
    Omitted(SanitizedCodeFileV1),
    Withheld {
        file: SanitizedCodeFileV1,
        reason: String,
    },
    /// Raw Git path bytes the portable logical-path grammar cannot carry (a
    /// literal backslash, a control character, non-UTF-8). No snapshot row
    /// can name it, so the snapshot records it as an omitted source.
    Unrepresentable(Vec<u8>),
    Absent,
}

#[derive(Default)]
pub(super) struct CapturedFileRosterV1 {
    pub(super) files: Vec<SanitizedCodeFileV1>,
    pub(super) captured_files: Vec<CodeIndexCapturedFileV1>,
    pub(super) sanitization_receipts: BTreeSet<SanitizationReceiptId>,
    pub(super) retained_bytes: Vec<Arc<[u8]>>,
    pub(super) retained_reservations: Vec<ResidentMemoryReservationV1>,
    pub(super) omitted_sources: Vec<OmittedCodeSourceV1>,
}

impl CapturedFileRosterV1 {
    pub(super) fn push(&mut self, outcome: CapturedFileOutcomeV1) {
        match outcome {
            CapturedFileOutcomeV1::Present(candidate) => {
                self.sanitization_receipts.insert(candidate.receipt_id);
                if let Some(reservation) = candidate.retained_reservation {
                    self.retained_reservations.push(reservation);
                }
                self.retained_bytes.push(candidate.retained);
                self.files.push(candidate.file);
                self.captured_files.push(candidate.captured);
            }
            CapturedFileOutcomeV1::Omitted(file) => self.files.push(file),
            CapturedFileOutcomeV1::Withheld { file, reason } => {
                self.omitted_sources.push(OmittedCodeSourceV1 {
                    git_path: file.logical_path.as_bytes().to_vec(),
                    reason: CodeSourceOmissionReasonV1::PrivacyWithheld { detail: reason },
                });
                self.files.push(file);
            }
            CapturedFileOutcomeV1::Unrepresentable(git_path) => {
                self.omitted_sources.push(OmittedCodeSourceV1 {
                    git_path,
                    reason: CodeSourceOmissionReasonV1::UnrepresentablePath,
                });
            }
            CapturedFileOutcomeV1::Absent => {}
        }
    }

    pub(super) fn finish(mut self) -> Result<Self, CodeIndexSchedulerErrorV1> {
        if self
            .files
            .iter()
            .any(|file| file.disposition == SnapshotFileDispositionV1::Ignored)
            && !self
                .files
                .iter()
                .any(|file| file.disposition == SnapshotFileDispositionV1::Present)
        {
            return Err(CodeIndexSchedulerErrorV1::Privacy(
                "every indexable source was withheld by the privacy boundary".to_owned(),
            ));
        }
        self.files.sort_by(|left, right| {
            (&left.logical_path, &left.file_occurrence_id)
                .cmp(&(&right.logical_path, &right.file_occurrence_id))
        });
        self.captured_files
            .sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
        self.omitted_sources
            .sort_by(|left, right| left.git_path.cmp(&right.git_path));
        Ok(self)
    }
}

/// The logical path naming `git_path`, or `None` when no snapshot row can.
pub(super) fn representable_logical_path(git_path: &[u8]) -> Option<&str> {
    std::str::from_utf8(git_path)
        .ok()
        .filter(|path| validate_code_logical_path(path).is_ok())
}

impl CodeIndexExecutionControlV1 for branch_generations::BranchGenerationReadControlV1 {
    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled)
    }

    fn is_deadline_exceeded(&self) -> bool {
        self.deadline.as_ref().is_some_and(|deadline| {
            deadline.is_elapsed_at(tracedecay_contracts::clock::now_micros())
        })
    }
}

impl DaemonCodeIndexPublicationStoreV1 {
    pub fn exact_git_evidence(
        &self,
        snapshot: &SanitizedCodeSnapshotV1,
    ) -> Result<Option<(String, String, String)>, CodeIndexPublicationStoreErrorV1> {
        let Some(source_revision) = snapshot.source_revision.as_ref() else {
            return Ok(None);
        };
        let Some(reference) = snapshot.reference.as_ref() else {
            return Ok(None);
        };
        let repository = tracedecay_runtime_core::git_open::open(&self.project_root)
            .map_err(Self::unavailable)?;
        let identity =
            identity::IndexingIdentityV1::resolve(&self.project_root).map_err(Self::unavailable)?;
        if snapshot.repository != *identity.repository_id()
            || snapshot.worktree.as_ref() != Some(identity.worktree_id())
        {
            return Ok(None);
        }
        // The reference must exist, evidence naming a ref this repository does
        // not have is provenance we cannot stand behind, but it is *only* the
        // provenance name. The commit is resolved by its own object id, because
        // demanding the reference still peel to this revision silently dropped
        // the Git evidence of every generation sealed at a revision the branch
        // has since moved past. Those entries then looked, to a later exact
        // read, exactly like revisions that were never indexed at all.
        if repository
            .try_find_reference(reference.as_str())
            .map_err(Self::unavailable)?
            .is_none()
        {
            return Err(Self::unavailable(
                "exact code-generation reference is missing",
            ));
        }
        let Some(commit) = gix::hash::ObjectId::from_hex(source_revision.as_str().as_bytes())
            .ok()
            .and_then(|object_id| repository.find_object(object_id).ok())
            .and_then(|object| object.try_into_commit().ok())
        else {
            // A revision this repository cannot resolve yields no evidence, but
            // it is not a reason to fail the publication: the generation is
            // still sound, it simply carries no exact-commit claim.
            return Ok(None);
        };
        let tree = commit.tree_id().map_err(Self::unavailable)?;
        Ok(Some((
            reference.as_str().to_owned(),
            source_revision.as_str().to_owned(),
            tree.to_string(),
        )))
    }

    pub fn validate_exact_git_evidence(
        &self,
        revision: &str,
        expected_tree: &str,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let repository = tracedecay_runtime_core::git_open::open(&self.project_root)
            .map_err(Self::unavailable)?;
        let object_id =
            gix::hash::ObjectId::from_hex(revision.as_bytes()).map_err(Self::unavailable)?;
        let commit = repository
            .find_object(object_id)
            .map_err(Self::unavailable)?
            .try_into_commit()
            .map_err(Self::unavailable)?;
        let actual_tree = commit.tree_id().map_err(Self::unavailable)?;
        if actual_tree != expected_tree {
            return Err(Self::unavailable(
                "durable code-generation index commit tree does not match Git",
            ));
        }
        Ok(())
    }
}

impl CodeIndexWorktreeSchedulerV1 {
    /// Whether one sealed row is what capture under this owner's path policy
    /// would produce: excluded paths, and only they, are omitted as
    /// `Generated`.
    pub(super) fn row_follows_path_policy(&self, file: &SanitizedCodeFileV1) -> bool {
        (file.disposition == SnapshotFileDispositionV1::Generated)
            == self.path_policy.excludes(&file.logical_path)
    }

    /// Whether a sealed roster was captured under this owner's path policy.
    /// A roster sealed under other `index.exclude.v1` / `index.include.v1`
    /// values is not reusable: its rows would keep or drop the wrong files.
    /// Explicitly admitted ignored sources bypass the policy by design.
    pub(super) fn roster_follows_path_policy(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> bool {
        let admitted = generation
            .ignored_source_admissions()
            .iter()
            .map(|admission| admission.logical_path.as_str())
            .collect::<BTreeSet<_>>();
        generation
            .snapshot()
            .files
            .iter()
            .filter(|file| !admitted.contains(file.logical_path.as_str()))
            .all(|file| self.row_follows_path_policy(file))
    }

    fn omitted_source_file(
        &self,
        logical_path: &str,
        raw_bytes: &[u8],
        disposition: SnapshotFileDispositionV1,
    ) -> Result<SanitizedCodeFileV1, CodeIndexSchedulerErrorV1> {
        let digest = content_digest(raw_bytes);
        let occurrence =
            omitted_file_occurrence_id(&self.repository_id, logical_path, &digest, disposition)?;
        Ok(SanitizedCodeFileV1 {
            file_occurrence_id: occurrence,
            logical_path: logical_path.to_owned(),
            language: None,
            content_digest: digest,
            disposition,
        })
    }

    pub(super) fn capture_candidate_bytes(
        &self,
        registry: &StaticLanguageRegistry,
        git_path: &[u8],
        raw_bytes: &[u8],
        explicitly_admitted: bool,
    ) -> Result<CapturedFileOutcomeV1, CodeIndexSchedulerErrorV1> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(cancelled_code_index_reconcile());
        }
        let Some(logical_path) = representable_logical_path(git_path) else {
            return Ok(CapturedFileOutcomeV1::Unrepresentable(git_path.to_vec()));
        };
        if !explicitly_admitted && self.path_policy.excludes(logical_path) {
            return self
                .omitted_source_file(
                    logical_path,
                    raw_bytes,
                    SnapshotFileDispositionV1::Generated,
                )
                .map(CapturedFileOutcomeV1::Omitted);
        }
        let descriptor = Path::new(logical_path)
            .extension()
            .and_then(|value| value.to_str())
            .and_then(|extension| registry.descriptor_for_extension(&extension.to_lowercase()));
        let Some(descriptor) = descriptor else {
            return self
                .omitted_source_file(
                    logical_path,
                    raw_bytes,
                    SnapshotFileDispositionV1::UnsupportedLanguage,
                )
                .map(CapturedFileOutcomeV1::Omitted);
        };
        let (sanitized_bytes, sensitivity_level, receipt_id) =
            match privacy::sanitize_code_file(&descriptor.language, raw_bytes) {
                Ok(sanitized) => sanitized,
                Err(CodeIndexSchedulerErrorV1::Privacy(reason)) => {
                    return Ok(CapturedFileOutcomeV1::Withheld {
                        file: self.omitted_source_file(
                            logical_path,
                            raw_bytes,
                            SnapshotFileDispositionV1::Ignored,
                        )?,
                        reason,
                    });
                }
                Err(error) => return Err(error),
            };
        let (digest, shared) = self.byte_pool.intern(sanitized_bytes);
        let retained_reservation = self.reserve_snapshot_memory(&digest, shared.len())?;
        let occurrence =
            file_occurrence_id(&self.repository_id, logical_path, &digest, &receipt_id)?;
        Ok(CapturedFileOutcomeV1::Present(CapturedCandidateV1 {
            file: SanitizedCodeFileV1 {
                file_occurrence_id: occurrence.clone(),
                logical_path: logical_path.to_owned(),
                language: Some(descriptor.language.clone()),
                content_digest: digest,
                disposition: SnapshotFileDispositionV1::Present,
            },
            captured: CodeIndexCapturedFileV1 {
                file_occurrence_id: occurrence,
                sanitized_bytes: Arc::clone(&shared),
                sensitivity_level,
            },
            receipt_id,
            retained: shared,
            retained_reservation,
        }))
    }

    #[tracing::instrument(
        name = "daemon.code_index.capture.exact_git_tree",
        level = "trace",
        skip_all
    )]
    pub(super) fn capture_exact_git_tree_snapshot(
        &self,
        source: &ExactGitTreeSourceV1,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<CapturedSnapshotV1, CodeIndexSearchUnavailableReasonV1> {
        control.termination().map_or(Ok(()), Err)?;
        let repository = tracedecay_runtime_core::git_open::open(&self.project_root)
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        if repository
            .try_find_reference(source.reference.as_str())
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?
            .is_none()
        {
            return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
        }
        let object_id = gix::hash::ObjectId::from_hex(source.revision.as_str().as_bytes())
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::InvalidRequest)?;
        let commit = repository
            .find_object(object_id)
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?
            .try_into_commit()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        let tree = commit
            .tree()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        if tree.id() != source.tree.as_str() {
            return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
        }
        let mut entries = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        entries.sort_by(|left, right| left.filepath.cmp(&right.filepath));

        self.capture_git_tree_blobs_snapshot(
            source.reference.clone(),
            Some(source.revision.clone()),
            source.tree.clone(),
            control,
            |visitor| {
                for entry in entries {
                    control.termination().map_or(Ok(()), Err)?;
                    if entry.mode.is_tree() || entry.mode.is_commit() {
                        continue;
                    }
                    let blob = repository
                        .find_blob(entry.oid)
                        .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
                    visitor(entry.filepath.as_ref(), &blob.data)?;
                }
                Ok(())
            },
        )
    }

    #[tracing::instrument(
        name = "daemon.code_index.capture.native_candidate_tree",
        level = "trace",
        skip_all
    )]
    fn capture_native_candidate_tree_snapshot(
        &self,
        reference: tracedecay_domain::RefId,
        expected_tree: &tracedecay_domain::TreeId,
        candidate: &tracedecay_runtime_core::git_repository::GitNativeCandidateTreeV1<'_>,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<CapturedSnapshotV1, CodeIndexSearchUnavailableReasonV1> {
        let actual_tree = candidate
            .tree()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        if actual_tree.as_str() != expected_tree.as_str() {
            return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
        }
        self.capture_git_tree_blobs_snapshot(
            reference,
            None,
            expected_tree.clone(),
            control,
            |visitor| {
                candidate.visit_blobs(visitor).map_err(|error| match error {
                    tracedecay_runtime_core::git_repository::GitNativeCandidateTreeVisitError::Repository(_) => {
                        control
                            .termination()
                            .unwrap_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
                    }
                    tracedecay_runtime_core::git_repository::GitNativeCandidateTreeVisitError::Visitor(reason) => reason,
                })
            },
        )
    }

    fn capture_git_tree_blobs_snapshot(
        &self,
        reference: tracedecay_domain::RefId,
        source_revision: Option<tracedecay_domain::CommitId>,
        tree: tracedecay_domain::TreeId,
        control: &branch_generations::BranchGenerationReadControlV1,
        visit: impl FnOnce(
            &mut dyn FnMut(&[u8], &[u8]) -> Result<(), CodeIndexSearchUnavailableReasonV1>,
        ) -> Result<(), CodeIndexSearchUnavailableReasonV1>,
    ) -> Result<CapturedSnapshotV1, CodeIndexSearchUnavailableReasonV1> {
        let registry = StaticLanguageRegistry::new();
        let mut roster = CapturedFileRosterV1::default();
        let _scan_batch = tracedecay_privacy::code_source_scan_batch();
        visit(&mut |git_path, raw_bytes| {
            control.termination().map_or(Ok(()), Err)?;
            let outcome = self.capture_candidate_bytes(
                &registry, git_path, raw_bytes, false,
            ).map_err(|error| {
                if self.shutting_down.load(Ordering::Acquire) {
                    CodeIndexSearchUnavailableReasonV1::Cancelled
                } else if matches!(&error, CodeIndexSchedulerErrorV1::SnapshotMemoryAdmission(_)) {
                    CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
                } else {
                    tracing::warn!(error = %error, path = %git_path.as_bstr(), "git_tree_capture_failed");
                    CodeIndexSearchUnavailableReasonV1::Internal
                }
            })?;
            roster.push(outcome);
            Ok(())
        })?;
        let CapturedFileRosterV1 {
            files,
            captured_files,
            sanitization_receipts,
            retained_bytes,
            retained_reservations,
            omitted_sources,
        } = roster
            .finish()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        let mut changed_paths = BTreeSet::new();
        if let Some(active) = self
            .publication
            .load_active_manifest()
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
        {
            let active_files = active
                .metadata
                .snapshot()
                .files
                .iter()
                .map(|file| (file.logical_path.as_str(), file))
                .collect::<BTreeMap<_, _>>();
            for file in &files {
                match active_files.get(file.logical_path.as_str()) {
                    Some(active_file) if **active_file == *file => {}
                    _ => {
                        changed_paths.insert(file.logical_path.clone());
                    }
                }
            }
            let captured_paths = files
                .iter()
                .map(|file| file.logical_path.as_str())
                .collect::<BTreeSet<_>>();
            changed_paths.extend(
                active_files
                    .keys()
                    .filter(|logical_path| !captured_paths.contains(**logical_path))
                    .map(|logical_path| (*logical_path).to_owned()),
            );
        } else {
            changed_paths.extend(files.iter().map(|file| file.logical_path.clone()));
        }
        let sanitization_receipts = sanitization_receipts.into_iter().collect::<Vec<_>>();
        let content_identity =
            snapshot_content_identity(&files, &sanitization_receipts, &omitted_sources);
        Ok(CapturedSnapshotV1 {
            repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
                tree: Some(tree),
                dirty: tracedecay_domain::RepositoryDirtyStateV1::Clean,
            },
            snapshot: SanitizedCodeSnapshotV1 {
                repository: self.repository_id.clone(),
                worktree: Some(self.worktree_id.clone()),
                reference: Some(reference),
                source_revision,
                sanitizer_revision: id::<SanitizerRevision>(CODE_SOURCE_SANITIZER_VERSION_V1)
                    .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?,
                sanitization_receipts,
                content_identity,
                captured_at: now_micros(),
                files,
                omitted_sources,
            },
            captured_files,
            changed_paths,
            retained_bytes,
            retained_reservations,
        })
    }

    pub fn publish_exact_git_tree_generation(
        &mut self,
        source: &ExactGitTreeSourceV1,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<LatestCompleteCodeIndexV1, CodeIndexSearchUnavailableReasonV1> {
        let _workers = self
            .ensure_worker_plan()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
        let _worker_memory = self.reserve_worker_memory().map_err(|error| match error {
            CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(_) => {
                CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
            }
            _ => CodeIndexSearchUnavailableReasonV1::Internal,
        })?;
        let captured = self.capture_exact_git_tree_snapshot(source, control)?;
        self.publish_captured_git_tree_generation(captured, control, false)
    }

    fn publish_captured_git_tree_generation(
        &mut self,
        captured: CapturedSnapshotV1,
        control: &branch_generations::BranchGenerationReadControlV1,
        require_retained_history: bool,
    ) -> Result<LatestCompleteCodeIndexV1, CodeIndexSearchUnavailableReasonV1> {
        let CapturedSnapshotV1 {
            snapshot,
            repository_parse_identity,
            captured_files,
            changed_paths,
            retained_bytes: _retained_bytes,
            retained_reservations: _retained_reservations,
        } = captured;
        let requested_scope = CodeIndexGenerationScopeV1::for_snapshot(&snapshot);
        // Retained-history generations live inside the active publication
        // pointer, so they need an active generation to ride on. A store with
        // no publication at all has no such anchor, there the mint itself
        // establishes the pointer, and every later exact mint (including the
        // second half of a both-sides miss in one call) rides it as history.
        let publication = match self
            .publication
            .load_active_manifest()
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
        {
            Some(active) if active.metadata.sealed_scope() == requested_scope => {
                self.publication.retained_history()
            }
            Some(_) => {
                // A retained generation for another ref/worktree cannot adopt
                // the active generation as its incremental parent. Preserve
                // that active pointer under the existing exact CAS authority,
                // but make the production owner build the requested scope from
                // its immutable Git tree instead of reporting the foreign
                // active slot as corruption.
                let pointer = self
                    .publication
                    .read_publication_pointer()
                    .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
                    .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
                self.publication
                    .for_undecoded_active_rebuild(&pointer)
                    .retained_history()
            }
            None if require_retained_history => {
                return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
            }
            None => self.publication.clone(),
        };
        let mut owner = open_production_code_index_owner_v1(
            self.production_config.clone(),
            publication,
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
        let generation = self
            .decoded_publication(&generation)
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?;
        Ok(self.bind_latest_complete(generation, None))
    }

    fn existing_exact_git_tree_generation(
        &self,
        source: &ExactGitTreeSourceV1,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSearchUnavailableReasonV1> {
        control.termination().map_or(Ok(()), Err)?;
        let Some(pointer) = self
            .publication
            .read_publication_pointer()
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
        else {
            return Ok(None);
        };
        let Some(entry) = pointer.generation_index.iter().find(|entry| {
            entry.source_reference.as_deref() == Some(source.reference.as_str())
                && entry.source_revision.as_deref() == Some(source.revision.as_str())
                && entry.source_tree.as_deref() == Some(source.tree.as_str())
        }) else {
            return Ok(None);
        };
        self.publication
            .validate_exact_git_evidence(source.revision.as_str(), source.tree.as_str())
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?;
        let generation_id = tracedecay_domain::CodeGenerationId::new(entry.generation_id.clone())
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
        let Some(generation) = self
            .publication
            .load_generation(&generation_id)
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
        else {
            return Ok(None);
        };
        if !generation
            .compatibility_with(&self.production_config)
            .is_reusable()
            || !self.roster_follows_path_policy(&generation)
        {
            return Ok(None);
        }
        if generation.snapshot().repository != self.repository_id
            || generation.snapshot().worktree.as_ref() != Some(&self.worktree_id)
            || generation.snapshot().reference.as_ref() != Some(&source.reference)
            || generation.snapshot().source_revision.as_ref() != Some(&source.revision)
            || generation.repository_parse_identity().tree.as_ref() != Some(&source.tree)
            || generation.snapshot().content_identity.as_str()
                != entry.snapshot_content_identity.as_str()
        {
            return Err(CodeIndexSearchUnavailableReasonV1::Internal);
        }
        Ok(Some(self.bind_latest_complete(generation, None)))
    }

    fn exact_git_tree_generation(
        &mut self,
        source: &ExactGitTreeSourceV1,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<LatestCompleteCodeIndexV1, CodeIndexSearchUnavailableReasonV1> {
        control.termination().map_or(Ok(()), Err)?;
        if let Some(generation) = self.existing_exact_git_tree_generation(source, control)? {
            return Ok(generation);
        }
        self.publish_exact_git_tree_generation(source, control)
    }

    /// Seal one native candidate and its three committed comparison sources
    /// under the existing publication authority without changing its active
    /// generation.
    pub fn publish_native_candidate_generations(
        &mut self,
        sources: &NativeCandidateGenerationSourcesV1,
        candidate: &tracedecay_runtime_core::git_repository::GitNativeCandidateTreeV1<'_>,
        control: &branch_generations::BranchGenerationReadControlV1,
    ) -> Result<NativeCandidateGenerationBindingsV1, CodeIndexSearchUnavailableReasonV1> {
        let active_before = self
            .publication
            .read_publication_pointer()
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
            .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?
            .generation_id;
        let merge_base = self.exact_git_tree_generation(&sources.merge_base, control)?;
        let source = self.exact_git_tree_generation(&sources.source, control)?;
        let destination = self.exact_git_tree_generation(&sources.destination, control)?;
        let _workers = self
            .ensure_worker_plan()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
        let _worker_memory = self.reserve_worker_memory().map_err(|error| match error {
            CodeIndexSchedulerErrorV1::WorkerMemoryAdmission(_) => {
                CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
            }
            _ => CodeIndexSearchUnavailableReasonV1::Internal,
        })?;
        let captured = self.capture_native_candidate_tree_snapshot(
            sources.candidate_reference.clone(),
            &sources.candidate_tree,
            candidate,
            control,
        )?;
        let candidate = self.publish_captured_git_tree_generation(captured, control, true)?;
        let active_after = self
            .publication
            .read_publication_pointer()
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
            .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?
            .generation_id;
        if active_after != active_before {
            return Err(CodeIndexSearchUnavailableReasonV1::Internal);
        }
        Ok(NativeCandidateGenerationBindingsV1 {
            merge_base,
            source,
            destination,
            candidate,
        })
    }

    /// Reopen only the exact sealed candidate named by a durable preview.
    /// Any missing or mismatched witness is an ordinary stale preview (`None`).
    pub fn load_native_candidate_generation(
        &self,
        expected: &NativeCandidateGenerationIdentityV1,
    ) -> Result<Option<LatestCompleteCodeIndexV1>, CodeIndexSearchUnavailableReasonV1> {
        let Some(generation) = self
            .publication
            .load_generation(&expected.generation_id)
            .map_err(DaemonCodeIndexPublicationStoreV1::exact_read_error)?
        else {
            return Ok(None);
        };
        let snapshot = generation.snapshot();
        let manifest = generation.manifest();
        let matches = generation
            .compatibility_with(&self.production_config)
            .is_reusable()
            && manifest.project_id == expected.project_id
            && manifest.generation_id == expected.generation_id
            && snapshot.repository == expected.repository_id
            && snapshot.worktree == expected.worktree_id
            && snapshot.reference == expected.reference
            && manifest.snapshot_digest == expected.snapshot_digest
            && snapshot.content_identity == expected.content_identity
            && snapshot
                .source_revision
                .as_ref()
                .map(tracedecay_domain::CommitId::as_str)
                == expected
                    .source_revision
                    .as_ref()
                    .map(tracedecay_domain::GitOidV1::as_str)
            && generation
                .repository_parse_identity()
                .tree
                .as_ref()
                .map(tracedecay_domain::TreeId::as_str)
                == Some(expected.source_tree.as_str())
            && manifest.seal.expected_digest == expected.seal_digest;
        if !matches {
            return Ok(None);
        }
        Ok(Some(self.bind_latest_complete(generation, None)))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    use tempfile::TempDir;
    use tracedecay_code_index::production::CodeIndexIgnoredSourceAdmissionV1;
    use tracedecay_domain::ProjectId;
    use tracedecay_runtime_core::cancellation::CancellationToken;
    use tracedecay_runtime_core::git_repository::{
        GitNativeIntegrationMode, GitRepositoryAuthority,
    };

    use super::{
        CapturedFileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexSearchUnavailableReasonV1,
        CodeIndexWorktreeSchedulerV1, ExactGitTreeSourceV1, NativeCandidateGenerationIdentityV1,
        NativeCandidateGenerationSourcesV1, branch_generations,
    };
    use crate::code_index_scheduler::SharedCodeIndexBytePoolV1;

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
        .current_dir(root)
        .args(arguments)
        .status()
        .expect("run git fixture command");
        assert!(
            status.success(),
            "git fixture command failed: {arguments:?}"
        );
    }

    fn git_output(root: &Path, arguments: &[&str]) -> String {
        let output = Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output")
            .trim()
            .to_owned()
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
            .filter(|file| {
                file.disposition == tracedecay_domain::SnapshotFileDispositionV1::Present
            })
            .map(|file| file.logical_path)
            .collect()
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

    #[test]
    fn a_privacy_refusal_withholds_only_its_own_path() {
        let (_project, _store, scheduler) = generated_source_fixture();
        let outcome = scheduler
            .capture_candidate_bytes(
                &super::StaticLanguageRegistry::new(),
                b"malformed.json",
                b"{broken",
                false,
            )
            .expect("privacy refusal is a file outcome");
        let CapturedFileOutcomeV1::Withheld { file, .. } = outcome else {
            panic!("malformed structured data must be withheld");
        };
        assert_eq!(file.logical_path, "malformed.json");
        assert_eq!(
            file.disposition,
            tracedecay_domain::SnapshotFileDispositionV1::Ignored
        );
        assert_eq!(file.content_digest, super::content_digest(b"{broken"));
    }

    /// A revision the branch has already moved past is still an immutable
    /// commit, and capturing it is the only way a base whose ref advanced
    /// mid-request, or a merge-base, or a deliberately pinned commit, ever
    /// gets indexed. The capture used to peel the reference and refuse every
    /// revision that was not its current tip, so all of those were permanently
    /// uncapturable while the reference itself carried no information the
    /// commit id did not already give.
    #[test]
    fn a_commit_the_reference_has_moved_past_is_still_captured() {
        let project = TempDir::new().expect("project root");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn superseded_tip_value() -> usize { 1 }\n",
        )
        .expect("superseded source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "superseded"]);
        let superseded_revision = git_output(project.path(), &["rev-parse", "HEAD"]);
        let superseded_tree = git_output(project.path(), &["rev-parse", "HEAD^{tree}"]);
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn current_tip_value() -> usize { 2 }\n",
        )
        .expect("current source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "current"]);
        assert_ne!(
            git_output(project.path(), &["rev-parse", "HEAD"]),
            superseded_revision,
            "the fixture must request a commit the reference no longer points at"
        );

        let store = TempDir::new().expect("code-index store");
        let scheduler = CodeIndexWorktreeSchedulerV1::open(
            ProjectId::new("project.superseded-commit-capture").expect("project id"),
            project.path(),
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open code-index scheduler");
        let control = branch_generations::BranchGenerationReadControlV1 {
            deadline: None,
            cancellation: None,
        };
        let source = ExactGitTreeSourceV1 {
            reference: tracedecay_domain::RefId::new("refs/heads/main").expect("reference"),
            revision: tracedecay_domain::CommitId::new(superseded_revision.clone())
                .expect("revision"),
            tree: tracedecay_domain::TreeId::new(superseded_tree.clone()).expect("tree"),
        };

        let captured = scheduler
            .capture_exact_git_tree_snapshot(&source, &control)
            .expect("capture the superseded commit's own tree");

        assert_eq!(
            captured
                .snapshot
                .source_revision
                .as_ref()
                .map(tracedecay_domain::CommitId::as_str),
            Some(superseded_revision.as_str())
        );
        assert_eq!(
            captured
                .repository_parse_identity
                .tree
                .as_ref()
                .map(tracedecay_domain::TreeId::as_str),
            Some(superseded_tree.as_str())
        );
        let bytes = captured
            .captured_files
            .iter()
            .map(|file| String::from_utf8_lossy(&file.sanitized_bytes).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            bytes.contains("superseded_tip_value"),
            "the captured bytes must be the requested commit's tree"
        );
        assert!(
            !bytes.contains("current_tip_value"),
            "the capture must never fall back to the reference's current tip"
        );
    }

    #[test]
    fn repeated_exact_capture_has_no_omitted_row_delta() {
        let (project, _store, mut scheduler) = generated_source_fixture();
        scheduler.reconcile_now().expect("publish immutable tree");
        let source = ExactGitTreeSourceV1 {
            reference: tracedecay_domain::RefId::new("refs/heads/main").expect("reference"),
            revision: tracedecay_domain::CommitId::new(git_output(
                project.path(),
                &["rev-parse", "HEAD"],
            ))
            .expect("revision"),
            tree: tracedecay_domain::TreeId::new(git_output(
                project.path(),
                &["rev-parse", "HEAD^{tree}"],
            ))
            .expect("tree"),
        };
        let captured = scheduler
            .capture_exact_git_tree_snapshot(
                &source,
                &branch_generations::BranchGenerationReadControlV1 {
                    deadline: None,
                    cancellation: None,
                },
            )
            .expect("recapture immutable tree");
        assert!(captured.changed_paths.is_empty());
        assert_eq!(
            captured.snapshot.files,
            scheduler
                .latest_complete()
                .expect("published")
                .generation()
                .snapshot()
                .files
        );
    }

    /// Two non-UTF-8 names that decode lossily to the same string, and a
    /// backslash name, are committed beside an ordinary source. None of them
    /// becomes a row under a name that does not exist; each is an omitted
    /// source under its exact Git bytes.
    #[cfg(unix)]
    #[test]
    fn exact_capture_names_unrepresentable_committed_paths_by_their_bytes() {
        let project = TempDir::new().expect("project root");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::write(project.path().join("src/lib.rs"), "pub fn kept() {}\n")
            .expect("ordinary source");
        for raw in [
            b"src/\xfe.rs".as_slice(),
            b"src/\xff.rs".as_slice(),
            b"src/odd\\name.rs".as_slice(),
        ] {
            std::fs::write(
                project.path().join(std::ffi::OsStr::from_bytes(raw)),
                "pub fn unrepresentable() {}\n",
            )
            .expect("unrepresentable source name");
        }
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "fixture"]);
        let store = TempDir::new().expect("code-index store");
        let scheduler = CodeIndexWorktreeSchedulerV1::open(
            ProjectId::new("project.unrepresentable-exact-capture").expect("project id"),
            project.path(),
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open code-index scheduler");
        let source = ExactGitTreeSourceV1 {
            reference: tracedecay_domain::RefId::new("refs/heads/main").expect("reference"),
            revision: tracedecay_domain::CommitId::new(git_output(
                project.path(),
                &["rev-parse", "HEAD"],
            ))
            .expect("revision"),
            tree: tracedecay_domain::TreeId::new(git_output(
                project.path(),
                &["rev-parse", "HEAD^{tree}"],
            ))
            .expect("tree"),
        };

        let captured = scheduler
            .capture_exact_git_tree_snapshot(
                &source,
                &branch_generations::BranchGenerationReadControlV1 {
                    deadline: None,
                    cancellation: None,
                },
            )
            .expect("capture a tree with unrepresentable paths");

        captured.snapshot.validate().expect("canonical snapshot");
        assert_eq!(
            captured
                .snapshot
                .files
                .iter()
                .map(|file| file.logical_path.as_str())
                .collect::<Vec<_>>(),
            ["src/lib.rs"]
        );
        assert_eq!(
            captured.snapshot.omitted_sources,
            [
                b"src/odd\\name.rs".as_slice(),
                b"src/\xfe.rs".as_slice(),
                b"src/\xff.rs".as_slice(),
            ]
            .map(|git_path| tracedecay_domain::OmittedCodeSourceV1 {
                git_path: git_path.to_vec(),
                reason: tracedecay_domain::CodeSourceOmissionReasonV1::UnrepresentablePath,
            })
        );
    }

    #[test]
    fn exact_git_capture_reuses_one_byte_owner_for_snapshot_and_production() {
        let (project, _store, scheduler) = generated_source_fixture();
        let revision = git_output(project.path(), &["rev-parse", "HEAD"]);
        let tree = git_output(project.path(), &["rev-parse", "HEAD^{tree}"]);

        let captured = scheduler
            .capture_exact_git_tree_snapshot(
                &ExactGitTreeSourceV1 {
                    reference: tracedecay_domain::RefId::new("refs/heads/main").expect("reference"),
                    revision: tracedecay_domain::CommitId::new(revision).expect("revision"),
                    tree: tracedecay_domain::TreeId::new(tree).expect("tree"),
                },
                &branch_generations::BranchGenerationReadControlV1 {
                    deadline: None,
                    cancellation: None,
                },
            )
            .expect("capture exact Git tree");

        assert_eq!(captured.captured_files.len(), captured.retained_bytes.len());
        assert!(
            captured.captured_files.iter().all(|captured_file| captured
                .retained_bytes
                .iter()
                .any(|retained| Arc::ptr_eq(&captured_file.sanitized_bytes, retained))),
            "production input must retain the snapshot's canonical byte allocation"
        );
    }

    #[test]
    fn synthetic_merge_candidate_seals_graph_as_retained_history() {
        let project = TempDir::new().expect("project root");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        // Native integration refuses repositories that require signed commits.
        // Pin the fixture's own write configuration so eligibility does not
        // inherit the host's global `commit.gpgsign` / `merge.gpgsign`.
        git(project.path(), &["config", "commit.gpgsign", "false"]);
        git(project.path(), &["config", "merge.gpgsign", "false"]);
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::write(
            project.path().join("src/base.rs"),
            "pub fn shared_base() -> usize { 1 }\n",
        )
        .expect("base source");
        std::fs::write(
            project.path().join("malformed.json"),
            "{ definitely not json",
        )
        .expect("malformed structured source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "base"]);
        git(project.path(), &["branch", "feature"]);
        std::fs::write(
            project.path().join("src/main.rs"),
            "pub fn main_only() -> usize { shared_base() }\n",
        )
        .expect("destination source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "main"]);
        let destination = git_output(project.path(), &["rev-parse", "HEAD"]);
        git(project.path(), &["switch", "-q", "feature"]);
        std::fs::write(
            project.path().join("src/feature.rs"),
            "pub fn feature_only() -> usize { shared_base() }\n",
        )
        .expect("source source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "feature"]);
        let source = git_output(project.path(), &["rev-parse", "HEAD"]);

        let project_id = ProjectId::new("project.native-candidate").expect("project id");
        let store = TempDir::new().expect("code-index store");
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            project.path(),
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open scheduler");
        scheduler.reconcile_now().expect("seed active generation");
        let active_before = scheduler
            .latest_complete()
            .expect("active generation")
            .generation()
            .manifest()
            .generation_id
            .clone();
        let authority = GitRepositoryAuthority::discover(project.path()).expect("repository");
        let control = branch_generations::BranchGenerationReadControlV1 {
            deadline: None,
            cancellation: None,
        };
        let source_oid = tracedecay_domain::GitOidV1::new(source).expect("source oid");
        let destination_oid =
            tracedecay_domain::GitOidV1::new(destination).expect("destination oid");
        let (preflight, bindings) = authority
            .preflight_native_integration_with_candidate(
                "refs/heads/feature",
                "refs/heads/main",
                &source_oid,
                &destination_oid,
                GitNativeIntegrationMode::TwoParentMerge,
                &CancellationToken::new(),
                |preflight, candidate| {
                    let exact =
                        |reference: &str,
                         revision: &tracedecay_domain::GitOidV1,
                         tree: &tracedecay_domain::GitOidV1| {
                            ExactGitTreeSourceV1 {
                                reference: tracedecay_domain::RefId::new(reference)
                                    .expect("reference"),
                                revision: tracedecay_domain::CommitId::new(
                                    revision.as_str().to_owned(),
                                )
                                .expect("revision"),
                                tree: tracedecay_domain::TreeId::new(tree.as_str().to_owned())
                                    .expect("tree"),
                            }
                        };
                    scheduler.publish_native_candidate_generations(
                        &NativeCandidateGenerationSourcesV1 {
                            merge_base: exact(
                                "refs/heads/main",
                                &preflight.merge_base,
                                &preflight.merge_base_tree,
                            ),
                            source: exact(
                                "refs/heads/feature",
                                &preflight.source_tip,
                                &preflight.source_tree,
                            ),
                            destination: exact(
                                "refs/heads/main",
                                &preflight.destination_tip,
                                &preflight.destination_tree,
                            ),
                            candidate_reference: tracedecay_domain::RefId::new("refs/heads/main")
                                .expect("candidate reference"),
                            candidate_tree: tracedecay_domain::TreeId::new(
                                preflight
                                    .candidate_tree
                                    .as_ref()
                                    .expect("candidate tree")
                                    .as_str()
                                    .to_owned(),
                            )
                            .expect("candidate tree"),
                        },
                        candidate,
                        &control,
                    )
                },
            )
            .expect("native preflight");
        let bindings = bindings.unwrap_or_else(|| {
            panic!(
                "eligible candidate bindings: preflight disposition was {:?}",
                preflight.disposition
            )
        });

        let candidate = bindings.candidate.generation();
        assert_eq!(
            bindings.source.generation().manifest().generation_id,
            active_before,
            "an already sealed exact source generation is reused"
        );
        assert!(candidate.snapshot().source_revision.is_none());
        assert_eq!(
            candidate
                .repository_parse_identity()
                .tree
                .as_ref()
                .map(tracedecay_domain::TreeId::as_str),
            preflight
                .candidate_tree
                .as_ref()
                .map(tracedecay_domain::GitOidV1::as_str)
        );
        let names = candidate
            .symbols()
            .symbols
            .iter()
            .map(|symbol| symbol.simple_name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(names.contains("main_only") && names.contains("feature_only"));
        assert!(
            candidate.snapshot().files.iter().any(|file| {
                file.logical_path == "malformed.json"
                    && file.disposition != tracedecay_domain::SnapshotFileDispositionV1::Present
            }),
            "privacy-refused candidate files remain explicitly withheld"
        );
        assert_eq!(
            scheduler
                .latest_complete()
                .expect("active generation remains")
                .generation()
                .manifest()
                .generation_id,
            active_before
        );

        let snapshot = candidate.snapshot();
        let identity = NativeCandidateGenerationIdentityV1 {
            generation_id: candidate.manifest().generation_id.clone(),
            project_id: project_id.clone(),
            repository_id: snapshot.repository.clone(),
            worktree_id: snapshot.worktree.clone(),
            reference: snapshot.reference.clone(),
            snapshot_digest: candidate.manifest().snapshot_digest.clone(),
            content_identity: snapshot.content_identity.clone(),
            source_revision: None,
            source_tree: tracedecay_domain::GitOidV1::new(
                candidate
                    .repository_parse_identity()
                    .tree
                    .as_ref()
                    .expect("tree")
                    .as_str()
                    .to_owned(),
            )
            .expect("tree oid"),
            seal_digest: candidate.manifest().seal.expected_digest.clone(),
        };
        let stale_content_identity = bindings
            .source
            .generation()
            .snapshot()
            .content_identity
            .clone();
        drop(bindings);
        drop(scheduler);
        let reopened = CodeIndexWorktreeSchedulerV1::open(
            project_id,
            project.path(),
            store.path().to_path_buf(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("reopen scheduler");
        assert!(
            reopened
                .load_native_candidate_generation(&identity)
                .expect("load retained candidate after restart")
                .is_some()
        );
        let mut stale = identity;
        stale.content_identity = stale_content_identity;
        assert!(
            reopened
                .load_native_candidate_generation(&stale)
                .expect("stale binding")
                .is_none()
        );
    }

    /// The reference is provenance, and provenance a repository cannot vouch
    /// for is refused: a capture may resolve any commit in the object database,
    /// but never stamp it with a branch name that does not exist.
    #[test]
    fn a_capture_still_refuses_a_reference_the_repository_does_not_have() {
        let (project, _store, scheduler) = generated_source_fixture();
        let revision = git_output(project.path(), &["rev-parse", "HEAD"]);
        let tree = git_output(project.path(), &["rev-parse", "HEAD^{tree}"]);

        let captured = scheduler.capture_exact_git_tree_snapshot(
            &ExactGitTreeSourceV1 {
                reference: tracedecay_domain::RefId::new("refs/heads/not-main")
                    .expect("absent reference"),
                revision: tracedecay_domain::CommitId::new(revision).expect("revision"),
                tree: tracedecay_domain::TreeId::new(tree).expect("tree"),
            },
            &branch_generations::BranchGenerationReadControlV1 {
                deadline: None,
                cancellation: None,
            },
        );

        assert!(
            matches!(
                captured,
                Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
            ),
            "a reference this repository does not have must not be captured"
        );
    }

    #[test]
    fn wholly_withheld_roster_refuses_both_capture_paths() {
        let (project, _store, scheduler) = generated_source_fixture();
        std::fs::remove_file(project.path().join("src/lib.rs")).expect("remove indexable source");
        std::fs::write(project.path().join("malformed.json"), "{broken").expect("withheld source");
        git(project.path(), &["add", "."]);
        git(
            project.path(),
            &["commit", "-qm", "only withheld and generated sources"],
        );
        let source = ExactGitTreeSourceV1 {
            reference: tracedecay_domain::RefId::new("refs/heads/main").expect("reference"),
            revision: tracedecay_domain::CommitId::new(git_output(
                project.path(),
                &["rev-parse", "HEAD"],
            ))
            .expect("revision"),
            tree: tracedecay_domain::TreeId::new(git_output(
                project.path(),
                &["rev-parse", "HEAD^{tree}"],
            ))
            .expect("tree"),
        };
        assert!(matches!(
            scheduler.capture_exact_git_tree_snapshot(
                &source,
                &branch_generations::BranchGenerationReadControlV1 {
                    deadline: None,
                    cancellation: None
                }
            ),
            Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
        ));
        assert!(matches!(
            scheduler.capture_authoritative_snapshot_without_active_generation_reuse(None),
            Err(CodeIndexSchedulerErrorV1::Privacy(_))
        ));
    }

    #[test]
    fn cancellation_is_not_a_withheld_file() {
        let (_project, _store, scheduler) = generated_source_fixture();
        scheduler
            .shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            scheduler.capture_candidate_bytes(
                &super::StaticLanguageRegistry::new(),
                b"src/lib.rs",
                b"pub fn kept() {}",
                false,
            ),
            Err(CodeIndexSchedulerErrorV1::Production(
                super::CodeIndexProductionErrorV1::Interrupted(_)
            ))
        ));
    }
}

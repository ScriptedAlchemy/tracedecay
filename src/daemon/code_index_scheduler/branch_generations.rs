//! Exact Git-revision reads over immutable sealed code-index generations.

use std::sync::{Arc, TryLockError};

use tracedecay_domain::{GitOidV1, RefId};
use tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1;

use super::{
    CodeIndexPublishedGenerationV1, CodeIndexSchedulerRegistryV1,
    DaemonCodeIndexPublicationStoreV1, LatestCompleteCodeIndexV1,
};

#[derive(Clone)]
pub(in crate::daemon) struct BranchGenerationReadControlV1 {
    pub deadline: Option<tracedecay_application::Deadline>,
    pub cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl BranchGenerationReadControlV1 {
    pub(in crate::daemon) fn termination(&self) -> Option<CodeIndexSearchUnavailableReasonV1> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
        {
            return Some(CodeIndexSearchUnavailableReasonV1::Cancelled);
        }
        self.deadline
            .as_ref()
            .is_some_and(|deadline| {
                deadline.is_elapsed_at(tracedecay_application::clock::now_micros())
            })
            .then_some(CodeIndexSearchUnavailableReasonV1::TimedOut)
    }
}

pub(in crate::daemon) struct BranchGenerationPairV1 {
    pub(in crate::daemon) base: LatestCompleteCodeIndexV1,
    pub(in crate::daemon) head: LatestCompleteCodeIndexV1,
}

/// Which half of a requested exact revision pair the durable index could not
/// serve. Carrying both sides separately is the whole point: the mint path used
/// to rebuild *base* whenever anything was missing, so a request whose base was
/// already sealed and whose head was not paid for a redundant full-tree capture
/// — and failed outright whenever that redundant base capture could not be
/// performed.
#[derive(Clone, Copy, Debug)]
struct ExactGenerationMissV1 {
    base: bool,
    head: bool,
}

/// Outcome of resolving one exact revision pair against the durable generation
/// index. A miss is deliberately *not* an error: sealing a generation from an
/// immutable commit tree is idempotent, so every shape of absence — never
/// indexed, pruned out of the bounded index, or named by an entry whose sealed
/// bytes are gone — is answered the same way, by minting it.
enum ExactGenerationPairV1 {
    Sealed(
        Arc<CodeIndexPublishedGenerationV1>,
        Arc<CodeIndexPublishedGenerationV1>,
    ),
    Missing(ExactGenerationMissV1),
}

impl DaemonCodeIndexPublicationStoreV1 {
    pub(super) fn exact_read_error(
        error: crate::code_index::production::CodeIndexPublicationStoreErrorV1,
    ) -> CodeIndexSearchUnavailableReasonV1 {
        match error {
            crate::code_index::production::CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(
                _,
            ) => CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired,
            _ => CodeIndexSearchUnavailableReasonV1::Internal,
        }
    }

    fn revisions(
        &self,
        base_reference: &RefId,
        base_revision: &GitOidV1,
        base_tree: &GitOidV1,
        head_reference: &RefId,
        head_revision: &GitOidV1,
        head_tree: &GitOidV1,
        control: &BranchGenerationReadControlV1,
    ) -> Result<ExactGenerationPairV1, CodeIndexSearchUnavailableReasonV1> {
        if let Some(reason) = control.termination() {
            return Err(reason);
        }
        let Some(pointer) = self
            .read_publication_pointer()
            .map_err(Self::exact_read_error)?
        else {
            // No publication at all is the emptiest possible index miss, and an
            // empty index is exactly as mint-eligible as a pruned one.
            return Ok(ExactGenerationPairV1::Missing(ExactGenerationMissV1 {
                base: true,
                head: true,
            }));
        };
        // `generation_index_truncated` says only that the absence of an entry no
        // longer proves the revision was never indexed. It used to be reported
        // here as `CapacityUnavailable`, which callers read as a transient
        // "retry later" — but the flag latches on the first eviction and never
        // clears, so once the bounded index had evicted anything, every
        // unindexed revision became permanently, and falsely, transient. The
        // truthful answer is the same one an untruncated miss gets: we do not
        // hold this generation, so mint it from the commit tree.
        let find = |reference: &RefId, revision: &GitOidV1, tree: &GitOidV1| {
            pointer
                .generation_index
                .iter()
                .find(|entry| {
                    entry.source_reference.as_deref() == Some(reference.as_str())
                        && entry.source_revision.as_deref() == Some(revision.as_str())
                        && entry.source_tree.as_deref() == Some(tree.as_str())
                })
                .cloned()
        };
        let same_revision = base_reference == head_reference
            && base_revision == head_revision
            && base_tree == head_tree;
        let base_entry = find(base_reference, base_revision, base_tree);
        let head_entry = if same_revision {
            base_entry.clone()
        } else {
            find(head_reference, head_revision, head_tree)
        };
        if let Some(reason) = control.termination() {
            return Err(reason);
        }
        let load =
            |entry: &crate::retention::code_index_generations::DurableGenerationIndexEntryV1,
             revision: &GitOidV1,
             tree: &GitOidV1,
             reference: &RefId| {
                self.validate_exact_git_evidence(revision.as_str(), tree.as_str())
                    .map_err(Self::exact_read_error)?;
                let generation_id =
                    tracedecay_domain::CodeGenerationId::new(entry.generation_id.clone())
                        .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
                // An index entry whose sealed bytes are gone is an absence, not
                // a corruption: it is reported as a miss so the caller mints the
                // generation back rather than failing a read the object database
                // can still answer.
                let Some(generation) = self
                    .load_generation(&generation_id)
                    .map_err(Self::exact_read_error)?
                else {
                    return Ok(None);
                };
                if generation
                    .snapshot()
                    .source_revision
                    .as_ref()
                    .map(tracedecay_domain::CommitId::as_str)
                    != Some(revision.as_str())
                    || generation
                        .snapshot()
                        .reference
                        .as_ref()
                        .map(tracedecay_domain::RefId::as_str)
                        != Some(reference.as_str())
                    || generation.snapshot().content_identity.as_str()
                        != entry.snapshot_content_identity
                {
                    return Err(CodeIndexSearchUnavailableReasonV1::Internal);
                }
                Ok(Some(generation))
            };
        let base = match base_entry.as_ref() {
            Some(entry) => load(entry, base_revision, base_tree, base_reference)?,
            None => None,
        };
        let head = if same_revision {
            base.clone()
        } else {
            match head_entry.as_ref() {
                Some(entry) => load(entry, head_revision, head_tree, head_reference)?,
                None => None,
            }
        };
        match (base, head) {
            (Some(base), Some(head)) => Ok(ExactGenerationPairV1::Sealed(base, head)),
            (base, head) => Ok(ExactGenerationPairV1::Missing(ExactGenerationMissV1 {
                base: base.is_none(),
                head: head.is_none(),
            })),
        }
    }
}

impl CodeIndexSchedulerRegistryV1 {
    pub(in crate::daemon) async fn generations_for_revisions(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        base_reference: &RefId,
        base_revision: &GitOidV1,
        base_tree: &GitOidV1,
        head_reference: &RefId,
        head_revision: &GitOidV1,
        head_tree: &GitOidV1,
        control: BranchGenerationReadControlV1,
    ) -> Result<BranchGenerationPairV1, CodeIndexSearchUnavailableReasonV1> {
        let scheduler = {
            let mounted = self.mounted.lock().await;
            let mut matched = None;
            for worktree in mounted.values() {
                if worktree.repository_id == scope.repository_id
                    && worktree.worktree_id == scope.worktree_id
                {
                    if matched.is_some() {
                        return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
                    }
                    matched = Some(Arc::clone(&worktree.scheduler));
                }
            }
            matched.ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?
        };
        let base_reference = base_reference.clone();
        let base_revision = base_revision.clone();
        let base_tree = base_tree.clone();
        let head_reference = head_reference.clone();
        let head_revision = head_revision.clone();
        let head_tree = head_tree.clone();
        let scope = scope.clone();
        let terminal_control = control.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(TryLockError::WouldBlock) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable);
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::Internal);
                }
            };
            let exact_source = |reference: &RefId, revision: &GitOidV1, tree: &GitOidV1| {
                Ok::<_, CodeIndexSearchUnavailableReasonV1>(
                    super::git_tree_capture::ExactGitTreeSourceV1 {
                        reference: reference.clone(),
                        revision: tracedecay_domain::CommitId::new(revision.as_str().to_owned())
                            .map_err(|_| CodeIndexSearchUnavailableReasonV1::InvalidRequest)?,
                        tree: tracedecay_domain::TreeId::new(tree.as_str().to_owned())
                            .map_err(|_| CodeIndexSearchUnavailableReasonV1::InvalidRequest)?,
                    },
                )
            };
            let same_revision = base_reference == head_reference
                && base_revision == head_revision
                && base_tree == head_tree;
            let revisions = scheduler.publication.revisions(
                &base_reference,
                &base_revision,
                &base_tree,
                &head_reference,
                &head_revision,
                &head_tree,
                &control,
            )?;
            let (base, head) = match revisions {
                ExactGenerationPairV1::Sealed(base, head) => (base, head),
                // Mint only the side the index could not serve. Capturing a
                // whole commit tree is the expensive part of this path, and a
                // base that is already sealed is evidence we already hold — the
                // unconditional base capture that used to run here charged every
                // head-only miss for a second full tree, and turned any base the
                // working repository could no longer capture into a hard failure
                // of a read the index could have answered.
                ExactGenerationPairV1::Missing(missing) => {
                    if missing.base {
                        scheduler.publish_exact_git_tree_generation(
                            &exact_source(&base_reference, &base_revision, &base_tree)?,
                            &control,
                        )?;
                    }
                    if missing.head && !same_revision {
                        scheduler.publish_exact_git_tree_generation(
                            &exact_source(&head_reference, &head_revision, &head_tree)?,
                            &control,
                        )?;
                    }
                    match scheduler.publication.revisions(
                        &base_reference,
                        &base_revision,
                        &base_tree,
                        &head_reference,
                        &head_revision,
                        &head_tree,
                        &control,
                    )? {
                        ExactGenerationPairV1::Sealed(base, head) => (base, head),
                        // A miss that survives its own mint is terminal: the
                        // generation is genuinely unavailable, not pending.
                        ExactGenerationPairV1::Missing(_) => {
                            return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
                        }
                    }
                }
            };
            let base = scheduler.bind_latest_complete(base);
            let head = scheduler.bind_latest_complete(head);
            if !super::registry::latest_matches_scope_identity(&base, &scope)
                || !super::registry::latest_matches_scope_identity(&head, &scope)
            {
                return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
            }
            Ok(BranchGenerationPairV1 { base, head })
        });
        match crate::daemon::park_admission(
            crate::daemon::code_index_task_support::settle_owned_blocking_task(
                task,
                std::time::Duration::from_millis(10),
                || terminal_control.termination(),
            ),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CodeIndexSearchUnavailableReasonV1::Internal),
            Err(reason) => Err(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;
    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::{GitOidV1, ProjectId};
    use tracedecay_query::code_search;

    use super::*;
    use crate::daemon::code_index_branch_diff::{bounded_diff, diff_symbols, generation_symbols};
    use crate::daemon::code_index_scheduler::{
        CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
    };

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output")
            .trim()
            .to_owned()
    }

    #[tokio::test]
    async fn mounted_store_diffs_two_clean_exact_commit_generations() {
        let project = TempDir::new().expect("project");
        let store = TempDir::new().expect("store");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn exact_branch_value() -> usize { 1 }\n",
        )
        .expect("base source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "base"]);
        let base_revision =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD"])).expect("base revision");
        let base_tree =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD^{tree}"])).expect("base tree");
        let project_id = ProjectId::new("project.branch-generation-diff").expect("project id");
        let canonical_project = project.path().canonicalize().expect("canonical project");
        let scoped_store = scoped_code_index_store_root(store.path(), &canonical_project);
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open scheduler");
        scheduler.reconcile_now().expect("publish base generation");

        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn exact_branch_value() -> u64 { 2 }\n",
        )
        .expect("head source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "head"]);
        let head_revision =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD"])).expect("head revision");
        let head_tree =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD^{tree}"])).expect("head tree");
        scheduler.reconcile_now().expect("publish head generation");
        let mut large_source = String::new();
        for index in 0..1_025 {
            use std::fmt::Write as _;
            writeln!(
                large_source,
                "pub fn bounded_generation_{index}() -> usize {{ {index} }}"
            )
            .expect("render large source");
        }
        std::fs::write(project.path().join("src/lib.rs"), large_source)
            .expect("large generation source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "large"]);
        let large_revision =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD"])).expect("large revision");
        let large_tree =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD^{tree}"])).expect("large tree");
        scheduler.reconcile_now().expect("publish large generation");
        drop(scheduler);
        let generations_root = scoped_store.join("code-generations-v1");
        for index in 0..512 {
            std::fs::write(
                generations_root.join(format!("generation-{index:064x}.json")),
                b"decoy generation bytes",
            )
            .expect("write decoy generation");
        }

        let registry = CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(
                project_id.clone(),
                &canonical_project,
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount sealed store");
        let identity = super::super::identity::IndexingIdentityV1::resolve(&canonical_project)
            .expect("indexing identity");
        let reference = identity.head_ref().cloned().expect("head reference");
        let scope = ResolvedScope::new(
            project_id,
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            Some(reference.clone()),
        )
        .expect("resolved scope");
        let control = BranchGenerationReadControlV1 {
            deadline: None,
            cancellation: None,
        };
        let read_timeout = std::time::Duration::from_secs(5);
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let pointer: serde_json::Value = serde_json::from_slice(
                &std::fs::read(scoped_store.join("active-code-generation-v1.json"))
                    .expect("read durable generation index"),
            )
            .expect("decode durable generation index");
            let generation_file = pointer["generation_index"]
                .as_array()
                .expect("generation index")
                .iter()
                .find(|entry| entry["source_revision"].as_str() == Some(base_revision.as_str()))
                .and_then(|entry| entry["generation_file"].as_str())
                .expect("base generation file");
            let target = scoped_store
                .join("code-generations-v1")
                .join(generation_file);
            let backup = target.with_extension("json.symlink-target");
            std::fs::rename(&target, &backup).expect("move exact generation behind symlink");
            symlink(&backup, &target).expect("replace exact generation with symlink");
            assert!(matches!(
                settled_pair(
                    &registry,
                    &scope,
                    (&reference, &base_revision, &base_tree),
                    (&reference, &head_revision, &head_tree),
                    &control,
                    read_timeout,
                )
                .await,
                Err(CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired)
            ));
            std::fs::remove_file(&target).expect("remove exact-generation symlink");
            std::fs::rename(&backup, &target).expect("restore exact generation");
        }
        let pair = settled_pair(
            &registry,
            &scope,
            (&reference, &base_revision, &base_tree),
            (&reference, &head_revision, &head_tree),
            &control,
            read_timeout,
        )
        .await
        .expect("both clean commit generations");
        let base_generation_id = pair.base.generation().manifest().generation_id.clone();
        // A revision paired with a tree that is not its own names no commit in
        // this repository, so neither the index nor a capture can serve it.
        // The mount worker may claim the scheduler between exact reads, so
        // settle only its documented transient lock-contention response.
        let mismatched_tree = settled_pair(
            &registry,
            &scope,
            (&reference, &base_revision, &head_tree),
            (&reference, &base_revision, &head_tree),
            &control,
            read_timeout,
        )
        .await
        .map(|_| "sealed pair");
        assert!(
            matches!(
                mismatched_tree,
                Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
            ),
            "a revision/tree pair Git does not have must be unavailable, got {mismatched_tree:?}"
        );
        let wrong_reference =
            tracedecay_domain::RefId::new("refs/heads/not-main").expect("wrong reference");
        let wrong_reference_result = settled_pair(
            &registry,
            &scope,
            (&wrong_reference, &base_revision, &base_tree),
            (&reference, &head_revision, &head_tree),
            &control,
            read_timeout,
        )
        .await;
        assert!(matches!(
            wrong_reference_result,
            Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
        ));
        let base =
            generation_symbols(pair.base.generation(), None, None, &control).expect("base symbols");
        let head =
            generation_symbols(pair.head.generation(), None, None, &control).expect("head symbols");
        let completed = diff_symbols(
            pair.base.generation().manifest().generation_id.as_str(),
            base,
            pair.head.generation().manifest().generation_id.as_str(),
            head,
        )
        .expect("diff symbols");

        assert!(matches!(
            completed.changes.as_slice(),
            [code_search::CodeIndexBranchChangeV1::Changed { base, head }]
                if head.qualified_name == "src/lib.rs::exact_branch_value"
                    && base.content_digest != head.content_digest
        ));

        let large_pair = settled_pair(
            &registry,
            &scope,
            (&reference, &large_revision, &large_tree),
            (&reference, &large_revision, &large_tree),
            &control,
            read_timeout,
        )
        .await
        .expect("large exact generation");
        let started = std::time::Instant::now();
        let outcome = bounded_diff(
            large_pair.base.generation(),
            large_pair.head.generation(),
            None,
            None,
            &control,
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "oversized generation admission must not materialize symbols"
        );
        assert_eq!(
            outcome,
            Err(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable),
            "a generation beyond the admitted resumable bound is unavailable, not an empty terminal partial"
        );

        let cancellation =
            tracedecay_application::CancellationSignal::active("cancel.large-generation")
                .expect("cancellation");
        cancellation.cancel(tracedecay_application::clock::now_micros());
        assert_eq!(
            bounded_diff(
                large_pair.base.generation(),
                large_pair.head.generation(),
                None,
                None,
                &BranchGenerationReadControlV1 {
                    deadline: None,
                    cancellation: Some(cancellation),
                },
            ),
            Err(CodeIndexSearchUnavailableReasonV1::Cancelled)
        );
        let expired =
            tracedecay_application::Deadline::new(tracedecay_application::clock::now_micros())
                .expect("expired deadline");
        assert_eq!(
            bounded_diff(
                large_pair.base.generation(),
                large_pair.head.generation(),
                None,
                None,
                &BranchGenerationReadControlV1 {
                    deadline: Some(expired),
                    cancellation: None,
                },
            ),
            Err(CodeIndexSearchUnavailableReasonV1::TimedOut)
        );

        let pointer_path = scoped_store.join("active-code-generation-v1.json");
        let mut pointer: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&pointer_path).expect("read durable generation index"),
        )
        .expect("decode durable generation index");
        pointer["generation_index"][0]["source_tree"] =
            serde_json::Value::String(head_tree.as_str().to_owned());
        std::fs::write(
            &pointer_path,
            serde_json::to_vec(&pointer).expect("encode tampered index"),
        )
        .expect("tamper durable generation index");
        assert!(matches!(
            settled_pair(
                &registry,
                &scope,
                (&reference, &base_revision, &base_tree),
                (&reference, &head_revision, &head_tree),
                &control,
                read_timeout,
            )
            .await,
            Err(CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired)
        ));
        assert!(matches!(
            registry.generation_for(&scope, &base_generation_id).await,
            Err(CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired)
        ));
    }

    #[tokio::test]
    async fn non_checked_out_refs_are_indexed_from_immutable_git_trees() {
        let project = TempDir::new().expect("project");
        let store = TempDir::new().expect("store");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn main_tree_value() -> usize { 1 }\n",
        )
        .expect("main source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "main"]);
        let main_revision =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD"])).expect("main revision");
        let main_tree =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD^{tree}"])).expect("main tree");

        git(project.path(), &["checkout", "-qb", "feature"]);
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn feature_tree_value() -> usize { 2 }\n",
        )
        .expect("feature source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "feature"]);
        let feature_revision =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD"])).expect("feature revision");
        let feature_tree = GitOidV1::new(git(project.path(), &["rev-parse", "HEAD^{tree}"]))
            .expect("feature tree");
        git(project.path(), &["checkout", "-q", "main"]);

        let project_id = ProjectId::new("project.non-checked-out-refs").expect("project id");
        let canonical_project = project.path().canonicalize().expect("canonical project");
        let scoped_store = scoped_code_index_store_root(store.path(), &canonical_project);
        let scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open scheduler");
        drop(scheduler);

        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn dirty_worktree_value() -> usize { 3 }\n",
        )
        .expect("dirty worktree source");

        let registry = CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(
                project_id.clone(),
                &canonical_project,
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount sealed store");
        let identity = super::super::identity::IndexingIdentityV1::resolve(&canonical_project)
            .expect("indexing identity");
        let main_reference =
            tracedecay_domain::RefId::new("refs/heads/main").expect("main reference");
        let feature_reference =
            tracedecay_domain::RefId::new("refs/heads/feature").expect("feature reference");
        let scope = ResolvedScope::new(
            project_id,
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            Some(main_reference.clone()),
        )
        .expect("resolved scope");
        let control = BranchGenerationReadControlV1 {
            deadline: None,
            cancellation: None,
        };

        let pair = registry
            .generations_for_revisions(
                &scope,
                &main_reference,
                &main_revision,
                &main_tree,
                &feature_reference,
                &feature_revision,
                &feature_tree,
                control.clone(),
            )
            .await
            .expect("exact generations for both refs");
        assert_eq!(
            pair.base.generation().snapshot().reference.as_ref(),
            Some(&main_reference)
        );
        assert_eq!(
            pair.head.generation().snapshot().reference.as_ref(),
            Some(&feature_reference)
        );
        let main_symbols =
            generation_symbols(pair.base.generation(), None, None, &control).expect("main symbols");
        let feature_symbols = generation_symbols(pair.head.generation(), None, None, &control)
            .expect("feature symbols");
        assert!(
            main_symbols
                .iter()
                .any(|symbol| symbol.name == "main_tree_value")
        );
        assert!(
            feature_symbols
                .iter()
                .any(|symbol| symbol.name == "feature_tree_value")
        );
        assert!(
            main_symbols
                .iter()
                .chain(feature_symbols.iter())
                .all(|symbol| symbol.name != "dirty_worktree_value"),
            "exact generations must read immutable commit-tree blobs, not dirty worktree bytes"
        );
    }

    #[tokio::test]
    async fn dirty_generation_at_unchanged_head_is_not_exact_commit_evidence() {
        let project = TempDir::new().expect("project");
        let store = TempDir::new().expect("store");
        git(project.path(), &["init", "-q", "-b", "main"]);
        git(project.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            project.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(project.path().join("src")).expect("source directory");
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn committed_value() -> usize { 1 }\n",
        )
        .expect("committed source");
        git(project.path(), &["add", "."]);
        git(project.path(), &["commit", "-qm", "base"]);
        let revision =
            GitOidV1::new(git(project.path(), &["rev-parse", "HEAD"])).expect("revision");
        let tree = GitOidV1::new(git(project.path(), &["rev-parse", "HEAD^{tree}"])).expect("tree");
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn dirty_value() -> usize { 2 }\n",
        )
        .expect("dirty source");

        let project_id = ProjectId::new("project.dirty-generation").expect("project id");
        let canonical_project = project.path().canonicalize().expect("canonical project");
        let scoped_store = scoped_code_index_store_root(store.path(), &canonical_project);
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open scheduler");
        scheduler.reconcile_now().expect("publish dirty generation");
        let dirty = scheduler.latest_complete().expect("dirty generation");
        assert!(
            dirty.generation().snapshot().source_revision.is_none(),
            "dirty capture must not claim the unchanged HEAD"
        );
        let dirty_content_identity = dirty
            .generation()
            .snapshot()
            .content_identity
            .as_str()
            .to_owned();
        drop(dirty);
        drop(scheduler);

        let registry = CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(
                project_id.clone(),
                &canonical_project,
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount sealed store");
        let identity = super::super::identity::IndexingIdentityV1::resolve(&canonical_project)
            .expect("indexing identity");
        let reference = identity.head_ref().cloned().expect("head reference");
        let scope = ResolvedScope::new(
            project_id,
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            Some(reference.clone()),
        )
        .expect("resolved scope");

        // The exact pair is no longer unavailable: the registry mints sealed
        // exact generations straight from the commit's ODB tree on demand.
        // The invariant this test guards is unchanged — the dirty sealed
        // generation itself must never serve as exact-commit evidence.
        let control = BranchGenerationReadControlV1 {
            deadline: None,
            cancellation: None,
        };
        let pair = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match registry
                    .generations_for_revisions(
                        &scope,
                        &reference,
                        &revision,
                        &tree,
                        &reference,
                        &revision,
                        &tree,
                        control.clone(),
                    )
                    .await
                {
                    Err(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable) => {
                        tokio::task::yield_now().await;
                    }
                    result => break result,
                }
            }
        })
        .await
        .expect("bounded exact-generation read")
        .expect("exact generation minted from the commit tree");
        for generation in [pair.base.generation(), pair.head.generation()] {
            assert_eq!(
                generation
                    .snapshot()
                    .source_revision
                    .as_ref()
                    .map(tracedecay_domain::CommitId::as_str),
                Some(revision.as_str()),
                "exact evidence must carry the requested commit revision"
            );
            assert_ne!(
                generation.snapshot().content_identity.as_str(),
                dirty_content_identity,
                "the dirty sealed generation must never serve as exact-commit evidence"
            );
        }
        let symbols = generation_symbols(pair.base.generation(), None, None, &control)
            .expect("exact symbols");
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "committed_value")
        );
        assert!(
            symbols.iter().all(|symbol| symbol.name != "dirty_value"),
            "exact generations must read immutable commit-tree blobs, not dirty worktree bytes"
        );
    }

    fn init_fixture_repository(root: &Path) {
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "TraceDecay Test"]);
        git(
            root,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        std::fs::create_dir_all(root.join("src")).expect("source directory");
    }

    fn commit_source(root: &Path, source: &str, message: &str) -> (GitOidV1, GitOidV1) {
        std::fs::write(root.join("src/lib.rs"), source).expect("fixture source");
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", message]);
        (
            GitOidV1::new(git(root, &["rev-parse", "HEAD"])).expect("fixture revision"),
            GitOidV1::new(git(root, &["rev-parse", "HEAD^{tree}"])).expect("fixture tree"),
        )
    }

    /// Latch the durable index's truncation flag the way an eviction does,
    /// while keeping every entry, so the test isolates the flag itself from the
    /// question of which entries survived.
    fn latch_generation_index_truncation(scoped_store: &Path) {
        use crate::retention::code_index_generations::{
            DurablePublicationPointerV1, durable_generation_index_digest,
        };

        let pointer_path = scoped_store.join("active-code-generation-v1.json");
        let mut pointer: DurablePublicationPointerV1 =
            serde_json::from_slice(&std::fs::read(&pointer_path).expect("read pointer"))
                .expect("decode publication pointer");
        pointer.generation_index_truncated = true;
        pointer.generation_index_digest = Some(
            durable_generation_index_digest(&pointer.generation_index, true)
                .expect("digest truncated publication index"),
        );
        std::fs::write(
            &pointer_path,
            serde_json::to_vec(&pointer).expect("encode truncated pointer"),
        )
        .expect("write truncated publication pointer");
    }

    async fn settled_pair(
        registry: &CodeIndexSchedulerRegistryV1,
        scope: &ResolvedScope,
        base: (&RefId, &GitOidV1, &GitOidV1),
        head: (&RefId, &GitOidV1, &GitOidV1),
        control: &BranchGenerationReadControlV1,
        timeout: std::time::Duration,
    ) -> Result<BranchGenerationPairV1, CodeIndexSearchUnavailableReasonV1> {
        tokio::time::timeout(timeout, async {
            loop {
                match registry
                    .generations_for_revisions(
                        scope,
                        base.0,
                        base.1,
                        base.2,
                        head.0,
                        head.1,
                        head.2,
                        control.clone(),
                    )
                    .await
                {
                    // Only scheduler-lock contention may be retried here: a
                    // capacity answer that outlives the deadline is exactly the
                    // spin these tests exist to rule out.
                    Err(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable) => {
                        tokio::task::yield_now().await;
                    }
                    result => break result,
                }
            }
        })
        .await
        .expect("bounded exact-generation read")
    }

    /// A revision the branch has already moved past is an ordinary request — a
    /// base whose ref advanced between the caller's rev-parse and this read, a
    /// merge-base, a deliberately pinned commit. The capture used to peel the
    /// reference and refuse anything that was not its current tip, which made
    /// every one of those revisions permanently unmintable.
    #[tokio::test]
    async fn exact_generations_mint_for_a_commit_that_is_no_longer_the_ref_tip() {
        let project = TempDir::new().expect("project");
        let store = TempDir::new().expect("store");
        init_fixture_repository(project.path());
        let (base_revision, base_tree) = commit_source(
            project.path(),
            "pub fn superseded_tip_value() -> usize { 1 }\n",
            "base",
        );
        let (head_revision, head_tree) = commit_source(
            project.path(),
            "pub fn current_tip_value() -> usize { 2 }\n",
            "head",
        );

        let project_id = ProjectId::new("project.superseded-tip-mint").expect("project id");
        let canonical_project = project.path().canonicalize().expect("canonical project");
        let scoped_store = scoped_code_index_store_root(store.path(), &canonical_project);
        // Only the current tip is indexed, so the pair's head is served from the
        // index and its base — a commit the branch has already left behind — is
        // the single half that has to be captured from the object database.
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open scheduler");
        scheduler.reconcile_now().expect("publish tip generation");
        let head_generation_id = scheduler
            .latest_complete()
            .expect("tip generation")
            .generation()
            .manifest()
            .generation_id
            .clone();
        drop(scheduler);

        let registry = CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(
                project_id.clone(),
                &canonical_project,
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount sealed store");
        let identity = super::super::identity::IndexingIdentityV1::resolve(&canonical_project)
            .expect("indexing identity");
        let reference = identity.head_ref().cloned().expect("head reference");
        let scope = ResolvedScope::new(
            project_id,
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            Some(reference.clone()),
        )
        .expect("resolved scope");
        let control = BranchGenerationReadControlV1 {
            deadline: None,
            cancellation: None,
        };

        let pair = settled_pair(
            &registry,
            &scope,
            (&reference, &base_revision, &base_tree),
            (&reference, &head_revision, &head_tree),
            &control,
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("the superseded commit is minted from its immutable tree");

        assert_eq!(
            pair.base
                .generation()
                .snapshot()
                .source_revision
                .as_ref()
                .map(tracedecay_domain::CommitId::as_str),
            Some(base_revision.as_str()),
            "the superseded commit must be captured as itself, not as the ref tip"
        );
        assert_eq!(
            pair.head.generation().manifest().generation_id,
            head_generation_id,
            "the indexed tip must be served, never re-minted"
        );
        let base_symbols =
            generation_symbols(pair.base.generation(), None, None, &control).expect("base symbols");
        let head_symbols =
            generation_symbols(pair.head.generation(), None, None, &control).expect("head symbols");
        assert!(
            base_symbols
                .iter()
                .any(|symbol| symbol.name == "superseded_tip_value")
        );
        assert!(
            head_symbols
                .iter()
                .any(|symbol| symbol.name == "current_tip_value")
        );
    }

    /// Once the bounded index evicts anything its truncation flag latches on
    /// forever, and a missing entry used to be reported as `CapacityUnavailable`
    /// — a transient answer callers retry until their deadline, for a generation
    /// that was never going to appear. A truncated miss is mint-eligible like
    /// any other, and only the missing half is minted: the base here is served
    /// from the index alone, since its reference no longer exists to capture.
    #[tokio::test]
    async fn a_truncated_index_mints_only_the_half_it_cannot_serve() {
        let project = TempDir::new().expect("project");
        let store = TempDir::new().expect("store");
        init_fixture_repository(project.path());
        let (base_revision, base_tree) = commit_source(
            project.path(),
            "pub fn retained_base_value() -> usize { 1 }\n",
            "base",
        );

        let project_id = ProjectId::new("project.truncated-index-mint").expect("project id");
        let canonical_project = project.path().canonicalize().expect("canonical project");
        let scoped_store = scoped_code_index_store_root(store.path(), &canonical_project);
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open scheduler");
        scheduler.reconcile_now().expect("publish base generation");
        let base_generation_id = scheduler
            .latest_complete()
            .expect("base generation")
            .generation()
            .manifest()
            .generation_id
            .clone();
        drop(scheduler);

        git(project.path(), &["checkout", "-qb", "feature"]);
        let (head_revision, head_tree) = commit_source(
            project.path(),
            "pub fn unindexed_head_value() -> usize { 2 }\n",
            "head",
        );
        let base_reference =
            tracedecay_domain::RefId::new("refs/heads/main").expect("base reference");
        let head_reference =
            tracedecay_domain::RefId::new("refs/heads/feature").expect("head reference");
        // Deleting the base branch leaves its sealed generation reachable only
        // through the index. If the read still minted the base unconditionally,
        // that capture would fail and take the whole answer with it.
        git(project.path(), &["branch", "-q", "-D", "main"]);
        latch_generation_index_truncation(&scoped_store);

        let registry = CodeIndexSchedulerRegistryV1::new(1);
        registry
            .mount_worktree(
                project_id.clone(),
                &canonical_project,
                store.path().to_path_buf(),
                None,
            )
            .await
            .expect("mount sealed store");
        let identity = super::super::identity::IndexingIdentityV1::resolve(&canonical_project)
            .expect("indexing identity");
        let scope = ResolvedScope::new(
            project_id,
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            Some(head_reference.clone()),
        )
        .expect("resolved scope");
        let control = BranchGenerationReadControlV1 {
            deadline: None,
            cancellation: None,
        };

        let pair = settled_pair(
            &registry,
            &scope,
            (&base_reference, &base_revision, &base_tree),
            (&head_reference, &head_revision, &head_tree),
            &control,
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("a truncated index miss is minted, not reported as capacity");

        assert_eq!(
            pair.base.generation().manifest().generation_id,
            base_generation_id,
            "the indexed base must be served, never re-minted"
        );
        let head_symbols =
            generation_symbols(pair.head.generation(), None, None, &control).expect("head symbols");
        assert!(
            head_symbols
                .iter()
                .any(|symbol| symbol.name == "unindexed_head_value")
        );
    }
}

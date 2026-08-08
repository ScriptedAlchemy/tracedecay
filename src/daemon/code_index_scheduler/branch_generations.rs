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
    ) -> Result<
        (
            Arc<CodeIndexPublishedGenerationV1>,
            Arc<CodeIndexPublishedGenerationV1>,
        ),
        CodeIndexSearchUnavailableReasonV1,
    > {
        if let Some(reason) = control.termination() {
            return Err(reason);
        }
        let pointer = self
            .read_publication_pointer()
            .map_err(Self::exact_read_error)?
            .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
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
                .ok_or(if pointer.generation_index_truncated {
                    CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
                } else {
                    CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
                })
        };
        let base_entry = find(base_reference, base_revision, base_tree)?;
        let head_entry = if base_reference == head_reference
            && base_revision == head_revision
            && base_tree == head_tree
        {
            base_entry.clone()
        } else {
            find(head_reference, head_revision, head_tree)?
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
                let generation = self
                    .load_generation(&generation_id)
                    .map_err(Self::exact_read_error)?
                    .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
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
                Ok(generation)
            };
        let base = load(&base_entry, base_revision, base_tree, base_reference)?;
        let head = if base_reference == head_reference
            && base_revision == head_revision
            && base_tree == head_tree
        {
            Arc::clone(&base)
        } else {
            load(&head_entry, head_revision, head_tree, head_reference)?
        };
        Ok((base, head))
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
            let revisions = scheduler.publication.revisions(
                &base_reference,
                &base_revision,
                &base_tree,
                &head_reference,
                &head_revision,
                &head_tree,
                &control,
            );
            let (base, head) = match revisions {
                Ok(generations) => generations,
                Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable) => {
                    scheduler.publish_exact_git_tree_generation(
                        &super::git_tree_capture::ExactGitTreeSourceV1 {
                            reference: base_reference.clone(),
                            revision: tracedecay_domain::CommitId::new(
                                base_revision.as_str().to_owned(),
                            )
                            .map_err(|_| CodeIndexSearchUnavailableReasonV1::InvalidRequest)?,
                            tree: tracedecay_domain::TreeId::new(base_tree.as_str().to_owned())
                                .map_err(|_| CodeIndexSearchUnavailableReasonV1::InvalidRequest)?,
                        },
                        &control,
                    )?;
                    if base_reference != head_reference
                        || base_revision != head_revision
                        || base_tree != head_tree
                    {
                        scheduler.publish_exact_git_tree_generation(
                            &super::git_tree_capture::ExactGitTreeSourceV1 {
                                reference: head_reference.clone(),
                                revision: tracedecay_domain::CommitId::new(
                                    head_revision.as_str().to_owned(),
                                )
                                .map_err(|_| CodeIndexSearchUnavailableReasonV1::InvalidRequest)?,
                                tree: tracedecay_domain::TreeId::new(head_tree.as_str().to_owned())
                                    .map_err(|_| {
                                        CodeIndexSearchUnavailableReasonV1::InvalidRequest
                                    })?,
                            },
                            &control,
                        )?;
                    }
                    scheduler.publication.revisions(
                        &base_reference,
                        &base_revision,
                        &base_tree,
                        &head_reference,
                        &head_revision,
                        &head_tree,
                        &control,
                    )?
                }
                Err(reason) => return Err(reason),
            };
            let base = scheduler.bind_latest_complete(base);
            let head = scheduler.bind_latest_complete(head);
            if !Self::latest_matches_scope_identity(&base, &scope)
                || !Self::latest_matches_scope_identity(&head, &scope)
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
                registry
                    .generations_for_revisions(
                        &scope,
                        &reference,
                        &base_revision,
                        &base_tree,
                        &reference,
                        &head_revision,
                        &head_tree,
                        control.clone(),
                    )
                    .await,
                Err(CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired)
            ));
            std::fs::remove_file(&target).expect("remove exact-generation symlink");
            std::fs::rename(&backup, &target).expect("restore exact generation");
        }
        let pair = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match registry
                    .generations_for_revisions(
                        &scope,
                        &reference,
                        &base_revision,
                        &base_tree,
                        &reference,
                        &head_revision,
                        &head_tree,
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
        .expect("both clean commit generations");
        let base_generation_id = pair.base.generation().manifest().generation_id.clone();
        assert!(matches!(
            registry
                .generations_for_revisions(
                    &scope,
                    &reference,
                    &base_revision,
                    &head_tree,
                    &reference,
                    &base_revision,
                    &head_tree,
                    control.clone(),
                )
                .await,
            Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
        ));
        let wrong_reference =
            tracedecay_domain::RefId::new("refs/heads/not-main").expect("wrong reference");
        assert!(matches!(
            registry
                .generations_for_revisions(
                    &scope,
                    &wrong_reference,
                    &base_revision,
                    &base_tree,
                    &reference,
                    &head_revision,
                    &head_tree,
                    control.clone(),
                )
                .await,
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

        let large_pair = registry
            .generations_for_revisions(
                &scope,
                &reference,
                &large_revision,
                &large_tree,
                &reference,
                &large_revision,
                &large_tree,
                control.clone(),
            )
            .await
            .expect("large exact generation");
        let started = std::time::Instant::now();
        let outcome = bounded_diff(
            large_pair.base.generation(),
            large_pair.head.generation(),
            None,
            None,
            100,
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
                100,
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
                100,
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
            registry
                .generations_for_revisions(
                    &scope,
                    &reference,
                    &base_revision,
                    &base_tree,
                    &reference,
                    &head_revision,
                    &head_tree,
                    control,
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
                    .map(|value| value.as_str()),
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
}

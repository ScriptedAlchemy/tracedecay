//! Exact Git-revision reads over immutable sealed code-index generations.

use std::sync::{Arc, TryLockError};

#[cfg(test)]
use std::path::Path;

use tracedecay_domain::{GitOidV1, RefId, RepositoryId, WorktreeId};
use tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1;

use crate::code_index::production::CodeIndexPublicationStoreErrorV1;

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
    fn revisions(
        &self,
        repository: &RepositoryId,
        worktree: &WorktreeId,
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
            .map_err(publication_read_reason)?
            .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        let find = |reference: &RefId, revision: &GitOidV1, tree: &GitOidV1| {
            pointer
                .generation_index
                .iter()
                .find(|entry| {
                    entry.repository == repository.as_str()
                        && entry.worktree.as_deref() == Some(worktree.as_str())
                        && entry.source_reference.as_deref() == Some(reference.as_str())
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
             reference: &RefId,
             revision: &GitOidV1| {
                let generation_id =
                    tracedecay_domain::CodeGenerationId::new(entry.generation_id.clone())
                        .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
                let generation = self
                    .load_generation(&generation_id)
                    .map_err(publication_read_reason)?
                    .ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
                if generation.snapshot().reference.as_ref().map(RefId::as_str)
                    != Some(reference.as_str())
                    || generation
                        .snapshot()
                        .source_revision
                        .as_ref()
                        .map(|source| source.as_str())
                        != Some(revision.as_str())
                    || generation.snapshot().content_identity.as_str()
                        != entry.snapshot_content_identity
                {
                    return Err(CodeIndexSearchUnavailableReasonV1::Internal);
                }
                Ok(generation)
            };
        let base = load(&base_entry, base_reference, base_revision)?;
        let head = if base_reference == head_reference
            && base_revision == head_revision
            && base_tree == head_tree
        {
            Arc::clone(&base)
        } else {
            load(&head_entry, head_reference, head_revision)?
        };
        Ok((base, head))
    }
}

fn publication_read_reason(
    error: CodeIndexPublicationStoreErrorV1,
) -> CodeIndexSearchUnavailableReasonV1 {
    match error {
        CodeIndexPublicationStoreErrorV1::Capacity(_) => {
            CodeIndexSearchUnavailableReasonV1::CapacityUnavailable
        }
        CodeIndexPublicationStoreErrorV1::Corrupt(_) => {
            CodeIndexSearchUnavailableReasonV1::CorruptGeneration
        }
        CodeIndexPublicationStoreErrorV1::CompareAndSwap
        | CodeIndexPublicationStoreErrorV1::Unavailable(_) => {
            CodeIndexSearchUnavailableReasonV1::Internal
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
        crate::daemon::park_admission(tokio::task::spawn_blocking(move || {
            let publication = match scheduler.try_lock() {
                Ok(scheduler) => scheduler.publication.clone(),
                Err(TryLockError::WouldBlock) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable);
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::Internal);
                }
            };
            // Sealed-file IO, digest verification, and decode run under this
            // owned blocking task but outside the scheduler mutex. A large
            // immutable generation cannot stall unrelated hint admission.
            let (base, head) = publication.revisions(
                &scope.repository_id,
                &scope.worktree_id,
                &base_reference,
                &base_revision,
                &base_tree,
                &head_reference,
                &head_revision,
                &head_tree,
                &control,
            )?;
            let scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(TryLockError::WouldBlock) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable);
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::Internal);
                }
            };
            let base = scheduler.bind_latest_complete(base);
            let head = scheduler.bind_latest_complete(head);
            if !Self::latest_matches_scope_identity(&base, &scope)
                || !Self::latest_matches_scope_identity(&head, &scope)
            {
                return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
            }
            Ok(BranchGenerationPairV1 { base, head })
        }))
        .await
        .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;
    use tracedecay_application::ResolvedScope;
    use tracedecay_domain::{GitOidV1, ProjectId, RefId};

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
        git(project.path(), &["branch", "alias"]);
        git(project.path(), &["switch", "-q", "alias"]);
        scheduler
            .reconcile_now()
            .expect("publish exact aliased-ref generation");
        git(project.path(), &["switch", "-q", "main"]);

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
        let reference = identity.head_ref().cloned().expect("attached HEAD");
        let scope = ResolvedScope::new(
            project_id,
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            identity.head_ref().cloned(),
        )
        .expect("resolved scope");
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
        let base =
            generation_symbols(pair.base.generation(), None, None, &control).expect("base symbols");
        let head =
            generation_symbols(pair.head.generation(), None, None, &control).expect("head symbols");
        let completed = diff_symbols(
            pair.base.generation().manifest().generation_id.as_str(),
            base,
            pair.head.generation().manifest().generation_id.as_str(),
            head,
        );

        assert!(completed.added.is_empty());
        assert!(completed.removed.is_empty());
        assert_eq!(completed.changed.len(), 1);
        assert_eq!(
            completed.changed[0].head.qualified_name,
            "src/lib.rs::exact_branch_value"
        );
        assert_ne!(
            completed.changed[0].base.content_digest,
            completed.changed[0].head.content_digest
        );

        let main_reference = RefId::new("refs/heads/main").expect("main ref");
        let alias_reference = RefId::new("refs/heads/alias").expect("alias ref");
        let aliases = registry
            .generations_for_revisions(
                &scope,
                &main_reference,
                &base_revision,
                &base_tree,
                &alias_reference,
                &base_revision,
                &base_tree,
                control.clone(),
            )
            .await
            .expect("aliased refs remain independently exact");
        assert_ne!(
            aliases.base.generation().manifest().generation_id,
            aliases.head.generation().manifest().generation_id
        );
        assert_eq!(
            aliases.base.generation().snapshot().reference.as_ref(),
            Some(&main_reference)
        );
        assert_eq!(
            aliases.head.generation().snapshot().reference.as_ref(),
            Some(&alias_reference)
        );

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
            &control,
        )
        .expect("bounded large diff");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "oversized generation admission must not materialize symbols"
        );
        assert!(matches!(
            outcome,
            tracedecay_query::code_search::CodeIndexBranchDiffOutcomeV1::Partial(
                tracedecay_query::code_search::CodeIndexBranchDiffPartialV1 {
                    reason:
                        tracedecay_query::code_search::CodeIndexBranchDiffPartialReasonV1::GenerationSymbolLimit,
                    added,
                    removed,
                    changed,
                    ..
                }
            ) if added.is_empty() && removed.is_empty() && changed.is_empty()
        ));

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
        assert!(
            scheduler
                .latest_complete()
                .expect("dirty generation")
                .generation()
                .snapshot()
                .source_revision
                .is_none(),
            "dirty capture must not claim the unchanged HEAD"
        );
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
        let reference = identity.head_ref().cloned().expect("attached HEAD");
        let scope = ResolvedScope::new(
            project_id,
            identity.repository_id().clone(),
            identity.worktree_id().clone(),
            identity.head_ref().cloned(),
        )
        .expect("resolved scope");

        assert!(matches!(
            registry
                .generations_for_revisions(
                    &scope,
                    &reference,
                    &revision,
                    &tree,
                    &reference,
                    &revision,
                    &tree,
                    BranchGenerationReadControlV1 {
                        deadline: None,
                        cancellation: None,
                    },
                )
                .await,
            Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)
        ));
    }
}

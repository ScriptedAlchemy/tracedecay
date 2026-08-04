//! Exact Git-revision reads over immutable sealed code-index generations.

use std::path::Path;
use std::sync::{Arc, TryLockError};

use tracedecay_domain::GitOidV1;
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
    fn revisions(
        &self,
        base_revision: &GitOidV1,
        head_revision: &GitOidV1,
        control: &BranchGenerationReadControlV1,
    ) -> Result<
        (
            Arc<CodeIndexPublishedGenerationV1>,
            Arc<CodeIndexPublishedGenerationV1>,
        ),
        CodeIndexSearchUnavailableReasonV1,
    > {
        let mut paths = std::fs::read_dir(&self.generations_root)
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        let mut base = None;
        let mut head = None;
        for path in paths {
            if let Some(reason) = control.termination() {
                return Err(reason);
            }
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(encoded_digest) = file_name
                .strip_prefix("generation-")
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            if !valid_sealed_generation_path(&path, encoded_digest)? {
                continue;
            }
            let bytes =
                std::fs::read(&path).map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
            if Self::state_digest(&bytes) != format!("sha256:{encoded_digest}") {
                return Err(CodeIndexSearchUnavailableReasonV1::Internal);
            }
            if !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(&bytes)
                .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?
            {
                continue;
            }
            self.cache.note_decode();
            let generation = Arc::new(
                CodeIndexPublishedGenerationV1::decode_sealed(&bytes)
                    .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?,
            );
            let revision = generation
                .snapshot()
                .source_revision
                .as_ref()
                .map(|revision| revision.as_str());
            if revision == Some(base_revision.as_str()) {
                if base.replace(Arc::clone(&generation)).is_some() {
                    return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
                }
            }
            if base_revision != head_revision && revision == Some(head_revision.as_str()) {
                if head.replace(Arc::clone(&generation)).is_some() {
                    return Err(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable);
                }
            }
        }
        let base = base.ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?;
        let head = if base_revision == head_revision {
            Arc::clone(&base)
        } else {
            head.ok_or(CodeIndexSearchUnavailableReasonV1::GenerationUnavailable)?
        };
        self.cache
            .remember(Arc::clone(&base))
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
        self.cache
            .remember(Arc::clone(&head))
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?;
        Ok((base, head))
    }
}

fn valid_sealed_generation_path(
    path: &Path,
    encoded_digest: &str,
) -> Result<bool, CodeIndexSearchUnavailableReasonV1> {
    Ok(encoded_digest.len() == 64
        && encoded_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && path
            .symlink_metadata()
            .map_err(|_| CodeIndexSearchUnavailableReasonV1::Internal)?
            .file_type()
            .is_file())
}

impl CodeIndexSchedulerRegistryV1 {
    pub(in crate::daemon) async fn generations_for_revisions(
        &self,
        scope: &tracedecay_application::ResolvedScope,
        base_revision: &GitOidV1,
        head_revision: &GitOidV1,
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
        let base_revision = base_revision.clone();
        let head_revision = head_revision.clone();
        let scope = scope.clone();
        crate::daemon::park_admission(tokio::task::spawn_blocking(move || {
            let scheduler = match scheduler.try_lock() {
                Ok(scheduler) => scheduler,
                Err(TryLockError::WouldBlock) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::CapacityUnavailable);
                }
                Err(TryLockError::Poisoned(_)) => {
                    return Err(CodeIndexSearchUnavailableReasonV1::Internal);
                }
            };
            let (base, head) =
                scheduler
                    .publication
                    .revisions(&base_revision, &head_revision, &control)?;
            let base = scheduler.bind_latest_complete(base);
            let head = scheduler.bind_latest_complete(head);
            if !Self::latest_matches_scope(&base, &scope)
                || !Self::latest_matches_scope(&head, &scope)
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
    use tracedecay_domain::{GitOidV1, ProjectId};

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
        let project_id = ProjectId::new("project.branch-generation-diff").expect("project id");
        let canonical_project = project.path().canonicalize().expect("canonical project");
        let scoped_store = scoped_code_index_store_root(store.path(), &canonical_project);
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store,
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
        scheduler.reconcile_now().expect("publish large generation");
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
                        &base_revision,
                        &head_revision,
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

        let large_pair = registry
            .generations_for_revisions(&scope, &large_revision, &large_revision, control.clone())
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
    }
}

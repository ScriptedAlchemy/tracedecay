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
pub(super) struct BranchGenerationReadControlV1 {
    pub deadline: Option<tracedecay_application::Deadline>,
    pub cancellation: Option<tracedecay_application::CancellationSignal>,
}

impl BranchGenerationReadControlV1 {
    pub(super) fn termination(&self) -> Option<CodeIndexSearchUnavailableReasonV1> {
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

pub(super) struct BranchGenerationPairV1 {
    pub base: LatestCompleteCodeIndexV1,
    pub head: LatestCompleteCodeIndexV1,
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
    pub(super) async fn generations_for_revisions(
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

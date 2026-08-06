//! Retained daemon drain for durable session-to-Git graph evidence journals.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tracedecay_domain::{
    GitGraphEvidenceIntent, GitGraphEvidencePublicationReceipt, GitGraphEvidenceTarget, ProjectId,
};
use tracedecay_graph_db::GraphDb;

const DRAIN_LIMIT: usize = 256;

#[derive(Debug, Error)]
pub(super) enum SessionGitEvidenceDrainError {
    #[error("session Git evidence owner identity is unavailable: {0}")]
    Identity(String),
    #[error("session Git evidence owner conflicts with an existing project owner")]
    IdentityConflict,
    #[error("session Git evidence owner registry is unavailable")]
    RegistryUnavailable,
    #[error("session Git evidence owner task failed: {0}")]
    Task(String),
}

struct SessionEvidenceReceiptSink {
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    source: tracedecay_sessions::runtime::git_correlation::SessionGitGraphPublicationIntent,
    wake: Arc<SessionEvidenceWake>,
}

impl crate::graph::git::GitEvidenceReceiptSink for SessionEvidenceReceiptSink {
    fn acknowledge<'a>(
        &'a self,
        intent: &'a GitGraphEvidenceIntent,
        receipt: GitGraphEvidencePublicationReceipt,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = std::result::Result<(), crate::graph::git::GitTopologyError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if !receipt_matches_session_source(&self.source, intent, &receipt) {
                return Err(crate::graph::git::GitTopologyError::Contract(
                    "session Git evidence acknowledgement does not match its source journal"
                        .to_owned(),
                ));
            }
            self.session_db
                .acknowledge_session_git_graph_publication(
                    &self.source,
                    &receipt,
                    tracedecay_application::now_micros().0,
                )
                .await
                .map_err(|error| {
                    crate::graph::git::GitTopologyError::Repository(format!(
                        "session Git evidence acknowledgement failed: {error}"
                    ))
                })?;
            self.wake.wake();
            Ok(())
        })
    }
}

fn receipt_matches_session_source(
    source: &tracedecay_sessions::runtime::git_correlation::SessionGitGraphPublicationIntent,
    intent: &GitGraphEvidenceIntent,
    receipt: &GitGraphEvidencePublicationReceipt,
) -> bool {
    receipt.intent_digest() == intent.intent_digest()
        && intent.commit() == &source.commit_oid
        && matches!(
            intent.target(),
            GitGraphEvidenceTarget::Session(session_id) if session_id == &source.session_id
        )
}

struct SessionEvidenceWake {
    sender: tokio::sync::mpsc::Sender<()>,
    cancelled: Arc<AtomicBool>,
}

impl SessionEvidenceWake {
    fn wake(&self) {
        if !self.cancelled.load(Ordering::Acquire) {
            let _ = self.sender.try_send(());
        }
    }
}

impl crate::global_db::SessionGitGraphPublicationWake for SessionEvidenceWake {
    fn wake(&self) {
        self.wake();
    }
}

struct SessionGitEvidenceDrainOwner {
    project_store_root: PathBuf,
    project_root: PathBuf,
    database: Arc<GraphDb>,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    wake: Arc<SessionEvidenceWake>,
    join: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SessionGitEvidenceDrainOwner {
    fn start(
        project_id: ProjectId,
        project_store_root: PathBuf,
        project_root: PathBuf,
        database: Arc<GraphDb>,
        session_db: Arc<crate::global_db::RegisteredGlobalDb>,
        graph_runtime: crate::daemon::embedded_graph_runtime::EmbeddedGraphRuntimeRegistry,
    ) -> Result<Arc<Self>, SessionGitEvidenceDrainError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let wake = Arc::new(SessionEvidenceWake {
            sender,
            cancelled: Arc::clone(&cancelled),
        });
        session_db
            .bind_session_git_graph_publication_wake(wake.clone())
            .map_err(|error| SessionGitEvidenceDrainError::Task(error.to_string()))?;
        let worker_wake = Arc::clone(&wake);
        let worker_store_root = project_store_root.clone();
        let worker_project_root = project_root.clone();
        let worker_database = Arc::clone(&database);
        let worker_session_db = Arc::clone(&session_db);
        let join = tokio::spawn(async move {
            while receiver.recv().await.is_some() {
                if cancelled.load(Ordering::Acquire) {
                    break;
                }
                let mut retry_delay = Duration::from_millis(50);
                loop {
                    match drain_once(
                        &project_id,
                        &worker_store_root,
                        &worker_project_root,
                        Arc::clone(&worker_database),
                        Arc::clone(&worker_session_db),
                        Arc::clone(&worker_wake),
                        &graph_runtime,
                    )
                    .await
                    {
                        Ok(count) => {
                            tracing::debug!(
                                event = "session_git_evidence_drain",
                                project_id = %project_id,
                                pending = count,
                            );
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(
                                event = "session_git_evidence_drain",
                                project_id = %project_id,
                                outcome = "retrying",
                                error = %error,
                            );
                            tokio::select! {
                                () = tokio::time::sleep(retry_delay) => {}
                                signal = receiver.recv() => {
                                    if signal.is_none() {
                                        return;
                                    }
                                }
                            }
                            if cancelled.load(Ordering::Acquire) {
                                return;
                            }
                            retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(5));
                        }
                    }
                }
            }
        });
        let owner = Arc::new(Self {
            project_store_root,
            project_root,
            database,
            session_db,
            wake,
            join: tokio::sync::Mutex::new(Some(join)),
        });
        owner.wake.wake();
        Ok(owner)
    }

    fn has_exact_identity(
        &self,
        project_store_root: &Path,
        project_root: &Path,
        database: &Arc<GraphDb>,
        session_db: &Arc<crate::global_db::RegisteredGlobalDb>,
    ) -> bool {
        self.project_store_root == project_store_root
            && self.project_root == project_root
            && Arc::ptr_eq(&self.database, database)
            && Arc::ptr_eq(&self.session_db, session_db)
    }

    async fn shutdown(&self) -> Result<(), SessionGitEvidenceDrainError> {
        self.wake.cancelled.store(true, Ordering::Release);
        let _ = self.wake.sender.try_send(());
        if let Some(join) = self.join.lock().await.take() {
            join.await
                .map_err(|error| SessionGitEvidenceDrainError::Task(error.to_string()))?;
        }
        Ok(())
    }
}

async fn drain_once(
    project_id: &ProjectId,
    project_store_root: &Path,
    project_root: &Path,
    database: Arc<GraphDb>,
    session_db: Arc<crate::global_db::RegisteredGlobalDb>,
    wake: Arc<SessionEvidenceWake>,
    graph_runtime: &crate::daemon::embedded_graph_runtime::EmbeddedGraphRuntimeRegistry,
) -> Result<usize, SessionGitEvidenceDrainError> {
    let pending = session_db
        .pending_session_git_graph_publications(DRAIN_LIMIT)
        .await
        .map_err(|error| SessionGitEvidenceDrainError::Task(error.to_string()))?;
    let count = pending.len();
    for source in pending {
        let intent = GitGraphEvidenceIntent::new(
            project_id.clone(),
            source.commit_oid.clone(),
            GitGraphEvidenceTarget::Session(source.session_id.clone()),
        )
        .map_err(|error| SessionGitEvidenceDrainError::Identity(error.to_string()))?;
        let sink: Arc<dyn crate::graph::git::GitEvidenceReceiptSink> =
            Arc::new(SessionEvidenceReceiptSink {
                session_db: Arc::clone(&session_db),
                source,
                wake: Arc::clone(&wake),
            });
        graph_runtime
            .enqueue_git_evidence(
                project_id,
                project_store_root,
                project_root,
                Arc::clone(&database),
                intent,
                sink,
            )
            .await
            .map_err(|error| SessionGitEvidenceDrainError::Task(error.to_string()))?;
    }
    Ok(count)
}

#[derive(Clone, Default)]
pub(super) struct SessionGitEvidenceDrainRegistry {
    owners: Arc<Mutex<BTreeMap<ProjectId, Arc<SessionGitEvidenceDrainOwner>>>>,
}

impl SessionGitEvidenceDrainRegistry {
    pub(super) fn bind(
        &self,
        project_id: &ProjectId,
        project_store_root: &Path,
        project_root: &Path,
        database: Arc<GraphDb>,
        session_db: Arc<crate::global_db::RegisteredGlobalDb>,
        graph_runtime: crate::daemon::embedded_graph_runtime::EmbeddedGraphRuntimeRegistry,
    ) -> Result<(), SessionGitEvidenceDrainError> {
        let project_store_root = project_store_root.canonicalize().map_err(|error| {
            SessionGitEvidenceDrainError::Identity(format!(
                "canonical project store {} is unavailable: {error}",
                project_store_root.display()
            ))
        })?;
        let project_root = project_root.canonicalize().map_err(|error| {
            SessionGitEvidenceDrainError::Identity(format!(
                "canonical project root {} is unavailable: {error}",
                project_root.display()
            ))
        })?;
        let mut owners = self
            .owners
            .lock()
            .map_err(|_| SessionGitEvidenceDrainError::RegistryUnavailable)?;
        if let Some(owner) = owners.get(project_id) {
            if !owner.has_exact_identity(&project_store_root, &project_root, &database, &session_db)
            {
                return Err(SessionGitEvidenceDrainError::IdentityConflict);
            }
            owner.wake.wake();
            return session_db
                .bind_session_git_graph_publication_wake(owner.wake.clone())
                .map_err(|error| SessionGitEvidenceDrainError::Task(error.to_string()));
        }
        let owner = SessionGitEvidenceDrainOwner::start(
            project_id.clone(),
            project_store_root,
            project_root,
            database,
            Arc::clone(&session_db),
            graph_runtime,
        )?;
        owners.insert(project_id.clone(), owner);
        Ok(())
    }

    pub(super) async fn shutdown(&self) -> Vec<SessionGitEvidenceDrainError> {
        let owners = match self.owners.lock() {
            Ok(mut owners) => std::mem::take(&mut *owners),
            Err(_) => return vec![SessionGitEvidenceDrainError::RegistryUnavailable],
        };
        let mut errors = Vec::new();
        for owner in owners.into_values() {
            if let Err(error) = owner.shutdown().await {
                errors.push(error);
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{ContentDigest, GitOidV1, SessionId};

    #[test]
    fn session_receipt_requires_exact_common_intent_identity() {
        let project = ProjectId::new("project.session-git-evidence").expect("project");
        let commit = GitOidV1::new("a".repeat(40)).expect("commit");
        let session = SessionId::new("session.session-git-evidence").expect("session");
        let source =
            tracedecay_sessions::runtime::git_correlation::SessionGitGraphPublicationIntent {
                source_sequence: 1,
                commit_oid: commit.clone(),
                session_id: session.clone(),
                intent_digest: "source-digest".to_owned(),
                source_committed_at: 1,
            };
        let intent =
            GitGraphEvidenceIntent::new(project, commit, GitGraphEvidenceTarget::Session(session))
                .expect("intent");
        let receipt = GitGraphEvidencePublicationReceipt::new(
            intent.intent_digest().clone(),
            "graph-watermark".to_owned(),
            1,
        )
        .expect("receipt");
        assert!(receipt_matches_session_source(&source, &intent, &receipt));

        let mismatched = GitGraphEvidencePublicationReceipt::new(
            ContentDigest::of_bytes(b"another-intent"),
            "graph-watermark".to_owned(),
            1,
        )
        .expect("mismatched receipt");
        assert!(!receipt_matches_session_source(
            &source,
            &intent,
            &mismatched
        ));
    }
}

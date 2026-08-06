//! Daemon bridge from the durable Work evidence journal to Git convergence.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;
use tracedecay_application::WorkExecutionPersistenceError;
use tracedecay_domain::{
    ContentDigest, GitGraphEvidenceIntent, GitGraphEvidencePublicationReceipt, WorkAuthority,
};
use tracedecay_rusqlite_runtime::work::{WorkGitGraphEvidenceNotifier, WorkSqliteStorage};

use crate::daemon::embedded_graph_runtime::{
    EmbeddedGraphRuntimeError, EmbeddedGraphRuntimeRegistry,
};
use crate::graph::git::{GitEvidenceReceiptSink, GitTopologyError};

const DRAIN_BATCH_SIZE: u32 = 1_000;

#[derive(Debug, Error)]
pub(in crate::daemon) enum WorkGitEvidenceDrainError {
    #[error("Work Git evidence pending journal read failed: {0}")]
    Pending(WorkExecutionPersistenceError),
    #[error("Work Git evidence pending journal task failed: {0}")]
    PendingTask(String),
    #[error("Work Git evidence enqueue failed: {0}")]
    Enqueue(EmbeddedGraphRuntimeError),
    #[error("Work Git evidence owner could not start: {0}")]
    Start(String),
    #[error("Work Git evidence owner task panicked")]
    Join,
    #[error("Work Git evidence owner lock is unavailable")]
    Lock,
}

pub(in crate::daemon) struct WorkGitGraphEvidencePublisher {
    wake: Arc<WorkGitEvidenceWake>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl WorkGitGraphEvidencePublisher {
    pub(in crate::daemon) fn start(
        storage: WorkSqliteStorage,
        authority: WorkAuthority,
        project_root: PathBuf,
        project_store_root: PathBuf,
        graph: Arc<tracedecay_graph_db::GraphDb>,
        graph_runtime: EmbeddedGraphRuntimeRegistry,
    ) -> Result<Arc<Self>, WorkGitEvidenceDrainError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| WorkGitEvidenceDrainError::Start(error.to_string()))?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let wake = Arc::new(WorkGitEvidenceWake {
            sender,
            cancelled: Arc::clone(&cancelled),
        });
        let worker_wake = Arc::clone(&wake);
        let worker_authority = authority.clone();
        let join = std::thread::Builder::new()
            .name("tracedecay-work-git-evidence".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
                    while receiver.recv().await.is_some() {
                        if cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        let mut retry_delay = Duration::from_millis(50);
                        loop {
                            match drain_once(
                                &worker_authority,
                                &project_store_root,
                                &project_root,
                                Arc::clone(&graph),
                                storage.clone(),
                                Arc::clone(&worker_wake),
                                &graph_runtime,
                            )
                            .await
                            {
                                Ok(pending) => {
                                    tracing::debug!(
                                        event = "work_git_graph_evidence_drain",
                                        project_id = %worker_authority.project_id(),
                                        pending,
                                    );
                                    break;
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        event = "work_git_graph_evidence_drain",
                                        project_id = %worker_authority.project_id(),
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
                                    retry_delay =
                                        retry_delay.saturating_mul(2).min(Duration::from_secs(5));
                                }
                            }
                        }
                    }
                });
            })
            .map_err(|error| WorkGitEvidenceDrainError::Start(error.to_string()))?;
        Ok(Arc::new(Self {
            wake,
            join: Mutex::new(Some(join)),
        }))
    }

    pub(in crate::daemon) fn shutdown(&self) -> Result<(), WorkGitEvidenceDrainError> {
        self.wake.cancelled.store(true, Ordering::Release);
        let _ = self.wake.sender.try_send(());
        let join = self
            .join
            .lock()
            .map_err(|_| WorkGitEvidenceDrainError::Lock)?
            .take();
        if let Some(join) = join {
            join.join().map_err(|_| WorkGitEvidenceDrainError::Join)?;
        }
        Ok(())
    }
}

impl WorkGitGraphEvidenceNotifier for WorkGitGraphEvidencePublisher {
    fn notify(&self) {
        self.wake.wake();
    }
}

impl Drop for WorkGitGraphEvidencePublisher {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::warn!(
                event = "work_git_graph_evidence_shutdown",
                outcome = "failed",
                error = %error,
            );
        }
    }
}

struct WorkGitEvidenceWake {
    sender: tokio::sync::mpsc::Sender<()>,
    cancelled: Arc<AtomicBool>,
}

impl WorkGitEvidenceWake {
    fn wake(&self) {
        if !self.cancelled.load(Ordering::Acquire) {
            let _ = self.sender.try_send(());
        }
    }
}

struct WorkGitEvidenceReceiptSink {
    storage: WorkSqliteStorage,
    authority: WorkAuthority,
    journal_sequence: u64,
    intent_digest: ContentDigest,
    wake: Arc<WorkGitEvidenceWake>,
}

impl GitEvidenceReceiptSink for WorkGitEvidenceReceiptSink {
    fn acknowledge<'a>(
        &'a self,
        intent: &'a GitGraphEvidenceIntent,
        receipt: GitGraphEvidencePublicationReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<(), GitTopologyError>> + Send + 'a>> {
        Box::pin(async move {
            if intent.intent_digest() != &self.intent_digest {
                return Err(GitTopologyError::Contract(
                    "Work Git evidence acknowledgement targeted another intent".to_owned(),
                ));
            }
            let storage = self.storage.clone();
            let authority = self.authority.clone();
            let journal_sequence = self.journal_sequence;
            tokio::task::spawn_blocking(move || {
                storage.acknowledge_git_graph_evidence(&authority, journal_sequence, &receipt)
            })
            .await
            .map_err(|error| {
                GitTopologyError::Repository(format!(
                    "Work Git evidence acknowledgement task failed: {error}"
                ))
            })?
            .map_err(map_persistence_error)?;
            self.wake.wake();
            Ok(())
        })
    }
}

async fn drain_once(
    authority: &WorkAuthority,
    project_store_root: &std::path::Path,
    project_root: &std::path::Path,
    graph: Arc<tracedecay_graph_db::GraphDb>,
    storage: WorkSqliteStorage,
    wake: Arc<WorkGitEvidenceWake>,
    graph_runtime: &EmbeddedGraphRuntimeRegistry,
) -> Result<usize, WorkGitEvidenceDrainError> {
    let pending_storage = storage.clone();
    let pending_authority = authority.clone();
    let pending = tokio::task::spawn_blocking(move || {
        pending_storage.pending_git_graph_evidence(&pending_authority, DRAIN_BATCH_SIZE)
    })
    .await
    .map_err(|error| WorkGitEvidenceDrainError::PendingTask(error.to_string()))?
    .map_err(WorkGitEvidenceDrainError::Pending)?;
    let count = pending.len();
    for entry in pending {
        if wake.cancelled.load(Ordering::Acquire) {
            break;
        }
        let sink: Arc<dyn GitEvidenceReceiptSink> = Arc::new(WorkGitEvidenceReceiptSink {
            storage: storage.clone(),
            authority: authority.clone(),
            journal_sequence: entry.journal_sequence(),
            intent_digest: entry.intent().intent_digest().clone(),
            wake: Arc::clone(&wake),
        });
        graph_runtime
            .enqueue_git_evidence(
                authority.project_id(),
                project_store_root,
                project_root,
                Arc::clone(&graph),
                entry.intent().clone(),
                sink,
            )
            .await
            .map_err(WorkGitEvidenceDrainError::Enqueue)?;
    }
    Ok(count)
}

fn map_persistence_error(error: WorkExecutionPersistenceError) -> GitTopologyError {
    match error {
        WorkExecutionPersistenceError::Conflict => GitTopologyError::Contract(
            "Work Git evidence acknowledgement conflicted with durable journal state".to_owned(),
        ),
        WorkExecutionPersistenceError::InvalidRequest => {
            GitTopologyError::Contract("Work Git evidence acknowledgement was invalid".to_owned())
        }
        WorkExecutionPersistenceError::Unavailable(message) => {
            GitTopologyError::Repository(message)
        }
    }
}

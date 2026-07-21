//! Bounded synchronous bridge to canonical `GlobalDb` transaction storage.
//!
//! The application Git port is deliberately synchronous because its native
//! executor is synchronous.  Calling an async database through `block_on` on
//! a Tokio worker would pin that worker while an `IMMEDIATE` writer waits. This
//! adapter instead owns one bounded actor thread; the actor owns the async
//! `GlobalDb` calls and every synchronous port call receives exactly one reply.
//! It has no filesystem path and cannot create a JSON side-file authority.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use tracedecay_domain::{
    GitIndexIdempotencyKey, GitIndexPreviewId, GitIndexPreviewV1, GitIndexTransactionId,
    GitIndexTransactionJournalV1, GitIndexTransactionReceiptV1, RepositoryId,
};
use tracedecay_store::{
    GitIndexTransactionBeginRequestV1, GitIndexTransactionBeginResultV1,
    GitIndexTransactionRecordV1, GitIndexTransactionStore, GitIndexTransactionStoreError,
    GitIndexTransactionStoreResult, GitIndexTransactionTerminalWriteV1,
};

use crate::global_db::GlobalDb;

/// The actor queue is intentionally finite: saturation fails closed instead of
/// accumulating unbounded mutation work while a durable writer is stalled.
const GIT_INDEX_TRANSACTION_STORE_ACTOR_CAPACITY: usize = 64;
// Keep the sync port bounded by the same five-second writer wait used by
// `GlobalDb`; callers can reconcile durable state after an unavailable result
// instead of pinning a daemon worker forever.
const GIT_INDEX_TRANSACTION_STORE_ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

type Reply<T> = SyncSender<GitIndexTransactionStoreResult<T>>;

enum StoreCommand {
    SavePreview(GitIndexPreviewV1, Reply<()>),
    ReadPreview(GitIndexPreviewId, Reply<Option<GitIndexPreviewV1>>),
    BeginOrReplay(
        GitIndexTransactionBeginRequestV1,
        Reply<GitIndexTransactionBeginResultV1>,
    ),
    CompareAndSwapJournal(
        GitIndexIdempotencyKey,
        u64,
        GitIndexTransactionJournalV1,
        Reply<GitIndexTransactionJournalV1>,
    ),
    WriteTerminal(
        GitIndexTransactionTerminalWriteV1,
        Reply<GitIndexTransactionReceiptV1>,
    ),
    RecoveryCandidates(RepositoryId, Reply<Vec<GitIndexTransactionRecordV1>>),
    RecoveryRepositories(Reply<Vec<RepositoryId>>),
    QuarantineRepository(RepositoryId, GitIndexTransactionId, Reply<()>),
    ClearRepositoryQuarantine(
        RepositoryId,
        GitIndexTransactionId,
        GitIndexTransactionReceiptV1,
        Reply<()>,
    ),
}

/// Synchronous `tracedecay-store` contract adapter over one already-open,
/// canonical project `GlobalDb`.
///
/// Dropping the last adapter closes the command channel and lets the dedicated
/// actor exit. It intentionally has no `Clone` implementation: one daemon
/// service owns one bounded queue and actor for its transaction authority.
pub(crate) struct DaemonGitIndexTransactionStore {
    commands: SyncSender<StoreCommand>,
}

impl DaemonGitIndexTransactionStore {
    pub(crate) fn open(database: Arc<GlobalDb>) -> GitIndexTransactionStoreResult<Self> {
        let (commands, receiver) = sync_channel(GIT_INDEX_TRANSACTION_STORE_ACTOR_CAPACITY);
        let (ready, started) = sync_channel::<GitIndexTransactionStoreResult<()>>(1);
        std::thread::Builder::new()
            .name("tracedecay-git-index-store".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = ready.send(Err(GitIndexTransactionStoreError::Unavailable));
                    return;
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                run_store_actor(runtime, database, receiver);
            })
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        started
            .recv_timeout(GIT_INDEX_TRANSACTION_STORE_ACTOR_TIMEOUT)
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)??;
        Ok(Self { commands })
    }

    fn submit(&self, command: StoreCommand) -> GitIndexTransactionStoreResult<()> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    GitIndexTransactionStoreError::Unavailable
                }
            })
    }

    fn await_reply<T>(
        &self,
        receiver: Receiver<GitIndexTransactionStoreResult<T>>,
    ) -> GitIndexTransactionStoreResult<T> {
        receiver
            .recv_timeout(GIT_INDEX_TRANSACTION_STORE_ACTOR_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected => {
                    GitIndexTransactionStoreError::Unavailable
                }
            })?
    }
}

impl GitIndexTransactionStore for DaemonGitIndexTransactionStore {
    fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::SavePreview(preview, reply))?;
        self.await_reply(receiver)
    }

    fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadPreview(preview_id.clone(), reply))?;
        self.await_reply(receiver)
    }

    fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::BeginOrReplay(request, reply))?;
        self.await_reply(receiver)
    }

    fn compare_and_swap_journal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        expected_phase_epoch: u64,
        replacement: GitIndexTransactionJournalV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionJournalV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::CompareAndSwapJournal(
            idempotency_key.clone(),
            expected_phase_epoch,
            replacement,
            reply,
        ))?;
        self.await_reply(receiver)
    }

    fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionReceiptV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::WriteTerminal(write, reply))?;
        self.await_reply(receiver)
    }

    fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::RecoveryCandidates(
            repository_id.clone(),
            reply,
        ))?;
        self.await_reply(receiver)
    }

    fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::RecoveryRepositories(reply))?;
        self.await_reply(receiver)
    }

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
    ) -> GitIndexTransactionStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::QuarantineRepository(
            repository_id.clone(),
            transaction_id.clone(),
            reply,
        ))?;
        self.await_reply(receiver)
    }

    fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
        recovery_receipt: GitIndexTransactionReceiptV1,
    ) -> GitIndexTransactionStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ClearRepositoryQuarantine(
            repository_id.clone(),
            transaction_id.clone(),
            recovery_receipt,
            reply,
        ))?;
        self.await_reply(receiver)
    }
}

fn run_store_actor(
    runtime: tokio::runtime::Runtime,
    database: Arc<GlobalDb>,
    receiver: Receiver<StoreCommand>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            StoreCommand::SavePreview(preview, reply) => {
                let result =
                    runtime.block_on(database.git_index_transaction_store().save_preview(preview));
                let _ = reply.send(result);
            }
            StoreCommand::ReadPreview(preview_id, reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .read_preview(&preview_id),
                );
                let _ = reply.send(result);
            }
            StoreCommand::BeginOrReplay(request, reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .begin_or_replay(request),
                );
                let _ = reply.send(result);
            }
            StoreCommand::CompareAndSwapJournal(key, epoch, replacement, reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .compare_and_swap_journal(&key, epoch, replacement),
                );
                let _ = reply.send(result);
            }
            StoreCommand::WriteTerminal(write, reply) => {
                let result =
                    runtime.block_on(database.git_index_transaction_store().write_terminal(write));
                let _ = reply.send(result);
            }
            StoreCommand::RecoveryCandidates(repository_id, reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .recovery_candidates(&repository_id),
                );
                let _ = reply.send(result);
            }
            StoreCommand::RecoveryRepositories(reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .recovery_repositories(),
                );
                let _ = reply.send(result);
            }
            StoreCommand::QuarantineRepository(repository_id, transaction_id, reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .quarantine_repository(&repository_id, &transaction_id),
                );
                let _ = reply.send(result);
            }
            StoreCommand::ClearRepositoryQuarantine(
                repository_id,
                transaction_id,
                receipt,
                reply,
            ) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .clear_repository_quarantine(&repository_id, &transaction_id, receipt),
                );
                let _ = reply.send(result);
            }
        }
    }
}

/// Shared handle to the one daemon-owned store actor for a project database.
///
/// This local newtype exists so the foreign `GitIndexTransactionStore` trait
/// can be implemented for a shareable handle without violating orphan rules
/// around `Arc<T>`.
#[derive(Clone)]
pub(crate) struct SharedDaemonGitIndexTransactionStore {
    inner: Arc<DaemonGitIndexTransactionStore>,
}

impl SharedDaemonGitIndexTransactionStore {
    pub(crate) fn from_arc(inner: Arc<DaemonGitIndexTransactionStore>) -> Self {
        Self { inner }
    }
}

impl GitIndexTransactionStore for SharedDaemonGitIndexTransactionStore {
    fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()> {
        self.inner.save_preview(preview)
    }

    fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        self.inner.read_preview(preview_id)
    }

    fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1> {
        self.inner.begin_or_replay(request)
    }

    fn compare_and_swap_journal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        expected_phase_epoch: u64,
        replacement: GitIndexTransactionJournalV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionJournalV1> {
        self.inner
            .compare_and_swap_journal(idempotency_key, expected_phase_epoch, replacement)
    }

    fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionReceiptV1> {
        self.inner.write_terminal(write)
    }

    fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>> {
        self.inner.recovery_candidates(repository_id)
    }

    fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>> {
        self.inner.recovery_repositories()
    }

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
    ) -> GitIndexTransactionStoreResult<()> {
        self.inner
            .quarantine_repository(repository_id, transaction_id)
    }

    fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
        recovery_receipt: GitIndexTransactionReceiptV1,
    ) -> GitIndexTransactionStoreResult<()> {
        self.inner
            .clear_repository_quarantine(repository_id, transaction_id, recovery_receipt)
    }
}

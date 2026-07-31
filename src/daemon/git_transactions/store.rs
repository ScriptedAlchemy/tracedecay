//! Bounded synchronous bridge to the registered session-store runtime.
//!
//! The application Git port is deliberately synchronous because its native
//! executor is synchronous.  Calling an async database through `block_on` on
//! a Tokio worker would pin that worker while an `IMMEDIATE` writer waits. This
//! adapter instead owns one bounded actor thread; the actor owns the async
//! rusqlite-runtime calls and every synchronous port call receives exactly one reply.
//! It has no filesystem path and cannot create a JSON side-file authority.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracedecay_domain::{
    GitIndexIdempotencyKey, GitIndexPreviewId, GitIndexPreviewV1, GitIndexTransactionId,
    GitIndexTransactionJournalV1, GitIndexTransactionReceiptV1, RepositoryId,
};
use tracedecay_store::{
    CodeReadOperationV1, CodeReadResultV1, CodeRecoveryCandidatesQueryV1,
    CodeRecoveryRepositoriesQueryV1, GitIndexTransactionBeginRequestV1,
    GitIndexTransactionBeginResultV1, GitIndexTransactionRecordV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError, GitIndexTransactionStoreResult,
    GitIndexTransactionTerminalWriteV1,
};

#[cfg(test)]
use crate::db::engine::TestConnection;
use crate::global_db::{
    GitIndexReadExecutor, GlobalDbGitIndexTransactionStore, RegisteredGlobalDb,
    ensure_git_index_transaction_schema,
};

/// The actor queue is intentionally finite: saturation fails closed instead of
/// accumulating unbounded mutation work while a durable writer is stalled.
const GIT_INDEX_TRANSACTION_STORE_ACTOR_CAPACITY: usize = 64;
// Keep the sync port bounded by the same five-second writer wait used by
// `RegisteredGlobalDb`; callers can reconcile durable state after an unavailable result
// instead of pinning a daemon worker forever.
const GIT_INDEX_TRANSACTION_STORE_ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

type Reply<T> = SyncSender<GitIndexTransactionStoreResult<T>>;

enum StoreCommand {
    SavePreview(GitIndexPreviewV1, Reply<()>),
    ReadCode(CodeReadOperationV1, Reply<CodeReadResultV1>),
    BeginOrReplay(
        Box<GitIndexTransactionBeginRequestV1>,
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
    QuarantineRepository(RepositoryId, GitIndexTransactionId, Reply<()>),
    ClearRepositoryQuarantine(
        RepositoryId,
        GitIndexTransactionId,
        GitIndexTransactionReceiptV1,
        Reply<()>,
    ),
}

/// Synchronous `tracedecay-store` contract adapter over one already-open,
/// canonical registered project session database.
///
/// Dropping the last adapter closes the command channel and lets the dedicated
/// actor exit. It intentionally has no `Clone` implementation: one daemon
/// service owns one bounded queue and actor for its transaction authority.
pub(crate) struct DaemonGitIndexTransactionStore {
    commands: Mutex<Option<SyncSender<StoreCommand>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

enum ActorDatabase {
    Registered(Arc<RegisteredGlobalDb>),
    #[cfg(test)]
    Engine(Box<TestConnection>),
}

impl ActorDatabase {
    fn git_index_transaction_store(&self) -> GlobalDbGitIndexTransactionStore<'_> {
        match self {
            Self::Registered(database) => database.git_index_transaction_store(),
            #[cfg(test)]
            Self::Engine(database) => GlobalDbGitIndexTransactionStore::for_engine_test(database),
        }
    }

    async fn ensure_schema(&self) -> GitIndexTransactionStoreResult<()> {
        match self {
            Self::Registered(database) => {
                let transaction = database
                    .begin_write_transaction()
                    .await
                    .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
                ensure_git_index_transaction_schema(&transaction)
                    .await
                    .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| GitIndexTransactionStoreError::Unavailable)
            }
            #[cfg(test)]
            Self::Engine(database) => {
                let transaction = database
                    .transaction_with_behavior(crate::db::engine::TransactionBehavior::Immediate)
                    .await
                    .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
                ensure_git_index_transaction_schema(&transaction)
                    .await
                    .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| GitIndexTransactionStoreError::Unavailable)
            }
        }
    }
}

impl DaemonGitIndexTransactionStore {
    pub(crate) fn open(database: Arc<RegisteredGlobalDb>) -> GitIndexTransactionStoreResult<Self> {
        Self::open_actor(ActorDatabase::Registered(database))
    }

    #[cfg(test)]
    pub(crate) fn open_engine_test(
        database: TestConnection,
    ) -> GitIndexTransactionStoreResult<Self> {
        Self::open_actor(ActorDatabase::Engine(Box::new(database)))
    }

    fn open_actor(database: ActorDatabase) -> GitIndexTransactionStoreResult<Self> {
        let (commands, receiver) = sync_channel(GIT_INDEX_TRANSACTION_STORE_ACTOR_CAPACITY);
        let (ready, started) = sync_channel::<GitIndexTransactionStoreResult<()>>(1);
        let worker = std::thread::Builder::new()
            .name("tracedecay-git-index-store".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = ready.send(Err(GitIndexTransactionStoreError::Unavailable));
                    return;
                };
                let schema = runtime.block_on(database.ensure_schema());
                let schema_ready = schema.is_ok();
                if ready.send(schema).is_err() || !schema_ready {
                    return;
                }
                run_store_actor(&runtime, &database, &receiver);
            })
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        let startup = started
            .recv_timeout(GIT_INDEX_TRANSACTION_STORE_ACTOR_TIMEOUT)
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)
            .and_then(|result| result);
        if let Err(error) = startup {
            drop(commands);
            let _ = worker.join();
            return Err(error);
        }
        Ok(Self {
            commands: Mutex::new(Some(commands)),
            worker: Mutex::new(Some(worker)),
        })
    }

    fn submit(&self, command: StoreCommand) -> GitIndexTransactionStoreResult<()> {
        self.commands
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?
            .as_ref()
            .ok_or(GitIndexTransactionStoreError::Unavailable)?
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    GitIndexTransactionStoreError::Unavailable
                }
            })
    }

    fn await_reply<T>(
        receiver: &Receiver<GitIndexTransactionStoreResult<T>>,
    ) -> GitIndexTransactionStoreResult<T> {
        receiver
            .recv_timeout(GIT_INDEX_TRANSACTION_STORE_ACTOR_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected => {
                    GitIndexTransactionStoreError::Unavailable
                }
            })?
    }

    fn execute_code_read(
        &self,
        operation: CodeReadOperationV1,
    ) -> GitIndexTransactionStoreResult<CodeReadResultV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadCode(operation, reply))?;
        Self::await_reply(&receiver)
    }
}

impl Drop for DaemonGitIndexTransactionStore {
    fn drop(&mut self) {
        if let Ok(commands) = self.commands.get_mut() {
            commands.take();
        }
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

impl GitIndexTransactionStore for DaemonGitIndexTransactionStore {
    fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::SavePreview(preview, reply))?;
        Self::await_reply(&receiver)
    }

    fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        match self.execute_code_read(CodeReadOperationV1::Preview(preview_id.clone()))? {
            CodeReadResultV1::Preview(preview) => Ok(*preview),
            _ => Err(GitIndexTransactionStoreError::Unavailable),
        }
    }

    fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::BeginOrReplay(Box::new(request), reply))?;
        Self::await_reply(&receiver)
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
        Self::await_reply(&receiver)
    }

    fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionReceiptV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::WriteTerminal(write, reply))?;
        Self::await_reply(&receiver)
    }

    fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>> {
        match self.execute_code_read(CodeReadOperationV1::RecoveryCandidates(
            CodeRecoveryCandidatesQueryV1 {
                repository_id: repository_id.clone(),
                after: None,
                limit: u32::MAX,
            },
        ))? {
            CodeReadResultV1::RecoveryCandidates(page) => Ok(page.records),
            _ => Err(GitIndexTransactionStoreError::Unavailable),
        }
    }

    fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>> {
        match self.execute_code_read(CodeReadOperationV1::RecoveryRepositories(
            CodeRecoveryRepositoriesQueryV1 {
                after: None,
                limit: u32::MAX,
            },
        ))? {
            CodeReadResultV1::RecoveryRepositories(page) => Ok(page.repositories),
            _ => Err(GitIndexTransactionStoreError::Unavailable),
        }
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
        Self::await_reply(&receiver)
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
        Self::await_reply(&receiver)
    }
}

fn run_store_actor(
    runtime: &tokio::runtime::Runtime,
    database: &ActorDatabase,
    receiver: &Receiver<StoreCommand>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            StoreCommand::SavePreview(preview, reply) => {
                let result =
                    runtime.block_on(database.git_index_transaction_store().save_preview(preview));
                let _ = reply.send(result);
            }
            StoreCommand::ReadCode(operation, reply) => {
                let store = database.git_index_transaction_store();
                let executor = GitIndexReadExecutor::new(&store);
                let result = runtime.block_on(executor.execute_read(&operation));
                let _ = reply.send(result);
            }
            StoreCommand::BeginOrReplay(request, reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .begin_or_replay(*request),
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

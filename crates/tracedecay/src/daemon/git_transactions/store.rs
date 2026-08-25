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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_domain::{
    GitIndexIdempotencyKey, GitIndexPreviewId, GitIndexPreviewInputV1, GitIndexPreviewV1,
    GitIndexTransactionId, GitIndexTransactionJournalV1, GitIndexTransactionReceiptV1,
    RepositoryId, UtcMicros,
};
use tracedecay_store::{
    CodeReadOperationV1, CodeReadResultV1, CodeRecoveryCandidatesQueryV1,
    CodeRecoveryRepositoriesQueryV1, GitIndexPreviewInputReadV1, GitIndexTransactionBeginRequestV1,
    GitIndexTransactionBeginResultV1, GitIndexTransactionRecordV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError, GitIndexTransactionStoreResult,
    GitIndexTransactionTerminalWriteV1, MAX_GIT_INDEX_PREVIEW_INPUT_GC_BATCH,
};

use crate::global_db::{
    GitIndexReadExecutor, GlobalDbGitIndexTransactionStore, RegisteredGlobalDbLeaseV1,
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
    SavePreviewInput(GitIndexPreviewInputV1, Reply<()>),
    ReadPreviewInput(
        GitIndexPreviewId,
        UtcMicros,
        Reply<GitIndexPreviewInputReadV1>,
    ),
    PurgeExpiredPreviewInputs(UtcMicros, usize, Reply<usize>),
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
    Registered {
        database: RegisteredGlobalDbLeaseV1,
        #[cfg(test)]
        gc_observer: Option<Arc<PreviewGcTestObserver>>,
    },
}

impl ActorDatabase {
    fn git_index_transaction_store(&self) -> GlobalDbGitIndexTransactionStore<'_> {
        match self {
            Self::Registered { database, .. } => database.git_index_transaction_store(),
        }
    }

    async fn next_live_preview_input_expiry(
        &self,
    ) -> GitIndexTransactionStoreResult<Option<UtcMicros>> {
        self.git_index_transaction_store()
            .next_live_preview_input_expiry()
            .await
    }

    async fn purge_expired_preview_inputs(
        &self,
        observed_at: UtcMicros,
        limit: usize,
    ) -> GitIndexTransactionStoreResult<crate::global_db::GitIndexPreviewInputGcResult> {
        #[cfg(test)]
        if let Self::Registered {
            gc_observer: Some(observer),
            ..
        } = self
        {
            observer
                .purge_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if observer
                .fail_purge
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(GitIndexTransactionStoreError::Unavailable);
            }
        }
        self.git_index_transaction_store()
            .purge_expired_preview_inputs_and_next(observed_at, limit)
            .await
    }
}

#[cfg(test)]
#[derive(Default)]
struct PreviewGcTestObserver {
    purge_attempts: std::sync::atomic::AtomicUsize,
    fail_purge: std::sync::atomic::AtomicBool,
}

impl DaemonGitIndexTransactionStore {
    pub(crate) fn open(
        database: RegisteredGlobalDbLeaseV1,
    ) -> GitIndexTransactionStoreResult<Self> {
        Self::open_actor(ActorDatabase::Registered {
            database,
            #[cfg(test)]
            gc_observer: None,
        })
    }

    #[cfg(test)]
    fn open_with_gc_observer(
        database: RegisteredGlobalDbLeaseV1,
        observer: Arc<PreviewGcTestObserver>,
    ) -> GitIndexTransactionStoreResult<Self> {
        Self::open_actor(ActorDatabase::Registered {
            database,
            gc_observer: Some(observer),
        })
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
                let next_expiry = runtime.block_on(database.next_live_preview_input_expiry());
                let Ok(next_expiry) = next_expiry else {
                    let _ = ready.send(Err(GitIndexTransactionStoreError::Unavailable));
                    return;
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                run_store_actor(&runtime, &database, &receiver, next_expiry);
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

    pub(crate) fn shutdown(&self) -> GitIndexTransactionStoreResult<bool> {
        self.commands
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?
            .take();
        let worker = self
            .worker
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?
            .take();
        let Some(worker) = worker else {
            return Ok(false);
        };
        worker
            .join()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        Ok(true)
    }
}

impl Drop for DaemonGitIndexTransactionStore {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl GitIndexTransactionStore for DaemonGitIndexTransactionStore {
    fn save_preview_input(
        &self,
        input: GitIndexPreviewInputV1,
    ) -> GitIndexTransactionStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::SavePreviewInput(input, reply))?;
        Self::await_reply(&receiver)
    }

    fn read_preview_input(
        &self,
        preview_id: &GitIndexPreviewId,
        observed_at: UtcMicros,
    ) -> GitIndexTransactionStoreResult<GitIndexPreviewInputReadV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadPreviewInput(
            preview_id.clone(),
            observed_at,
            reply,
        ))?;
        Self::await_reply(&receiver)
    }

    fn purge_expired_preview_inputs(
        &self,
        observed_at: UtcMicros,
        limit: usize,
    ) -> GitIndexTransactionStoreResult<usize> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::PurgeExpiredPreviewInputs(
            observed_at,
            limit,
            reply,
        ))?;
        Self::await_reply(&receiver)
    }

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

    fn read_record(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexTransactionRecordV1>> {
        match self.execute_code_read(CodeReadOperationV1::TransactionRecord(
            idempotency_key.clone(),
        ))? {
            CodeReadResultV1::TransactionRecord(record) => Ok(*record),
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
    mut next_expiry: Option<UtcMicros>,
) {
    let mut gc_due = false;
    loop {
        let timeout = if gc_due {
            None
        } else if let Some(expires_at) = next_expiry {
            let observed_at = match current_utc_micros() {
                Ok(observed_at) => observed_at,
                Err(_) => match receiver.recv() {
                    Ok(command) => {
                        reject_store_command(command);
                        continue;
                    }
                    Err(_) => break,
                },
            };
            Some(duration_until(expires_at, observed_at))
        } else {
            None
        };
        let command = match timeout {
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => None,
            },
            None => match receiver.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            },
        };

        let observed_at = match current_utc_micros() {
            Ok(observed_at) => observed_at,
            Err(_) => {
                if let Some(command) = command {
                    reject_store_command(command);
                }
                continue;
            }
        };
        gc_due |= next_expiry.is_some_and(|expires_at| expires_at <= observed_at);
        if gc_due {
            match runtime.block_on(
                database.purge_expired_preview_inputs(
                    observed_at,
                    MAX_GIT_INDEX_PREVIEW_INPUT_GC_BATCH,
                ),
            ) {
                Ok(result) => {
                    next_expiry = result.next_expiry;
                    gc_due = false;
                }
                Err(error) => {
                    tracing::warn!(%error, "git index preview input GC failed");
                    if let Some(command) = command {
                        reject_store_command(command);
                    }
                    continue;
                }
            }
        }
        let Some(command) = command else {
            continue;
        };
        match command {
            StoreCommand::SavePreviewInput(input, reply) => {
                let expires_at = input.expires_at;
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .save_preview_input(input),
                );
                if result.is_ok() && next_expiry.is_none_or(|current| expires_at < current) {
                    next_expiry = Some(expires_at);
                }
                let _ = reply.send(result);
            }
            StoreCommand::ReadPreviewInput(preview_id, observed_at, reply) => {
                let result = runtime.block_on(
                    database
                        .git_index_transaction_store()
                        .read_preview_input(&preview_id, observed_at),
                );
                let _ = reply.send(result);
            }
            StoreCommand::PurgeExpiredPreviewInputs(observed_at, limit, reply) => {
                let result =
                    runtime.block_on(database.purge_expired_preview_inputs(observed_at, limit));
                match result {
                    Ok(result) => {
                        next_expiry = result.next_expiry;
                        let _ = reply.send(Ok(result.purged));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
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

fn duration_until(expires_at: UtcMicros, observed_at: UtcMicros) -> Duration {
    if expires_at <= observed_at {
        return Duration::ZERO;
    }
    Duration::from_micros(expires_at.0.saturating_sub(observed_at.0).unsigned_abs())
}

fn reject_store_command(command: StoreCommand) {
    let error = || GitIndexTransactionStoreError::Unavailable;
    match command {
        StoreCommand::SavePreviewInput(_, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::ReadPreviewInput(_, _, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::PurgeExpiredPreviewInputs(_, _, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::SavePreview(_, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::ReadCode(_, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::BeginOrReplay(_, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::CompareAndSwapJournal(_, _, _, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::WriteTerminal(_, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::QuarantineRepository(_, _, reply) => {
            let _ = reply.send(Err(error()));
        }
        StoreCommand::ClearRepositoryQuarantine(_, _, _, reply) => {
            let _ = reply.send(Err(error()));
        }
    }
}

fn current_utc_micros() -> GitIndexTransactionStoreResult<UtcMicros> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    let micros = i64::try_from(elapsed.as_micros())
        .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
    Ok(UtcMicros(micros))
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

    pub(crate) fn shutdown(&self) -> GitIndexTransactionStoreResult<bool> {
        self.inner.shutdown()
    }
}

impl GitIndexTransactionStore for SharedDaemonGitIndexTransactionStore {
    fn save_preview_input(
        &self,
        input: GitIndexPreviewInputV1,
    ) -> GitIndexTransactionStoreResult<()> {
        self.inner.save_preview_input(input)
    }

    fn read_preview_input(
        &self,
        preview_id: &GitIndexPreviewId,
        observed_at: UtcMicros,
    ) -> GitIndexTransactionStoreResult<GitIndexPreviewInputReadV1> {
        self.inner.read_preview_input(preview_id, observed_at)
    }

    fn purge_expired_preview_inputs(
        &self,
        observed_at: UtcMicros,
        limit: usize,
    ) -> GitIndexTransactionStoreResult<usize> {
        self.inner.purge_expired_preview_inputs(observed_at, limit)
    }

    fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()> {
        self.inner.save_preview(preview)
    }

    fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        self.inner.read_preview(preview_id)
    }

    fn read_record(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexTransactionRecordV1>> {
        self.inner.read_record(idempotency_key)
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

#[cfg(test)]
mod gc_tests {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::global_db::tests::harness::RegisteredGlobalDbHarness;

    fn registered_database(label: &str) -> RegisteredGlobalDbHarness {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("registered fixture runtime");
        runtime.block_on(RegisteredGlobalDbHarness::open(label))
    }

    fn store_with_gc_observer(
        label: &str,
        observer: Arc<PreviewGcTestObserver>,
    ) -> (RegisteredGlobalDbHarness, DaemonGitIndexTransactionStore) {
        let database = registered_database(label);
        let store = DaemonGitIndexTransactionStore::open_with_gc_observer(
            database.registered.clone(),
            observer,
        )
        .expect("registered store actor");
        (database, store)
    }

    fn preview_input(
        suffix: &str,
        created_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> GitIndexPreviewInputV1 {
        let template =
            super::super::test_support::preview_input(&super::super::test_support::preview());
        GitIndexPreviewInputV1::new_commit(
            GitIndexPreviewId::new(format!("preview.gc.{suffix}")).expect("preview id"),
            template.repository_snapshot,
            template.commit_intent.expect("commit intent"),
            created_at,
            expires_at,
        )
        .expect("preview input")
    }

    fn wait_for_attempts(observer: &PreviewGcTestObserver, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while observer.purge_attempts.load(Ordering::SeqCst) < expected && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            observer.purge_attempts.load(Ordering::SeqCst),
            expected,
            "expected bounded preview GC attempt"
        );
    }

    #[test]
    fn empty_store_has_zero_gc_writes_over_time() {
        let observer = Arc::new(PreviewGcTestObserver::default());
        let (_database, _store) =
            store_with_gc_observer("git-index-gc-empty", Arc::clone(&observer));

        std::thread::sleep(Duration::from_millis(150));

        assert_eq!(observer.purge_attempts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn one_expiry_causes_one_bounded_purge_and_then_sleeps() {
        let observer = Arc::new(PreviewGcTestObserver::default());
        let (_database, store) =
            store_with_gc_observer("git-index-gc-one-expiry", Arc::clone(&observer));
        let created_at = current_utc_micros().expect("clock");
        let input = preview_input(
            "one",
            created_at,
            UtcMicros(created_at.0.saturating_add(100_000)),
        );
        let preview_id = input.preview_id.clone();
        store.save_preview_input(input).expect("save preview input");

        wait_for_attempts(&observer, 1);
        std::thread::sleep(Duration::from_millis(150));

        assert_eq!(observer.purge_attempts.load(Ordering::SeqCst), 1);
        assert!(matches!(
            store
                .read_preview_input(&preview_id, current_utc_micros().expect("clock"))
                .expect("read tombstone"),
            GitIndexPreviewInputReadV1::Expired {
                purged_at: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn save_command_wake_lowers_the_known_expiry_deadline() {
        let observer = Arc::new(PreviewGcTestObserver::default());
        let (_database, store) = store_with_gc_observer("git-index-gc-wake", Arc::clone(&observer));
        let created_at = current_utc_micros().expect("clock");
        store
            .save_preview_input(preview_input(
                "later",
                created_at,
                UtcMicros(created_at.0.saturating_add(2_000_000)),
            ))
            .expect("save later input");
        store
            .save_preview_input(preview_input(
                "sooner",
                created_at,
                UtcMicros(created_at.0.saturating_add(100_000)),
            ))
            .expect("save sooner input");

        wait_for_attempts(&observer, 1);

        assert_eq!(observer.purge_attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn restart_reconstructs_expired_cleanup_deadline() {
        let database = registered_database("git-index-gc-restart");
        let now = current_utc_micros().expect("clock");
        // The input is created durable and already expired, and the daemon
        // actor that wrote it is gone: only the restarted actor can
        // reconstruct the cleanup deadline. Seeding through the registered
        // store keeps the pre-restart daemon GC from racing the fixture.
        let created_at = UtcMicros(now.0.saturating_sub(1_000_000));
        let input = preview_input(
            "restart",
            created_at,
            UtcMicros(created_at.0.saturating_add(100_000)),
        );
        let preview_id = input.preview_id.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("seed runtime");
        runtime
            .block_on(
                database
                    .registered
                    .git_index_transaction_store()
                    .save_preview_input(input),
            )
            .expect("save preview input");

        let observer = Arc::new(PreviewGcTestObserver::default());
        let store = DaemonGitIndexTransactionStore::open_with_gc_observer(
            database.registered.clone(),
            Arc::clone(&observer),
        )
        .expect("restarted registered store actor");
        wait_for_attempts(&observer, 1);

        assert!(matches!(
            store
                .read_preview_input(&preview_id, current_utc_micros().expect("clock"))
                .expect("read restart tombstone"),
            GitIndexPreviewInputReadV1::Expired {
                purged_at: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn failed_due_gc_waits_for_command_and_never_fabricates_success() {
        let observer = Arc::new(PreviewGcTestObserver::default());
        observer.fail_purge.store(true, Ordering::SeqCst);
        let (_database, store) =
            store_with_gc_observer("git-index-gc-failure", Arc::clone(&observer));
        let created_at = current_utc_micros().expect("clock");
        let input = preview_input(
            "failure",
            created_at,
            UtcMicros(created_at.0.saturating_add(100_000)),
        );
        let preview_id = input.preview_id.clone();
        store.save_preview_input(input).expect("save preview input");
        wait_for_attempts(&observer, 1);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(observer.purge_attempts.load(Ordering::SeqCst), 1);

        assert_eq!(
            store.read_preview_input(&preview_id, current_utc_micros().expect("clock")),
            Err(GitIndexTransactionStoreError::Unavailable)
        );
        assert_eq!(observer.purge_attempts.load(Ordering::SeqCst), 2);

        observer.fail_purge.store(false, Ordering::SeqCst);
        assert!(matches!(
            store
                .read_preview_input(&preview_id, current_utc_micros().expect("clock"))
                .expect("retry purge before command"),
            GitIndexPreviewInputReadV1::Expired {
                purged_at: Some(_),
                ..
            }
        ));
        assert_eq!(observer.purge_attempts.load(Ordering::SeqCst), 3);
    }
}

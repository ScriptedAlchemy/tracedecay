//! Bounded synchronous bridge to the registered session-store runtime.
//!
//! The native-integration transaction coordinator is deliberately synchronous
//! because its native Git mechanics are synchronous. Calling an async database
//! through `block_on` on a Tokio worker would pin that worker while an
//! `IMMEDIATE` writer waits, so this adapter owns one bounded actor thread;
//! the actor owns the async rusqlite-runtime calls and every synchronous port
//! call receives exactly one reply. It has no filesystem path and cannot
//! create a side-file authority.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracedecay_domain::{
    NativeIntegrationApprovalId, NativeIntegrationApprovalV1, NativeIntegrationPreviewId,
    NativeIntegrationPreviewV1, NativeIntegrationReceiptV1, NativeIntegrationTransactionId,
    NativeIntegrationTransactionStatusV1, RepositoryId,
};
use tracedecay_store::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStore,
    NativeIntegrationStoreError, NativeIntegrationStoreResult,
};

#[cfg(test)]
use crate::db::engine::TestConnection;
use crate::global_db::{GlobalDbNativeIntegrationStore, RegisteredGlobalDb};

/// The actor queue is intentionally finite: saturation fails closed instead of
/// accumulating unbounded mutation work while a durable writer is stalled.
const NATIVE_INTEGRATION_STORE_ACTOR_CAPACITY: usize = 64;
// Keep the sync port bounded by the same five-second writer wait used by
// `RegisteredGlobalDb`; callers can reconcile durable state after an
// unavailable result instead of pinning a daemon worker forever.
const NATIVE_INTEGRATION_STORE_ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

type Reply<T> = SyncSender<NativeIntegrationStoreResult<T>>;

enum StoreCommand {
    SavePreview(Box<NativeIntegrationPreviewV1>, Reply<()>),
    ReadPreview(
        NativeIntegrationPreviewId,
        Reply<Option<NativeIntegrationPreviewV1>>,
    ),
    SaveApproval(Box<NativeIntegrationApprovalV1>, Reply<()>),
    ReadApproval(
        NativeIntegrationApprovalId,
        Reply<Option<NativeIntegrationApprovalV1>>,
    ),
    BeginOrReplay(
        Box<NativeIntegrationRecordV1>,
        Reply<NativeIntegrationBeginResultV1>,
    ),
    ReadStatus(
        NativeIntegrationTransactionId,
        Reply<Option<NativeIntegrationTransactionStatusV1>>,
    ),
    ReadRecord(
        NativeIntegrationTransactionId,
        Reply<Option<NativeIntegrationRecordV1>>,
    ),
    ReadReceipt(
        NativeIntegrationTransactionId,
        Reply<Option<NativeIntegrationReceiptV1>>,
    ),
    CompareAndSwapStatus(
        NativeIntegrationTransactionId,
        u64,
        Box<NativeIntegrationTransactionStatusV1>,
        Reply<NativeIntegrationTransactionStatusV1>,
    ),
    WriteTerminal(
        NativeIntegrationTransactionId,
        u64,
        Box<NativeIntegrationReceiptV1>,
        Reply<NativeIntegrationReceiptV1>,
    ),
    PendingTransactions(Option<RepositoryId>, Reply<Vec<NativeIntegrationRecordV1>>),
    ApprovalConsumed(NativeIntegrationApprovalId, Reply<bool>),
    QuarantineRepository(RepositoryId, NativeIntegrationTransactionId, Reply<()>),
}

enum ActorDatabase {
    Registered(Arc<RegisteredGlobalDb>),
    #[cfg(test)]
    Engine(Box<TestConnection>),
}

impl ActorDatabase {
    fn native_integration_store(&self) -> GlobalDbNativeIntegrationStore<'_> {
        match self {
            Self::Registered(database) => GlobalDbNativeIntegrationStore::new(database),
            #[cfg(test)]
            Self::Engine(database) => GlobalDbNativeIntegrationStore::for_engine_test(database),
        }
    }
}

/// Synchronous `tracedecay-store` contract adapter over one already-open,
/// canonical registered project session database.
///
/// Dropping the last adapter closes the command channel and lets the dedicated
/// actor exit. It intentionally has no `Clone` implementation: one daemon
/// owner holds one bounded queue and actor for its transaction authority;
/// sharing goes through [`SharedDaemonNativeIntegrationStore`].
pub(crate) struct DaemonNativeIntegrationStore {
    commands: Mutex<Option<SyncSender<StoreCommand>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DaemonNativeIntegrationStore {
    pub(crate) fn open(database: Arc<RegisteredGlobalDb>) -> NativeIntegrationStoreResult<Self> {
        Self::open_actor(ActorDatabase::Registered(database))
    }

    #[cfg(test)]
    pub(crate) fn open_engine_test(database: TestConnection) -> NativeIntegrationStoreResult<Self> {
        Self::open_actor(ActorDatabase::Engine(Box::new(database)))
    }

    fn open_actor(database: ActorDatabase) -> NativeIntegrationStoreResult<Self> {
        let (commands, receiver) = sync_channel(NATIVE_INTEGRATION_STORE_ACTOR_CAPACITY);
        let (ready, started) = sync_channel::<NativeIntegrationStoreResult<()>>(1);
        let worker = std::thread::Builder::new()
            .name("tracedecay-native-integration-store".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let Ok(runtime) = runtime else {
                    let _ = ready.send(Err(NativeIntegrationStoreError::Unavailable));
                    return;
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                run_store_actor(&runtime, &database, &receiver);
            })
            .map_err(|_| NativeIntegrationStoreError::Unavailable)?;
        let startup = started
            .recv_timeout(NATIVE_INTEGRATION_STORE_ACTOR_TIMEOUT)
            .map_err(|_| NativeIntegrationStoreError::Unavailable)
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

    fn submit(&self, command: StoreCommand) -> NativeIntegrationStoreResult<()> {
        self.commands
            .lock()
            .map_err(|_| NativeIntegrationStoreError::Unavailable)?
            .as_ref()
            .ok_or(NativeIntegrationStoreError::Unavailable)?
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    NativeIntegrationStoreError::Unavailable
                }
            })
    }

    fn await_reply<T>(
        receiver: &Receiver<NativeIntegrationStoreResult<T>>,
    ) -> NativeIntegrationStoreResult<T> {
        receiver
            .recv_timeout(NATIVE_INTEGRATION_STORE_ACTOR_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected => {
                    NativeIntegrationStoreError::Unavailable
                }
            })?
    }

    /// Persists one issued approval commitment (approval issuance operation).
    pub(crate) fn save_approval(
        &self,
        approval: NativeIntegrationApprovalV1,
    ) -> NativeIntegrationStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::SaveApproval(Box::new(approval), reply))?;
        Self::await_reply(&receiver)
    }

    /// Reads one issued approval commitment for apply resolution.
    pub(crate) fn read_approval(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationApprovalV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadApproval(approval_id.clone(), reply))?;
        Self::await_reply(&receiver)
    }

    pub(crate) fn shutdown(&self) -> NativeIntegrationStoreResult<bool> {
        self.commands
            .lock()
            .map_err(|_| NativeIntegrationStoreError::Unavailable)?
            .take();
        let worker = self
            .worker
            .lock()
            .map_err(|_| NativeIntegrationStoreError::Unavailable)?
            .take();
        let Some(worker) = worker else {
            return Ok(false);
        };
        worker
            .join()
            .map_err(|_| NativeIntegrationStoreError::Unavailable)?;
        Ok(true)
    }
}

impl Drop for DaemonNativeIntegrationStore {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl NativeIntegrationStore for DaemonNativeIntegrationStore {
    fn save_preview(
        &self,
        preview: NativeIntegrationPreviewV1,
    ) -> NativeIntegrationStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::SavePreview(Box::new(preview), reply))?;
        Self::await_reply(&receiver)
    }

    fn read_preview(
        &self,
        preview_id: &NativeIntegrationPreviewId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationPreviewV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadPreview(preview_id.clone(), reply))?;
        Self::await_reply(&receiver)
    }

    fn begin_or_replay(
        &self,
        record: NativeIntegrationRecordV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::BeginOrReplay(Box::new(record), reply))?;
        Self::await_reply(&receiver)
    }

    fn read_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadStatus(transaction_id.clone(), reply))?;
        Self::await_reply(&receiver)
    }

    fn read_record(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadRecord(transaction_id.clone(), reply))?;
        Self::await_reply(&receiver)
    }

    fn read_receipt(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationReceiptV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ReadReceipt(transaction_id.clone(), reply))?;
        Self::await_reply(&receiver)
    }

    fn compare_and_swap_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        replacement: NativeIntegrationTransactionStatusV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationTransactionStatusV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::CompareAndSwapStatus(
            transaction_id.clone(),
            expected_phase_revision,
            Box::new(replacement),
            reply,
        ))?;
        Self::await_reply(&receiver)
    }

    fn write_terminal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        receipt: NativeIntegrationReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationReceiptV1> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::WriteTerminal(
            transaction_id.clone(),
            expected_phase_revision,
            Box::new(receipt),
            reply,
        ))?;
        Self::await_reply(&receiver)
    }

    fn pending_transactions(
        &self,
        repository_id: Option<&RepositoryId>,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::PendingTransactions(
            repository_id.cloned(),
            reply,
        ))?;
        Self::await_reply(&receiver)
    }

    fn approval_consumed(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<bool> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::ApprovalConsumed(approval_id.clone(), reply))?;
        Self::await_reply(&receiver)
    }

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()> {
        let (reply, receiver) = sync_channel(1);
        self.submit(StoreCommand::QuarantineRepository(
            repository_id.clone(),
            transaction_id.clone(),
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
        let store = database.native_integration_store();
        match command {
            StoreCommand::SavePreview(preview, reply) => {
                let _ = reply.send(runtime.block_on(store.save_preview(*preview)));
            }
            StoreCommand::ReadPreview(preview_id, reply) => {
                let _ = reply.send(runtime.block_on(store.read_preview(&preview_id)));
            }
            StoreCommand::SaveApproval(approval, reply) => {
                let _ = reply.send(runtime.block_on(store.save_approval(*approval)));
            }
            StoreCommand::ReadApproval(approval_id, reply) => {
                let _ = reply.send(runtime.block_on(store.read_approval(&approval_id)));
            }
            StoreCommand::BeginOrReplay(record, reply) => {
                let _ = reply.send(runtime.block_on(store.begin_or_replay(*record)));
            }
            StoreCommand::ReadStatus(transaction_id, reply) => {
                let _ = reply.send(runtime.block_on(store.read_status(&transaction_id)));
            }
            StoreCommand::ReadRecord(transaction_id, reply) => {
                let _ = reply.send(runtime.block_on(store.read_record(&transaction_id)));
            }
            StoreCommand::ReadReceipt(transaction_id, reply) => {
                let _ = reply.send(runtime.block_on(store.read_receipt(&transaction_id)));
            }
            StoreCommand::CompareAndSwapStatus(
                transaction_id,
                expected_phase_revision,
                replacement,
                reply,
            ) => {
                let _ = reply.send(runtime.block_on(store.compare_and_swap_status(
                    &transaction_id,
                    expected_phase_revision,
                    *replacement,
                )));
            }
            StoreCommand::WriteTerminal(
                transaction_id,
                expected_phase_revision,
                receipt,
                reply,
            ) => {
                let _ = reply.send(runtime.block_on(store.write_terminal(
                    &transaction_id,
                    expected_phase_revision,
                    *receipt,
                )));
            }
            StoreCommand::PendingTransactions(repository_id, reply) => {
                let _ = reply
                    .send(runtime.block_on(store.pending_transactions(repository_id.as_ref())));
            }
            StoreCommand::ApprovalConsumed(approval_id, reply) => {
                let _ = reply.send(runtime.block_on(store.approval_consumed(&approval_id)));
            }
            StoreCommand::QuarantineRepository(repository_id, transaction_id, reply) => {
                let _ =
                    reply
                        .send(runtime.block_on(
                            store.quarantine_repository(&repository_id, &transaction_id),
                        ));
            }
        }
    }
}

/// Cloneable handle over the one retained store actor for a project database.
#[derive(Clone)]
pub(crate) struct SharedDaemonNativeIntegrationStore {
    inner: Arc<DaemonNativeIntegrationStore>,
}

impl SharedDaemonNativeIntegrationStore {
    pub(crate) fn from_arc(inner: Arc<DaemonNativeIntegrationStore>) -> Self {
        Self { inner }
    }

    pub(crate) fn shutdown(&self) -> NativeIntegrationStoreResult<bool> {
        self.inner.shutdown()
    }

    pub(crate) fn save_approval(
        &self,
        approval: NativeIntegrationApprovalV1,
    ) -> NativeIntegrationStoreResult<()> {
        self.inner.save_approval(approval)
    }

    pub(crate) fn read_approval(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationApprovalV1>> {
        self.inner.read_approval(approval_id)
    }
}

impl NativeIntegrationStore for SharedDaemonNativeIntegrationStore {
    fn save_preview(
        &self,
        preview: NativeIntegrationPreviewV1,
    ) -> NativeIntegrationStoreResult<()> {
        self.inner.save_preview(preview)
    }

    fn read_preview(
        &self,
        preview_id: &NativeIntegrationPreviewId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationPreviewV1>> {
        self.inner.read_preview(preview_id)
    }

    fn begin_or_replay(
        &self,
        record: NativeIntegrationRecordV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1> {
        self.inner.begin_or_replay(record)
    }

    fn read_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>> {
        self.inner.read_status(transaction_id)
    }

    fn read_record(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>> {
        self.inner.read_record(transaction_id)
    }

    fn read_receipt(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationReceiptV1>> {
        self.inner.read_receipt(transaction_id)
    }

    fn compare_and_swap_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        replacement: NativeIntegrationTransactionStatusV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationTransactionStatusV1> {
        self.inner
            .compare_and_swap_status(transaction_id, expected_phase_revision, replacement)
    }

    fn write_terminal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        receipt: NativeIntegrationReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationReceiptV1> {
        self.inner
            .write_terminal(transaction_id, expected_phase_revision, receipt)
    }

    fn pending_transactions(
        &self,
        repository_id: Option<&RepositoryId>,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>> {
        self.inner.pending_transactions(repository_id)
    }

    fn approval_consumed(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<bool> {
        self.inner.approval_consumed(approval_id)
    }

    fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()> {
        self.inner
            .quarantine_repository(repository_id, transaction_id)
    }
}

//! Native integration transaction store over the registered project session
//! database.
//!
//! Every call awaits the async global-db store directly. Each mutation there
//! is one `IMMEDIATE` SQLite transaction (one-use approval consumption and
//! every phase compare-and-swap included), so concurrent callers need no actor
//! to serialize them; per-transaction ordering comes from the coordinator
//! awaiting each step in turn under its repository lock. It has no filesystem
//! path and cannot create a side-file authority.

use std::sync::{Arc, Mutex, PoisonError};

use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, NativeIntegrationApprovalId, NativeIntegrationApprovalV1,
    NativeIntegrationPreviewId, NativeIntegrationPreviewV1, NativeIntegrationReceiptV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1,
    NativeWorktreeCleanupReceiptV1, NativeWorktreeCleanupTransactionV1, RepositoryId, UtcMicros,
};
use tracedecay_store::{
    NativeIntegrationBeginResultV1, NativeIntegrationRecordV1, NativeIntegrationStore,
    NativeIntegrationStoreError, NativeIntegrationStoreResult, NativeWorktreeCleanupBeginResultV1,
};

use tracedecay_global_db::{GlobalDbNativeIntegrationStore, RegisteredGlobalDbLeaseV1};

/// Shared handle to the one daemon-owned store for a project database.
///
/// Clones share the lease. `shutdown`, or dropping the last clone, releases
/// it; calls after shutdown fail closed as unavailable, while a call already
/// in flight finishes on the lease it holds.
#[derive(Clone)]
pub struct DaemonNativeIntegrationStore {
    database: Arc<Mutex<Option<RegisteredGlobalDbLeaseV1>>>,
}

impl DaemonNativeIntegrationStore {
    #[tracing::instrument(
        name = "daemon.native_integration.store_open",
        level = "trace",
        skip_all
    )]
    pub(crate) fn open(database: RegisteredGlobalDbLeaseV1) -> Self {
        Self {
            database: Arc::new(Mutex::new(Some(database))),
        }
    }

    /// Releases the lease. Returns whether this call closed the store.
    pub(crate) fn shutdown(&self) -> bool {
        self.database
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_some()
    }

    fn database(&self) -> NativeIntegrationStoreResult<RegisteredGlobalDbLeaseV1> {
        self.database
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                NativeIntegrationStoreError::unavailable("native integration store is shut down")
            })
    }

    /// Persists one issued approval commitment (approval issuance operation).
    #[tracing::instrument(
        name = "agent_hosts.native_store.save_approval",
        level = "trace",
        skip_all
    )]
    pub async fn save_approval(
        &self,
        approval: NativeIntegrationApprovalV1,
    ) -> NativeIntegrationStoreResult<()> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .save_approval(approval)
            .await
    }

    /// Reads one issued approval commitment for apply resolution.
    #[tracing::instrument(
        name = "agent_hosts.native_store.read_approval",
        level = "trace",
        skip_all
    )]
    pub async fn read_approval(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationApprovalV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_approval(approval_id)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.read_preview_by_digest",
        level = "trace",
        skip_all
    )]
    pub async fn read_preview_by_digest(
        &self,
        preview_digest: &ManifestDigest,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationPreviewV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_preview_by_digest(preview_digest)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.read_receipt_by_digest",
        level = "trace",
        skip_all
    )]
    pub async fn read_receipt_by_digest(
        &self,
        receipt_digest: &ManifestDigest,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationReceiptV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_receipt_by_digest(receipt_digest)
            .await
    }
}

impl NativeIntegrationStore for DaemonNativeIntegrationStore {
    #[tracing::instrument(
        name = "agent_hosts.native_store.save_preview",
        level = "trace",
        skip_all
    )]
    async fn save_preview(
        &self,
        preview: NativeIntegrationPreviewV1,
    ) -> NativeIntegrationStoreResult<()> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .save_preview(preview)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.read_preview",
        level = "trace",
        skip_all
    )]
    async fn read_preview(
        &self,
        preview_id: &NativeIntegrationPreviewId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationPreviewV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_preview(preview_id)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.begin_or_replay",
        level = "trace",
        skip_all
    )]
    async fn begin_or_replay(
        &self,
        record: NativeIntegrationRecordV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationBeginResultV1> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .begin_or_replay(record)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.read_status",
        level = "trace",
        skip_all
    )]
    async fn read_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationTransactionStatusV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_status(transaction_id)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.read_record",
        level = "trace",
        skip_all
    )]
    async fn read_record(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationRecordV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_record(transaction_id)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.read_receipt",
        level = "trace",
        skip_all
    )]
    async fn read_receipt(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<Option<NativeIntegrationReceiptV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_receipt(transaction_id)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.compare_and_swap_status",
        level = "trace",
        skip_all
    )]
    async fn compare_and_swap_status(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        replacement: NativeIntegrationTransactionStatusV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationTransactionStatusV1> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .compare_and_swap_status(transaction_id, expected_phase_revision, replacement)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.write_terminal",
        level = "trace",
        skip_all
    )]
    async fn write_terminal(
        &self,
        transaction_id: &NativeIntegrationTransactionId,
        expected_phase_revision: u64,
        receipt: NativeIntegrationReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeIntegrationReceiptV1> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .write_terminal(transaction_id, expected_phase_revision, receipt)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.pending_transactions",
        level = "trace",
        skip_all
    )]
    async fn pending_transactions(
        &self,
        repository_id: Option<&RepositoryId>,
    ) -> NativeIntegrationStoreResult<Vec<NativeIntegrationRecordV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .pending_transactions(repository_id)
            .await
    }

    async fn live_candidate_generation_bindings(
        &self,
        repository_id: &RepositoryId,
        observed_at: UtcMicros,
    ) -> NativeIntegrationStoreResult<Vec<CodeGenerationId>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .live_candidate_generation_bindings(repository_id, observed_at)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.approval_consumed",
        level = "trace",
        skip_all
    )]
    async fn approval_consumed(
        &self,
        approval_id: &NativeIntegrationApprovalId,
    ) -> NativeIntegrationStoreResult<bool> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .approval_consumed(approval_id)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.quarantine_repository",
        level = "trace",
        skip_all
    )]
    async fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &NativeIntegrationTransactionId,
    ) -> NativeIntegrationStoreResult<()> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .quarantine_repository(repository_id, transaction_id)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.begin_worktree_cleanup",
        level = "trace",
        skip_all
    )]
    async fn begin_worktree_cleanup(
        &self,
        transaction: NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupBeginResultV1> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .begin_worktree_cleanup(transaction)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.read_worktree_cleanup",
        level = "trace",
        skip_all
    )]
    async fn read_worktree_cleanup(
        &self,
        confirmation_digest: &ManifestDigest,
    ) -> NativeIntegrationStoreResult<Option<NativeWorktreeCleanupTransactionV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .read_worktree_cleanup(confirmation_digest)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.pending_worktree_cleanups",
        level = "trace",
        skip_all
    )]
    async fn pending_worktree_cleanups(
        &self,
        repository_id: &RepositoryId,
        limit: u32,
    ) -> NativeIntegrationStoreResult<Vec<NativeWorktreeCleanupTransactionV1>> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .pending_worktree_cleanups(repository_id, limit)
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.compare_and_swap_worktree_cleanup",
        level = "trace",
        skip_all
    )]
    async fn compare_and_swap_worktree_cleanup(
        &self,
        confirmation_digest: &ManifestDigest,
        expected_phase_revision: u64,
        replacement: NativeWorktreeCleanupTransactionV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupTransactionV1> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .compare_and_swap_worktree_cleanup(
                confirmation_digest,
                expected_phase_revision,
                replacement,
            )
            .await
    }

    #[tracing::instrument(
        name = "agent_hosts.native_store.write_worktree_cleanup_terminal",
        level = "trace",
        skip_all
    )]
    async fn write_worktree_cleanup_terminal(
        &self,
        confirmation_digest: &ManifestDigest,
        expected_phase_revision: u64,
        receipt: NativeWorktreeCleanupReceiptV1,
    ) -> NativeIntegrationStoreResult<NativeWorktreeCleanupReceiptV1> {
        let database = self.database()?;
        GlobalDbNativeIntegrationStore::new(&database)
            .write_worktree_cleanup_terminal(confirmation_digest, expected_phase_revision, receipt)
            .await
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::test_fixtures::digest;
    use tracedecay_domain::{ActorId, CapabilityId};
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome};

    use super::*;

    fn approval(approval_id: &str) -> NativeIntegrationApprovalV1 {
        NativeIntegrationApprovalV1 {
            approval_id: NativeIntegrationApprovalId::new(approval_id).expect("approval id"),
            preview_id: NativeIntegrationPreviewId::new("preview.native-store").expect("preview"),
            preview_digest: digest('1'),
            principal: ActorId::new("actor.native-store").expect("principal"),
            delegated_agent: None,
            capability: CapabilityId::new("capability.git.native-integration-apply")
                .expect("capability"),
            grant_digest: digest('2'),
            issued_at: UtcMicros(1),
            expires_at: UtcMicros(2_000_000),
            approval_digest: digest('3'),
        }
        .seal()
        .expect("sealed approval")
    }

    #[tokio::test]
    async fn store_awaited_on_a_native_worker_never_parks_it() {
        let database = RegisteredGlobalDbHarness::open("native-integration-native-worker").await;
        let registered = database.registered.clone();
        let writer_waits = native_runtime::native_thread_writer_waits();
        let reader_waits = native_runtime::native_thread_reader_waits();

        let task = NativeTask::spawn(async move {
            assert!(
                native_runtime::Runtime::current_handle().is_some(),
                "the store must be awaited on a native worker"
            );
            let store = DaemonNativeIntegrationStore::open(registered);
            let approval = approval("approval.native-store.worker");
            store.save_approval(approval.clone()).await?;
            let read = store.read_approval(&approval.approval_id).await;
            store.shutdown();
            read.map(|read| (read, approval))
        })
        .expect("native task admitted");

        let NativeTaskOutcome::Completed(read) = task.wait().await else {
            panic!("store task did not complete");
        };
        let (read, approval) = read.expect("store calls settle on a native worker");
        assert_eq!(read, Some(approval));
        assert_eq!(
            native_runtime::native_thread_writer_waits(),
            writer_waits,
            "no writer wait parked a native worker"
        );
        assert_eq!(
            native_runtime::native_thread_reader_waits(),
            reader_waits,
            "no reader wait parked a native worker"
        );
    }

    #[tokio::test]
    async fn shutdown_closes_every_clone_once_and_fails_later_calls_closed() {
        let database = RegisteredGlobalDbHarness::open("native-integration-shutdown").await;
        let store = DaemonNativeIntegrationStore::open(database.registered.clone());
        let clone = store.clone();
        let approval = approval("approval.native-store.shutdown");
        clone
            .save_approval(approval.clone())
            .await
            .expect("open store accepts writes");

        assert!(store.shutdown(), "the first shutdown closes the store");
        assert!(!clone.shutdown(), "a clone shares the one closed lease");
        assert!(matches!(
            clone.read_approval(&approval.approval_id).await,
            Err(NativeIntegrationStoreError::Unavailable(reason)) if reason.contains("shut down")
        ));
        assert!(matches!(
            clone.pending_transactions(None).await,
            Err(NativeIntegrationStoreError::Unavailable(_))
        ));
    }
}

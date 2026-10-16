//! Git index transaction store over the registered project session database.
//!
//! Every call awaits the async global-db store directly. Each mutation there is
//! one `IMMEDIATE` SQLite transaction (journal compare-and-swap included), so
//! concurrent callers need no actor to serialize them, and per-transaction
//! ordering comes from the caller awaiting each step in turn. It has no
//! filesystem path and cannot create a JSON side-file authority.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;
use tracedecay_domain::{
    GitIndexIdempotencyKey, GitIndexPreviewId, GitIndexPreviewInputV1, GitIndexPreviewV1,
    GitIndexTransactionId, GitIndexTransactionJournalV1, GitIndexTransactionReceiptV1,
    RepositoryId, UtcMicros,
};
use tracedecay_global_db::{GitIndexReadExecutor, RegisteredGlobalDbLeaseV1};
use tracedecay_runtime_core::native_runtime::{self, NativeTask, TimeoutError};
use tracedecay_store::{
    CodeReadOperationV1, CodeReadResultV1, CodeRecoveryCandidatesQueryV1,
    CodeRecoveryRepositoriesQueryV1, GitIndexPreviewInputReadV1, GitIndexTransactionBeginRequestV1,
    GitIndexTransactionBeginResultV1, GitIndexTransactionRecordV1, GitIndexTransactionStore,
    GitIndexTransactionStoreError, GitIndexTransactionStoreResult,
    GitIndexTransactionTerminalWriteV1, MAX_GIT_INDEX_PREVIEW_INPUT_GC_BATCH,
};

/// A failed preview-input purge retries after this delay, the writer wait
/// budget of `RegisteredGlobalDb`, instead of spinning on a stalled writer.
const PREVIEW_INPUT_GC_RETRY: Duration = Duration::from_secs(5);

/// Shared handle to the one daemon-owned store for a project database.
///
/// Clones share the lease and the preview-input GC task. `shutdown`, or
/// dropping the last clone, releases the lease and stops the GC task; calls
/// after shutdown fail closed as unavailable.
#[derive(Clone)]
pub struct DaemonGitIndexTransactionStore {
    inner: Arc<StoreInner>,
}

struct StoreInner {
    shared: Arc<StoreShared>,
    gc_task: NativeTask<()>,
}

impl Drop for StoreInner {
    fn drop(&mut self) {
        self.close();
    }
}

impl StoreInner {
    fn close(&self) -> bool {
        self.gc_task.abort();
        self.shared
            .database
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_some()
    }
}

/// State the GC task shares with the store handles. The task holds only this,
/// so the handles alone decide when the lease is released.
struct StoreShared {
    database: Mutex<Option<RegisteredGlobalDbLeaseV1>>,
    next_expiry: Mutex<Option<UtcMicros>>,
    rearm: Notify,
    retry_delay: Duration,
    #[cfg(test)]
    gc_observer: Option<Arc<PreviewGcTestObserver>>,
}

impl StoreShared {
    fn database(&self) -> GitIndexTransactionStoreResult<RegisteredGlobalDbLeaseV1> {
        self.database
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                GitIndexTransactionStoreError::unavailable(
                    "git index transaction store is shut down",
                )
            })
    }

    fn next_expiry(&self) -> Option<UtcMicros> {
        *self
            .next_expiry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Records an expiry deadline, waking the GC task when it moves earlier.
    fn note_expiry(&self, expires_at: UtcMicros) {
        if self.merge_expiry(Some(expires_at)) {
            self.rearm.notify_one();
        }
    }

    fn merge_expiry(&self, expires_at: Option<UtcMicros>) -> bool {
        let Some(expires_at) = expires_at else {
            return false;
        };
        let mut next = self
            .next_expiry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let earlier = next.is_none_or(|current| expires_at < current);
        if earlier {
            *next = Some(expires_at);
        }
        earlier
    }

    /// Takes the due deadline before a purge, so a deadline a concurrent save
    /// records while the purge runs is merged with the purge's result instead
    /// of being overwritten by it.
    fn claim_expiry(&self) -> Option<UtcMicros> {
        self.next_expiry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    async fn purge_due(&self) -> GitIndexTransactionStoreResult<Option<UtcMicros>> {
        #[cfg(test)]
        if let Some(observer) = &self.gc_observer {
            observer
                .purge_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if observer
                .fail_purge
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(GitIndexTransactionStoreError::unavailable(
                    "injected preview-input purge failure",
                ));
            }
        }
        let database = self.database()?;
        database
            .git_index_transaction_store()
            .purge_expired_preview_inputs_and_next(
                current_utc_micros()?,
                MAX_GIT_INDEX_PREVIEW_INPUT_GC_BATCH,
            )
            .await
            .map(|result| result.next_expiry)
    }
}

#[cfg(test)]
#[derive(Default)]
struct PreviewGcTestObserver {
    purge_attempts: std::sync::atomic::AtomicUsize,
    fail_purge: std::sync::atomic::AtomicBool,
}

/// Purges expired preview inputs in bounded batches at each input's expiry
/// deadline. Saves move the deadline earlier through `rearm`; a full batch
/// leaves a due deadline, so the next batch runs at once.
async fn run_preview_input_gc(shared: Arc<StoreShared>) {
    loop {
        let rearmed = shared.rearm.notified();
        let Some(expires_at) = shared.next_expiry() else {
            rearmed.await;
            continue;
        };
        let wait = current_utc_micros().map_or(shared.retry_delay, |observed_at| {
            duration_until(expires_at, observed_at)
        });
        if !wait.is_zero() {
            match native_runtime::timeout(wait, rearmed).await {
                Ok(()) => continue,
                Err(TimeoutError::Elapsed) => {}
                Err(TimeoutError::TimerUnavailable(error)) => {
                    tracing::warn!(%error, "git index preview input GC timer unavailable");
                    shared.rearm.notified().await;
                    continue;
                }
            }
        }
        let claimed = shared.claim_expiry();
        match shared.purge_due().await {
            Ok(next_expiry) => {
                shared.merge_expiry(next_expiry);
            }
            Err(error) => {
                tracing::warn!(%error, "git index preview input GC failed");
                shared.merge_expiry(claimed);
                let _ = native_runtime::timeout(shared.retry_delay, shared.rearm.notified()).await;
            }
        }
    }
}

impl DaemonGitIndexTransactionStore {
    #[tracing::instrument(name = "daemon.git.tx.store_open", level = "trace", skip_all)]
    pub async fn open(database: RegisteredGlobalDbLeaseV1) -> GitIndexTransactionStoreResult<Self> {
        Self::open_with(
            database,
            PREVIEW_INPUT_GC_RETRY,
            #[cfg(test)]
            None,
        )
        .await
    }

    async fn open_with(
        database: RegisteredGlobalDbLeaseV1,
        retry_delay: Duration,
        #[cfg(test)] gc_observer: Option<Arc<PreviewGcTestObserver>>,
    ) -> GitIndexTransactionStoreResult<Self> {
        let next_expiry = database
            .git_index_transaction_store()
            .next_live_preview_input_expiry()
            .await?;
        let shared = Arc::new(StoreShared {
            database: Mutex::new(Some(database)),
            next_expiry: Mutex::new(next_expiry),
            rearm: Notify::new(),
            retry_delay,
            #[cfg(test)]
            gc_observer,
        });
        let gc_task = NativeTask::spawn(run_preview_input_gc(Arc::clone(&shared)))
            .map_err(GitIndexTransactionStoreError::unavailable)?;
        Ok(Self {
            inner: Arc::new(StoreInner { shared, gc_task }),
        })
    }

    /// Releases the lease and stops the GC task. Returns whether this call
    /// closed the store.
    pub fn shutdown(&self) -> bool {
        self.inner.close()
    }

    fn database(&self) -> GitIndexTransactionStoreResult<RegisteredGlobalDbLeaseV1> {
        self.inner.shared.database()
    }

    async fn execute_code_read(
        &self,
        operation: CodeReadOperationV1,
    ) -> GitIndexTransactionStoreResult<CodeReadResultV1> {
        let database = self.database()?;
        let store = database.git_index_transaction_store();
        GitIndexReadExecutor::new(&store)
            .execute_read(&operation)
            .await
    }
}

fn unexpected_read_result() -> GitIndexTransactionStoreError {
    GitIndexTransactionStoreError::unavailable("git index store returned an unexpected read result")
}

impl GitIndexTransactionStore for DaemonGitIndexTransactionStore {
    async fn save_preview_input(
        &self,
        input: GitIndexPreviewInputV1,
    ) -> GitIndexTransactionStoreResult<()> {
        let expires_at = input.expires_at;
        self.database()?
            .git_index_transaction_store()
            .save_preview_input(input)
            .await?;
        self.inner.shared.note_expiry(expires_at);
        Ok(())
    }

    async fn read_preview_input(
        &self,
        preview_id: &GitIndexPreviewId,
        observed_at: UtcMicros,
    ) -> GitIndexTransactionStoreResult<GitIndexPreviewInputReadV1> {
        self.database()?
            .git_index_transaction_store()
            .read_preview_input(preview_id, observed_at)
            .await
    }

    async fn purge_expired_preview_inputs(
        &self,
        observed_at: UtcMicros,
        limit: usize,
    ) -> GitIndexTransactionStoreResult<usize> {
        self.database()?
            .git_index_transaction_store()
            .purge_expired_preview_inputs(observed_at, limit)
            .await
    }

    async fn save_preview(&self, preview: GitIndexPreviewV1) -> GitIndexTransactionStoreResult<()> {
        self.database()?
            .git_index_transaction_store()
            .save_preview(preview)
            .await
    }

    async fn read_preview(
        &self,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexPreviewV1>> {
        match self
            .execute_code_read(CodeReadOperationV1::Preview(preview_id.clone()))
            .await?
        {
            CodeReadResultV1::Preview(preview) => Ok(*preview),
            _ => Err(unexpected_read_result()),
        }
    }

    async fn read_record(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
    ) -> GitIndexTransactionStoreResult<Option<GitIndexTransactionRecordV1>> {
        match self
            .execute_code_read(CodeReadOperationV1::TransactionRecord(
                idempotency_key.clone(),
            ))
            .await?
        {
            CodeReadResultV1::TransactionRecord(record) => Ok(*record),
            _ => Err(unexpected_read_result()),
        }
    }

    async fn begin_or_replay(
        &self,
        request: GitIndexTransactionBeginRequestV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionBeginResultV1> {
        self.database()?
            .git_index_transaction_store()
            .begin_or_replay(request)
            .await
    }

    async fn compare_and_swap_journal(
        &self,
        idempotency_key: &GitIndexIdempotencyKey,
        expected_phase_epoch: u64,
        replacement: GitIndexTransactionJournalV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionJournalV1> {
        self.database()?
            .git_index_transaction_store()
            .compare_and_swap_journal(idempotency_key, expected_phase_epoch, replacement)
            .await
    }

    async fn write_terminal(
        &self,
        write: GitIndexTransactionTerminalWriteV1,
    ) -> GitIndexTransactionStoreResult<GitIndexTransactionReceiptV1> {
        self.database()?
            .git_index_transaction_store()
            .write_terminal(write)
            .await
    }

    async fn recovery_candidates(
        &self,
        repository_id: &RepositoryId,
    ) -> GitIndexTransactionStoreResult<Vec<GitIndexTransactionRecordV1>> {
        match self
            .execute_code_read(CodeReadOperationV1::RecoveryCandidates(
                CodeRecoveryCandidatesQueryV1 {
                    repository_id: repository_id.clone(),
                    after: None,
                    limit: u32::MAX,
                },
            ))
            .await?
        {
            CodeReadResultV1::RecoveryCandidates(page) => Ok(page.records),
            _ => Err(unexpected_read_result()),
        }
    }

    async fn recovery_repositories(&self) -> GitIndexTransactionStoreResult<Vec<RepositoryId>> {
        match self
            .execute_code_read(CodeReadOperationV1::RecoveryRepositories(
                CodeRecoveryRepositoriesQueryV1 {
                    after: None,
                    limit: u32::MAX,
                },
            ))
            .await?
        {
            CodeReadResultV1::RecoveryRepositories(page) => Ok(page.repositories),
            _ => Err(unexpected_read_result()),
        }
    }

    async fn quarantine_repository(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
    ) -> GitIndexTransactionStoreResult<()> {
        self.database()?
            .git_index_transaction_store()
            .quarantine_repository(repository_id, transaction_id)
            .await
    }

    async fn clear_repository_quarantine(
        &self,
        repository_id: &RepositoryId,
        transaction_id: &GitIndexTransactionId,
        recovery_receipt: GitIndexTransactionReceiptV1,
    ) -> GitIndexTransactionStoreResult<()> {
        self.database()?
            .git_index_transaction_store()
            .clear_repository_quarantine(repository_id, transaction_id, recovery_receipt)
            .await
    }
}

fn duration_until(expires_at: UtcMicros, observed_at: UtcMicros) -> Duration {
    if expires_at <= observed_at {
        return Duration::ZERO;
    }
    Duration::from_micros(expires_at.0.saturating_sub(observed_at.0).unsigned_abs())
}

fn current_utc_micros() -> GitIndexTransactionStoreResult<UtcMicros> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(GitIndexTransactionStoreError::unavailable)?;
    let micros =
        i64::try_from(elapsed.as_micros()).map_err(GitIndexTransactionStoreError::unavailable)?;
    Ok(UtcMicros(micros))
}

#[cfg(test)]
mod gc_tests {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use super::*;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::native_runtime::NativeTaskOutcome;

    async fn store_with_gc_observer(
        label: &str,
        observer: Arc<PreviewGcTestObserver>,
        retry_delay: Duration,
    ) -> (RegisteredGlobalDbHarness, DaemonGitIndexTransactionStore) {
        let database = RegisteredGlobalDbHarness::open(label).await;
        let store = DaemonGitIndexTransactionStore::open_with(
            database.registered.clone(),
            retry_delay,
            Some(observer),
        )
        .await
        .expect("registered store");
        (database, store)
    }

    fn preview_input(
        suffix: &str,
        created_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> GitIndexPreviewInputV1 {
        let template =
            super::super::test_support::preview_input(&super::super::test_support::preview());
        let preview_id =
            GitIndexPreviewId::new(format!("preview.gc.{suffix}")).expect("preview id");
        let hunks = template
            .hunks
            .iter()
            .map(|hunk| tracedecay_domain::HunkRefV1 {
                preview_id: preview_id.as_str().to_owned(),
                ..hunk.clone()
            })
            .collect();
        GitIndexPreviewInputV1::new_hunk_selection(
            preview_id,
            template.operation,
            template.repository_snapshot,
            hunks,
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

    /// A purge attempt is counted before its write commits, so a reader
    /// waits for the tombstone itself.
    async fn wait_for_tombstone(
        store: &DaemonGitIndexTransactionStore,
        preview_id: &GitIndexPreviewId,
    ) -> GitIndexPreviewInputReadV1 {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let read = store
                .read_preview_input(preview_id, current_utc_micros().expect("clock"))
                .await
                .expect("read preview input");
            let purged = matches!(
                read,
                GitIndexPreviewInputReadV1::Expired {
                    purged_at: Some(_),
                    ..
                }
            );
            if purged || Instant::now() >= deadline {
                return read;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn one_expiry_causes_one_bounded_purge_and_then_sleeps() {
        let observer = Arc::new(PreviewGcTestObserver::default());
        let (_database, store) = store_with_gc_observer(
            "git-index-gc-one-expiry",
            Arc::clone(&observer),
            PREVIEW_INPUT_GC_RETRY,
        )
        .await;
        let created_at = current_utc_micros().expect("clock");
        let input = preview_input(
            "one",
            created_at,
            UtcMicros(created_at.0.saturating_add(100_000)),
        );
        let preview_id = input.preview_id.clone();
        store
            .save_preview_input(input)
            .await
            .expect("save preview input");

        wait_for_attempts(&observer, 1);
        std::thread::sleep(Duration::from_millis(150));

        assert_eq!(observer.purge_attempts.load(Ordering::SeqCst), 1);
        assert!(matches!(
            wait_for_tombstone(&store, &preview_id).await,
            GitIndexPreviewInputReadV1::Expired {
                purged_at: Some(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn restart_reconstructs_expired_cleanup_deadline() {
        let database = RegisteredGlobalDbHarness::open("git-index-gc-restart").await;
        let now = current_utc_micros().expect("clock");
        // The input is durable and already expired, and the store that wrote
        // it is gone: only the reopened store can reconstruct the cleanup
        // deadline. Seeding through the registered store keeps a pre-restart
        // GC task from racing the fixture.
        let created_at = UtcMicros(now.0.saturating_sub(1_000_000));
        let input = preview_input(
            "restart",
            created_at,
            UtcMicros(created_at.0.saturating_add(100_000)),
        );
        let preview_id = input.preview_id.clone();
        database
            .registered
            .git_index_transaction_store()
            .save_preview_input(input)
            .await
            .expect("save preview input");

        let observer = Arc::new(PreviewGcTestObserver::default());
        let store = DaemonGitIndexTransactionStore::open_with(
            database.registered.clone(),
            PREVIEW_INPUT_GC_RETRY,
            Some(Arc::clone(&observer)),
        )
        .await
        .expect("reopened registered store");
        wait_for_attempts(&observer, 1);

        assert!(matches!(
            wait_for_tombstone(&store, &preview_id).await,
            GitIndexPreviewInputReadV1::Expired {
                purged_at: Some(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn failed_gc_retries_after_its_delay_without_failing_commands() {
        let observer = Arc::new(PreviewGcTestObserver::default());
        observer.fail_purge.store(true, Ordering::SeqCst);
        let (_database, store) = store_with_gc_observer(
            "git-index-gc-failure",
            Arc::clone(&observer),
            Duration::from_secs(1),
        )
        .await;
        let created_at = current_utc_micros().expect("clock");
        let input = preview_input(
            "failure",
            created_at,
            UtcMicros(created_at.0.saturating_add(100_000)),
        );
        let preview_id = input.preview_id.clone();
        store
            .save_preview_input(input)
            .await
            .expect("save preview input");
        wait_for_attempts(&observer, 1);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            observer.purge_attempts.load(Ordering::SeqCst),
            1,
            "a failed purge waits out its retry delay"
        );

        assert!(matches!(
            store
                .read_preview_input(&preview_id, current_utc_micros().expect("clock"))
                .await
                .expect("a failed purge does not fail unrelated commands"),
            GitIndexPreviewInputReadV1::Expired {
                purged_at: None,
                ..
            }
        ));

        observer.fail_purge.store(false, Ordering::SeqCst);
        wait_for_attempts(&observer, 2);
        assert!(matches!(
            wait_for_tombstone(&store, &preview_id).await,
            GitIndexPreviewInputReadV1::Expired {
                purged_at: Some(_),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn store_awaited_on_a_native_worker_never_parks_it() {
        let database = RegisteredGlobalDbHarness::open("git-index-native-worker").await;
        let registered = database.registered.clone();
        let writer_waits = native_runtime::native_thread_writer_waits();
        let reader_waits = native_runtime::native_thread_reader_waits();

        let task = NativeTask::spawn(async move {
            assert!(
                native_runtime::Runtime::current_handle().is_some(),
                "the store must be awaited on a native worker"
            );
            let store = DaemonGitIndexTransactionStore::open(registered).await?;
            let input =
                super::super::test_support::preview_input(&super::super::test_support::preview());
            let preview_id = input.preview_id.clone();
            let observed_at = input.created_at;
            store.save_preview_input(input).await?;
            let read = store.read_preview_input(&preview_id, observed_at).await;
            store.shutdown();
            read
        })
        .expect("native task admitted");

        let NativeTaskOutcome::Completed(read) = task.wait().await else {
            panic!("store task did not complete");
        };
        assert!(matches!(
            read.expect("store calls settle on a native worker"),
            GitIndexPreviewInputReadV1::Available(_)
        ));
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
}

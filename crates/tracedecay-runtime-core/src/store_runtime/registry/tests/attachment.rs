use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak, mpsc};
use std::time::Duration;

use tracedecay_store::{RuntimeMaintenanceStateV1, StoreShardScopeV1};

use super::super::*;
use super::support::*;

struct DbHandleProxy(Arc<AtomicUsize>);

impl Drop for DbHandleProxy {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct OperationGate {
    blocked: AtomicBool,
    entered: AtomicBool,
    released: Mutex<bool>,
    wake: Condvar,
}

impl OperationGate {
    fn block(&self) {
        self.entered.store(false, Ordering::SeqCst);
        *self.released.lock().unwrap() = false;
        self.blocked.store(true, Ordering::SeqCst);
    }

    fn wait(&self) {
        if !self.blocked.load(Ordering::SeqCst) {
            return;
        }
        self.entered.store(true, Ordering::SeqCst);
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.wake.wait(released).unwrap();
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.blocked.store(false, Ordering::SeqCst);
        self.wake.notify_all();
    }
}

struct FakeAttachment {
    snapshot: Mutex<PhysicalRuntimeSnapshot>,
    fail_drain: AtomicBool,
    retain_work_after_drain: AtomicBool,
    drain_calls: AtomicUsize,
    close_calls: AtomicUsize,
    drain_gate: OperationGate,
    close_gate: OperationGate,
    db: Mutex<Option<DbHandleProxy>>,
}

impl FakeAttachment {
    fn new(db_drops: Arc<AtomicUsize>) -> Self {
        Self {
            snapshot: Mutex::new(PhysicalRuntimeSnapshot {
                healthy: true,
                writer_present: true,
                reader_handles: 2,
                general_reader_waiters: 0,
                health_reader_waiters: 0,
                queued_operations: 3,
                queued_bytes: 512,
                writer_busy_events: 0,
                wal_bytes: 4_096,
                memory_estimate_bytes: 8_192,
            }),
            fail_drain: AtomicBool::new(false),
            retain_work_after_drain: AtomicBool::new(false),
            drain_calls: AtomicUsize::new(0),
            close_calls: AtomicUsize::new(0),
            drain_gate: OperationGate::default(),
            close_gate: OperationGate::default(),
            db: Mutex::new(Some(DbHandleProxy(db_drops))),
        }
    }
}

impl PhysicalRuntimeAttachment for FakeAttachment {
    fn snapshot(&self) -> PhysicalRuntimeSnapshot {
        *self.snapshot.lock().unwrap()
    }

    fn opened_file_identity(&self) -> Result<u64, String> {
        Ok(1)
    }

    fn drain(&self) -> Result<(), String> {
        self.drain_calls.fetch_add(1, Ordering::SeqCst);
        self.drain_gate.wait();
        if self.fail_drain.load(Ordering::SeqCst) {
            return Err("injected drain failure".to_owned());
        }
        if self.retain_work_after_drain.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut snapshot = self.snapshot.lock().unwrap();
        snapshot.writer_present = false;
        snapshot.reader_handles = 0;
        snapshot.queued_operations = 0;
        snapshot.queued_bytes = 0;
        Ok(())
    }

    fn close_and_join(&self) -> Result<(), String> {
        self.close_calls.fetch_add(1, Ordering::SeqCst);
        self.close_gate.wait();
        self.db.lock().unwrap().take();
        Ok(())
    }
}

#[derive(Default)]
struct AttachmentPublisher {
    calls: AtomicUsize,
    db_drops: Arc<AtomicUsize>,
    attachments: Mutex<Vec<Weak<FakeAttachment>>>,
}

impl AttachmentPublisher {
    fn attachment(&self, index: usize) -> Arc<FakeAttachment> {
        self.attachments.lock().unwrap()[index].upgrade().unwrap()
    }
}

impl ShardRuntimePublisher for AttachmentPublisher {
    fn publish(
        &self,
        request: ShardRuntimeBuildRequest,
    ) -> StoreRuntimeRegistryFuture<'_, Result<PublishedShardRuntime, StoreRuntimeRegistryFailure>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let runtime = Arc::new(ShardRuntime::new(
                request.binding().clone(),
                matches!(request.binding().shard_id.scope, StoreShardScopeV1::Profile),
            ));
            runtime
                .transition(RuntimeMaintenanceStateV1::Opening)
                .unwrap();
            runtime
                .transition(RuntimeMaintenanceStateV1::Ready)
                .unwrap();
            let attachment = Arc::new(FakeAttachment::new(Arc::clone(&self.db_drops)));
            self.attachments
                .lock()
                .unwrap()
                .push(Arc::downgrade(&attachment));
            Ok(PublishedShardRuntime::new(runtime, attachment))
        })
    }
}

fn attachment_registry() -> (StoreRuntimeRegistry, Arc<AttachmentPublisher>) {
    let publisher = Arc::new(AttachmentPublisher::default());
    let registry = StoreRuntimeRegistry::with_config(
        Arc::new(TestResolver::default()),
        publisher.clone(),
        StoreRuntimeRegistryConfig::new(1).unwrap(),
    )
    .unwrap();
    (registry, publisher)
}

async fn wait_for_gate(gate: &OperationGate) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("eviction reached the blocking attachment operation");
}

#[tokio::test]
async fn publication_and_aliases_share_and_retain_one_physical_attachment() {
    let (registry, publisher) = attachment_registry();
    let pin = profile_pin(&registry).await;
    let request = project_request("project.attachment-shared", &pin);
    let first = open_published(&registry, request.clone()).await;
    let alias = open_published(&registry, request).await;
    let physical = Arc::downgrade(&first.inner.attachment);

    assert_eq!(publisher.calls.load(Ordering::SeqCst), 2);
    assert!(Arc::ptr_eq(
        &first.inner.attachment,
        &alias.inner.attachment
    ));
    let inventory = registry.inventory(tracedecay_store::AdmissionConfigV1::default(), 0);
    let observed = inventory
        .entries
        .iter()
        .find(|entry| entry.health.binding == *first.binding())
        .unwrap();
    assert!(observed.health.writer_present);
    assert_eq!(observed.physical.reader_handles, 2);
    assert_eq!(observed.health.queued_operations, 3);
    assert_eq!(observed.health.queued_bytes, 512);
    assert_eq!(observed.health.wal_bytes, 4_096);
    assert_eq!(observed.health.memory_estimate_bytes, 8_192);
    drop(first);
    drop(alias);
    assert!(
        physical.upgrade().is_some(),
        "registry retains physical lifetime"
    );
}

#[tokio::test]
async fn eviction_drains_verifies_closes_once_and_drops_database_proxy() {
    let (registry, publisher) = attachment_registry();
    let pin = profile_pin(&registry).await;
    let first = open_published(&registry, code_request("worktree.attachment-first", &pin)).await;
    let attachment = publisher.attachment(1);
    drop(first);

    open_published(&registry, code_request("worktree.attachment-second", &pin)).await;

    assert_eq!(attachment.drain_calls.load(Ordering::SeqCst), 1);
    assert_eq!(attachment.close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(publisher.db_drops.load(Ordering::SeqCst), 1);
    assert!(attachment.snapshot().is_drained());
}

#[tokio::test]
async fn drain_failure_is_terminal_and_retains_evicting_attachment() {
    let (registry, publisher) = attachment_registry();
    let pin = profile_pin(&registry).await;
    let first = open_published(&registry, code_request("worktree.attachment-fault", &pin)).await;
    let binding = first.binding().clone();
    let attachment = publisher.attachment(1);
    attachment.fail_drain.store(true, Ordering::SeqCst);
    drop(first);

    assert!(matches!(
        registry.begin_or_join_open(&code_request("worktree.after-fault", &pin)),
        StoreRuntimeOpenBegin::Rejected(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "drain",
            ..
        })
    ));
    assert_eq!(attachment.close_calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        registry.lookup(&binding),
        StoreRuntimeLookup::Evicting { .. }
    ));
}

#[tokio::test]
async fn incomplete_drain_is_verified_before_physical_close() {
    let (registry, publisher) = attachment_registry();
    let pin = profile_pin(&registry).await;
    let first = open_published(
        &registry,
        code_request("worktree.attachment-incomplete-drain", &pin),
    )
    .await;
    let binding = first.binding().clone();
    let attachment = publisher.attachment(1);
    attachment
        .retain_work_after_drain
        .store(true, Ordering::SeqCst);
    drop(first);

    assert!(matches!(
        registry.begin_or_join_open(&code_request("worktree.after-incomplete-drain", &pin)),
        StoreRuntimeOpenBegin::Rejected(
            StoreRuntimeRegistryFailure::PhysicalRuntimeNotDrained { .. }
        )
    ));
    assert_eq!(attachment.drain_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        attachment.close_calls.load(Ordering::SeqCst),
        0,
        "close must not run until the registry verifies a complete drain"
    );
    assert!(matches!(
        registry.lookup(&binding),
        StoreRuntimeLookup::Evicting { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_join_releases_registry_lock_and_reserves_evicted_and_opening_keys() {
    let (registry, publisher) = attachment_registry();
    let pin = profile_pin(&registry).await;
    let evicted_request = code_request("worktree.blocking-join-first", &pin);
    let opening_request = code_request("worktree.blocking-join-second", &pin);
    let first = open_published(&registry, evicted_request.clone()).await;
    let binding = first.binding().clone();
    let attachment = publisher.attachment(1);
    attachment.close_gate.block();
    drop(first);

    let eviction_registry = registry.clone();
    let eviction_request = opening_request.clone();
    let runtime = tokio::runtime::Handle::current();
    let eviction = std::thread::spawn(move || {
        let _runtime = runtime.enter();
        eviction_registry.begin_or_join_open(&eviction_request)
    });
    wait_for_gate(&attachment.close_gate).await;

    let inventory_registry = registry.clone();
    let (inventory_sender, inventory_receiver) = mpsc::channel();
    let inventory_thread = std::thread::spawn(move || {
        let inventory =
            inventory_registry.inventory(tracedecay_store::AdmissionConfigV1::default(), 0);
        inventory_sender.send(inventory).unwrap();
    });
    let inventory = inventory_receiver.recv_timeout(Duration::from_secs(1));
    let observations = inventory.ok().map(|inventory| {
        let draining = inventory
            .entries
            .iter()
            .find(|entry| entry.health.binding == binding)
            .expect("eviction reservation remains visible to telemetry");
        assert_eq!(draining.health.state, RuntimeMaintenanceStateV1::Draining);

        let evicted_open = registry.begin_or_join_open(&evicted_request);
        let duplicate_open = registry.begin_or_join_open(&opening_request);
        let lease = registry.acquire_lease(active_lease(&binding, "lease.eviction-race"));
        (evicted_open, duplicate_open, lease)
    });

    attachment.close_gate.release();
    let initial_open = eviction.join().unwrap();
    inventory_thread.join().unwrap();
    let Some((evicted_open, duplicate_open, lease)) = observations else {
        panic!("registry lock remained held during close_and_join");
    };

    assert!(matches!(
        evicted_open,
        StoreRuntimeOpenBegin::Rejected(
            StoreRuntimeRegistryFailure::RuntimeEvictionInProgress { .. }
        )
    ));
    assert!(matches!(
        lease,
        StoreRuntimeLeaseAcquireResult::Evicting { .. }
    ));
    let initial_join = match initial_open {
        StoreRuntimeOpenBegin::Started(join) => join,
        other => panic!("reserved open did not start after eviction: {other:?}"),
    };
    let duplicate_join = match duplicate_open {
        StoreRuntimeOpenBegin::Joined(join) => join,
        other => panic!("duplicate open did not join the reservation: {other:?}"),
    };
    let (initial, duplicate) = tokio::join!(initial_join.wait(), duplicate_join.wait());
    let (StoreRuntimeOpenResult::Published(initial), StoreRuntimeOpenResult::Published(duplicate)) =
        (initial, duplicate)
    else {
        panic!("reserved open was not published to both joiners");
    };
    assert!(Arc::ptr_eq(initial.runtime(), duplicate.runtime()));
    assert_eq!(attachment.drain_calls.load(Ordering::SeqCst), 1);
    assert_eq!(attachment.close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_blocking_drain_restores_fault_and_wakes_reserved_open_joiners() {
    let (registry, publisher) = attachment_registry();
    let pin = profile_pin(&registry).await;
    let evicted_request = code_request("worktree.blocking-drain-first", &pin);
    let opening_request = code_request("worktree.blocking-drain-second", &pin);
    let first = open_published(&registry, evicted_request.clone()).await;
    let binding = first.binding().clone();
    let attachment = publisher.attachment(1);
    attachment.fail_drain.store(true, Ordering::SeqCst);
    attachment.drain_gate.block();
    drop(first);

    let eviction_registry = registry.clone();
    let eviction_request = opening_request.clone();
    let runtime = tokio::runtime::Handle::current();
    let eviction = std::thread::spawn(move || {
        let _runtime = runtime.enter();
        eviction_registry.begin_or_join_open(&eviction_request)
    });
    wait_for_gate(&attachment.drain_gate).await;

    let probe_registry = registry.clone();
    let probe_opening_request = opening_request.clone();
    let probe_evicted_request = evicted_request.clone();
    let (probe_sender, probe_receiver) = mpsc::channel();
    let probe = std::thread::spawn(move || {
        let duplicate = probe_registry.begin_or_join_open(&probe_opening_request);
        let evicted = probe_registry.begin_or_join_open(&probe_evicted_request);
        probe_sender.send((duplicate, evicted)).unwrap();
    });
    let observations = probe_receiver.recv_timeout(Duration::from_secs(1)).ok();

    attachment.drain_gate.release();
    let initial = eviction.join().unwrap();
    probe.join().unwrap();
    let Some((duplicate, evicted)) = observations else {
        panic!("registry lock remained held during drain");
    };
    let duplicate_join = match duplicate {
        StoreRuntimeOpenBegin::Joined(join) => join,
        other => panic!("duplicate open did not join the reservation: {other:?}"),
    };
    assert!(matches!(
        evicted,
        StoreRuntimeOpenBegin::Rejected(
            StoreRuntimeRegistryFailure::RuntimeEvictionInProgress { .. }
        )
    ));

    assert!(matches!(
        initial,
        StoreRuntimeOpenBegin::Rejected(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "drain",
            ..
        })
    ));
    assert!(matches!(
        duplicate_join.wait().await,
        StoreRuntimeOpenResult::Failed(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
            operation: "drain",
            ..
        })
    ));
    assert_eq!(attachment.close_calls.load(Ordering::SeqCst), 0);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        registry.lookup(&binding),
        StoreRuntimeLookup::Evicting { .. }
    ));
}

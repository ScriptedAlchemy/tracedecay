use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_mcp::server::{
    McpBackgroundTaskOwner, ProjectServerResponseLifecycle, StartupCatchUpMachineV1,
};

struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[tokio::test]
async fn background_owner_aborts_joins_and_closes_admission() {
    let owner = McpBackgroundTaskOwner::default();
    let dropped = Arc::new(AtomicBool::new(false));
    let task_dropped = Arc::clone(&dropped);
    assert!(owner.spawn(async move {
        let _signal = DropSignal(task_dropped);
        std::future::pending::<()>().await;
    }));
    tokio::task::yield_now().await;

    assert!(owner.shutdown().await.is_empty());
    assert!(dropped.load(Ordering::Acquire));
    assert!(!owner.spawn(async {}));
}

#[tokio::test]
async fn response_retirement_waits_for_the_admitted_delivery_lease() {
    let lifecycle = ProjectServerResponseLifecycle::default();
    let admitted = Arc::clone(lifecycle.response_gate()).read_owned().await;
    let mut retirement = Box::pin(lifecycle.revoke_after_request_drain());
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(retirement.as_mut(), context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert!(!lifecycle.response_revoked().is_cancelled());

    drop(admitted);
    retirement.await;
    assert!(lifecycle.response_revoked().is_cancelled());
}

#[test]
fn startup_machine_makes_dispatch_and_shutdown_explicit() {
    let startup = StartupCatchUpMachineV1::default();
    assert!(startup.settled());
    assert!(startup.try_claim_dispatch());
    assert!(!startup.settled());
    assert!(!startup.try_claim_dispatch());
    startup.settle();
    assert!(startup.settled());
    startup.mark_cancelled();
    assert!(startup.settled());
}

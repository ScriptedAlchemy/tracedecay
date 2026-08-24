//! Serialization boundary for dashboard writes that are still projected by
//! the automation jobs adapter.
//!
//! Automation task execution is not implemented here. The daemon retains the
//! canonical run authority and supplies it through `DashboardAutomationAuthorityV1`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use super::DashboardState;

pub type DashboardAutomationWriteFuture =
    Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'static>>;
pub type DashboardAutomationWriteOperation =
    Box<dyn FnOnce() -> DashboardAutomationWriteFuture + Send + 'static>;
pub type DashboardAutomationWriter = Arc<
    dyn Fn(DashboardAutomationWriteOperation) -> DashboardAutomationWriteFuture
        + Send
        + Sync
        + 'static,
>;

pub fn standalone_dashboard_automation_writer() -> DashboardAutomationWriter {
    let writer = Arc::new(tokio::sync::Mutex::new(()));
    Arc::new(move |operation| {
        let writer = Arc::clone(&writer);
        Box::pin(async move {
            let _guard = writer.lock().await;
            operation().await
        })
    })
}

pub async fn execute_dashboard_automation_write<Operation, OperationFuture>(
    state: &DashboardState,
    operation: Operation,
) -> Result<Value, String>
where
    Operation: FnOnce(DashboardState) -> OperationFuture + Send + 'static,
    OperationFuture: Future<Output = Result<Value, String>> + Send + 'static,
{
    let writer = Arc::clone(&state.automation_writer);
    let state = state.clone();
    writer(Box::new(move || Box::pin(operation(state)))).await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn standalone_writer_executes_operation_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let writer = standalone_dashboard_automation_writer();

        let result = writer(Box::new(move || {
            Box::pin(async move {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(json!({ "status": "ok" }))
            })
        }))
        .await
        .expect("standalone dashboard automation write should succeed");

        assert_eq!(result, json!({ "status": "ok" }));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn standalone_writer_serializes_operations() {
        let writer = standalone_dashboard_automation_writer();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let operation = |active: Arc<AtomicUsize>, maximum: Arc<AtomicUsize>| {
            Box::new(move || {
                Box::pin(async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(Value::Null)
                }) as DashboardAutomationWriteFuture
            }) as DashboardAutomationWriteOperation
        };

        let first = writer(operation(Arc::clone(&active), Arc::clone(&maximum)));
        let second = writer(operation(active, Arc::clone(&maximum)));
        let (first, second) = tokio::join!(first, second);

        first.expect("first standalone write");
        second.expect("second standalone write");
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }
}

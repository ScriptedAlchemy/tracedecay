//! Production connection context the `rmcp` adapter dispatches through.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::JsonRpcResponse;
use tracedecay_domain::errors::Result;

use super::McpDispatchRequest;

/// Selected-project response authority retained through transport delivery.
pub trait McpResponseLease: Send + 'static {
    fn revoked(&self) -> &tracedecay_runtime_core::cancellation::CancellationToken;
}

/// Per-connection routing state owned by the production request context.
pub trait McpConnectionState: Send + Sync + 'static {
    type ResponseLease: McpResponseLease;

    fn memory_request_scope(&self) -> &str;
    #[must_use]
    fn fork_for_independent_read(&self) -> Self;
    fn take_selected_response_lease(&mut self) -> Option<Self::ResponseLease>;
}

/// The one production context the `rmcp` connection adapter requires.
pub trait McpConnectionContext: Send + Sync + 'static {
    type Connection: McpConnectionState;

    fn new_connection(&self) -> Result<Self::Connection>;
    fn timings_enabled(&self) -> bool;
    fn build_version(&self) -> Result<&'static str>;
    fn max_concurrent_reads(&self) -> usize;
    fn tool_is_read_only(&self, tool_name: &str) -> bool;
    /// The token is sticky: once cancelled it stays cancelled, so a late
    /// sample never misses a cancel. Sticky is not the same as interruptible.
    /// An implementation that only samples after resolving its route leaves
    /// the whole route window uncancellable, so the token must be raced
    /// *around* asynchronous route resolution as well as sampled before
    /// dispatch admission.
    ///
    /// Sticky is not interruptible, and that is the contract, not an
    /// oversight: once a request is admitted, its selected target owns the
    /// cancelled terminal and the request/error accounting, and the target is
    /// not known until the route resolves. An implementation must therefore
    /// finish route resolution and let the target settle the cancel rather
    /// than abandoning the dispatch and answering from the caller, so a cancel
    /// arriving during a long route becomes visible only once routing
    /// completes.
    ///
    /// A cancel that [`Self::cancel_request`] reports as unregistered is not
    /// automatically "never registerable": route resolution has not yet called
    /// `prepare_dispatch_control`. Transports must wait on
    /// [`Self::cancellation_registered`] so the sticky sample and selected
    /// target still settle. Abandon only when the cancel can never register,
    /// no registration wait channel.
    fn dispatch<'a>(
        &'a self,
        request: McpDispatchRequest<'a>,
        timings_enabled: bool,
        connection: &'a mut Self::Connection,
        cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Option<JsonRpcResponse>> + Send + 'a>>;
    fn cancel_request(&self, id: &Value, connection_scope: &str) -> bool;
    fn cancellation_registered(&self) -> &tokio::sync::Notify;
    fn run_in_connection_admission<'a, T, F>(
        &'a self,
        future: F,
    ) -> Pin<Box<dyn Future<Output = T> + Send + 'a>>
    where
        T: Send + 'a,
        F: Future<Output = T> + Send + 'a;
}

/// Races asynchronous route resolution against the request's own cancellation.
///
/// Returns `None` when the cancel wins, in which case the route future is
/// dropped: no request authority, deadline, or settlement state exists yet, so
/// abandoning it is the complete unwind. The route arm is polled first so a
/// route that is already resolved still reports its outcome instead of losing
/// it to a cancel that arrived in the same poll.
pub async fn await_route_with_cancellation<F, N>(routing: F, cancellation: N) -> Option<F::Output>
where
    F: Future,
    N: Future<Output = ()>,
{
    tokio::pin!(routing);
    tokio::pin!(cancellation);
    tokio::select! {
        biased;
        routed = &mut routing => Some(routed),
        () = &mut cancellation => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use super::await_route_with_cancellation;

    #[tokio::test]
    async fn route_resolution_is_abandoned_when_the_request_is_cancelled() {
        let routing_polls = Arc::new(AtomicIsize::new(0));
        let polls = Arc::clone(&routing_polls);
        let routing = async move {
            polls.fetch_add(1, Ordering::AcqRel);
            std::future::pending::<&str>().await
        };

        let routed = await_route_with_cancellation(routing, std::future::ready(())).await;

        assert_eq!(routed, None, "a cancel during routing abandons the route");
        assert_eq!(
            routing_polls.load(Ordering::Acquire),
            1,
            "the route arm is polled first so a ready route is never lost"
        );
    }

    #[tokio::test]
    async fn a_resolved_route_wins_over_a_simultaneous_cancel() {
        let routed = await_route_with_cancellation(
            std::future::ready("selected-server"),
            std::future::ready(()),
        )
        .await;

        assert_eq!(routed, Some("selected-server"));
    }
}

//! In-flight-only coalescing for identical read tools.
//!
//! Keys include the physical graph database, scope prefix, tool, and routed
//! arguments. Completed results are removed immediately, so this never becomes
//! a cross-generation response cache or widens a project's privacy boundary.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;
use tracedecay_domain::canonical_text::canonical_framed_sha256_bytes;

use crate::mcp::tools::mcp_dispatch_contract;
use tracedecay_mcp::ToolResult;
use tracedecay_runtime_core::weak_registry::WeakRegistry;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ReadFlightKey([u8; 32]);

#[derive(Default)]
struct ReadCoalescingInner {
    flights: WeakRegistry<ReadFlightKey, ReadFlight>,
    leaders: AtomicU64,
    followers: AtomicU64,
    active_followers: Arc<AtomicU64>,
}

#[derive(Clone, Default)]
pub(super) struct IdenticalReadCoalescer {
    inner: Arc<ReadCoalescingInner>,
}

enum ReadFlightState {
    Pending,
    Complete(Arc<ToolResult>),
    Abandoned,
}

pub(super) struct ReadFlight {
    // A flight exists for one in-flight request only. Instrumenting this
    // per-request lock retains profiler histograms after the flight is gone;
    // follower_wait and the coalescer methods provide stable timing instead.
    state: Mutex<ReadFlightState>,
    completed: tokio::sync::Notify,
    active_followers: Arc<AtomicU64>,
}

pub(super) enum ReadFlightClaim {
    Leader(ReadFlightLeader),
    Follower(Arc<ReadFlight>),
}

pub(super) struct ReadFlightLeader {
    key: ReadFlightKey,
    flight: Arc<ReadFlight>,
    owner: Weak<ReadCoalescingInner>,
    finished: bool,
}

struct ReadFollowerWaitGuard {
    active: Arc<AtomicU64>,
}

impl ReadFollowerWaitGuard {
    fn enter(active: Arc<AtomicU64>) -> Self {
        active.fetch_add(1, Ordering::AcqRel);
        hotpath::gauge!("mcp.server.read_coalescing.followers_active").inc(1_u64);
        Self { active }
    }
}

impl Drop for ReadFollowerWaitGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
        hotpath::gauge!("mcp.server.read_coalescing.followers_active").dec(1_u64);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReadCoalescingSnapshot {
    pub(super) leaders: u64,
    pub(super) followers: u64,
    pub(super) active_followers: u64,
    pub(super) active_flights: usize,
}

impl IdenticalReadCoalescer {
    pub(super) fn claim(
        &self,
        engine_identity: &str,
        tool_name: &str,
        arguments: &Value,
        scope_prefix: Option<&str>,
    ) -> ReadFlightClaim {
        let key = read_flight_key(engine_identity, tool_name, arguments, scope_prefix);
        let (flight, hit) = self.inner.flights.get_or_insert_with(key, || {
            Arc::new(ReadFlight {
                state: Mutex::new(ReadFlightState::Pending),
                completed: tokio::sync::Notify::new(),
                active_followers: Arc::clone(&self.inner.active_followers),
            })
        });
        if hit {
            self.inner.followers.fetch_add(1, Ordering::Relaxed);
            return ReadFlightClaim::Follower(flight);
        }

        self.inner.leaders.fetch_add(1, Ordering::Relaxed);
        ReadFlightClaim::Leader(ReadFlightLeader {
            key,
            flight,
            owner: Arc::downgrade(&self.inner),
            finished: false,
        })
    }

    pub(super) fn snapshot(&self) -> ReadCoalescingSnapshot {
        let active_flights = {
            self.inner.flights.retain_live();
            self.inner.flights.len()
        };
        ReadCoalescingSnapshot {
            leaders: self.inner.leaders.load(Ordering::Relaxed),
            followers: self.inner.followers.load(Ordering::Relaxed),
            active_followers: self.inner.active_followers.load(Ordering::Acquire),
            active_flights,
        }
    }
}

impl ReadFlight {
    #[hotpath::measure(label = "mcp.server.read_coalescing.follower_wait", future = true)]
    pub(super) async fn wait(&self) -> Option<Arc<ToolResult>> {
        let _active = ReadFollowerWaitGuard::enter(Arc::clone(&self.active_followers));
        loop {
            let completed = self.completed.notified();
            match &*self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
            {
                ReadFlightState::Pending => {}
                ReadFlightState::Complete(result) => return Some(Arc::clone(result)),
                ReadFlightState::Abandoned => return None,
            }
            completed.await;
        }
    }
}

impl ReadFlightLeader {
    pub(super) fn complete(mut self, result: ToolResult) -> ToolResult {
        self.finished = true;
        self.remove_registration();
        // Deregistration closes this flight to new followers (a later
        // identical claim starts its own flight), so a lone strong reference
        // proves nobody is waiting and the result can be handed back without
        // the shared-state round trip and clone.
        if Arc::strong_count(&self.flight) == 1 {
            return result;
        }
        let shared = Arc::new(result);
        *self
            .flight
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            ReadFlightState::Complete(Arc::clone(&shared));
        self.flight.completed.notify_waiters();
        Arc::try_unwrap(shared).unwrap_or_else(|shared| (*shared).clone())
    }

    fn remove_registration(&self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        owner.flights.remove_if_same(&self.key, &self.flight);
    }
}

impl Drop for ReadFlightLeader {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        *self
            .flight
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ReadFlightState::Abandoned;
        self.remove_registration();
        self.flight.completed.notify_waiters();
    }
}

pub(super) fn tool_allows_identical_read_coalescing(tool_name: &str) -> bool {
    if matches!(
        tool_name,
        "tracedecay_search"
            | "tracedecay_pr_context"
            | "tracedecay_git_status"
            | "tracedecay_git_diff"
            | "tracedecay_git_history"
            | "tracedecay_git_blame"
            | "tracedecay_git_hunks"
            | "tracedecay_dead_code"
            | "tracedecay_circular"
            | "tracedecay_affected"
            | "tracedecay_simplify_scan"
            | "tracedecay_dependency_depth"
            | "tracedecay_health"
            | "tracedecay_dsm"
    ) {
        return false;
    }
    mcp_dispatch_contract(tool_name)
        .is_ok_and(tracedecay_tool_catalog::McpDispatchContractV1::read_only)
}

fn read_flight_key(
    engine_identity: &str,
    tool_name: &str,
    arguments: &Value,
    scope_prefix: Option<&str>,
) -> ReadFlightKey {
    let arguments = serde_json::to_vec(arguments).unwrap_or_default();
    let scope_present: &[u8] = if scope_prefix.is_some() { b"1" } else { b"0" };
    let scope = scope_prefix.unwrap_or_default();
    ReadFlightKey(canonical_framed_sha256_bytes(
        b"tracedecay.mcp.read-flight.v1",
        &[
            engine_identity.as_bytes(),
            tool_name.as_bytes(),
            scope_present,
            scope.as_bytes(),
            &arguments,
        ],
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::*;

    #[tokio::test]
    async fn identical_reads_share_one_in_flight_result() {
        let coalescer = IdenticalReadCoalescer::default();
        let leader = match coalescer.claim(
            "graph-main",
            "tracedecay_search",
            &json!({"query": "DaemonEngine"}),
            None,
        ) {
            ReadFlightClaim::Leader(leader) => leader,
            ReadFlightClaim::Follower(_) => panic!("first caller must lead"),
        };
        let follower = match coalescer.claim(
            "graph-main",
            "tracedecay_search",
            &json!({"query": "DaemonEngine"}),
            None,
        ) {
            ReadFlightClaim::Follower(follower) => follower,
            ReadFlightClaim::Leader(_) => panic!("identical caller must follow"),
        };

        let waiter = tokio::spawn(async move { follower.wait().await });
        leader.complete(tracedecay_mcp::ToolResult::new(
            json!({"content": [{"type": "text", "text": "shared"}]}),
            vec!["src/daemon.rs".to_string()],
        ));
        let result = waiter
            .await
            .expect("follower task")
            .expect("leader completed");

        assert_eq!(result.value["content"][0]["text"], "shared");
        assert_eq!(result.touched_files, vec!["src/daemon.rs"]);
        assert_eq!(
            coalescer.snapshot(),
            ReadCoalescingSnapshot {
                leaders: 1,
                followers: 1,
                active_followers: 0,
                active_flights: 0,
            }
        );
    }

    #[test]
    fn distinct_scope_or_arguments_do_not_coalesce() {
        let coalescer = IdenticalReadCoalescer::default();
        let first = coalescer.claim(
            "graph-main",
            "tracedecay_search",
            &json!({"query": "a"}),
            None,
        );
        let different_arguments = coalescer.claim(
            "graph-main",
            "tracedecay_search",
            &json!({"query": "b"}),
            None,
        );
        let different_scope = coalescer.claim(
            "graph-main",
            "tracedecay_search",
            &json!({"query": "a"}),
            Some("src"),
        );
        let different_branch = coalescer.claim(
            "graph-feature",
            "tracedecay_search",
            &json!({"query": "a"}),
            None,
        );

        assert!(matches!(first, ReadFlightClaim::Leader(_)));
        assert!(matches!(different_arguments, ReadFlightClaim::Leader(_)));
        assert!(matches!(different_scope, ReadFlightClaim::Leader(_)));
        assert!(matches!(different_branch, ReadFlightClaim::Leader(_)));
    }

    #[test]
    fn representative_parallel_reads_reduce_dispatch_count() {
        const CLIENTS: usize = 32;

        let coalescer = IdenticalReadCoalescer::default();
        let mut claims = Vec::with_capacity(CLIENTS);
        for _ in 0..CLIENTS {
            claims.push(coalescer.claim(
                "graph-main",
                "tracedecay_context",
                &json!({"task": "map daemon state"}),
                None,
            ));
        }
        let snapshot = coalescer.snapshot();

        assert_eq!(snapshot.leaders, 1);
        assert_eq!(snapshot.followers, (CLIENTS - 1) as u64);
        assert_eq!(snapshot.active_flights, 1);
        eprintln!(
            "identical_read_dispatch_proxy baseline_dispatches={} candidate_dispatches={} reduction_percent={:.3}",
            CLIENTS,
            snapshot.leaders,
            100.0 * (CLIENTS as f64 - snapshot.leaders as f64) / CLIENTS as f64
        );
    }

    #[tokio::test]
    async fn cancelled_leader_releases_followers_and_future_claims() {
        let coalescer = IdenticalReadCoalescer::default();
        let leader = match coalescer.claim(
            "graph-main",
            "tracedecay_outline",
            &json!({"file": "src/lib.rs"}),
            None,
        ) {
            ReadFlightClaim::Leader(leader) => leader,
            ReadFlightClaim::Follower(_) => panic!("first caller must lead"),
        };
        let follower = match coalescer.claim(
            "graph-main",
            "tracedecay_outline",
            &json!({"file": "src/lib.rs"}),
            None,
        ) {
            ReadFlightClaim::Follower(follower) => follower,
            ReadFlightClaim::Leader(_) => panic!("identical caller must follow"),
        };

        drop(leader);
        assert!(follower.wait().await.is_none());
        assert!(matches!(
            coalescer.claim(
                "graph-main",
                "tracedecay_outline",
                &json!({"file": "src/lib.rs"}),
                None
            ),
            ReadFlightClaim::Leader(_)
        ));
    }

    #[tokio::test]
    async fn cancelled_follower_releases_the_active_waiter_count() {
        let coalescer = IdenticalReadCoalescer::default();
        let _leader = match coalescer.claim(
            "graph-main",
            "tracedecay_outline",
            &json!({"file": "src/lib.rs"}),
            None,
        ) {
            ReadFlightClaim::Leader(leader) => leader,
            ReadFlightClaim::Follower(_) => panic!("first caller must lead"),
        };
        let follower = match coalescer.claim(
            "graph-main",
            "tracedecay_outline",
            &json!({"file": "src/lib.rs"}),
            None,
        ) {
            ReadFlightClaim::Follower(follower) => follower,
            ReadFlightClaim::Leader(_) => panic!("identical caller must follow"),
        };

        let waiter = tokio::spawn(async move { follower.wait().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while coalescer.snapshot().active_followers == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("follower did not begin waiting");
        assert_eq!(coalescer.snapshot().active_followers, 1);

        waiter.abort();
        let _ = waiter.await;
        assert_eq!(coalescer.snapshot().active_followers, 0);
    }

    #[test]
    fn canonical_tool_annotations_gate_coalescing() {
        for controlled_read in [
            "tracedecay_search",
            "tracedecay_pr_context",
            "tracedecay_git_status",
            "tracedecay_git_diff",
            "tracedecay_git_history",
            "tracedecay_git_blame",
            "tracedecay_git_hunks",
            "tracedecay_dead_code",
            "tracedecay_circular",
            "tracedecay_affected",
            "tracedecay_simplify_scan",
            "tracedecay_dependency_depth",
            "tracedecay_health",
            "tracedecay_dsm",
        ] {
            assert!(
                !tool_allows_identical_read_coalescing(controlled_read),
                "{controlled_read} has caller-specific cancellation and deadline controls"
            );
        }
        assert!(tool_allows_identical_read_coalescing("tracedecay_outline"));
        assert!(!tool_allows_identical_read_coalescing(
            "tracedecay_str_replace"
        ));
    }
}

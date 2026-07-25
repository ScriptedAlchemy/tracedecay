//! Process-global in-memory tap for **live agent activity** observed by the
//! daemon, feeding `/api/events`.
//!
//! The daemon already observes real work as it happens — host hooks arriving on
//! the MCP boundary, transcript messages landing in the session store, touched
//! paths entering the code-index queue, tool calls being dispatched. None of it
//! reached the dashboard, because [`super::events_api`] only *polls* two
//! registry/storage digests. This module is the missing seam: producers publish
//! a one-line pulse at the exact point of observation, and the SSE task
//! subscribes, coalesces, and emits envelope-disciplined events.
//!
//! Design constraints this module exists to satisfy:
//!
//! - **No durable writes.** A pulse is an in-memory broadcast send. Nothing is
//!   persisted; the tap has no store, no watermark, and no replay.
//! - **Free when nobody is watching.** [`enabled`] is one relaxed atomic read.
//!   With no dashboard connected, a producer pays that read and returns — no
//!   allocation, no `PathBuf` clone, no lock.
//! - **Lossy on purpose.** The broadcast channel drops the oldest pulse under
//!   backpressure. Losing a pulse loses a *fraction of a count*, never a
//!   correctness signal — the dashboard refetches its canonical read models on
//!   its own schedule. A tap that blocked the hook boundary would be a bug.
//! - **The producer names its own scope.** Every pulse carries the project root
//!   the work happened in, and the registered project id when the producer
//!   already knows it. The consumer resolves the rest from the project registry
//!   it polls anyway, so a pulse never triggers a lookup on the hot path.
//!
//! Coalescing and revision assignment deliberately live in the consumer
//! ([`super::events_api`]), not here: they are per-connection concerns, and two
//! dashboards must not share a revision sequence.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::broadcast;

/// Pulses buffered per subscriber before the oldest are dropped. Sized for a
/// busy multi-agent machine to ride out a ~500 ms consumer flush window without
/// lag, while staying trivially small in memory.
const BUS_CAPACITY: usize = 1024;

/// One observed activity family. Each maps to a distinct SSE event name and a
/// distinct `kind.family` tag, so the frontend can style and route them
/// independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ActivityFamilyV1 {
    /// A host lifecycle hook was admitted on the MCP hook boundary.
    Hook,
    /// Transcript messages were durably persisted into the session store.
    SessionIngest,
    /// Touched paths entered a mounted worktree's incremental code-index queue.
    CodeIndex,
    /// A `tools/call` was dispatched by the daemon's MCP server.
    ToolCall,
}

impl ActivityFamilyV1 {
    /// The SSE `event:` name carrying this family. The frontend subscribes to
    /// named events, so this list is part of the wire contract.
    pub(crate) const fn stream_name(self) -> &'static str {
        match self {
            Self::Hook => "hook_activity",
            Self::SessionIngest => "session_ingest",
            Self::CodeIndex => "code_index_activity",
            Self::ToolCall => "tool_call",
        }
    }

    /// Every family, in a stable order. Used by tests and by the wire-contract
    /// assertions that keep the frontend's subscription list honest.
    pub(crate) const ALL: [Self; 4] = [
        Self::Hook,
        Self::SessionIngest,
        Self::CodeIndex,
        Self::ToolCall,
    ];
}

/// One observation. Cheap to clone: two `Arc`-free owned strings at most, and
/// only ever constructed when a dashboard is actually listening.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityPulseV1 {
    pub(crate) family: ActivityFamilyV1,
    /// Project root the work happened in. Not required to be canonical; the
    /// consumer canonicalizes once per emitted bucket, not once per pulse.
    pub(crate) project_root: PathBuf,
    /// Registered project id, when the producer already holds it. `None` means
    /// "resolve me from the registry", not "no project".
    pub(crate) project_id: Option<String>,
    /// How many underlying units this pulse represents (hook events, messages,
    /// queued files, tool calls). Always at least 1.
    pub(crate) units: u64,
    /// A short producer-supplied label (hook kind, provider, tool name). Bounded
    /// by construction — every current producer passes a static or already-short
    /// identifier, never user content.
    pub(crate) detail: Option<String>,
}

/// `true` once at least one subscriber has ever attached. Producers read this
/// before doing any work at all, so an unwatched daemon pays a single relaxed
/// load per observation.
static ANY_SUBSCRIBER: AtomicBool = AtomicBool::new(false);

fn sender() -> &'static broadcast::Sender<ActivityPulseV1> {
    static BUS: OnceLock<broadcast::Sender<ActivityPulseV1>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(BUS_CAPACITY).0)
}

/// Whether publishing can reach anyone. Producers that must build a value (a
/// `PathBuf` clone, a store-layout read) should gate on this first.
pub(crate) fn enabled() -> bool {
    ANY_SUBSCRIBER.load(Ordering::Relaxed) && sender().receiver_count() > 0
}

/// Attach a consumer. The first call arms [`enabled`] for the process; it is
/// never disarmed, because a dashboard reconnect is the common case and the
/// residual cost of a `receiver_count()` read is already negligible.
pub(crate) fn subscribe() -> broadcast::Receiver<ActivityPulseV1> {
    let receiver = sender().subscribe();
    ANY_SUBSCRIBER.store(true, Ordering::Relaxed);
    receiver
}

/// Publish one observation. Never blocks, never fails loudly, never allocates
/// when no dashboard is connected.
pub(crate) fn publish(
    family: ActivityFamilyV1,
    project_root: &Path,
    project_id: Option<&str>,
    units: u64,
    detail: Option<&str>,
) {
    if !enabled() {
        return;
    }
    let _ = sender().send(ActivityPulseV1 {
        family,
        project_root: project_root.to_path_buf(),
        project_id: project_id.map(ToOwned::to_owned),
        units: units.max(1),
        detail: detail.map(ToOwned::to_owned),
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn family_stream_names_are_distinct_and_stable() {
        let mut names = ActivityFamilyV1::ALL
            .iter()
            .map(|family| family.stream_name())
            .collect::<Vec<_>>();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(names.len(), unique, "stream names must be distinct");
        assert_eq!(ActivityFamilyV1::Hook.stream_name(), "hook_activity");
        assert_eq!(
            ActivityFamilyV1::SessionIngest.stream_name(),
            "session_ingest"
        );
        assert_eq!(
            ActivityFamilyV1::CodeIndex.stream_name(),
            "code_index_activity"
        );
        assert_eq!(ActivityFamilyV1::ToolCall.stream_name(), "tool_call");
    }

    /// One test, not four: the bus is a process-global, so parallel test cases
    /// would observe each other's pulses. This case owns the subscriber for the
    /// whole sequence and asserts the tap's three contracts in order.
    #[tokio::test]
    async fn the_tap_delivers_producer_scope_normalizes_units_and_never_replays() {
        let mut receiver = subscribe();
        assert!(enabled(), "subscribing arms the tap");

        // 1. A pulse arrives with exactly the scope the producer named.
        publish(
            ActivityFamilyV1::Hook,
            Path::new("/repo/alpha"),
            Some("proj-alpha"),
            3,
            Some("file_edit"),
        );
        let pulse = receiver.recv().await.expect("pulse");
        assert_eq!(pulse.family, ActivityFamilyV1::Hook);
        assert_eq!(pulse.project_root, PathBuf::from("/repo/alpha"));
        assert_eq!(pulse.project_id.as_deref(), Some("proj-alpha"));
        assert_eq!(pulse.units, 3);
        assert_eq!(pulse.detail.as_deref(), Some("file_edit"));

        // 2. A zero-unit pulse still counts as one observation.
        publish(
            ActivityFamilyV1::ToolCall,
            Path::new("/repo/beta"),
            None,
            0,
            None,
        );
        let pulse = receiver.recv().await.expect("pulse");
        assert_eq!(pulse.units, 1);
        assert!(pulse.project_id.is_none());

        // 3. Publishing with no receiver attached is a no-op with no replay:
        // a later subscriber must not see work published while unwatched.
        drop(receiver);
        publish(
            ActivityFamilyV1::CodeIndex,
            Path::new("/repo/gamma"),
            None,
            1,
            None,
        );
        let mut late = subscribe();
        assert!(
            late.try_recv().is_err(),
            "the tap must not replay pulses published while unwatched"
        );
    }
}

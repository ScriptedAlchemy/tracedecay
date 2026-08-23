//! Static Hotpath names and record helpers for graph-db operation boundaries.
//!
//! Lock-wait labels stay separate from generation, read, and traversal work.
//! Gauges and `val!` keys are bounded: counts and enumerated hydration sources
//! only — never paths, digests, query text, or identifiers.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HydrationSource {
    Live,
    Snapshot,
    Replay,
    Staged,
    Recovered,
    Sealed,
    Metadata,
    Supplied,
    Inline,
    SemanticVector,
}

impl HydrationSource {
    #[cfg(any(feature = "hotpath", test, feature = "test-helpers"))]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Snapshot => "snapshot",
            Self::Replay => "replay",
            Self::Staged => "staged",
            Self::Recovered => "recovered",
            Self::Sealed => "sealed",
            Self::Metadata => "metadata",
            Self::Supplied => "supplied",
            Self::Inline => "inline",
            Self::SemanticVector => "semantic_vector",
        }
    }
}

pub(crate) const LOCK_WAIT_DATABASE_READ: &str = "graph_db.lock.wait.database.read";
pub(crate) const LOCK_WAIT_DATABASE_WRITE: &str = "graph_db.lock.wait.database.write";
pub(crate) const LOCK_WAIT_STATE_WRITE: &str = "graph_db.lock.wait.state.write";
pub(crate) const LOCK_WAIT_SNAPSHOT_GATE_READ: &str = "graph_db.lock.wait.snapshot_gate.read";
pub(crate) const LOCK_WAIT_SNAPSHOT_GATE_WRITE: &str = "graph_db.lock.wait.snapshot_gate.write";
pub(crate) const LOCK_WAIT_SNAPSHOT_GATE_UPGRADABLE: &str =
    "graph_db.lock.wait.snapshot_gate.upgradable";
pub(crate) const LOCK_WAIT_SNAPSHOT_GATE_UPGRADE: &str = "graph_db.lock.wait.snapshot_gate.upgrade";
pub(crate) const LOCK_WAIT_REGISTRY: &str = "graph_db.lock.wait.registry";
pub(crate) const LOCK_WAIT_VERIFIED_GENERATIONS: &str = "graph_db.lock.wait.verified_generations";

#[inline(always)]
pub(crate) fn wait_lock<T>(label: &'static str, acquire: impl FnOnce() -> T) -> T {
    #[cfg(feature = "hotpath")]
    {
        hotpath::measure_block!(label, acquire())
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = label;
        acquire()
    }
}

#[inline(always)]
pub(crate) fn record_counts(
    nodes: usize,
    edges: usize,
    replay_rows: usize,
    generation_bytes: usize,
) {
    #[cfg(any(test, feature = "test-helpers"))]
    counters::record(nodes, edges, replay_rows, generation_bytes);
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("graph_db.nodes").set(nodes as f64);
        hotpath::gauge!("graph_db.edges").set(edges as f64);
        hotpath::gauge!("graph_db.replay_rows").set(replay_rows as f64);
        hotpath::gauge!("graph_db.generation_bytes").set(generation_bytes as f64);
    }
    #[cfg(not(any(feature = "hotpath", test, feature = "test-helpers")))]
    {
        let _ = (nodes, edges, replay_rows, generation_bytes);
    }
}

#[inline(always)]
pub(crate) fn record_hydration_source(source: HydrationSource) {
    #[cfg(any(test, feature = "test-helpers"))]
    counters::record_source(source);
    #[cfg(feature = "hotpath")]
    {
        hotpath::val!("graph_db.hydration_source").set(&source.as_str());
    }
    #[cfg(not(any(feature = "hotpath", test, feature = "test-helpers")))]
    {
        let _ = source;
    }
}

#[cfg(any(test, feature = "test-helpers"))]
mod counters {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    use super::HydrationSource;

    static NODES: AtomicU64 = AtomicU64::new(0);
    static EDGES: AtomicU64 = AtomicU64::new(0);
    static REPLAY_ROWS: AtomicU64 = AtomicU64::new(0);
    static GENERATION_BYTES: AtomicU64 = AtomicU64::new(0);
    static LAST_SOURCE: AtomicU8 = AtomicU8::new(0);

    fn source_code(source: HydrationSource) -> u8 {
        match source {
            HydrationSource::Live => 1,
            HydrationSource::Snapshot => 2,
            HydrationSource::Replay => 3,
            HydrationSource::Staged => 4,
            HydrationSource::Recovered => 5,
            HydrationSource::Sealed => 6,
            HydrationSource::Metadata => 7,
            HydrationSource::Supplied => 8,
            HydrationSource::Inline => 9,
            HydrationSource::SemanticVector => 10,
        }
    }

    fn source_from_code(code: u8) -> Option<&'static str> {
        match code {
            1 => Some(HydrationSource::Live.as_str()),
            2 => Some(HydrationSource::Snapshot.as_str()),
            3 => Some(HydrationSource::Replay.as_str()),
            4 => Some(HydrationSource::Staged.as_str()),
            5 => Some(HydrationSource::Recovered.as_str()),
            6 => Some(HydrationSource::Sealed.as_str()),
            7 => Some(HydrationSource::Metadata.as_str()),
            8 => Some(HydrationSource::Supplied.as_str()),
            9 => Some(HydrationSource::Inline.as_str()),
            10 => Some(HydrationSource::SemanticVector.as_str()),
            _ => None,
        }
    }

    pub(super) fn record(nodes: usize, edges: usize, replay_rows: usize, generation_bytes: usize) {
        NODES.fetch_add(nodes as u64, Ordering::Relaxed);
        EDGES.fetch_add(edges as u64, Ordering::Relaxed);
        REPLAY_ROWS.fetch_add(replay_rows as u64, Ordering::Relaxed);
        GENERATION_BYTES.fetch_add(generation_bytes as u64, Ordering::Relaxed);
    }

    pub(super) fn record_source(source: HydrationSource) {
        LAST_SOURCE.store(source_code(source), Ordering::Relaxed);
    }

    pub(crate) fn take() -> crate::GraphDbHydrationCounters {
        crate::GraphDbHydrationCounters {
            nodes: NODES.swap(0, Ordering::Relaxed),
            edges: EDGES.swap(0, Ordering::Relaxed),
            replay_rows: REPLAY_ROWS.swap(0, Ordering::Relaxed),
            generation_bytes: GENERATION_BYTES.swap(0, Ordering::Relaxed),
            hydration_source: source_from_code(LAST_SOURCE.swap(0, Ordering::Relaxed)),
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn take_hydration_counters() -> crate::GraphDbHydrationCounters {
    counters::take()
}

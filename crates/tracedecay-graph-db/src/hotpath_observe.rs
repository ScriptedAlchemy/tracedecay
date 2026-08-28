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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GrafeoMemoryPhase {
    Open,
    PublishStart,
    ReplayHydrated,
    NativeVerified,
    Published,
    RecoveryStart,
    Recovered,
}

impl GrafeoMemoryPhase {
    #[cfg(feature = "hotpath")]
    const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::PublishStart => "publish_start",
            Self::ReplayHydrated => "replay_hydrated",
            Self::NativeVerified => "native_verified",
            Self::Published => "published",
            Self::RecoveryStart => "recovery_start",
            Self::Recovered => "recovered",
        }
    }
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

/// Records Grafeo's own memory census at coarse lifecycle boundaries. The
/// census walks internal store structures, so it runs only in opt-in Hotpath
/// builds and never on graph query paths.
#[inline(always)]
#[cfg(feature = "hotpath")]
pub(crate) fn record_grafeo_memory(database: &grafeo_engine::GrafeoDB, phase: GrafeoMemoryPhase) {
    let usage = hotpath::measure_block!("graph_db.memory.census", database.memory_usage());
    hotpath::val!("graph_db.memory.phase").set(&phase.as_str());
    hotpath::gauge!("graph_db.memory.total_bytes").set(usage.total_bytes as f64);
    hotpath::gauge!("graph_db.memory.store_bytes").set(usage.store.total_bytes as f64);
    hotpath::gauge!("graph_db.memory.index_bytes").set(usage.indexes.total_bytes as f64);
    hotpath::gauge!("graph_db.memory.mvcc_bytes").set(usage.mvcc.total_bytes as f64);
    hotpath::gauge!("graph_db.memory.cache_bytes").set(usage.caches.total_bytes as f64);
    hotpath::gauge!("graph_db.memory.string_pool_bytes").set(usage.string_pool.total_bytes as f64);
    hotpath::gauge!("graph_db.memory.buffer_budget_bytes")
        .set(usage.buffer_manager.budget_bytes as f64);
    hotpath::gauge!("graph_db.memory.buffer_allocated_bytes")
        .set(usage.buffer_manager.allocated_bytes as f64);
}

/// Hydration observation counters for tests.
///
/// These are **thread-local**, not process-global. Every hydration site in this
/// crate records synchronously on the calling thread (the only `thread::spawn`
/// calls in the crate live inside `#[cfg(test)]` modules), so a thread-local
/// tally observes exactly the work the observing thread performed.
///
/// Process-global statics would be wrong here: `libtest` runs each test on its
/// own thread, so a shared tally lets any concurrently running test's
/// publication bleed into another test's `take()`. That made
/// `no_change_recover_does_not_reread_full_generation` fail roughly 3 runs in 10
/// at `--test-threads=16` while passing in isolation — the assertion was
/// correct, the counter was not isolated.
#[cfg(any(test, feature = "test-helpers"))]
mod counters {
    use std::cell::Cell;

    use super::HydrationSource;

    thread_local! {
        static NODES: Cell<u64> = const { Cell::new(0) };
        static EDGES: Cell<u64> = const { Cell::new(0) };
        static REPLAY_ROWS: Cell<u64> = const { Cell::new(0) };
        static GENERATION_BYTES: Cell<u64> = const { Cell::new(0) };
        static LAST_SOURCE: Cell<u8> = const { Cell::new(0) };
    }

    fn add(cell: &'static std::thread::LocalKey<Cell<u64>>, amount: usize) {
        cell.with(|value| value.set(value.get().saturating_add(amount as u64)));
    }

    fn take_u64(cell: &'static std::thread::LocalKey<Cell<u64>>) -> u64 {
        cell.with(|value| value.replace(0))
    }

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
        add(&NODES, nodes);
        add(&EDGES, edges);
        add(&REPLAY_ROWS, replay_rows);
        add(&GENERATION_BYTES, generation_bytes);
    }

    pub(super) fn record_source(source: HydrationSource) {
        LAST_SOURCE.with(|value| value.set(source_code(source)));
    }

    pub(crate) fn take() -> crate::GraphDbHydrationCounters {
        crate::GraphDbHydrationCounters {
            nodes: take_u64(&NODES),
            edges: take_u64(&EDGES),
            replay_rows: take_u64(&REPLAY_ROWS),
            generation_bytes: take_u64(&GENERATION_BYTES),
            hydration_source: source_from_code(LAST_SOURCE.with(|value| value.replace(0))),
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn take_hydration_counters() -> crate::GraphDbHydrationCounters {
    counters::take()
}

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
    #[cfg(feature = "hotpath")]
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
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("graph_db.nodes").set(nodes as f64);
        hotpath::gauge!("graph_db.edges").set(edges as f64);
        hotpath::gauge!("graph_db.replay_rows").set(replay_rows as f64);
        hotpath::gauge!("graph_db.generation_bytes").set(generation_bytes as f64);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = (nodes, edges, replay_rows, generation_bytes);
    }
}

#[inline(always)]
pub(crate) fn record_hydration_source(source: HydrationSource) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::val!("graph_db.hydration_source").set(&source.as_str());
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = source;
    }
}

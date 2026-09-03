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
    #[hotpath::skip]
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
    #[hotpath::skip]
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

/// Records how one sealed generation's recovered digest was established.
///
/// `canonical_bytes` is the size of the canonical row stream the full proof
/// hashes. A marker hit reports the byte count the earlier proof recorded, so
/// the two gauges are directly comparable: `marker_hit` bytes are the bytes
/// *not* re-hashed on this open.
#[inline(always)]
pub(crate) fn record_generation_verification(
    outcome: crate::verified_marker::GenerationVerification,
    canonical_bytes: u64,
) {
    #[cfg(any(test, feature = "test-helpers"))]
    counters::record_verification(outcome, canonical_bytes);
    #[cfg(feature = "hotpath")]
    {
        use crate::verified_marker::GenerationVerification;
        match outcome {
            GenerationVerification::VerifiedFresh => {
                hotpath::gauge!("graph_db.generation.verify.marker_hit").inc(1);
                hotpath::gauge!("graph_db.generation.verify.marker_hit_bytes")
                    .set(canonical_bytes as f64);
            }
            GenerationVerification::Reverified => {
                hotpath::gauge!("graph_db.generation.verify.full").inc(1);
                hotpath::gauge!("graph_db.generation.verify.full_bytes")
                    .set(canonical_bytes as f64);
            }
        }
        hotpath::val!("graph_db.generation.verify.outcome").set(&outcome.as_str());
    }
    #[cfg(not(any(feature = "hotpath", test, feature = "test-helpers")))]
    {
        let _ = (outcome, canonical_bytes);
    }
}

/// Records how one sealed per-generation copy's recovered digest was
/// established at open: a full post-reopen row proof, or a
/// verified-generation marker over byte-identical container bytes.
///
/// Kept apart from [`record_generation_verification`] so the staging
/// container's proof counters — which the publication contract tests pin —
/// stay a statement about the authority's rows.
#[inline(always)]
pub(crate) fn record_sealed_copy_verification(
    outcome: crate::verified_marker::GenerationVerification,
    canonical_bytes: u64,
) {
    #[cfg(feature = "hotpath")]
    {
        use crate::verified_marker::GenerationVerification;
        match outcome {
            GenerationVerification::VerifiedFresh => {
                hotpath::gauge!("graph_db.sealed_store.verify.marker_hit").inc(1);
                hotpath::gauge!("graph_db.sealed_store.verify.marker_hit_bytes")
                    .set(canonical_bytes as f64);
            }
            GenerationVerification::Reverified => {
                hotpath::gauge!("graph_db.sealed_store.verify.full").inc(1);
                hotpath::gauge!("graph_db.sealed_store.verify.full_bytes")
                    .set(canonical_bytes as f64);
            }
        }
        hotpath::val!("graph_db.sealed_store.verify.outcome").set(&outcome.as_str());
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = (outcome, canonical_bytes);
    }
}

/// Records the size of one vector index, in vectors and in heap bytes.
///
/// Emitted wherever an index's coverage is inspected, so a build that
/// silently covered nothing shows up as a zero rather than as an
/// indistinguishable success.
#[inline(always)]
pub(crate) fn record_vector_index_size(vectors: usize, bytes: usize) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("graph_db.vector_index.vectors").set(vectors as f64);
        hotpath::gauge!("graph_db.vector_index.bytes").set(bytes as f64);
    }
    #[cfg(not(feature = "hotpath"))]
    {
        let _ = (vectors, bytes);
    }
}

/// Records what an open found already indexed, before anything rebuilds.
///
/// The gauge that answers "did the reopen restore the index or is the
/// daemon about to pay for it again": non-zero means the store came back
/// with coverage, zero means every vector search is unavailable until a
/// rebuild finishes. Hotpath-only like [`record_grafeo_memory`]: its sole
/// caller takes the census behind the same feature gate.
#[inline(always)]
#[cfg(feature = "hotpath")]
pub(crate) fn record_vector_index_restore(indexes: usize, vectors: usize, bytes: usize) {
    hotpath::gauge!("graph_db.vector_index.restore.indexes").set(indexes as f64);
    hotpath::gauge!("graph_db.vector_index.restore.vectors").set(vectors as f64);
    hotpath::gauge!("graph_db.vector_index.restore.bytes").set(bytes as f64);
}

/// Records what a close is about to write out as index topology.
///
/// Paired with [`record_vector_index_restore`]: the bytes reported here
/// are the bytes the next open should find, and a restore gauge well
/// below the preceding persist gauge is the signal that durability is
/// leaking somewhere between the two.
#[inline(always)]
#[cfg(feature = "hotpath")]
pub(crate) fn record_vector_index_persist(indexes: usize, vectors: usize, bytes: usize) {
    hotpath::gauge!("graph_db.vector_index.persist.indexes").set(indexes as f64);
    hotpath::gauge!("graph_db.vector_index.persist.vectors").set(vectors as f64);
    hotpath::gauge!("graph_db.vector_index.persist.bytes").set(bytes as f64);
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

#[cfg(any(test, feature = "test-helpers"))]
/// Test-observable counters for hydration and sealed-generation verification.
///
/// Every counter here is **thread-local**. Both surfaces are drained by tests
/// that assert on exact counts, and the work they count runs synchronously on
/// whichever thread called into the store. Process-global counters therefore
/// let a concurrently running test's publication land inside another test's
/// reading -- a flake that tracks the scheduler rather than the code under
/// test. Scoping to the calling thread makes each reading exactly the work
/// that thread drove, at any `--test-threads`.
///
/// The one thing this cannot see is work performed on a thread the caller did
/// not drive. That fails a counting assertion loudly rather than silently
/// miscounting, which is the failure mode to prefer.
mod counters {
    use super::HydrationSource;

    /// Verification outcomes are counted **per thread**, not per process.
    ///
    /// A test that proves a marker hit skipped the row enumeration asserts on
    /// an exact count, and sealed-generation verification runs synchronously on
    /// whichever thread called `publish_verified` or
    /// `recover_verified_snapshot`. Process-global counters therefore let any
    /// concurrently running test's publications land in another test's reading,
    /// which is a flake that appears and disappears with the scheduler rather
    /// than with the code under test. Scoping to the calling thread makes each
    /// reading exactly the work that thread performed, at any `--test-threads`.
    ///
    /// The one thing this cannot see is a verification performed on a thread
    /// the caller did not drive. That fails a counting assertion loudly rather
    /// than silently miscounting, which is the failure mode to prefer.
    #[derive(Clone, Copy, Default)]
    struct VerificationCounts {
        marker_hits: u64,
        marker_hit_bytes: u64,
        full_verifications: u64,
        full_verification_bytes: u64,
    }

    thread_local! {
        static VERIFICATION: std::cell::Cell<VerificationCounts> =
            const { std::cell::Cell::new(VerificationCounts {
                marker_hits: 0,
                marker_hit_bytes: 0,
                full_verifications: 0,
                full_verification_bytes: 0,
            }) };
    }

    pub(super) fn record_verification(
        outcome: crate::verified_marker::GenerationVerification,
        canonical_bytes: u64,
    ) {
        use crate::verified_marker::GenerationVerification;
        VERIFICATION.with(|cell| {
            let mut counts = cell.get();
            match outcome {
                GenerationVerification::VerifiedFresh => {
                    counts.marker_hits += 1;
                    counts.marker_hit_bytes += canonical_bytes;
                }
                GenerationVerification::Reverified => {
                    counts.full_verifications += 1;
                    counts.full_verification_bytes += canonical_bytes;
                }
            }
            cell.set(counts);
        });
    }

    /// Drains this thread's verification counts, leaving them at zero.
    pub(crate) fn take_verification() -> crate::GraphDbVerificationCounters {
        let counts = VERIFICATION.with(|cell| cell.replace(VerificationCounts::default()));
        crate::GraphDbVerificationCounters {
            marker_hits: counts.marker_hits,
            marker_hit_bytes: counts.marker_hit_bytes,
            full_verifications: counts.full_verifications,
            full_verification_bytes: counts.full_verification_bytes,
        }
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

    #[derive(Clone, Copy, Default)]
    struct HydrationCounts {
        nodes: u64,
        edges: u64,
        replay_rows: u64,
        generation_bytes: u64,
        last_source: u8,
    }

    thread_local! {
        static HYDRATION: std::cell::Cell<HydrationCounts> =
            const { std::cell::Cell::new(HydrationCounts {
                nodes: 0,
                edges: 0,
                replay_rows: 0,
                generation_bytes: 0,
                last_source: 0,
            }) };
    }

    pub(super) fn record(nodes: usize, edges: usize, replay_rows: usize, generation_bytes: usize) {
        HYDRATION.with(|cell| {
            let mut counts = cell.get();
            counts.nodes += nodes as u64;
            counts.edges += edges as u64;
            counts.replay_rows += replay_rows as u64;
            counts.generation_bytes += generation_bytes as u64;
            cell.set(counts);
        });
    }

    pub(super) fn record_source(source: HydrationSource) {
        HYDRATION.with(|cell| {
            let mut counts = cell.get();
            counts.last_source = source_code(source);
            cell.set(counts);
        });
    }

    /// Drains this thread's hydration counts, leaving them at zero.
    pub(crate) fn take() -> crate::GraphDbHydrationCounters {
        let counts = HYDRATION.with(|cell| cell.replace(HydrationCounts::default()));
        crate::GraphDbHydrationCounters {
            nodes: counts.nodes,
            edges: counts.edges,
            replay_rows: counts.replay_rows,
            generation_bytes: counts.generation_bytes,
            hydration_source: source_from_code(counts.last_source),
        }
    }
}

#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn take_hydration_counters() -> crate::GraphDbHydrationCounters {
    counters::take()
}

#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn take_verification_counters() -> crate::GraphDbVerificationCounters {
    counters::take_verification()
}

#[inline(always)]
pub(crate) fn record_property_decode() {
    #[cfg(any(test, feature = "test-helpers"))]
    traversal_counters::record_property_decode();
}

#[inline(always)]
pub(crate) fn record_relation_identity_decode() {
    #[cfg(any(test, feature = "test-helpers"))]
    traversal_counters::record_relation_identity_decode();
}

#[inline(always)]
pub(crate) fn record_quarantine_lock() {
    #[cfg(any(test, feature = "test-helpers"))]
    traversal_counters::record_quarantine_lock();
}

#[inline(always)]
pub(crate) fn record_label_universe_scan() {
    #[cfg(any(test, feature = "test-helpers"))]
    traversal_counters::record_label_universe_scan();
}

#[inline(always)]
pub(crate) fn record_adjacency_index_build() {
    #[cfg(any(test, feature = "test-helpers"))]
    traversal_counters::record_adjacency_index_build();
}

#[inline(always)]
pub(crate) fn record_adjacency_index_hit() {
    #[cfg(any(test, feature = "test-helpers"))]
    traversal_counters::record_adjacency_index_hit();
}

#[cfg(any(test, feature = "test-helpers"))]
pub(crate) fn take_traversal_counters() -> crate::GraphDbTraversalCounters {
    traversal_counters::take()
}

/// Operation counts for ID-only fan-out, quarantine snapshots, and label/adjacency
/// caches. Thread-local for the same reason as hydration counters: a parallel
/// test must not observe another test's decode or lock work.
#[cfg(any(test, feature = "test-helpers"))]
mod traversal_counters {
    #[derive(Clone, Copy, Default)]
    struct Counts {
        property_decodes: u64,
        relation_identity_decodes: u64,
        quarantine_lock_acquisitions: u64,
        label_universe_scans: u64,
        adjacency_index_builds: u64,
        adjacency_index_hits: u64,
    }

    thread_local! {
        static COUNTS: std::cell::Cell<Counts> = const { std::cell::Cell::new(Counts {
            property_decodes: 0,
            relation_identity_decodes: 0,
            quarantine_lock_acquisitions: 0,
            label_universe_scans: 0,
            adjacency_index_builds: 0,
            adjacency_index_hits: 0,
        }) };
    }

    fn bump(update: impl FnOnce(&mut Counts)) {
        COUNTS.with(|cell| {
            let mut counts = cell.get();
            update(&mut counts);
            cell.set(counts);
        });
    }

    pub(super) fn record_property_decode() {
        bump(|counts| counts.property_decodes += 1);
    }

    pub(super) fn record_relation_identity_decode() {
        bump(|counts| counts.relation_identity_decodes += 1);
    }

    pub(super) fn record_quarantine_lock() {
        bump(|counts| counts.quarantine_lock_acquisitions += 1);
    }

    pub(super) fn record_label_universe_scan() {
        bump(|counts| counts.label_universe_scans += 1);
    }

    pub(super) fn record_adjacency_index_build() {
        bump(|counts| counts.adjacency_index_builds += 1);
    }

    pub(super) fn record_adjacency_index_hit() {
        bump(|counts| counts.adjacency_index_hits += 1);
    }

    pub(crate) fn take() -> crate::GraphDbTraversalCounters {
        let counts = COUNTS.with(|cell| cell.replace(Counts::default()));
        crate::GraphDbTraversalCounters {
            property_decodes: counts.property_decodes,
            relation_identity_decodes: counts.relation_identity_decodes,
            quarantine_lock_acquisitions: counts.quarantine_lock_acquisitions,
            label_universe_scans: counts.label_universe_scans,
            adjacency_index_builds: counts.adjacency_index_builds,
            adjacency_index_hits: counts.adjacency_index_hits,
        }
    }
}

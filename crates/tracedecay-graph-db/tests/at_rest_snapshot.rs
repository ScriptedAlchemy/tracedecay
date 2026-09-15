//! At-rest snapshot probe: full LPG replay vs a columnar `CompactStore` base.
//!
//! Ignored by default. This is a measurement harness, not a contract — nothing
//! here asserts on a timing or a memory figure. It answers one question:
//!
//! > Reopening a *sealed* generation costs a full replay of every node and edge
//! > into grafeo's in-RAM `LpgStore`. If the generation were frozen into a
//! > columnar `CompactStore` at seal time instead, what would the reopen cost?
//!
//! # Running it
//!
//! One scenario per process, on purpose. `VmHWM` is a process-wide high-water
//! mark that only ever rises, so two scenarios in one process would report each
//! other's peaks and the second one's publish peak would be meaningless. The
//! mode and the scale come from the environment so a single `#[test]` can be
//! invoked once per cell of the matrix:
//!
//! ```text
//! TRACEDECAY_ATREST_MODE=replay  TRACEDECAY_ATREST_ROWS=500000 \
//!   cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
//!   --test at_rest_snapshot -- --ignored --nocapture --exact at_rest_reopen_probe
//!
//! TRACEDECAY_ATREST_MODE=compact TRACEDECAY_ATREST_ROWS=500000 \
//!   cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
//!   --test at_rest_snapshot -- --ignored --nocapture --exact at_rest_reopen_probe
//! ```
//!
//! `graph-disk-tier` is required for the `compact` mode: it turns on
//! `grafeo-engine/compact-store`, without which `GrafeoDB::compact()` does not
//! exist. The `replay` mode runs under either feature state and is the baseline.
//!
//! # What the two modes do differently
//!
//! Exactly one call. Both stage the same rows through the same public
//! `apply_unverified` path and close through the same registry. `compact` mode
//! additionally calls [`GraphDb::compact_snapshot_for_bench`] after the last
//! commit and before the close. That single call swaps grafeo's live store for a
//! `LayeredStore` (columnar base + empty overlay), which changes what `close`
//! serializes and what the next `open` has to reconstruct:
//!
//! * replay: close writes a `SectionType::LpgStore` block log; open replays it
//!   mutation-by-mutation back into MVCC arenas.
//! * compact: close writes a `SectionType::CompactStore` columnar section plus
//!   an empty overlay; open deserializes the columnar tables directly.
//!
//! # Reading the numbers
//!
//! * `open wall` is the figure that maps onto activation of a sealed
//!   generation. This is the 619s number the investigation is chasing.
//! * `VmRSS after open` is the residency a daemon carries per open generation.
//! * `first read` exercises the reads that actually have to work afterwards —
//!   point lookups and a bounded traversal — so a fast open that cannot answer
//!   a query is not scored as a win.
//! * `VmHWM delta over close` is the publish peak: the transient cost of
//!   serializing the generation out. Only comparable across separate processes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDbLeaseV1, GraphEntity, GraphEntityId, GraphLabel, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName, GraphRelation,
    GraphRelationId, GraphRelationKind, GraphTraversalDirection, GraphWatermark, GraphWriteBatch,
    NeverCancelled, SourceGeneration, TraversalRequest,
};

mod support;

use support::RegisteredGraph;

/// Mirrors the crate-private `limits::MAX_NATIVE_GENERATION_STAGE_MUTATIONS`,
/// the page size the native generation runtime actually flushes at.
const PRODUCTION_STAGE_PAGE: usize = 65_536;

/// Default row count when `TRACEDECAY_ATREST_ROWS` is unset.
const DEFAULT_ROWS: usize = 500_000;

/// Relations staged per entity, as a divisor: `rows / RELATION_DIVISOR`
/// relations are written. They chain `entity-i -> entity-(i+1)` so the graph
/// has a genuinely deep path for the traversal probe to walk rather than a
/// star. Edges are not free and a node-only probe would flatter the columnar
/// store, which stores adjacency as CSR.
const RELATION_DIVISOR: usize = 4;

/// Point reads issued after open, spread across the whole id range so the
/// measurement is not served by whatever the open path happened to touch last.
const POINT_READS: usize = 64;

/// Depth bound for the traversal probe.
const TRAVERSAL_DEPTH: usize = 8;

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn namespace() -> GraphNamespace {
    GraphNamespace::new("project").unwrap()
}

fn projection() -> GraphProjectionId {
    GraphProjectionId::new("code").unwrap()
}

fn entity_id(index: usize) -> GraphEntityId {
    GraphEntityId::new(format!("entity-{index}")).unwrap()
}

fn relation_kind() -> GraphRelationKind {
    GraphRelationKind::new("calls").unwrap()
}

fn entity(index: usize) -> GraphEntity {
    let mut labels = BTreeSet::new();
    labels.insert(GraphLabel::new("Symbol").unwrap());
    let mut properties = BTreeMap::new();
    properties.insert(
        GraphPropertyName::new("name").unwrap(),
        GraphProperty::String(format!("symbol-{index}")),
    );
    GraphEntity::new(entity_id(index), labels, properties).unwrap()
}

fn relation(index: usize) -> GraphRelation {
    GraphRelation::new(
        GraphRelationId::new(format!("relation-{index}")).unwrap(),
        entity_id(index),
        entity_id(index + 1),
        relation_kind(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn batch(watermark: &str, mutations: Vec<GraphMutation>) -> GraphWriteBatch {
    GraphWriteBatch::new(
        namespace(),
        projection(),
        SourceGeneration::new("generation-1").unwrap(),
        GraphWatermark::new(watermark).unwrap(),
        mutations,
        live(),
    )
    .unwrap()
}

fn status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let prefix = format!("{field}:");
    status
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

fn peak_rss_kib() -> Option<u64> {
    status_kib("VmHWM")
}

fn live_rss_kib() -> Option<u64> {
    status_kib("VmRSS")
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

/// Which at-rest form this run measures.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Today's behaviour: close serializes the LPG block log, open replays it.
    Replay,
    /// Freeze to a columnar `CompactStore` base before close.
    Compact,
}

impl Mode {
    fn from_env() -> Self {
        match std::env::var("TRACEDECAY_ATREST_MODE").as_deref() {
            Ok("compact") => Self::Compact,
            _ => Self::Replay,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Replay => "replay (LpgStore block log)",
            Self::Compact => "compact (CompactStore columnar base)",
        }
    }
}

/// Stages `rows` entities and `rows / RELATION_DIVISOR` relations in
/// production-sized pages.
fn stage(db: &GraphDbLeaseV1, rows: usize) {
    for (page, start) in (0..rows).step_by(PRODUCTION_STAGE_PAGE).enumerate() {
        let end = (start + PRODUCTION_STAGE_PAGE).min(rows);
        let mutations = (start..end)
            .map(|index| GraphMutation::UpsertEntity(entity(index)))
            .collect();
        db.apply_unverified(batch(&format!("watermark-entity-{page}"), mutations))
            .unwrap();
    }

    let relations = rows / RELATION_DIVISOR;
    for (page, start) in (0..relations).step_by(PRODUCTION_STAGE_PAGE).enumerate() {
        let end = (start + PRODUCTION_STAGE_PAGE).min(relations);
        let mutations = (start..end)
            .map(|index| GraphMutation::UpsertRelation(relation(index)))
            .collect();
        db.apply_unverified(batch(&format!("watermark-relation-{page}"), mutations))
            .unwrap();
    }
}

/// Outcome of the post-open read probe.
///
/// Read failures are *recorded*, not panicked on. A store that opens fast but
/// cannot answer a query is the most interesting result this harness can
/// produce, and it is worth a full numbers table rather than a backtrace.
struct FirstReads {
    point_wall: Duration,
    hits: usize,
    point_error: Option<String>,
    traversal_wall: Duration,
    visits: usize,
    traversal_error: Option<String>,
}

/// Exercises the reads a reopened generation actually has to serve.
///
/// These are deliberately real calls through the public read surface rather
/// than a claim that the store "would" answer them: a columnar base that opens
/// instantly but cannot resolve an entity is not a win, and this is where that
/// shows up — as a zero hit count or a resolution error.
fn first_reads(db: &GraphDbLeaseV1, rows: usize) -> FirstReads {
    let namespace = namespace();

    let stride = (rows / POINT_READS).max(1);
    let started = Instant::now();
    let mut hits = 0_usize;
    let mut point_error = None;
    for step in 0..POINT_READS {
        let index = (step * stride) % rows;
        match db.entity(&namespace, &entity_id(index), live()) {
            Ok(Some(_)) => hits += 1,
            Ok(None) => {}
            Err(error) => {
                point_error.get_or_insert_with(|| error.to_string());
            }
        }
    }
    let point_wall = started.elapsed();

    let mut kinds = BTreeSet::new();
    kinds.insert(relation_kind());
    let started = Instant::now();
    let traversal = db.traverse(TraversalRequest {
        namespace,
        start: entity_id(0),
        relation_kinds: kinds,
        direction: GraphTraversalDirection::Outgoing,
        max_depth: TRAVERSAL_DEPTH,
        max_visits: 4096,
        max_results: 4096,
        cancellation: live(),
    });
    let traversal_wall = started.elapsed();

    let (visits, traversal_error) = match traversal {
        Ok(result) => (result.visits.len(), None),
        Err(error) => (0, Some(error.to_string())),
    };

    FirstReads {
        point_wall,
        hits,
        point_error,
        traversal_wall,
        visits,
        traversal_error,
    }
}

#[test]
#[ignore = "at-rest measurement harness; run one scenario per process, see module docs"]
fn at_rest_reopen_probe() {
    let (Some(hwm_start), Some(rss_start)) = (peak_rss_kib(), live_rss_kib()) else {
        println!("/proc/self/status unavailable on this platform; skipping");
        return;
    };

    let rows = std::env::var("TRACEDECAY_ATREST_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    let mode = Mode::from_env();

    #[cfg(not(feature = "graph-disk-tier"))]
    if mode == Mode::Compact {
        println!("compact mode needs --features graph-disk-tier; skipping");
        return;
    }

    let temp = TempDir::new().unwrap();
    let root = temp.path().join("store");
    std::fs::create_dir_all(&root).unwrap();
    let (registered, db) = RegisteredGraph::open_lease(&root).unwrap();

    let started = Instant::now();
    stage(&db, rows);
    let stage_wall = started.elapsed();

    // The seal. In compact mode this is the whole intervention.
    // Only ever assigned under `graph-disk-tier`, where `compact()` exists.
    #[cfg_attr(not(feature = "graph-disk-tier"), allow(unused_mut))]
    let mut compact_error: Option<String> = None;
    let compact_started = Instant::now();
    #[cfg(feature = "graph-disk-tier")]
    if mode == Mode::Compact
        && let Err(error) = db.compact_snapshot_for_bench()
    {
        compact_error = Some(error.to_string());
    }
    let compact_wall = compact_started.elapsed();

    let hwm_staged = peak_rss_kib().unwrap();
    let rss_staged = live_rss_kib().unwrap();

    // ---- publish path ----
    let close_started = Instant::now();
    drop(db);
    assert!(registered.close().unwrap());
    let close_wall = close_started.elapsed();

    let hwm_closed = peak_rss_kib().unwrap();
    let rss_closed = live_rss_kib().unwrap();

    let on_disk = std::fs::metadata(root.join("graph.grafeo"))
        .map(|meta| meta.len())
        .unwrap_or(0);

    // ---- activation path ----
    let open_started = Instant::now();
    let reopened = registered.reopen_lease().unwrap();
    let open_wall = open_started.elapsed();

    let rss_open = live_rss_kib().unwrap();
    let hwm_open = peak_rss_kib().unwrap();

    let reads = first_reads(&reopened, rows);

    let rss_after_reads = live_rss_kib().unwrap();

    drop(reopened);
    assert!(registered.close().unwrap());

    let relations = rows / RELATION_DIVISOR;
    println!("=== at-rest reopen probe ===");
    println!("mode              : {}", mode.label());
    println!(
        "rows              : {rows} entities + {relations} relations, pages of {PRODUCTION_STAGE_PAGE}"
    );
    println!("--- staging ---");
    println!("stage wall        : {:.2}s", stage_wall.as_secs_f64());
    println!("compact wall      : {:.2}s", compact_wall.as_secs_f64());
    match &compact_error {
        Some(error) => println!("compact result    : FAILED -- {error}"),
        None if mode == Mode::Compact => println!("compact result    : ok"),
        None => {}
    }
    println!(
        "VmRSS staged      : {rss_staged} KiB ({:.1} MiB)",
        mib(rss_staged)
    );
    println!("--- publish (close) ---");
    println!("close wall        : {:.2}s", close_wall.as_secs_f64());
    println!(
        "VmHWM over close  : {} KiB ({:.1} MiB)  <- publish peak",
        hwm_closed - hwm_start,
        mib(hwm_closed - hwm_start)
    );
    println!(
        "VmRSS after close : {rss_closed} KiB ({:.1} MiB)",
        mib(rss_closed)
    );
    println!(
        "on-disk store     : {on_disk} bytes ({:.1} MiB)",
        on_disk as f64 / (1024.0 * 1024.0)
    );
    println!("--- activation (reopen) ---");
    println!(
        "open wall         : {:.3}s  <- activation cost",
        open_wall.as_secs_f64()
    );
    println!(
        "VmRSS after open  : {rss_open} KiB ({:.1} MiB)",
        mib(rss_open)
    );
    println!("--- first reads ---");
    println!(
        "point reads       : {POINT_READS} reads in {:.3}ms, {} hits",
        reads.point_wall.as_secs_f64() * 1000.0,
        reads.hits
    );
    if let Some(error) = &reads.point_error {
        println!("point read error  : {error}");
    }
    println!(
        "traversal         : depth {TRAVERSAL_DEPTH} in {:.3}ms, {} visits",
        reads.traversal_wall.as_secs_f64() * 1000.0,
        reads.visits
    );
    if let Some(error) = &reads.traversal_error {
        println!("traversal error   : {error}");
    }
    println!(
        "VmRSS after reads : {rss_after_reads} KiB ({:.1} MiB)",
        mib(rss_after_reads)
    );
    println!("--- process ---");
    println!(
        "VmRSS baseline    : {rss_start} KiB ({:.1} MiB)",
        mib(rss_start)
    );
    println!(
        "VmHWM final       : {hwm_open} KiB ({:.1} MiB)",
        mib(hwm_open)
    );
    println!(
        "VmHWM staged      : {hwm_staged} KiB ({:.1} MiB)",
        mib(hwm_staged)
    );

    // The only assertions in the harness, and they are correctness gates
    // rather than performance ones: a reopen that cannot answer reads is not a
    // faster reopen, it is a broken one. They run last so the numbers above are
    // always printed, including on the runs where the at-rest form is the thing
    // that breaks.
    assert_eq!(
        reads.hits,
        POINT_READS,
        "reopened store lost point reads (mode: {})",
        mode.label()
    );
    assert!(
        reads.visits > 1,
        "reopened store lost adjacency (mode: {})",
        mode.label()
    );
}

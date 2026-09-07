//! Schema-neutral at-rest probe, straight against grafeo.
//!
//! Ignored by default; a measurement harness, not a contract.
//!
//! # Why this exists alongside `at_rest_snapshot.rs`
//!
//! `at_rest_snapshot.rs` measures TraceDecay's *actual* graph and finds that
//! `GrafeoDB::compact()` cannot be applied to it at all: TraceDecay mints one
//! synthetic key label per entity and per relation
//! (`schema::entity_key_label` / `schema::relation_key_label`) and resolves
//! every lookup through `GraphStore::nodes_by_label`, so a graph of N entities
//! and M relations presents roughly `N + M` distinct labels. `CompactStore`
//! allocates one columnar node table per distinct label key and addresses
//! tables with a `u16`, so compaction hard-fails past 32,767 of them.
//!
//! That failure is a property of *TraceDecay's schema*, not of the columnar
//! format. This probe removes the schema from the question: it builds the same
//! order of magnitude of nodes and edges under a handful of shared labels —
//! the shape TraceDecay would have if entity identity were a property with an
//! index rather than a label — and measures the same close/reopen cycle.
//!
//! The point is to price the schema change. If the columnar form is not
//! materially faster to reopen even under a sane label count, then rewriting
//! TraceDecay's entity index buys nothing and the at-rest work should stop
//! here. If it is much faster, that number is the payoff the schema change is
//! competing for.
//!
//! # Running it
//!
//! One scenario per process, same reasoning as `at_rest_snapshot.rs`: `VmHWM`
//! is process-wide and monotonic.
//!
//! ```text
//! TRACEDECAY_ATREST_MODE=replay  TRACEDECAY_ATREST_ROWS=500000 \
//!   cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
//!   --test at_rest_grafeo_probe -- --ignored --nocapture --exact grafeo_at_rest_probe
//!
//! TRACEDECAY_ATREST_MODE=compact TRACEDECAY_ATREST_ROWS=500000 \
//!   cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
//!   --test at_rest_grafeo_probe -- --ignored --nocapture --exact grafeo_at_rest_probe
//! ```

use std::time::{Duration, Instant};

use grafeo_common::types::{NodeId, Value};
use grafeo_engine::Config;
use grafeo_engine::GrafeoDB;
use grafeo_engine::config::StorageFormat;
use tempfile::TempDir;

/// Default node count when `TRACEDECAY_ATREST_ROWS` is unset.
const DEFAULT_ROWS: usize = 500_000;

/// Edges per node, as a divisor, matching `at_rest_snapshot.rs`.
const EDGE_DIVISOR: usize = 4;

/// Distinct node labels. A realistic small set, not one per entity — that is
/// the whole point of this probe.
const LABELS: [&str; 4] = ["Symbol", "File", "Module", "Chunk"];

/// Point lookups issued after reopen.
const POINT_READS: usize = 64;

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

fn compact_mode() -> bool {
    matches!(
        std::env::var("TRACEDECAY_ATREST_MODE").as_deref(),
        Ok("compact")
    )
}

fn config(path: &std::path::Path) -> Config {
    Config::persistent(path).with_storage_format(StorageFormat::SingleFile)
}

/// Builds `rows` nodes across [`LABELS`] plus `rows / EDGE_DIVISOR` chained
/// edges. Returns the node ids in creation order.
fn build(database: &GrafeoDB, rows: usize) -> Vec<NodeId> {
    let store = database
        .graph_store_mut()
        .expect("grafeo database exposes a mutable store");
    let mut ids = Vec::with_capacity(rows);
    for index in 0..rows {
        let label = LABELS[index % LABELS.len()];
        let node = store.create_node(&[label]);
        store.set_node_property(
            node,
            "name",
            Value::String(format!("symbol-{index}").into()),
        );
        store.set_node_property(node, "idx", Value::Int64(index as i64));
        ids.push(node);
    }

    let edges = rows / EDGE_DIVISOR;
    for index in 0..edges {
        store.create_edge(ids[index], ids[index + 1], "calls");
    }

    ids
}

/// Point lookups plus an adjacency walk, so a fast open that cannot answer a
/// query shows up as a miss rather than passing silently.
fn first_reads(database: &GrafeoDB, ids: &[NodeId]) -> (Duration, usize, Duration, usize) {
    let store = database.graph_store();

    let stride = (ids.len() / POINT_READS).max(1);
    let started = Instant::now();
    let mut hits = 0_usize;
    for step in 0..POINT_READS {
        let index = (step * stride) % ids.len();
        if store.get_node(ids[index]).is_some() {
            hits += 1;
        }
    }
    let point_wall = started.elapsed();

    let started = Instant::now();
    let mut walked = 0_usize;
    let mut current = ids[0];
    for _ in 0..8 {
        let outgoing = store.edges_from(current, grafeo_core::graph::Direction::Outgoing);
        let Some(&(next, _)) = outgoing.first() else {
            break;
        };
        walked += 1;
        current = next;
    }
    let walk_wall = started.elapsed();

    (point_wall, hits, walk_wall, walked)
}

#[test]
#[ignore = "at-rest measurement harness; run one scenario per process, see module docs"]
fn grafeo_at_rest_probe() {
    let (Some(hwm_start), Some(rss_start)) = (peak_rss_kib(), live_rss_kib()) else {
        println!("/proc/self/status unavailable on this platform; skipping");
        return;
    };

    let rows = std::env::var("TRACEDECAY_ATREST_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    let compact = compact_mode();

    let temp = TempDir::new().unwrap();
    let path = temp.path().join("probe.grafeo");

    // `compact()` takes `&mut self`, and only exists under `graph-disk-tier`.
    #[cfg_attr(not(feature = "graph-disk-tier"), allow(unused_mut))]
    let mut database = GrafeoDB::with_config(config(&path)).unwrap();

    let started = Instant::now();
    let ids = build(&database, rows);
    let build_wall = started.elapsed();

    let mut compact_error = None;
    let compact_started = Instant::now();
    if compact {
        #[cfg(feature = "graph-disk-tier")]
        if let Err(error) = database.compact() {
            compact_error = Some(error.to_string());
        }
        #[cfg(not(feature = "graph-disk-tier"))]
        {
            compact_error = Some("graph-disk-tier feature is off".to_owned());
        }
    }
    let compact_wall = compact_started.elapsed();

    let rss_built = live_rss_kib().unwrap();

    let close_started = Instant::now();
    database.close().unwrap();
    drop(database);
    let close_wall = close_started.elapsed();

    let hwm_closed = peak_rss_kib().unwrap();
    let rss_closed = live_rss_kib().unwrap();

    let on_disk = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);

    let open_started = Instant::now();
    let reopened = GrafeoDB::open(&path).unwrap();
    let open_wall = open_started.elapsed();

    let rss_open = live_rss_kib().unwrap();
    let (point_wall, hits, walk_wall, walked) = first_reads(&reopened, &ids);

    let node_count = reopened.graph_store().node_count();
    drop(reopened);

    let edges = rows / EDGE_DIVISOR;
    println!("=== grafeo schema-neutral at-rest probe ===");
    println!(
        "mode              : {}",
        if compact {
            "compact (CompactStore base)"
        } else {
            "replay (LpgStore block log)"
        }
    );
    println!(
        "rows              : {rows} nodes over {} labels + {edges} edges",
        LABELS.len()
    );
    println!("build wall        : {:.2}s", build_wall.as_secs_f64());
    println!("compact wall      : {:.2}s", compact_wall.as_secs_f64());
    match &compact_error {
        Some(error) => println!("compact result    : FAILED -- {error}"),
        None if compact => println!("compact result    : ok"),
        None => {}
    }
    println!(
        "VmRSS built       : {rss_built} KiB ({:.1} MiB)",
        mib(rss_built)
    );
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
    println!(
        "open wall         : {:.3}s  <- activation cost",
        open_wall.as_secs_f64()
    );
    println!(
        "VmRSS after open  : {rss_open} KiB ({:.1} MiB)",
        mib(rss_open)
    );
    println!("node count        : {node_count}");
    println!(
        "point reads       : {POINT_READS} reads in {:.3}ms, {hits} hits",
        point_wall.as_secs_f64() * 1000.0
    );
    println!(
        "adjacency walk    : {:.3}ms, {walked} hops",
        walk_wall.as_secs_f64() * 1000.0
    );
    println!(
        "VmRSS baseline    : {rss_start} KiB ({:.1} MiB)",
        mib(rss_start)
    );

    assert_eq!(hits, POINT_READS, "reopened store lost point reads");
    assert!(walked > 1, "reopened store lost adjacency");
}

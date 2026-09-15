//! Peak-RSS probe for the opt-in tiered graph storage features.
//!
//! Ignored by default: these build six- and seven-figure synthetic generations
//! and are measurement harnesses, not contracts. Nothing here asserts on a
//! memory figure — the numbers are printed for a human to compare across
//! feature states. Run them explicitly:
//!
//! ```text
//! cargo test -p tracedecay-graph-db --features test-helpers \
//!     --test tiered_storage_rss -- --ignored --nocapture --test-threads=1
//!
//! cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
//!     --test tiered_storage_rss -- --ignored --nocapture --test-threads=1
//!
//! cargo test -p tracedecay-graph-db --features test-helpers,graph-tiered-storage \
//!     --test tiered_storage_rss -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` matters: `VmHWM` and `VmRSS` are process-wide, so two
//! probes running concurrently would report each other's allocations.
//!
//! # Reading the numbers
//!
//! Two different figures, and they answer different questions:
//!
//! * `VmRSS` is the process's *live* resident set at the instant it is read.
//!   Sampled with the store open and the generation committed, this is the
//!   figure that matters for a long-lived daemon: how much RAM the graph
//!   actually holds onto. A working disk tier has to move this number.
//! * `VmHWM` is the kernel's high-water mark. It only ever rises, so it
//!   captures transient staging peaks too (the mutation vector, the commit
//!   path, and the serialization that happens on close). It is the right
//!   figure for "how much machine did this need", and the wrong one for "how
//!   much does the store cost at rest".
//!
//! Both are read again after the store closes, because closing serializes the
//! store and can peak well above anything the commit loop reached.
//!
//! Linux-only: `/proc/self/status` does not exist elsewhere, and the probes
//! report that and return rather than failing on other platforms.
//!
//! # Why the small probe does not hit the arena ceiling
//!
//! Under `graph-tiered-storage` each `apply_unverified` commit advances the
//! epoch, and every epoch gets its own arena. `SMALL_BATCH_SIZE` entities at 32
//! bytes per `NodeRecord` fit in one chunk, so `synthetic_generation_peak_rss`
//! never approaches the addressing limit. It is also why the tiered numbers
//! come out *worse* there: 20 batches means 20 arenas holding 20 MiB of largely
//! empty chunks, plus the `VersionIndex` entries layered on the records.
//!
//! `production_page_generation_rss` is the probe that actually stresses the
//! arena, because it commits in pages of
//! [`MAX_NATIVE_GENERATION_STAGE_MUTATIONS`]-many mutations — the real staging
//! page size the native generation runtime uses. At 32 bytes per record that
//! page is 2 MiB, which is exactly what overflowed the published grafeo
//! 0.5.42 arena (a `u32` offset inside a single 1 MiB chunk, ceiling ~32 Ki
//! records) and panicked. The fork this workspace patches in spans an epoch
//! arena across chunks, so the page commits instead of aborting.
//!
//! [`MAX_NATIVE_GENERATION_STAGE_MUTATIONS`]: (crate-private; mirrored below)

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};

mod support;

use support::RegisteredGraph;

/// Total synthetic entities in the small probe.
const ENTITY_COUNT: usize = 100_000;

/// Entities per write batch in the small probe. Deliberately far below the
/// production staging page so the two probes bracket the arena behaviour.
const SMALL_BATCH_SIZE: usize = 5_000;

/// Mirrors the crate-private `limits::MAX_NATIVE_GENERATION_STAGE_MUTATIONS`.
/// The native generation runtime flushes a staging page once it reaches this
/// many mutations, so this is the largest single-epoch commit production ever
/// performs. Kept in sync by hand; it is a measurement input, not a contract.
const PRODUCTION_STAGE_PAGE: usize = 65_536;

/// Total entities in the production-shaped probe. Override with
/// `TRACEDECAY_RSS_ENTITIES` to scale the sweep without a rebuild.
const PRODUCTION_ENTITY_COUNT: usize = 500_000;

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn entity(index: usize) -> GraphEntity {
    let mut labels = BTreeSet::new();
    labels.insert(tracedecay_graph_db::GraphLabel::new("Symbol").unwrap());
    let mut properties = BTreeMap::new();
    properties.insert(
        tracedecay_graph_db::GraphPropertyName::new("name").unwrap(),
        tracedecay_graph_db::GraphProperty::String(format!("symbol-{index}")),
    );
    GraphEntity::new(
        GraphEntityId::new(format!("entity-{index}")).unwrap(),
        labels,
        properties,
    )
    .unwrap()
}

fn batch(watermark: &str, mutations: Vec<GraphMutation>) -> GraphWriteBatch {
    GraphWriteBatch::new(
        GraphNamespace::new("project").unwrap(),
        GraphProjectionId::new("code").unwrap(),
        SourceGeneration::new("generation-1").unwrap(),
        GraphWatermark::new(watermark).unwrap(),
        mutations,
        live(),
    )
    .unwrap()
}

/// Which of the three feature states this binary was compiled in.
fn feature_state() -> &'static str {
    if cfg!(feature = "graph-tiered-storage") {
        "graph-tiered-storage (disk tier + grafeo-core LPG arena)"
    } else if cfg!(feature = "graph-disk-tier") {
        "graph-disk-tier (disk tier only)"
    } else {
        "baseline (no tiered feature)"
    }
}

/// Reads a `/proc/self/status` field in KiB.
fn status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let prefix = format!("{field}:");
    status
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// Peak resident set size since process start, in KiB. Monotonic.
fn peak_rss_kib() -> Option<u64> {
    status_kib("VmHWM")
}

/// Live resident set size at this instant, in KiB.
fn live_rss_kib() -> Option<u64> {
    status_kib("VmRSS")
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

/// One measured run: stage `entity_count` entities in pages of `page_size`,
/// print every figure, and assert nothing.
fn measure(label: &str, entity_count: usize, page_size: usize) {
    let (Some(hwm_before), Some(rss_before)) = (peak_rss_kib(), live_rss_kib()) else {
        println!("/proc/self/status unavailable on this platform; skipping {label}");
        return;
    };

    let temp = TempDir::new().unwrap();
    let root = temp.path().join("store");
    std::fs::create_dir_all(&root).unwrap();
    let (registered, db) = RegisteredGraph::open_lease(&root).unwrap();

    let started = Instant::now();
    for (batch_index, chunk_start) in (0..entity_count).step_by(page_size).enumerate() {
        let chunk_end = (chunk_start + page_size).min(entity_count);
        let mutations = (chunk_start..chunk_end)
            .map(|index| GraphMutation::UpsertEntity(entity(index)))
            .collect();
        db.apply_unverified(batch(&format!("watermark-{batch_index}"), mutations))
            .unwrap();
    }
    let commit_elapsed = started.elapsed();

    // Sampled with the store still open and the generation committed: this is
    // the residency a daemon would carry.
    let hwm_committed = peak_rss_kib().unwrap();
    let rss_committed = live_rss_kib().unwrap();

    let close_started = Instant::now();
    drop(db);
    assert!(registered.close().unwrap());
    let close_elapsed = close_started.elapsed();

    // Closing serializes the store; on every config measured so far this is
    // where the true high-water mark lands, well above the commit loop.
    let hwm_closed = peak_rss_kib().unwrap();
    let rss_closed = live_rss_kib().unwrap();

    let on_disk = std::fs::metadata(root.join("graph.grafeo"))
        .map(|meta| meta.len())
        .unwrap_or(0);

    println!("--- {label} ---");
    println!("feature state    : {}", feature_state());
    println!("entities         : {entity_count} in pages of {page_size}");
    println!("commit wall      : {:.2}s", commit_elapsed.as_secs_f64());
    println!("close wall       : {:.2}s", close_elapsed.as_secs_f64());
    println!(
        "total wall       : {:.2}s",
        (commit_elapsed + close_elapsed).as_secs_f64()
    );
    println!(
        "VmRSS baseline   : {rss_before} KiB ({:.1} MiB)",
        mib(rss_before)
    );
    println!(
        "VmRSS committed  : {rss_committed} KiB ({:.1} MiB)  <- live residency, store open",
        mib(rss_committed)
    );
    println!(
        "VmRSS after close: {rss_closed} KiB ({:.1} MiB)",
        mib(rss_closed)
    );
    println!(
        "VmHWM baseline   : {hwm_before} KiB ({:.1} MiB)",
        mib(hwm_before)
    );
    println!(
        "VmHWM committed  : {hwm_committed} KiB ({:.1} MiB)",
        mib(hwm_committed)
    );
    println!(
        "VmHWM after close: {hwm_closed} KiB ({:.1} MiB)  <- true process peak",
        mib(hwm_closed)
    );
    println!(
        "VmHWM delta      : {} KiB ({:.1} MiB)",
        hwm_closed - hwm_before,
        mib(hwm_closed - hwm_before)
    );
    println!("on-disk store    : {on_disk} bytes ({:.1} MiB)", {
        on_disk as f64 / (1024.0 * 1024.0)
    });
}

#[test]
#[ignore = "RSS measurement harness; run explicitly and compare feature states"]
fn synthetic_generation_peak_rss() {
    measure(
        "tiered storage peak RSS probe (small pages)",
        ENTITY_COUNT,
        SMALL_BATCH_SIZE,
    );
}

/// Commits at the real native staging page size, which is what production
/// actually asks grafeo for and what the published-crate arena could not take.
#[test]
#[ignore = "RSS measurement harness; run explicitly and compare feature states"]
fn production_page_generation_rss() {
    let entity_count = std::env::var("TRACEDECAY_RSS_ENTITIES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(PRODUCTION_ENTITY_COUNT);
    measure(
        "tiered storage peak RSS probe (production staging page)",
        entity_count,
        PRODUCTION_STAGE_PAGE,
    );
}

//! Peak-RSS probe for the opt-in tiered graph storage features.
//!
//! Ignored by default: this builds a six-figure synthetic generation and is a
//! measurement harness, not a contract. Run it explicitly and compare the two
//! feature states:
//!
//! ```text
//! cargo test -p tracedecay-graph-db --features test-helpers \
//!     --test tiered_storage_rss -- --ignored --nocapture
//!
//! cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
//!     --test tiered_storage_rss -- --ignored --nocapture
//! ```
//!
//! `VmHWM` in `/proc/self/status` is the kernel's high-water mark for resident
//! set size. It only ever rises within a process, so the figure printed here is
//! the peak across the whole test binary, not just the build loop — read it as
//! a comparison between two otherwise identical runs, never as an absolute
//! cost attributable to the generation alone.
//!
//! Linux-only: `/proc/self/status` does not exist elsewhere, and the test
//! reports that and returns rather than failing on other platforms.
//!
//! # Why this does not hit the arena ceiling
//!
//! Under `graph-tiered-storage` each `apply_unverified` commit advances the
//! epoch, and every epoch gets its own arena with a fresh 1 MiB primary chunk.
//! `BATCH_SIZE` entities at 32 bytes per `NodeRecord` fit comfortably, so this
//! probe never trips the `AllocError::InsufficientSpace` panic that the
//! generation-runtime tests hit when they stage 65k rows inside a *single*
//! epoch. Do not read a passing run here as evidence that the ceiling is gone.
//!
//! That per-epoch chunk is also why the tiered numbers come out *worse*: 20
//! batches means 20 arenas holding 20 MiB of largely empty chunks, plus the
//! `VersionIndex` entries layered on top of the records they point at.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphEntity, GraphEntityId, GraphMutation, GraphNamespace,
    GraphProjectionId, GraphWatermark, GraphWriteBatch, NeverCancelled, SourceGeneration,
};

mod support;

use support::RegisteredGraph;

/// Total synthetic entities in the generation.
const ENTITY_COUNT: usize = 100_000;

/// Entities per write batch. Keeps the per-batch mutation vector bounded so the
/// figure reflects store residency rather than one enormous staging vector.
const BATCH_SIZE: usize = 5_000;

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

/// Reads `VmHWM` (peak resident set size, in KiB) from `/proc/self/status`.
///
/// Returns `None` on platforms without procfs.
fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

#[test]
#[ignore = "RSS measurement harness; run explicitly and compare feature states"]
fn synthetic_generation_peak_rss() {
    let Some(before) = peak_rss_kib() else {
        println!("VmHWM unavailable on this platform; skipping measurement");
        return;
    };

    let feature_state = if cfg!(feature = "graph-tiered-storage") {
        "graph-tiered-storage (disk tier + grafeo-core LPG arena)"
    } else if cfg!(feature = "graph-disk-tier") {
        "graph-disk-tier (disk tier only)"
    } else {
        "baseline (no tiered feature)"
    };

    let temp = TempDir::new().unwrap();
    let root = temp.path().join("store");
    std::fs::create_dir_all(&root).unwrap();
    let (registered, db) = RegisteredGraph::open_lease(&root).unwrap();

    for (batch_index, chunk_start) in (0..ENTITY_COUNT).step_by(BATCH_SIZE).enumerate() {
        let chunk_end = (chunk_start + BATCH_SIZE).min(ENTITY_COUNT);
        let mutations = (chunk_start..chunk_end)
            .map(|index| GraphMutation::UpsertEntity(entity(index)))
            .collect();
        db.apply_unverified(batch(&format!("watermark-{batch_index}"), mutations))
            .unwrap();
    }

    let after = peak_rss_kib().unwrap();

    drop(db);
    assert!(registered.close().unwrap());

    println!("--- tiered storage peak RSS probe ---");
    println!("feature state : {feature_state}");
    println!("entities      : {ENTITY_COUNT} in batches of {BATCH_SIZE}");
    println!("VmHWM before  : {before} KiB ({:.1} MiB)", mib(before));
    println!("VmHWM after   : {after} KiB ({:.1} MiB)", mib(after));
    println!(
        "VmHWM delta   : {} KiB ({:.1} MiB)",
        after - before,
        mib(after - before)
    );
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

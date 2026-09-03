//! Residency probe for consecutive code-generation cycles on one incarnation
//! (issue #762): cycle `i` stages the full corpus as generation `i` in its
//! own physical namespace, then retires generation `i - 1` by deleting its
//! rows the way `delete_generation_contents` does. Red when a cycle's
//! post-retirement baseline does not return near the first cycle's — the
//! operator daemon's cross-cycle retention shape, where every generation
//! swap stacked another resident corpus until the kernel killed the daemon.
//!
//! Ignored by default. This is a measurement harness, not a contract —
//! nothing asserts on a memory figure.
//!
//! ```text
//! TRACEDECAY_GCR_CYCLES=3 TRACEDECAY_GCR_ROWS=250000 \
//!   cargo test -p tracedecay-graph-db --features test-helpers \
//!   --test generation_cycle_rss -- --ignored --nocapture \
//!   --exact generation_cycle_residency_probe
//! ```
//!
//! Batches are paged so one cycle commits on the order of a hundred
//! transactions, matching an operator-scale generation (~80 pages of 65 536
//! mutations): the engine's version GC runs on a fixed commit cadence, so a
//! probe that reached the same corpus in a handful of oversized commits would
//! never cross that cadence and would misreport the daemon's steady state.
//!
//! One scenario per process: `VmHWM` is process-wide and monotonic.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphCancellation, GraphDbLeaseV1, GraphEntity, GraphEntityId, GraphLabel, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName, GraphRelation,
    GraphRelationId, GraphRelationKind, GraphWatermark, GraphWriteBatch, NeverCancelled,
    SourceGeneration,
};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

mod support;

use support::RegisteredGraph;

const DEFAULT_CYCLES: usize = 3;
const DEFAULT_ROWS: usize = 250_000;
const RELATION_DIVISOR: usize = 4;
/// Entity pages per cycle; with the relation and retirement pages this puts
/// one cycle at roughly the operator-scale commit count (see module docs).
const ENTITY_PAGES: usize = 48;

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn projection() -> GraphProjectionId {
    GraphProjectionId::new("code-generation").unwrap()
}

/// One generation's physical namespace, mirroring the per-generation
/// namespaces production derives from a generation locator.
fn cycle_namespace(cycle: usize) -> GraphNamespace {
    GraphNamespace::new(format!("code-scope-00--gen-{cycle:02}")).unwrap()
}

fn entity_id(index: usize) -> GraphEntityId {
    GraphEntityId::new(format!("entity-{index}")).unwrap()
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
        GraphRelationKind::new("calls").unwrap(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn batch(cycle: usize, watermark: &str, mutations: Vec<GraphMutation>) -> GraphWriteBatch {
    GraphWriteBatch::new(
        cycle_namespace(cycle),
        projection(),
        SourceGeneration::new(format!("generation-{cycle}")).unwrap(),
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

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn sample(label: &str) {
    let rss = status_kib("VmRSS").unwrap_or(0);
    let hwm = status_kib("VmHWM").unwrap_or(0);
    println!(
        "{label:34} VmRSS={rss:>10} KiB ({:>8.1} MiB)  VmHWM={hwm:>10} KiB ({:>8.1} MiB)",
        mib(rss),
        mib(hwm),
    );
}

fn page_size(rows: usize) -> usize {
    rows.div_ceil(ENTITY_PAGES).max(1)
}

fn stage_cycle(db: &GraphDbLeaseV1, cycle: usize, rows: usize) {
    let page_rows = page_size(rows);
    for (page, start) in (0..rows).step_by(page_rows).enumerate() {
        let end = (start + page_rows).min(rows);
        let mutations = (start..end)
            .map(|index| GraphMutation::UpsertEntity(entity(index)))
            .collect();
        db.apply_unverified(batch(cycle, &format!("wm-entity-{page}"), mutations))
            .unwrap();
    }
    let relations = rows / RELATION_DIVISOR;
    for (page, start) in (0..relations).step_by(page_rows).enumerate() {
        let end = (start + page_rows).min(relations);
        let mutations = (start..end)
            .map(|index| GraphMutation::UpsertRelation(relation(index)))
            .collect();
        db.apply_unverified(batch(cycle, &format!("wm-relation-{page}"), mutations))
            .unwrap();
    }
}

/// Deletes a retired cycle's rows the way `delete_generation_contents`
/// does — relations first, then entities — through the public apply path.
fn retire_cycle(db: &GraphDbLeaseV1, cycle: usize, rows: usize) {
    let page_rows = page_size(rows);
    let relations = rows / RELATION_DIVISOR;
    for (page, start) in (0..relations).step_by(page_rows).enumerate() {
        let end = (start + page_rows).min(relations);
        let mutations = (start..end)
            .map(|index| {
                GraphMutation::DeleteRelation(
                    GraphRelationId::new(format!("relation-{index}")).unwrap(),
                )
            })
            .collect();
        db.apply_unverified(batch(
            cycle,
            &format!("wm-retire-relation-{page}"),
            mutations,
        ))
        .unwrap();
    }
    for (page, start) in (0..rows).step_by(page_rows).enumerate() {
        let end = (start + page_rows).min(rows);
        let mutations = (start..end)
            .map(|index| GraphMutation::DeleteEntity(entity_id(index)))
            .collect();
        db.apply_unverified(batch(cycle, &format!("wm-retire-entity-{page}"), mutations))
            .unwrap();
    }
}

#[test]
#[ignore = "residency measurement harness; run one scenario per process, see module docs"]
fn generation_cycle_residency_probe() {
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("generation-cycle-residency").build();

    if status_kib("VmRSS").is_none() {
        println!("/proc/self/status unavailable on this platform; skipping");
        return;
    }

    let cycles: usize = std::env::var("TRACEDECAY_GCR_CYCLES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CYCLES);
    let rows: usize = std::env::var("TRACEDECAY_GCR_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    println!(
        "generation-cycle residency probe: cycles={cycles} rows={rows} (+{} relations each)",
        rows / RELATION_DIVISOR
    );

    let root = TempDir::new().unwrap();
    sample("baseline");

    let (registered, db) = RegisteredGraph::open_lease(root.path()).unwrap();
    sample("open (fresh store)");

    let mut baselines: Vec<u64> = Vec::new();
    for cycle in 0..cycles {
        let started = Instant::now();
        stage_cycle(&db, cycle, rows);
        sample(&format!(
            "cycle {cycle}: staged ({:.1?})",
            started.elapsed()
        ));

        if cycle > 0 {
            let started = Instant::now();
            retire_cycle(&db, cycle - 1, rows);
            sample(&format!(
                "cycle {cycle}: retired gen {} ({:.1?})",
                cycle - 1,
                started.elapsed()
            ));
        }
        let baseline = status_kib("VmRSS").unwrap_or(0);
        baselines.push(baseline);
        println!(
            "cycle {cycle}: post-cycle baseline {baseline} KiB ({:.1} MiB)",
            mib(baseline)
        );
    }

    drop(db);
    registered.close().unwrap();
    sample("closed");

    if baselines.len() >= 2 {
        let first = baselines[0];
        let last = *baselines.last().unwrap();
        let per_cycle = (last as i64 - first as i64) / (baselines.len() as i64 - 1);
        println!(
            "cross-cycle retention: first baseline {:.1} MiB, last {:.1} MiB, {:+.1} MiB per retired generation",
            mib(first),
            mib(last),
            per_cycle as f64 / 1024.0
        );
    }
}

//! Residency probe for the quarantine-then-replay journey (issue #762 class):
//! one staging database accumulating whole-corpus generations for several
//! code scopes, then a store reset followed by a full replay of every scope.
//!
//! Ignored by default. This is a measurement harness, not a contract —
//! nothing asserts on a memory figure. It answers two questions:
//!
//! > 1. As scope generations are staged one after another into the shared
//! >    staging database, does the process working set grow by the full
//! >    corpus per scope and never come back down within the incarnation?
//! > 2. After a quarantine (store reset), what does the replay of all scopes
//! >    cost, and does a reopen of the accumulated container pay the whole
//! >    accumulated residency again?
//!
//! ```text
//! TRACEDECAY_QRR_SCOPES=6 TRACEDECAY_QRR_ROWS=250000 \
//!   cargo test -p tracedecay-graph-db --features test-helpers \
//!   --test quarantine_replay_rss -- --ignored --nocapture \
//!   --exact quarantine_replay_residency_probe
//! ```
//!
//! `TRACEDECAY_QRR_RELEASE=1` additionally releases the staging apply-state
//! after each scope — the engine MVCC GC plus allocator trim a completed
//! publication performs through `maybe_release_apply_state`; comparing the
//! two runs prices exactly what the release buys at each scope boundary.
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

mod support;

use support::RegisteredGraph;

/// Mirrors the crate-private `limits::MAX_NATIVE_GENERATION_STAGE_MUTATIONS`.
const PRODUCTION_STAGE_PAGE: usize = 65_536;

const DEFAULT_SCOPES: usize = 6;
const DEFAULT_ROWS: usize = 250_000;
const RELATION_DIVISOR: usize = 4;

fn live() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn namespace(scope: usize) -> GraphNamespace {
    GraphNamespace::new(format!("code-scope-{scope:02}")).unwrap()
}

fn projection() -> GraphProjectionId {
    GraphProjectionId::new("code-generation").unwrap()
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

fn batch(scope: usize, watermark: &str, mutations: Vec<GraphMutation>) -> GraphWriteBatch {
    GraphWriteBatch::new(
        namespace(scope),
        projection(),
        SourceGeneration::new(format!("generation-{scope}")).unwrap(),
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
        "{label:32} VmRSS={rss:>10} KiB ({:>8.1} MiB)  VmHWM={hwm:>10} KiB ({:>8.1} MiB)",
        mib(rss),
        mib(hwm),
    );
}

fn stage_scope(db: &GraphDbLeaseV1, scope: usize, rows: usize) {
    for (page, start) in (0..rows).step_by(PRODUCTION_STAGE_PAGE).enumerate() {
        let end = (start + PRODUCTION_STAGE_PAGE).min(rows);
        let mutations = (start..end)
            .map(|index| GraphMutation::UpsertEntity(entity(index)))
            .collect();
        db.apply_unverified(batch(scope, &format!("wm-entity-{page}"), mutations))
            .unwrap();
    }
    let relations = rows / RELATION_DIVISOR;
    for (page, start) in (0..relations).step_by(PRODUCTION_STAGE_PAGE).enumerate() {
        let end = (start + PRODUCTION_STAGE_PAGE).min(relations);
        let mutations = (start..end)
            .map(|index| GraphMutation::UpsertRelation(relation(index)))
            .collect();
        db.apply_unverified(batch(scope, &format!("wm-relation-{page}"), mutations))
            .unwrap();
    }
}

fn store_disk_kib(root: &std::path::Path) -> u64 {
    fn walk(path: &std::path::Path) -> u64 {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return 0;
        };
        if metadata.is_file() {
            return metadata.len();
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return 0;
        };
        entries.flatten().map(|entry| walk(&entry.path())).sum()
    }
    walk(root) / 1024
}

#[test]
#[ignore = "residency measurement harness; run one scenario per process, see module docs"]
fn quarantine_replay_residency_probe() {
    if status_kib("VmRSS").is_none() {
        println!("/proc/self/status unavailable on this platform; skipping");
        return;
    }

    let scopes: usize = std::env::var("TRACEDECAY_QRR_SCOPES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_SCOPES);
    let rows: usize = std::env::var("TRACEDECAY_QRR_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    let release = std::env::var("TRACEDECAY_QRR_RELEASE").as_deref() == Ok("1");
    println!(
        "quarantine-replay residency probe: scopes={scopes} rows={rows} (+{} relations each) release={release}",
        rows / RELATION_DIVISOR
    );

    let root = TempDir::new().unwrap();
    sample("baseline");

    // ---- incarnation 1: scopes staged one after another ----
    let (registered, db) = RegisteredGraph::open_lease(root.path()).unwrap();
    sample("open (fresh store)");
    for scope in 0..scopes {
        let started = Instant::now();
        stage_scope(&db, scope, rows);
        let wall = started.elapsed();
        sample(&format!("staged scope {scope} ({wall:.1?})"));
        if release {
            let started = Instant::now();
            let released = db.release_apply_state(&|| Ok(())).unwrap();
            assert!(released, "a live staging store must release");
            sample(&format!("released scope {scope} ({:.1?})", started.elapsed()));
        }
    }
    drop(db);
    let closed = registered.close().unwrap();
    assert!(closed, "close must retire the only lease");
    sample("closed (all scopes staged)");
    println!(
        "store on disk after close: {} KiB ({:.1} MiB)",
        store_disk_kib(root.path()),
        mib(store_disk_kib(root.path()))
    );

    // ---- incarnation 2: reopen of the accumulated container ----
    let started = Instant::now();
    let db = registered.reopen_lease().unwrap();
    println!("reopen wall: {:.1?}", started.elapsed());
    sample("reopened (accumulated)");
    drop(db);
    registered.close().unwrap();
    sample("closed again");

    // ---- quarantine: reset the store, then replay every scope ----
    drop(registered);
    std::fs::remove_dir_all(root.path()).unwrap();
    std::fs::create_dir_all(root.path()).unwrap();
    sample("store quarantined (reset)");

    let (registered, db) = RegisteredGraph::open_lease(root.path()).unwrap();
    sample("open (post-quarantine)");
    for scope in 0..scopes {
        let started = Instant::now();
        stage_scope(&db, scope, rows);
        let wall = started.elapsed();
        sample(&format!("replayed scope {scope} ({wall:.1?})"));
        if release {
            let started = Instant::now();
            let released = db.release_apply_state(&|| Ok(())).unwrap();
            assert!(released, "a live staging store must release");
            sample(&format!(
                "released replay {scope} ({:.1?})",
                started.elapsed()
            ));
        }
    }
    drop(db);
    registered.close().unwrap();
    sample("closed (replay complete)");
}

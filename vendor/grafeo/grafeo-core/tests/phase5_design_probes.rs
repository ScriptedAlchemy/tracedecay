//! Phase 5 design probes: measure LpgStore-as-overlay memory cost vs the
//! lean `WalOverlay` struct, and verify `LayeredStore::swap_base` works
//! sequentially. Run with `--ignored --nocapture` to see numbers.
//!
//! These are one-shot measurements informing the Phase 5 architectural
//! choice (LayeredStore-extension path vs WalOverlay-wiring path). Not
//! part of the regression suite.

#![cfg(all(feature = "compact-store", feature = "lpg"))]

use std::sync::Arc;

use grafeo_common::types::{NodeId, Value};
use grafeo_core::graph::compact::builder::CompactStoreBuilder;
use grafeo_core::graph::compact::layered::LayeredStore;
use grafeo_core::graph::lpg::LpgStore;
use grafeo_core::graph::lpg::overlay::WalOverlay;
use grafeo_core::graph::traits::{GraphStore, GraphStoreMut};

/// Probe 1: per-mutation memory cost.
///
/// Inserts N nodes with one Int64 property each into:
///   (a) a fresh LpgStore (the heavy overlay candidate)
///   (b) a fresh WalOverlay (the lean overlay candidate)
/// Reports total bytes and bytes-per-mutation for each.
#[test]
#[ignore = "design probe; run with --ignored --nocapture"]
fn probe_memory_cost_lpg_vs_walov() {
    const N: usize = 10_000;

    // ── (a) LpgStore overlay ─────────────────────────────────────────
    let lpg = LpgStore::new().expect("alloc");
    for i in 0..N {
        let id = lpg.create_node(&["Person"]);
        let age = i64::try_from(i).expect("probe N fits i64");
        lpg.set_node_property(id, "age", Value::Int64(age));
    }
    let (store_mem, idx_mem, mvcc_mem, pool_mem) = lpg.memory_breakdown();
    let lpg_total =
        store_mem.total_bytes + idx_mem.total_bytes + mvcc_mem.total_bytes + pool_mem.total_bytes;

    // ── (b) WalOverlay (lean) ────────────────────────────────────────
    let wal = WalOverlay::new();
    for i in 0..N {
        let id = NodeId::new(i as u64);
        wal.insert_node(id, vec!["Person".to_string()]);
        let age = i64::try_from(i).expect("probe N fits i64");
        wal.set_node_property(id, "age".to_string(), Value::Int64(age));
    }
    let wal_total = wal.approximate_memory_bytes();

    eprintln!("\n=== PROBE 1: per-mutation memory cost ({N} nodes + 1 property each) ===");
    eprintln!(
        "LpgStore overlay:    {:>9} bytes total, {:>5} B/node",
        lpg_total,
        lpg_total / N
    );
    eprintln!(
        "  store      : {:>9} B  ({} B/node)",
        store_mem.total_bytes,
        store_mem.total_bytes / N
    );
    eprintln!(
        "  indexes    : {:>9} B  ({} B/node)",
        idx_mem.total_bytes,
        idx_mem.total_bytes / N
    );
    eprintln!(
        "  mvcc       : {:>9} B  ({} B/node)",
        mvcc_mem.total_bytes,
        mvcc_mem.total_bytes / N
    );
    eprintln!(
        "  string_pool: {:>9} B  ({} B/node)",
        pool_mem.total_bytes,
        pool_mem.total_bytes / N
    );
    eprintln!(
        "WalOverlay (lean):   {:>9} bytes total, {:>5} B/node",
        wal_total,
        wal_total / N
    );
    eprintln!(
        "Ratio LpgStore/WalOverlay: {:.1}x",
        lpg_total as f64 / wal_total.max(1) as f64
    );
}

/// Probe 3: LayeredStore::swap_base sequential correctness.
///
/// Builds a layered store with a CompactStore base + LpgStore overlay,
/// records mutations in the overlay, then swaps the base for a different
/// CompactStore and verifies (a) overlay reads still work and (b) base
/// reads now reflect the new base.
#[test]
#[ignore = "design probe; run with --ignored --nocapture"]
fn probe_swap_base_preserves_overlay_reads() {
    // Base 1: Person nodes with ages 25, 30, 35.
    let base1 = CompactStoreBuilder::new()
        .node_table("Person", |t| t.column_bitpacked("age", &[25, 30, 35], 6))
        .build()
        .unwrap();
    let max_node = 3;
    let max_edge = 0;
    let layered = LayeredStore::new(base1, max_node, max_edge).unwrap();

    // Overlay mutation: insert one new node (id 100, label "Animal").
    let new_node_id = layered.create_node(&["Animal"]);
    eprintln!("Created overlay node id = {new_node_id:?}");

    // Sanity: pre-swap, overlay node is visible.
    let pre_swap = layered.get_node(new_node_id);
    assert!(pre_swap.is_some(), "overlay node visible pre-swap");

    // Base 2: completely different content (Cities table).
    let base2 = CompactStoreBuilder::new()
        .node_table("City", |t| t.column_dict("name", &["Amsterdam", "Berlin"]))
        .build()
        .unwrap();

    let _old_base = layered.swap_base(Arc::new(base2));
    eprintln!("swap_base completed");

    // Post-swap assertions:
    // (1) Overlay node still visible (overlay survived swap).
    let post_swap_overlay = layered.get_node(new_node_id);
    eprintln!(
        "post-swap overlay node visible: {}",
        post_swap_overlay.is_some()
    );
    assert!(
        post_swap_overlay.is_some(),
        "overlay node MUST remain visible after base swap"
    );

    // (2) Base reads now reflect base2 (City label exists).
    let cities = layered.nodes_by_label("City");
    eprintln!("post-swap nodes_by_label(City) = {} entries", cities.len());
    assert_eq!(cities.len(), 2, "base2 contributes 2 City nodes");

    // (3) Old base content (Person ages 25/30/35) now invisible from base.
    let people = layered.nodes_by_label("Person");
    // The overlay's "Animal" node is not under "Person"; the old base's
    // Person nodes are gone. Should be 0.
    eprintln!(
        "post-swap nodes_by_label(Person) = {} entries",
        people.len()
    );
    assert_eq!(
        people.len(),
        0,
        "old base's Person nodes must be invisible after swap"
    );

    eprintln!("\n=== PROBE 3: swap_base correctness — PASS ===");
}

/// Probe 1 supplement: zero-mutation overhead.
///
/// What's the baseline cost of a fresh LpgStore vs WalOverlay? This
/// matters because the OnDisk overlay starts empty after every checkpoint.
#[test]
#[ignore = "design probe; run with --ignored --nocapture"]
fn probe_baseline_overhead() {
    let lpg = LpgStore::new().expect("alloc");
    let (s, i, m, p) = lpg.memory_breakdown();
    let lpg_total = s.total_bytes + i.total_bytes + m.total_bytes + p.total_bytes;

    let wal = WalOverlay::new();
    let wal_total = wal.approximate_memory_bytes();

    eprintln!("\n=== PROBE 1b: baseline overhead (zero mutations) ===");
    eprintln!("LpgStore (empty):  {lpg_total} bytes");
    eprintln!("WalOverlay (empty): {wal_total} bytes");
    eprintln!(
        "Difference: {} bytes ({:.1}x)",
        lpg_total.saturating_sub(wal_total),
        lpg_total as f64 / wal_total.max(1) as f64
    );
}

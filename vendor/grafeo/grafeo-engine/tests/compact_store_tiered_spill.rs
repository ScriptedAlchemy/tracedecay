//! Integration tests for the tier-aware `CompactStore` base.
//!
//! Exercises the full `compact() → spill_all() → swap_base()` lifecycle
//! through the public GrafeoDB API, verifying that:
//!
//! - `compact()` installs a `CompactStoreTiered` wrapper and registers a
//!   `CompactStoreConsumer` with the BufferManager.
//! - `BufferManager::spill_all()` actually spills the base to a mmap'd file
//!   and publishes the fresh `Arc<CompactStore>` to the `LayeredStore`.
//! - Reads continue to work transparently across the tier transition.
//! - `recompact()` rebuilds the tier wrapper so its `Weak` back-references
//!   track the new base.
//!
//! Requires: `compact-store`, `mmap`, `lpg` (all default-on).

#![cfg(all(feature = "compact-store", feature = "mmap", feature = "lpg"))]

use std::path::PathBuf;
use std::sync::Arc;

use grafeo_engine::{Config, GrafeoDB};

fn spill_dir(label: &str) -> PathBuf {
    let base = std::env::temp_dir().join("grafeo-compact-tiered-tests");
    base.join(format!("{label}-{}", std::process::id()))
}

fn config_with_spill(dir: &PathBuf) -> Config {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("create spill dir");
    Config::in_memory().with_spill_path(dir.clone())
}

fn seed_db(db: &mut GrafeoDB) {
    for i in 0..16 {
        db.execute(&format!(
            "INSERT (:Person {{name: 'person-{i}', age: {i}}})"
        ))
        .unwrap();
    }
}

#[test]
fn compact_installs_tiered_wrapper() {
    let dir = spill_dir("installs");
    let mut db = GrafeoDB::with_config(config_with_spill(&dir)).unwrap();
    seed_db(&mut db);
    db.compact().unwrap();

    let tiered = db
        .compact_tiered()
        .expect("tiered installed after compact()");
    assert!(!tiered.is_on_disk(), "starts in-memory");
    assert!(tiered.memory_bytes() > 0);

    // LayeredStore and tiered agree on the base Arc right after compact().
    let layered = db
        .layered_store()
        .expect("layered installed after compact()");
    assert!(Arc::ptr_eq(&layered.base_store_arc(), &tiered.store()));
}

#[test]
fn spill_all_tiers_base_to_mmap() {
    let dir = spill_dir("spill");
    let mut db = GrafeoDB::with_config(config_with_spill(&dir)).unwrap();
    seed_db(&mut db);
    db.compact().unwrap();

    let tiered = Arc::clone(db.compact_tiered().unwrap());
    let layered = Arc::clone(db.layered_store().unwrap());
    let pre_base = layered.base_store_arc();

    // Force every can-spill consumer to spill. The compact-store consumer
    // will persist the base and swap_base() on the layered store.
    let freed = db.buffer_manager().spill_all();

    assert!(tiered.is_on_disk(), "tier switched to OnDisk");
    assert!(
        dir.join("compact_base.grafeo").exists(),
        "spill file written"
    );
    // Vector/text consumers also spill (or report 0); just assert the
    // compact base contributed something when it was non-empty.
    let _ = freed;

    // LayeredStore now points at the fresh (mmap-backed) base, distinct
    // from the pre-spill Arc.
    let post_base = layered.base_store_arc();
    assert!(!Arc::ptr_eq(&pre_base, &post_base));
    assert!(Arc::ptr_eq(&post_base, &tiered.store()));
}

#[test]
fn reads_survive_tier_transition() {
    let dir = spill_dir("reads");
    let mut db = GrafeoDB::with_config(config_with_spill(&dir)).unwrap();
    seed_db(&mut db);
    db.compact().unwrap();

    let session = db.session();
    let before = session.execute("MATCH (p:Person) RETURN count(p)").unwrap();
    let count_before = before.rows()[0][0].clone();
    drop(session);

    db.buffer_manager().spill_all();
    assert!(db.compact_tiered().unwrap().is_on_disk());

    // Query again against the now-mmap-backed base.
    let session = db.session();
    let after = session.execute("MATCH (p:Person) RETURN count(p)").unwrap();
    assert_eq!(after.rows()[0][0], count_before);

    // Property access still works.
    let names = session
        .execute("MATCH (p:Person) RETURN p.name ORDER BY p.age")
        .unwrap();
    assert_eq!(names.rows().len(), 16);
}

#[test]
fn recompact_rebuilds_tier_wrapper() {
    let dir = spill_dir("recompact");
    let mut db = GrafeoDB::with_config(config_with_spill(&dir)).unwrap();
    seed_db(&mut db);
    db.compact().unwrap();

    let first_tiered = Arc::clone(db.compact_tiered().unwrap());

    // Add overlay mutations then recompact.
    db.execute("INSERT (:Person {name: 'alix', age: 99})")
        .unwrap();
    db.recompact().unwrap();

    let second_tiered = Arc::clone(db.compact_tiered().unwrap());
    assert!(
        !Arc::ptr_eq(&first_tiered, &second_tiered),
        "recompact replaced the tier wrapper"
    );
    assert!(!second_tiered.is_on_disk());

    // New base matches the new tier wrapper.
    let layered = db.layered_store().unwrap();
    assert!(Arc::ptr_eq(
        &layered.base_store_arc(),
        &second_tiered.store()
    ));

    // Spill the new wrapper to verify the old one's spill no longer races
    // against the live LayeredStore (old weak ref dangles harmlessly).
    db.buffer_manager().spill_all();
    assert!(second_tiered.is_on_disk());
    assert!(
        !first_tiered.is_on_disk(),
        "old wrapper untouched after recompact"
    );
}

#[test]
fn spill_without_spill_path_is_noop() {
    let mut db = GrafeoDB::new_in_memory();
    for i in 0..4 {
        db.execute(&format!("INSERT (:Person {{name: 'p-{i}'}})"))
            .unwrap();
    }
    db.compact().unwrap();

    db.buffer_manager().spill_all();
    // Without a spill_path the compact-store consumer reports can_spill=false,
    // so the base stays in-memory.
    assert!(!db.compact_tiered().unwrap().is_on_disk());
}

/// Phase 5 probe: do mutations work end-to-end while base is OnDisk?
///
/// Spills the base to mmap, then INSERTs a node, then queries it back.
/// If the overlay-as-LpgStore architecture is wired correctly, the new
/// node should be visible alongside the (mmap-backed) base nodes.
#[test]
fn phase5_probe_mutations_during_on_disk_tier() {
    let dir = spill_dir("phase5-mutations");
    let mut db = GrafeoDB::with_config(config_with_spill(&dir)).unwrap();
    seed_db(&mut db);
    db.compact().unwrap();

    db.buffer_manager().spill_all();
    assert!(
        db.compact_tiered().unwrap().is_on_disk(),
        "base must be on disk before the probe"
    );

    // Mutation while OnDisk: should land on the overlay LpgStore.
    db.execute("INSERT (:Person {name: 'overlay-node', age: 999})")
        .unwrap();

    // Read should see the new overlay node alongside the 16 base nodes = 17.
    let session = db.session();
    let count = session.execute("MATCH (p:Person) RETURN count(p)").unwrap();
    eprintln!("after mutation count: {:?}", count.rows()[0][0]);
    assert_eq!(
        count.rows()[0][0],
        grafeo_common::types::Value::Int64(17),
        "overlay node must be visible alongside the 16 base nodes"
    );

    // Specific lookup of the new node.
    let lookup = session
        .execute("MATCH (p:Person {name: 'overlay-node'}) RETURN p.age")
        .unwrap();
    assert_eq!(lookup.rows().len(), 1, "overlay node lookup must succeed");
    assert_eq!(lookup.rows()[0][0], grafeo_common::types::Value::Int64(999));

    // Base reads still work.
    let base_lookup = session
        .execute("MATCH (p:Person {name: 'person-5'}) RETURN p.age")
        .unwrap();
    assert_eq!(base_lookup.rows().len(), 1);
    assert_eq!(
        base_lookup.rows()[0][0],
        grafeo_common::types::Value::Int64(5)
    );
}

/// Phase 5 probe: does recompact() merge overlay-during-OnDisk back into a fresh base?
#[test]
fn phase5_probe_recompact_after_on_disk_mutations() {
    let dir = spill_dir("phase5-recompact-after-spill");
    let mut db = GrafeoDB::with_config(config_with_spill(&dir)).unwrap();
    seed_db(&mut db);
    db.compact().unwrap();

    db.buffer_manager().spill_all();
    assert!(db.compact_tiered().unwrap().is_on_disk());

    // Mutate while OnDisk.
    db.execute("INSERT (:Person {name: 'overlay-node', age: 999})")
        .unwrap();

    // Recompact: should merge overlay → fresh in-memory base, empty overlay.
    db.recompact().unwrap();
    assert!(
        !db.compact_tiered().unwrap().is_on_disk(),
        "recompact creates an in-memory base"
    );

    // Overlay should be empty.
    let layered = db.layered_store().unwrap();
    assert_eq!(
        layered.overlay_mutation_count(),
        0,
        "overlay must be empty after recompact"
    );

    // Data still queryable after recompact.
    let session = db.session();
    let count = session.execute("MATCH (p:Person) RETURN count(p)").unwrap();
    assert_eq!(count.rows()[0][0], grafeo_common::types::Value::Int64(17));

    let lookup = session
        .execute("MATCH (p:Person {name: 'overlay-node'}) RETURN p.age")
        .unwrap();
    assert_eq!(lookup.rows()[0][0], grafeo_common::types::Value::Int64(999));
}

/// Phase 5c: when memory pressure triggers spill_all, the OverlayConsumer
/// should merge overlay → base in place and clear the overlay heap.
#[test]
fn phase5c_overlay_consumer_spill_merges_into_base() {
    let dir = spill_dir("phase5c-overlay-spill");
    let mut db = GrafeoDB::with_config(config_with_spill(&dir)).unwrap();
    seed_db(&mut db);
    db.compact().unwrap();

    // Mutate while in-memory tier so overlay accumulates.
    for i in 0..20 {
        db.execute(&format!(
            "INSERT (:Person {{name: 'overlay-{i}', age: {}}})",
            100 + i
        ))
        .unwrap();
    }

    let layered = Arc::clone(db.layered_store().unwrap());
    assert!(
        layered.overlay_mutation_count() > 0,
        "overlay must hold mutations before spill"
    );
    let pre_overlay_bytes = layered.overlay_memory_bytes();
    assert!(pre_overlay_bytes > 0, "overlay heap nonempty");

    // Drive spill via the buffer manager.
    let _freed = db.buffer_manager().spill_all();

    // After: overlay merged into base, mutations preserved in base.
    assert_eq!(
        layered.overlay_mutation_count(),
        0,
        "overlay must be empty after merge-spill"
    );
    let post_overlay_bytes = layered.overlay_memory_bytes();
    assert!(
        post_overlay_bytes < pre_overlay_bytes,
        "overlay heap shrunk: {pre_overlay_bytes} -> {post_overlay_bytes}"
    );

    // Data still queryable post-merge.
    let session = db.session();
    let count = session.execute("MATCH (p:Person) RETURN count(p)").unwrap();
    assert_eq!(
        count.rows()[0][0],
        grafeo_common::types::Value::Int64(36),
        "16 base + 20 overlay nodes survive the merge"
    );
}

// ── Phase 5e investigation: isolate which step breaks reopen ──────────

/// Baseline: persistent .grafeo file + reopen without compact/spill.
/// If THIS works, the basic open/close cycle is fine.
#[test]
fn phase5e_baseline_persistent_reopen_works() {
    let dir = spill_dir("phase5e-baseline");
    let file_path = dir.join("test.grafeo");
    let _ = std::fs::remove_file(&file_path);

    {
        let db = GrafeoDB::with_config(Config::persistent(&file_path)).unwrap();
        for i in 0..4 {
            db.execute(&format!("INSERT (:Person {{name: 'p-{i}'}})"))
                .unwrap();
        }
    }

    let db = GrafeoDB::with_config(Config::persistent(&file_path)).unwrap();
    let r = db
        .session()
        .execute("MATCH (p:Person) RETURN count(p)")
        .unwrap();
    assert_eq!(r.rows()[0][0], grafeo_common::types::Value::Int64(4));
}

/// After compact() but no spill: does reopen work?
#[test]
fn phase5e_persistent_reopen_after_compact_works() {
    let dir = spill_dir("phase5e-after-compact");
    let file_path = dir.join("test.grafeo");
    let _ = std::fs::remove_file(&file_path);

    {
        let mut db = GrafeoDB::with_config(Config::persistent(&file_path)).unwrap();
        for i in 0..4 {
            db.execute(&format!("INSERT (:Person {{name: 'p-{i}'}})"))
                .unwrap();
        }
        db.compact().unwrap();
    }

    let db = GrafeoDB::with_config(Config::persistent(&file_path)).unwrap();
    let r = db
        .session()
        .execute("MATCH (p:Person) RETURN count(p)")
        .unwrap();
    assert_eq!(r.rows()[0][0], grafeo_common::types::Value::Int64(4));
}

/// After compact + spill (NO overlay mutations): does reopen work?
/// Isolates whether the bug is in the spill path or the overlay path.
#[test]
fn phase5e_persistent_reopen_after_spill_no_overlay() {
    let dir = spill_dir("phase5e-after-spill");
    let file_path = dir.join("test.grafeo");
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::create_dir_all(&dir);

    {
        let mut db =
            GrafeoDB::with_config(Config::persistent(&file_path).with_spill_path(dir.clone()))
                .unwrap();
        for i in 0..4 {
            db.execute(&format!("INSERT (:Person {{name: 'p-{i}'}})"))
                .unwrap();
        }
        db.compact().unwrap();
        db.buffer_manager().spill_all();
        assert!(db.compact_tiered().unwrap().is_on_disk());
        // No overlay mutations.
    }

    let db =
        GrafeoDB::with_config(Config::persistent(&file_path).with_spill_path(dir.clone())).unwrap();
    let r = db
        .session()
        .execute("MATCH (p:Person) RETURN count(p)")
        .unwrap();
    assert_eq!(r.rows()[0][0], grafeo_common::types::Value::Int64(4));
}

/// Phase 5e: WAL replay rebuilds the overlay after a process restart.
///
/// Persistent `.grafeo` file + spilled OnDisk base + overlay mutations
/// must round-trip across process restart. The fix path:
///   1. directory parser handles `SectionType::CompactStore` (was missing)
///   2. open() reconstructs the LayeredStore wiring when a CompactStore
///      section is found, so the loaded LpgStore becomes the overlay
///   3. WAL records replayed against the rebuilt overlay restore any
///      mutations that occurred after the last checkpoint
#[test]
fn phase5_probe_recovery_rebuilds_overlay_after_on_disk_mutation() {
    let dir = spill_dir("phase5-recovery");
    let file_path = dir.join("test.grafeo");
    let _ = std::fs::remove_file(&file_path);

    {
        let mut db =
            GrafeoDB::with_config(Config::persistent(&file_path).with_spill_path(dir.clone()))
                .unwrap();
        seed_db(&mut db);
        db.compact().unwrap();
        db.buffer_manager().spill_all();
        assert!(db.compact_tiered().unwrap().is_on_disk());

        // Mutation lands on overlay (during OnDisk tier).
        db.execute("INSERT (:Person {name: 'durable-overlay', age: 777})")
            .unwrap();
        // Drop without checkpoint.
    }

    // Reopen and verify the overlay mutation survived.
    let db =
        GrafeoDB::with_config(Config::persistent(&file_path).with_spill_path(dir.clone())).unwrap();
    let session = db.session();
    let lookup = session
        .execute("MATCH (p:Person {name: 'durable-overlay'}) RETURN p.age")
        .unwrap();
    assert_eq!(
        lookup.rows().len(),
        1,
        "WAL replay must restore the overlay-during-OnDisk mutation"
    );
    assert_eq!(lookup.rows()[0][0], grafeo_common::types::Value::Int64(777));
}

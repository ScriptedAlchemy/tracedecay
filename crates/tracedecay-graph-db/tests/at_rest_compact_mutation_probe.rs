//! Whether a compacted store survives being written to afterwards.
//!
//! Ignored by default, and pure grafeo: no TraceDecay schema, no registry, no
//! generation runtime. It exists because "seal implies compact" — freezing a
//! sealed generation into a columnar base at the end of
//! `finalize_staged_generation` — cannot ship until this answers cleanly.
//!
//! # What it found
//!
//! Deleting a node that was created in the *overlay* after `compact()` also
//! removes an unrelated node from the *columnar base*, and the loss is
//! persisted. Observed at grafeo rev `22b8614`:
//!
//! ```text
//! life1 post-compact:  node_count=3  Alpha=1 Beta=1 Gamma=1
//! life2 after create:  node_count=4  Alpha=1 Beta=1 Gamma=1 Delta=1
//! life2 after delete:  node_count=3  Alpha=0 Beta=1 Gamma=1 Delta=0   <-- Alpha
//! life3 open:          node_count=2  Alpha=0 Beta=1 Gamma=1
//! ```
//!
//! `Alpha` is never touched. It lives in the compacted base, and deleting
//! `Delta` from the overlay takes it with it: `LayeredStore` records the
//! deletion by `NodeId` in the set that masks base rows, and the overlay's
//! freshly allocated id collides with a base id. The next open is down a node
//! for good.
//!
//! This is not reachable today — nothing in the crate calls `compact()` outside
//! the bench lane — but it is exactly what seal-time compaction would do: the
//! generation lifecycle deletes rows after a seal (whole-generation retirement,
//! `generation_runtime.rs`), and quarantine markers are created and cleared on
//! the recovery path.
//!
//! # Running it
//!
//! ```text
//! cargo test -p tracedecay-graph-db --features test-helpers,graph-disk-tier \
//!   --test at_rest_compact_mutation_probe -- --ignored --nocapture
//! ```
//!
//! It asserts the behaviour that *should* hold, so it fails while the defect is
//! present. Re-run it after a grafeo bump: when it passes, this blocker is
//! cleared and the seal-time compaction in `docs/graph-at-rest/README.md` phase
//! 2 becomes implementable.

#![cfg(feature = "graph-disk-tier")]

use grafeo_common::types::Value;
use grafeo_core::graph::GraphStore;
use grafeo_engine::GrafeoDB;
use tempfile::TempDir;

/// The three labels seeded before compaction, one node each.
const BASE_LABELS: [&str; 3] = ["Alpha", "Beta", "Gamma"];

/// The label created and deleted *after* compaction, in the overlay.
const OVERLAY_LABEL: &str = "Delta";

fn open(path: &std::path::Path) -> GrafeoDB {
    GrafeoDB::open(path).unwrap()
}

/// Prints the store's node census and returns the per-label counts.
fn census(store: &dyn GraphStore, tag: &str) -> Vec<(String, usize)> {
    let mut per_label: Vec<(String, usize)> = store
        .all_labels()
        .into_iter()
        .map(|label| {
            let count = store.nodes_by_label(&label).len();
            (label, count)
        })
        .collect();
    per_label.sort();
    println!(
        "{tag:<26} node_count={} per_label={per_label:?}",
        store.node_count()
    );
    per_label
}

fn count_of(census: &[(String, usize)], label: &str) -> usize {
    census
        .iter()
        .find(|(name, _)| name == label)
        .map_or(0, |(_, count)| *count)
}

#[test]
#[ignore = "at-rest blocker probe; documents a grafeo defect, see module docs"]
fn compacted_base_survives_an_overlay_delete() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");

    // Life 1: seed one node per label, freeze into a columnar base, close.
    {
        let mut database = open(&path);
        for label in BASE_LABELS {
            database
                .session()
                .create_node_with_props(&[label], [("k", Value::from(label))])
                .unwrap();
        }
        census(database.graph_store().as_ref(), "life1 pre-compact");
        database.compact().unwrap();
        census(database.graph_store().as_ref(), "life1 post-compact");
        database.close().unwrap();
    }

    // Life 2: reopen onto the base, create one overlay node, delete it, close.
    // This is the whole intervention. Nothing touches the base rows.
    let after_delete = {
        let database = open(&path);
        census(database.graph_store().as_ref(), "life2 open");
        let transient = database
            .session()
            .create_node_with_props(&[OVERLAY_LABEL], [("k", Value::from(OVERLAY_LABEL))])
            .unwrap();
        census(database.graph_store().as_ref(), "life2 after create");
        database
            .execute(&format!(
                "MATCH (n:{OVERLAY_LABEL}) WHERE id(n) = {} DELETE n",
                transient.as_u64()
            ))
            .unwrap();
        let after_delete = census(database.graph_store().as_ref(), "life2 after delete");
        database.close().unwrap();
        after_delete
    };

    // Life 3: reopen and see what is left on disk.
    let reopened = {
        let database = open(&path);
        let reopened = census(database.graph_store().as_ref(), "life3 open");
        database.close().unwrap();
        reopened
    };

    // Assertions last, so the census above is always printed. Each base label
    // still owns exactly the one node it was seeded with, in the live store and
    // after the reopen.
    for label in BASE_LABELS {
        assert_eq!(
            count_of(&after_delete, label),
            1,
            "deleting an overlay node dropped base label `{label}` from the live store"
        );
        assert_eq!(
            count_of(&reopened, label),
            1,
            "base label `{label}` did not survive the reopen"
        );
    }
    assert_eq!(
        count_of(&after_delete, OVERLAY_LABEL),
        0,
        "the overlay node was supposed to be deleted"
    );
}

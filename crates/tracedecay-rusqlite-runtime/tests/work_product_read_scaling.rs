//! Compare current-head reads against short and long published histories.

#[path = "support/work_product_graph.rs"]
mod work_product_graph;
#[path = "rusqlite_suite/work_registered_store/mod.rs"]
pub mod work_registered_store;

use tracedecay_contracts::{AddWorkTaskRequestV1, WorkGraphReadV1, WorkProductExpectedAuthorityV1};
use tracedecay_domain::{UtcMicros, WorkGraphVersionV1};
use work_product_graph::{
    binding, context, create, item, mutation, mutations, read_current, repository_selection,
};
use work_registered_store::RegisteredWorkStore;

fn published_history(versions: u64) -> (RegisteredWorkStore, WorkGraphVersionV1) {
    let store = RegisteredWorkStore::start("work-product-long-history");
    let mut head = create(
        &store,
        "command.work-product.long-history.create",
        UtcMicros(100),
        vec![item("task.history.0", &[], 1)],
    )
    .expect("create the work product");
    for version in 1..versions {
        let mut next = mutation(
            &format!("command.work-product.long-history.add.{version}"),
            UtcMicros(100 + i64::try_from(version).expect("version fits")),
        );
        next.expected_authority = WorkProductExpectedAuthorityV1::Verified {
            verified_version: head.verified_graph_version().clone(),
        };
        head = mutations(&store)
            .add_task(
                &context(),
                &binding(),
                AddWorkTaskRequestV1 {
                    selection: repository_selection(),
                    item: item(&format!("task.history.{version}"), &[], 1),
                    mutation: next,
                },
            )
            .expect("publish the next graph version");
    }
    let version = head.verified_graph_version().graph_version();
    (store, version)
}

fn current_read(store: &RegisteredWorkStore, version: WorkGraphVersionV1) -> std::time::Duration {
    let started = std::time::Instant::now();
    let WorkGraphReadV1::Current { snapshot, .. } =
        read_current(store).expect("read the current head")
    else {
        panic!("a current read must answer with a current snapshot");
    };
    let elapsed = started.elapsed();
    assert_eq!(snapshot.graph().version(), version);
    assert_eq!(
        snapshot.graph().items().len(),
        usize::try_from(version.get()).expect("versions fit")
    );
    elapsed
}

/// Every Work mutation reads its current head first, so a current read that
/// re-derived every historical version would make each mutation cost the
/// whole history again and stall a long-running Work product's daemon.
/// Growing the history eightfold grows a single fold of the journal near
/// quadratically, while refolding it once per version grows it near cubically.
/// A read's fixed cost dilutes both: one fold measures about 15x and refolding
/// about 136x, so 48x separates them by roughly threefold either way.
#[test]
fn a_current_read_folds_a_long_history_once_rather_than_once_per_version() {
    let (short_store, short_version) = published_history(16);
    let (long_store, long_version) = published_history(128);
    current_read(&short_store, short_version);
    current_read(&long_store, long_version);

    // Prepare both histories before timing so publication and changing compiler
    // load do not separate the short and long measurement windows.
    let mut samples: Vec<_> = (0..5)
        .map(|sample| {
            let (short, long) = if sample % 2 == 0 {
                let short = current_read(&short_store, short_version);
                (short, current_read(&long_store, long_version))
            } else {
                let long = current_read(&long_store, long_version);
                (current_read(&short_store, short_version), long)
            };
            (long.as_secs_f64() / short.as_secs_f64(), short, long)
        })
        .collect();
    samples.sort_by(|left, right| left.0.total_cmp(&right.0));
    let (growth, short, long) = samples[samples.len() / 2];
    assert!(
        growth < 48.0,
        "an 8x longer history made a current read {growth:.0}x slower \
         ({short:?} -> {long:?}); it must not refold every published version"
    );
}

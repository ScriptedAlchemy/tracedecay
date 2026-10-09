//! Current-head read scaling runs alone so other storage tests cannot skew its ratio.

#[path = "support/work_product_graph.rs"]
mod work_product_graph;
#[path = "rusqlite_suite/work_registered_store/mod.rs"]
pub mod work_registered_store;

use tracedecay_contracts::{AddWorkTaskRequestV1, WorkGraphReadV1, WorkProductExpectedAuthorityV1};
use tracedecay_domain::UtcMicros;
use work_product_graph::{
    binding, context, create, item, mutation, mutations, read_current, repository_selection,
};
use work_registered_store::RegisteredWorkStore;

/// Publish `versions` graph versions, one task per version, and return the
/// fastest of several current reads of the head.
fn fastest_current_read_after(versions: u64) -> std::time::Duration {
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
    (0..5)
        .map(|_| {
            let started = std::time::Instant::now();
            let WorkGraphReadV1::Current { snapshot, .. } =
                read_current(&store).expect("read the current head")
            else {
                panic!("a current read must answer with a current snapshot");
            };
            let elapsed = started.elapsed();
            assert_eq!(
                snapshot.graph().version(),
                head.verified_graph_version().graph_version()
            );
            assert_eq!(
                snapshot.graph().items().len(),
                usize::try_from(versions).expect("versions fit")
            );
            elapsed
        })
        .min()
        .expect("at least one read")
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
    let short = fastest_current_read_after(16);
    let long = fastest_current_read_after(128);
    let growth = long.as_secs_f64() / short.as_secs_f64();
    assert!(
        growth < 48.0,
        "an 8x longer history made a current read {growth:.0}x slower \
         ({short:?} -> {long:?}); it must not refold every published version"
    );
}

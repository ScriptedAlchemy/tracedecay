use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, atomic::AtomicBool};
use std::thread;

use tracedecay_graph_db::{GraphCancellation, GraphProperty, GraphRelationId, NeverCancelled};

use super::*;
use crate::graph_projection::schema::{FILE_IMPORT_EDGE_KIND, IMPORT_LABEL};

struct CancellationBudget {
    observations: AtomicUsize,
    allowed: usize,
}

#[derive(Default)]
struct CountingCancellation {
    observations: AtomicUsize,
}

impl CountingCancellation {
    fn observations(&self) -> usize {
        self.observations.load(Ordering::SeqCst)
    }
}

impl GraphCancellation for CountingCancellation {
    fn is_cancelled(&self) -> bool {
        self.observations.fetch_add(1, Ordering::SeqCst);
        thread::yield_now();
        false
    }
}

struct PausingCancellation {
    observations: AtomicUsize,
    pause_at: usize,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    cancelled: AtomicBool,
}

impl PausingCancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

impl GraphCancellation for PausingCancellation {
    fn is_cancelled(&self) -> bool {
        let observation = self.observations.fetch_add(1, Ordering::SeqCst) + 1;
        if observation == self.pause_at {
            self.entered.wait();
            self.release.wait();
        }
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl CancellationBudget {
    fn new(allowed: usize) -> Self {
        Self {
            observations: AtomicUsize::new(0),
            allowed,
        }
    }
}

impl GraphCancellation for CancellationBudget {
    fn is_cancelled(&self) -> bool {
        self.observations.fetch_add(1, Ordering::SeqCst) >= self.allowed
    }
}

fn cancellation_budget(allowed: usize) -> Arc<dyn GraphCancellation> {
    Arc::new(CancellationBudget::new(allowed))
}

fn assert_catalog_is_cold(store: &CodeGraphProjectionStore) {
    assert!(
        store
            .interactive_catalog_is_warm()
            .is_ok_and(|is_warm| !is_warm),
        "failed warming must not expose a partial catalog"
    );
}

fn assert_catalog_warm_state_is_contended(store: &CodeGraphProjectionStore) {
    assert!(
        matches!(
            store.interactive_catalog_is_warm(),
            Err(CodeGraphProjectionError::Unavailable(_))
        ),
        "warm-state inspection must not fabricate cold while the builder owns the slot"
    );
}

fn warm_observations(store: &CodeGraphProjectionStore) -> usize {
    let cancellation = Arc::new(CountingCancellation::default());
    store
        .warm_interactive_catalog_with_cancellation(cancellation.clone())
        .expect("counted warm");
    cancellation.observations()
}

fn catalog_authority(store: &CodeGraphProjectionStore) -> usize {
    let slot = store
        .interactive_catalog
        .read()
        .expect("interactive catalog lock");
    Arc::as_ptr(slot.as_ref().expect("warm catalog authority")) as usize
}

fn many_import_manifest() -> GraphGenerationManifest {
    let mut files = Vec::new();
    let mut imports = Vec::new();
    for index in 0..32 {
        let occurrence = format!("file.warm.import.{index:02}");
        let path = format!("src/warm-import-{index:02}.ts");
        files.push(file(&occurrence, &path));
        imports.push(super::imports::external_type_import(
            &occurrence,
            &path,
            "pkg",
            "Foo",
            "Foo",
            0,
        ));
    }
    super::imports::import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION)
}

#[test]
fn warmed_symbol_catalog_serves_a_budget_that_cannot_scan_the_cold_projection() {
    let cold_store = store_for(production_manifest());
    assert_eq!(
        reader(&cold_store)
            .resolve_qualified_name("beta::run", None, 8, cancellation_budget(3))
            .expect_err("three cancellation observations cannot build the cold catalog"),
        CodeGraphProjectionError::Cancelled
    );

    let warm_store = store_for(production_manifest());
    assert_catalog_is_cold(&warm_store);
    warm_store
        .warm_interactive_catalog_with_cancellation(Arc::new(NeverCancelled))
        .expect("warm valid symbol catalog");
    assert!(
        warm_store
            .interactive_catalog_is_warm()
            .expect("read warm state")
    );
    let hits = reader(&warm_store)
        .resolve_qualified_name("beta::run", None, 8, cancellation_budget(3))
        .expect("bounded lookup reuses the warm catalog");
    assert_eq!(occurrences(&hits), vec!["sym.beta.run".to_owned()]);
}

#[test]
fn cached_warm_still_honors_cancellation() {
    let store = store_for(production_manifest());
    store
        .warm_interactive_catalog_with_cancellation(Arc::new(NeverCancelled))
        .expect("warm valid catalog");
    assert_eq!(
        store
            .warm_interactive_catalog_with_cancellation(Arc::new(CancelledNow))
            .expect_err("cached warming still checks cancellation"),
        CodeGraphProjectionError::Cancelled
    );
}

#[test]
fn concurrent_warmers_share_one_full_catalog_build() {
    let cold_observations = warm_observations(&store_for(many_import_manifest()));
    let cached_baseline = store_for(many_import_manifest());
    cached_baseline
        .warm_interactive_catalog_with_cancellation(Arc::new(NeverCancelled))
        .expect("warm cached baseline");
    let cached_observations = warm_observations(&cached_baseline);
    assert!(
        cold_observations > cached_observations,
        "fixture distinguishes a full build from a cached warm"
    );

    let store = Arc::new(store_for(many_import_manifest()));
    let start = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let store = Arc::clone(&store);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                let cancellation = Arc::new(CountingCancellation::default());
                start.wait();
                let result = store.warm_interactive_catalog_with_cancellation(cancellation.clone());
                (
                    result,
                    cancellation.observations(),
                    catalog_authority(&store),
                )
            })
        })
        .collect();
    start.wait();
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("warm thread"))
        .collect();

    assert!(outcomes.iter().all(|(result, _, _)| result.is_ok()));
    assert_eq!(
        outcomes
            .iter()
            .filter(|(_, observations, _)| *observations >= cold_observations)
            .count(),
        1,
        "exactly one concurrent warmer may pay for a full projection scan"
    );
    assert!(
        outcomes
            .iter()
            .any(|(_, observations, _)| *observations <= cached_observations),
        "the follower must reuse the completed catalog instead of scanning"
    );
    assert_eq!(
        outcomes[0].2, outcomes[1].2,
        "both warmers return through the same cached catalog authority"
    );
}

#[test]
fn warmed_import_catalog_serves_a_budget_that_cannot_scan_the_cold_projection() {
    let cold_store = store_for(many_import_manifest());
    assert_eq!(
        reader(&cold_store)
            .external_type_import_candidates("pkg", None, 1, cancellation_budget(6))
            .expect_err("six cancellation observations cannot build the cold catalog"),
        CodeGraphProjectionError::Cancelled
    );

    let warm_store = store_for(many_import_manifest());
    warm_store
        .warm_interactive_catalog_with_cancellation(Arc::new(NeverCancelled))
        .expect("warm valid import catalog");
    let candidates = reader(&warm_store)
        .external_type_import_candidates("pkg", None, 1, cancellation_budget(6))
        .expect("bounded import lookup reuses the warm catalog");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].logical_path, "src/warm-import-00.ts");
}

#[test]
fn corrupt_import_payload_and_link_fail_warming_without_exposure() {
    let (files, imports) = super::imports::two_import_fixture();
    let mut malformed =
        super::imports::import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);
    let import = malformed
        .entities
        .iter_mut()
        .find(|entity| has_label(entity, IMPORT_LABEL))
        .expect("import entity");
    *import
        .properties
        .values_mut()
        .next()
        .expect("import payload") = GraphProperty::Bytes(vec![b'{']);
    let malformed_store = store_for(malformed);
    assert!(matches!(
        malformed_store.warm_interactive_catalog_with_cancellation(Arc::new(NeverCancelled)),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));
    assert_catalog_is_cold(&malformed_store);
    assert!(matches!(
        reader(&malformed_store).external_type_import_candidates("pkg", None, 8, request()),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));

    let mut wrong_link =
        super::imports::import_manifest(&files, &imports, CODE_GRAPH_PROJECTOR_REVISION);
    wrong_link
        .relations
        .iter_mut()
        .find(|relation| relation.kind.as_str() == FILE_IMPORT_EDGE_KIND)
        .expect("file-import relation")
        .identity =
        GraphRelationId::new("relation.forged.warm-import").expect("forged relation identity");
    wrong_link
        .relations
        .sort_by(|left, right| left.identity.cmp(&right.identity));
    let wrong_link_store = store_for(wrong_link);
    assert!(matches!(
        wrong_link_store.warm_interactive_catalog_with_cancellation(Arc::new(NeverCancelled)),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));
    assert_catalog_is_cold(&wrong_link_store);
    assert!(matches!(
        reader(&wrong_link_store).external_type_import_candidates("pkg", None, 8, request()),
        Err(CodeGraphProjectionError::Corrupt(_))
    ));
}

#[test]
fn cancellation_during_warm_leaves_the_catalog_cold() {
    let cached_baseline = store_for(many_import_manifest());
    cached_baseline
        .warm_interactive_catalog_with_cancellation(Arc::new(NeverCancelled))
        .expect("warm pause baseline");
    let pause_at = warm_observations(&cached_baseline);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let cancellation = Arc::new(PausingCancellation {
        observations: AtomicUsize::new(0),
        pause_at,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
        cancelled: AtomicBool::new(false),
    });
    let store = Arc::new(store_for(many_import_manifest()));
    let warm_store = Arc::clone(&store);
    let warm_cancellation = Arc::clone(&cancellation);
    let warmer = thread::spawn(move || {
        warm_store.warm_interactive_catalog_with_cancellation(warm_cancellation)
    });

    entered.wait();
    assert_catalog_warm_state_is_contended(&store);
    cancellation.cancel();
    release.wait();
    assert_eq!(
        warmer
            .join()
            .expect("warm thread")
            .expect_err("warm is cancelled during the projection scan"),
        CodeGraphProjectionError::Cancelled
    );
    assert_catalog_is_cold(&store);
    assert_eq!(
        reader(&store)
            .external_type_import_candidates("pkg", None, 1, cancellation_budget(6))
            .expect_err("cancelled warm did not make the catalog usable"),
        CodeGraphProjectionError::Cancelled
    );
}

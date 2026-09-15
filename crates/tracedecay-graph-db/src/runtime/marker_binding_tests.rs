//! A verified-generation marker authorizes exactly the container the engine
//! opened -- never whichever file occupied the path at an earlier or later
//! observation.
//!
//! Two valid containers, `A` and `B`, hold the same generation locator with
//! different rows, so each has its own recovered digest and its own marker.
//! Every test swaps `B` into `A`'s path at a deterministic runtime barrier
//! (`test_seams`) and proves that `A`'s marker cannot vouch for `B`'s rows.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use super::test_seams::{self, Seam};
use super::{GraphDb, GraphEngineOpenSite};
use crate::generation::{
    recovered_generation_enumerations, reset_recovered_generation_enumerations,
};
use crate::location::PersistentGraphStoreState;
use crate::{
    GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity, GraphEntityId,
    GraphFormatVersion, GraphGenerationId, GraphGenerationManifest,
    GraphGenerationManifestIdentity, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
    GraphProperty, GraphPropertyName, GraphWatermark, NeverCancelled, SourceGeneration,
    take_graph_db_verification_counters,
};

fn container_path(temp: &TempDir, name: &str) -> PathBuf {
    let directory = temp.path().join(name);
    fs::create_dir_all(&directory).unwrap();
    directory.join("graph.grafeo")
}

fn open_options(path: &Path) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.to_path_buf()),
        expected_format: GraphFormatVersion::current(),
        durability: GraphDurability::WalSync,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn open_eager(path: &Path) -> Arc<GraphDb> {
    GraphDb::open(open_options(path)).unwrap()
}

fn open_lazy(path: &Path) -> Arc<GraphDb> {
    GraphDb::open_lazy_with_store_state(open_options(path), PersistentGraphStoreState::Existing)
        .unwrap()
}

/// One generation locator; `payload` is the only thing that differs between
/// the two containers, so their identities agree and only their recovered
/// digests differ.
fn manifest(payload: &str) -> GraphGenerationManifest {
    GraphGenerationManifest::new(
        GraphProjectionIdentity::new(
            GraphNamespace::new("marker-binding").unwrap(),
            GraphProjectionId::new("rows").unwrap(),
        ),
        GraphGenerationId::new("g1").unwrap(),
        SourceGeneration::new("source:g1").unwrap(),
        GraphWatermark::new("watermark:g1").unwrap(),
        vec![],
        (0..4)
            .map(|index| {
                GraphEntity::new(
                    GraphEntityId::new(format!("entity:{index}")).unwrap(),
                    BTreeSet::new(),
                    BTreeMap::from([(
                        GraphPropertyName::new("payload").unwrap(),
                        GraphProperty::String(format!("{payload}-{index}")),
                    )]),
                )
                .unwrap()
            })
            .collect(),
        vec![],
    )
    .unwrap()
}

/// A closed container at `path` whose generation was proven in full and whose
/// marker records that proof.
fn build_proven_container(
    path: &Path,
    manifest: &GraphGenerationManifest,
) -> GraphRecoveredGenerationDigestV1 {
    let database = open_eager(path);
    database
        .apply_generation_unverified(Arc::new(manifest.clone()), &|| Ok(()))
        .unwrap();
    let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
    database
        .verify_existing_generation(&manifest.identity(), &expected, &|| Ok(()))
        .unwrap();
    database.close().unwrap();
    assert!(
        path.with_extension("verified").is_file(),
        "closing a proven container must publish its marker"
    );
    expected
}

struct Fixture {
    _temp: TempDir,
    a: PathBuf,
    b: PathBuf,
    /// An empty sibling directory's container path, for moving `A` aside.
    a_aside: PathBuf,
    identity: GraphGenerationManifestIdentity,
    digest_a: GraphRecoveredGenerationDigestV1,
    digest_b: GraphRecoveredGenerationDigestV1,
}

fn fixture() -> Fixture {
    let temp = TempDir::new().unwrap();
    let a = container_path(&temp, "a");
    let b = container_path(&temp, "b");
    let a_aside = container_path(&temp, "a-aside");
    let manifest_a = manifest("rows-a");
    let manifest_b = manifest("rows-b");
    assert_eq!(manifest_a.identity(), manifest_b.identity());
    let digest_a = build_proven_container(&a, &manifest_a);
    let digest_b = build_proven_container(&b, &manifest_b);
    assert_ne!(
        digest_a, digest_b,
        "the two containers must hold different rows"
    );
    Fixture {
        _temp: temp,
        a,
        b,
        a_aside,
        identity: manifest_a.identity(),
        digest_a,
        digest_b,
    }
}

/// The fast path is refused and the rows the engine actually holds are the
/// ones that get proven: `digest_a` fails against `B`'s rows after a real
/// enumeration, and `digest_b` verifies through the same real enumeration.
fn assert_real_verification_yields_b(fixture: &Fixture, database: &GraphDb) {
    let _ = take_graph_db_verification_counters();
    reset_recovered_generation_enumerations();
    let stale =
        database.verify_activated_generation(&fixture.identity, &fixture.digest_a, &|| Ok(()));
    assert!(
        matches!(stale, Err(GraphDbError::GenerationMismatch { .. })),
        "A's digest must fail the real proof over B's rows, got {stale:?}"
    );
    assert_eq!(
        recovered_generation_enumerations(),
        1,
        "the refused fast path must fall through to a real row enumeration"
    );
    let verified = database
        .verify_activated_generation(&fixture.identity, &fixture.digest_b, &|| Ok(()))
        .unwrap();
    assert_eq!(verified, fixture.digest_b);
    let counters = take_graph_db_verification_counters();
    assert_eq!(
        counters.marker_hits, 0,
        "A's marker must never authorize B's rows, saw {counters:?}"
    );
    assert_eq!(counters.full_verifications, 1, "{counters:?}");
}

#[test]
fn a_container_replaced_before_the_eager_engine_open_is_re_proven() {
    let fixture = fixture();
    let a = fixture.a.clone();
    let b = fixture.b.clone();
    let _seam = test_seams::install(move |seam| {
        if seam == Seam::EngineOpen(GraphEngineOpenSite::Eager) {
            fs::rename(&b, &a).unwrap();
        }
    });
    let database = open_eager(&fixture.a);
    assert!(
        !fixture.b.exists(),
        "the barrier must have swapped B into A's path before the engine opened"
    );
    assert_real_verification_yields_b(&fixture, &database);
    database.close().unwrap();
}

#[test]
fn a_container_replaced_before_the_lazy_first_use_open_is_re_proven() {
    let fixture = fixture();
    let database = open_lazy(&fixture.a);
    let a = fixture.a.clone();
    let b = fixture.b.clone();
    let _seam = test_seams::install(move |seam| {
        if seam == Seam::EngineOpen(GraphEngineOpenSite::LazyFirstUse) {
            fs::rename(&b, &a).unwrap();
        }
    });
    assert!(
        fixture.b.exists(),
        "constructing the lazy handle must not open the engine"
    );
    assert_real_verification_yields_b(&fixture, &database);
    assert!(!fixture.b.exists());
    database.close().unwrap();
}

#[test]
fn a_container_replaced_between_engine_close_and_publish_keeps_a_proofs_off_b() {
    let fixture = fixture();
    // A's own marker resolves A's proof; that proof is what the close is about
    // to publish.
    let database = open_eager(&fixture.a);
    let _ = take_graph_db_verification_counters();
    database
        .verify_activated_generation(&fixture.identity, &fixture.digest_a, &|| Ok(()))
        .unwrap();
    assert_eq!(take_graph_db_verification_counters().marker_hits, 1);
    // A survives at a side path so its identity can be read back afterwards.
    let a = fixture.a.clone();
    let aside = fixture.a_aside.clone();
    let b = fixture.b.clone();
    let seam = test_seams::install(move |seam| {
        if seam == Seam::MarkerPublish {
            fs::rename(&a, &aside).unwrap();
            fs::rename(&b, &a).unwrap();
        }
    });
    database.close().unwrap();
    drop(seam);
    assert!(
        !fixture.b.exists(),
        "the barrier must have swapped B in before publish"
    );

    // The published marker names the container the closed engine's own handle
    // reported -- A -- not the file that occupied the path when it was written.
    let marker: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.a.with_extension("verified")).unwrap()).unwrap();
    let recorded = marker["body"]["container"].clone();
    let surviving_a = open_eager(&fixture.a_aside);
    assert_eq!(
        recorded,
        serde_json::to_value(surviving_a.inner.markers.bound_identity().unwrap()).unwrap(),
        "the marker must record the identity of the container the engine closed"
    );
    surviving_a.close().unwrap();

    // B now sits at A's path beside that marker. The marker carries A's proof
    // under A's identity, so it is not admissible for B.
    let reopened = open_eager(&fixture.a);
    assert_ne!(
        recorded,
        serde_json::to_value(reopened.inner.markers.bound_identity().unwrap()).unwrap(),
    );
    assert_real_verification_yields_b(&fixture, &reopened);
    reopened.close().unwrap();
}

/// Proofs are bound to an engine incarnation, and every incarnation that opens
/// the same container re-admits them: a hibernated lazy handle resolves by
/// marker again after it reopens, instead of paying a full proof for the rest
/// of the handle's life.
#[test]
fn a_reopened_engine_re_admits_the_marker_for_its_own_incarnation() {
    let fixture = fixture();
    let database = open_lazy(&fixture.a);
    let _ = take_graph_db_verification_counters();
    reset_recovered_generation_enumerations();
    database
        .verify_activated_generation(&fixture.identity, &fixture.digest_a, &|| Ok(()))
        .unwrap();
    assert!(database.staging_engine_is_open());
    database.hibernate_if_lazy().unwrap();
    assert!(!database.staging_engine_is_open());
    assert!(
        database.inner.markers.bound_identity().is_none(),
        "a hibernated handle has no engine whose container a proof could be about"
    );

    database
        .verify_activated_generation(&fixture.identity, &fixture.digest_a, &|| Ok(()))
        .unwrap();
    let counters = take_graph_db_verification_counters();
    assert_eq!(counters.marker_hits, 2, "{counters:?}");
    assert_eq!(counters.full_verifications, 0, "{counters:?}");
    assert_eq!(recovered_generation_enumerations(), 0);
    database.close().unwrap();
}

#[test]
fn an_unchanged_container_still_takes_the_fast_path_on_both_open_sites() {
    let fixture = fixture();
    for database in [open_eager(&fixture.a), open_lazy(&fixture.a)] {
        let _ = take_graph_db_verification_counters();
        reset_recovered_generation_enumerations();
        let verified = database
            .verify_activated_generation(&fixture.identity, &fixture.digest_a, &|| Ok(()))
            .unwrap();
        assert_eq!(verified, fixture.digest_a);
        let counters = take_graph_db_verification_counters();
        assert_eq!(counters.marker_hits, 1, "{counters:?}");
        assert_eq!(counters.full_verifications, 0, "{counters:?}");
        assert_eq!(recovered_generation_enumerations(), 0);
        database.close().unwrap();
    }
}

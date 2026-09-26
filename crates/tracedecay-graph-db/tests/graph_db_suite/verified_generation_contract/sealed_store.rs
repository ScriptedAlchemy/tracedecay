//! Sealed per-generation compact store contract: seal builds an isolated,
//! digest-proven store; reads serve from it while the next generation stages
//! and seals; recovery adopts it from disk; retirement deletes it; and a
//! post-seal conflicting restage receives the typed immutable refusal.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tracedecay_graph_db::{
    GraphGenerationRowSpill, GraphGenerationRows, GraphLabel, GraphTraversalDirection,
    TraversalRequest,
};

use super::*;

fn sealed_store_root(root: &Path) -> PathBuf {
    support::graph_path(root).with_extension("sealed")
}

/// Every sealed receipt currently on disk, as raw JSON strings.
fn remove_sealed_checks(root: &Path) {
    let Ok(entries) = std::fs::read_dir(sealed_store_root(root)) else {
        return;
    };
    for entry in entries.map(Result::unwrap) {
        let check = entry.path().join("sealed.checked");
        if check.is_file() {
            std::fs::remove_file(check).unwrap();
        }
    }
}

fn sealed_receipts(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(sealed_store_root(root)) else {
        return Vec::new();
    };
    let mut receipts = Vec::new();
    for entry in entries.map(Result::unwrap) {
        let receipt = entry.path().join("sealed.json");
        if receipt.is_file() {
            receipts.push(std::fs::read_to_string(receipt).unwrap());
        }
    }
    receipts
}

fn receipt_for_generation(root: &Path, generation: &str) -> Option<String> {
    sealed_receipts(root)
        .into_iter()
        .find(|receipt| receipt.contains(&format!("\"generation\": \"{generation}\"")))
}

fn rich_manifest(
    projection_identity: GraphProjectionIdentity,
    generation: &str,
    marker: &str,
) -> GraphGenerationManifest {
    let from = GraphEntityRef::new(
        projection_identity.clone(),
        GraphEntityId::new("entity:a").unwrap(),
    );
    let to = GraphEntityRef::new(
        projection_identity.clone(),
        GraphEntityId::new("entity:b").unwrap(),
    );
    GraphGenerationManifest::new(
        projection_identity,
        GraphGenerationId::new(generation).unwrap(),
        SourceGeneration::new(format!("source:{generation}")).unwrap(),
        GraphWatermark::new(format!("watermark:{generation}")).unwrap(),
        Vec::new(),
        vec![entity("entity:a", marker), entity("entity:b", marker)],
        vec![
            GraphGenerationRelation::new(
                GraphRelationId::new("relation:a-b").unwrap(),
                from,
                to,
                GraphRelationKind::new("references").unwrap(),
                BTreeMap::from([(
                    GraphPropertyName::new("weight").unwrap(),
                    GraphProperty::I64(7),
                )]),
            )
            .unwrap(),
        ],
    )
    .unwrap()
}

fn assert_snapshot_reads(
    snapshot: &tracedecay_graph_db::VerifiedGraphSnapshot,
    identity: &GraphProjectionIdentity,
    marker: &str,
) {
    let entity = snapshot
        .entity(
            &GraphEntityRef::new(identity.clone(), GraphEntityId::new("entity:a").unwrap()),
            Arc::new(TestCancellation),
        )
        .unwrap()
        .expect("sealed entity:a must resolve");
    assert_eq!(
        entity
            .properties
            .get(&GraphPropertyName::new("marker").unwrap()),
        Some(&GraphProperty::String(marker.to_owned())),
    );
    let relation = snapshot
        .relation(
            &GraphRelationRef::new(
                identity.clone(),
                GraphRelationId::new("relation:a-b").unwrap(),
            ),
            Arc::new(TestCancellation),
        )
        .unwrap()
        .expect("sealed relation must resolve");
    assert_eq!(relation.from.identity.as_str(), "entity:a");
    assert_eq!(relation.to.identity.as_str(), "entity:b");
    let traversal = snapshot
        .traverse(TraversalRequest {
            namespace: identity.namespace.clone(),
            start: GraphEntityId::new("entity:a").unwrap(),
            relation_kinds: BTreeSet::new(),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 2,
            max_visits: 16,
            max_results: 16,
            cancellation: Arc::new(TestCancellation),
        })
        .unwrap();
    let visited: Vec<_> = traversal
        .visits
        .iter()
        .map(|visit| visit.entity.identity.as_str().to_owned())
        .collect();
    assert_eq!(visited, vec!["entity:a".to_owned(), "entity:b".to_owned()]);
}

fn publish(
    registered: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    key: &GraphPublicationKeyV1,
) -> tracedecay_graph_db::VerifiedGraphCommit {
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), root),
            authority,
            &context,
            key,
            None,
        )
        .unwrap()
}

fn stage_sealed_manifest(
    authority: &mut RelationalAuthority,
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    manifest: &GraphGenerationManifest,
    idempotency: &str,
    expected: Option<GraphVerifiedHeadV1>,
    input: char,
) -> GraphPublicationReplayRecordV1 {
    let source = SealedCodeGenerationReplay {
        repository: RepositoryId::new("repository.graph-staging-release").unwrap(),
        generation: CodeGenerationId::new(format!(
            "code-generation.{}",
            manifest.generation.as_str()
        ))
        .unwrap(),
        sealed_state_digest: SealedGraphStateDigest::try_from(format!(
            "sha256:{}",
            input.to_string().repeat(64)
        ))
        .unwrap(),
        projector_revision: GraphProjectorRevision::try_from(
            "projector.graph-staging-release".to_owned(),
        )
        .unwrap(),
    };
    authority.stage(
        manifest
            .relational_sealed_replay(
                binding.shard_id.clone(),
                GraphIdempotencyKey::new(idempotency).unwrap(),
                digest(input),
                expected,
                source,
                &|| Ok(()),
            )
            .unwrap(),
    )
}

fn publish_sealed(
    registered: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    record: &GraphPublicationReplayRecordV1,
    manifest: &GraphGenerationManifest,
) -> tracedecay_graph_db::VerifiedGraphCommit {
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), root),
            authority,
            &context,
            &record.publication.key,
            Some(Arc::new(manifest.clone()).into()),
        )
        .unwrap()
}

/// Puts `manifest`'s rows in the shared staging database before it is
/// published: the on-disk shape of every sealed-replay code generation
/// published before generations sealed straight from their manifest. Release
/// and recovery over those rows stay under contract through this fixture.
fn stage_rows_before_publish(
    registered: &RegisteredGraph,
    root: &Path,
    manifest: &GraphGenerationManifest,
) {
    registered
        .registry
        .resolve(registration(registered.binding.clone(), root))
        .unwrap()
        .stage_generation_rows_unpublished(Arc::new(manifest.clone()))
        .unwrap();
}

/// A dependency-free sealed-replay generation seals straight from its
/// manifest: no staging row is ever written for it, it is sealed-only from
/// its first instant, and the release sweep has nothing to delete.
///
/// Fails if publication stages the rows first (counts come back `(2, 1)`),
/// if the ledger still claims staging rows for it (`is_generation_sealed_only`
/// false), or if the artifact does not serve reads and telemetry.
#[test]
fn dependency_free_sealed_head_seals_directly_without_staging_rows() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:direct", "code");
    let manifest = rich_manifest(identity.clone(), "direct-g1", "direct");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:direct-g1",
        None,
        '0',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (0, 0)
    );
    assert!(
        database
            .is_generation_sealed_only(&manifest.identity())
            .unwrap()
    );
    assert!(commit.snapshot.serves_from_sealed_store());
    assert!(receipt_for_generation(temp.path(), "direct-g1").is_some());
    assert_snapshot_reads(&commit.snapshot, &identity, "direct");
    let telemetry = commit
        .snapshot
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: identity.namespace.clone(),
            projection: identity.projection.clone(),
            cancellation: Arc::new(TestCancellation),
        })
        .unwrap()
        .expect("sealed projection telemetry must resolve");
    assert_eq!(telemetry.entity_count, 2);
    assert_eq!(telemetry.relation_count, 1);

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::AlreadyReleased
    );
    assert_snapshot_reads(&commit.snapshot, &identity, "direct");
}

/// A sealed head whose rows are already in the staging database (a database
/// written before direct sealing) releases them once and keeps serving from
/// the artifact.
#[test]
fn dependency_free_sealed_head_releases_staging_and_keeps_serving() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:release", "code");
    let manifest = rich_manifest(identity.clone(), "released-g1", "sealed-only");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:released-g1",
        None,
        '1',
    );
    stage_rows_before_publish(&registered, temp.path(), &manifest);
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (2, 1)
    );
    assert!(
        !database
            .is_generation_sealed_only(&manifest.identity())
            .unwrap(),
        "a generation with staging rows must not be claimed sealed-only"
    );

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        }
    );
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (0, 0)
    );
    assert_snapshot_reads(&commit.snapshot, &identity, "sealed-only");
    let page = commit
        .snapshot
        .read_projection(GraphProjectionReadRequest {
            namespace: identity.namespace.clone(),
            projection: identity.projection.clone(),
            after_entity: None,
            after_relation: None,
            max_entities: 8,
            max_relations: 8,
            cancellation: Arc::new(TestCancellation),
        })
        .unwrap();
    assert_eq!(page.entities.len(), 2);
    assert_eq!(page.relations.len(), 1);
    let telemetry = commit
        .snapshot
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: identity.namespace.clone(),
            projection: identity.projection.clone(),
            cancellation: Arc::new(TestCancellation),
        })
        .unwrap()
        .expect("sealed projection telemetry must resolve");
    assert_eq!(telemetry.entity_count, 2);
    assert_eq!(telemetry.relation_count, 1);

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::AlreadyReleased
    );
}

/// A remounted daemon has no seated lease and no installed sealed reader.
/// Release still deletes the duplicate staging rows, but it must not open the
/// sealed engine or hydrate the canonical source to do that: that is the
/// whole-generation recovery that overran one maintenance tick (#1247).
#[test]
fn remounted_release_does_not_reprove_or_rehydrate_the_sealed_generation() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:bounded-release", "code");
    let manifest = rich_manifest(identity, "bounded-g1", "bounded");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:bounded-g1",
        None,
        '6',
    );
    stage_rows_before_publish(&registered, temp.path(), &manifest);
    publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    assert!(registered.close().unwrap());
    registered.mount().unwrap();

    let _ = take_graph_db_verification_counters();
    let _ = take_graph_db_hydration_counters();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        }
    );
    let verification = take_graph_db_verification_counters();
    let hydration = take_graph_db_hydration_counters();
    assert_eq!(
        (verification.full_verifications, verification.marker_hits),
        (0, 0),
        "release must not open or prove the sealed generation"
    );
    assert_eq!(
        (hydration.nodes, hydration.edges),
        (0, 0),
        "release must not hydrate sealed generation rows"
    );
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (0, 0)
    );
}

/// A crash after `sealed.json` is renamed into place and before the
/// post-reopen proof persists `sealed.checked`. That receipt must not
/// authorize deleting the only reconstructable staging rows.
#[test]
fn pre_proof_sealed_receipt_does_not_authorize_staging_release() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:pre-proof-receipt", "code");
    let manifest = rich_manifest(identity, "pre-proof-g1", "pre-proof");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:pre-proof-g1",
        None,
        '6',
    );
    stage_rows_before_publish(&registered, temp.path(), &manifest);
    publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    remove_sealed_checks(temp.path());
    assert!(registered.close().unwrap());
    registered.mount().unwrap();

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::Retained(SealedStagingRetentionReason::NoSealedStore)
    );
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (2, 1)
    );
}

#[test]
fn release_retains_rows_without_an_installed_sealed_store() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:no-release", "code");
    let manifest = rich_manifest(identity, "unsealed-g1", "retained");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:unsealed-g1",
        None,
        '2',
    );
    stage_rows_before_publish(&registered, temp.path(), &manifest);
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    database
        .discard_sealed_generation_reader(&manifest.identity())
        .unwrap();
    std::fs::remove_dir_all(sealed_store_root(temp.path())).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::Retained(SealedStagingRetentionReason::NoSealedStore)
    );
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (2, 1)
    );
    assert_snapshot_reads(&commit.snapshot, &manifest.projection, "retained");
}

/// A dependency-bearing generation stages through the shared database (its
/// endpoints resolve against the base's staging rows), and release keeps its
/// rows. The base carries staging rows here: a directly sealed base is
/// sealed-only, and staging a dependent against it is the typed
/// `require_exact_dependencies` conflict.
#[test]
fn release_retains_dependency_bearing_generation_rows() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let base_identity = projection("sealed-store:dependency-base", "code");
    let base = rich_manifest(base_identity.clone(), "base-g1", "base");
    let base_record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &base,
        "publish:base-g1",
        None,
        '3',
    );
    stage_rows_before_publish(&registered, temp.path(), &base);
    publish_sealed(
        &registered,
        temp.path(),
        &mut authority,
        &base_record,
        &base,
    );

    let identity = projection("sealed-store:dependency-owner", "code");
    let dependency = GraphGenerationDependency::new(
        base_identity,
        base.generation.clone(),
        GraphIdempotencyKey::new("publish:base-g1").unwrap(),
    );
    let manifest = GraphGenerationManifest::new(
        identity,
        GraphGenerationId::new("dependent-g1").unwrap(),
        SourceGeneration::new("source:dependent-g1").unwrap(),
        GraphWatermark::new("watermark:dependent-g1").unwrap(),
        vec![dependency],
        vec![entity("entity:dependent", "dependent")],
        Vec::new(),
    )
    .unwrap();
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:dependent-g1",
        None,
        '4',
    );
    publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::Retained(SealedStagingRetentionReason::DependencyBearing)
    );
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (1, 0)
    );
}

#[test]
fn missing_sealed_only_artifact_requires_reset_and_allows_republish() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:artifact-loss", "code");
    let manifest = rich_manifest(identity.clone(), "artifact-loss-g1", "restaged");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:artifact-loss-g1",
        None,
        '5',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    assert!(
        database
            .is_generation_sealed_only(&manifest.identity())
            .unwrap()
    );
    drop(commit);
    database
        .discard_sealed_generation_reader(&manifest.identity())
        .unwrap();
    std::fs::remove_dir_all(sealed_store_root(temp.path())).unwrap();

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        registered.registry.recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        ),
        Err(GraphDbError::ResetRequired { .. })
    ));

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let republished = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            Some(Arc::new(manifest.clone()).into()),
        )
        .unwrap();
    assert_snapshot_reads(&republished.snapshot, &identity, "restaged");
}

/// The core seal -> compact-isolated-store -> reopen -> read journey, with a
/// second generation staging and sealing in parallel with reads on the first
/// generation's sealed store.
#[test]
fn seal_builds_compact_store_while_second_generation_stages_and_seals() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:parallel", "code");

    let g1 = rich_manifest(identity.clone(), "sealed-g1", "one");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:sealed-g1",
        None,
        '1',
    );
    let g1_commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &g1_record.publication.key,
    );
    assert!(
        g1_commit.snapshot.serves_from_sealed_store(),
        "the sealed generation must serve from its isolated store"
    );
    // String/i64 rows round-trip the columnar codecs, so the artifact is a
    // real compacted store, not the replay fallback.
    let receipt = receipt_for_generation(temp.path(), "sealed-g1")
        .expect("seal must write the artifact receipt");
    assert!(
        receipt.contains("\"form\": \"compact\""),
        "byte-free rows must seal in compact form: {receipt}"
    );
    assert_snapshot_reads(&g1_commit.snapshot, &identity, "one");

    // A second generation stages and seals while a reader hammers the first
    // generation's sealed store.
    let stop = Arc::new(AtomicBool::new(false));
    let reader_snapshot = g1_commit.snapshot.clone();
    let reader_identity = identity.clone();
    let reader_stop = Arc::clone(&stop);
    let reader = thread::spawn(move || {
        let mut reads = 0usize;
        while !reader_stop.load(Ordering::SeqCst) {
            assert_snapshot_reads(&reader_snapshot, &reader_identity, "one");
            reads += 1;
        }
        reads
    });

    let g2 = rich_manifest(identity.clone(), "sealed-g2", "two");
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:sealed-g2",
        Some(g1_commit.head.clone()),
        '2',
    );
    let g2_commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &g2_record.publication.key,
    );
    stop.store(true, Ordering::SeqCst);
    let reads = reader.join().unwrap();
    assert!(
        reads > 0,
        "the reader must have exercised the sealed store during the second seal"
    );

    assert!(g2_commit.snapshot.serves_from_sealed_store());
    assert_snapshot_reads(&g2_commit.snapshot, &identity, "two");
    // Both generations now hold their own isolated artifacts.
    assert!(receipt_for_generation(temp.path(), "sealed-g1").is_some());
    assert!(receipt_for_generation(temp.path(), "sealed-g2").is_some());
    // The first generation's sealed store still answers after the second seal.
    assert_snapshot_reads(&g1_commit.snapshot, &identity, "one");
}

/// Generations carrying Bytes properties seal in compact form and read every
/// byte back exactly: the compact dictionary carries a typed Bytes entry, so
/// no size threshold or replay fallback stands between a Bytes row and the
/// columnar artifact.
#[test]
fn bytes_rows_seal_compact_and_read_exactly() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:bytes", "code");

    let mut g1 = rich_manifest(identity.clone(), "bytes-g1", "payload");
    let payload = vec![0u8, 159, 146, 150];
    for entity in &mut g1.entities {
        entity.properties.insert(
            GraphPropertyName::new("record").unwrap(),
            GraphProperty::Bytes(payload.clone()),
        );
    }
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:bytes-g1",
        None,
        '3',
    );
    let commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &record.publication.key,
    );
    assert!(commit.snapshot.serves_from_sealed_store());
    let receipt = receipt_for_generation(temp.path(), "bytes-g1")
        .expect("seal must write the artifact receipt");
    assert!(
        receipt.contains("\"form\": \"compact\""),
        "every sealed generation is a compact artifact: {receipt}"
    );
    assert_snapshot_reads(&commit.snapshot, &identity, "payload");
    let entity = commit
        .snapshot
        .entity(
            &GraphEntityRef::new(identity.clone(), GraphEntityId::new("entity:b").unwrap()),
            Arc::new(TestCancellation),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        entity
            .properties
            .get(&GraphPropertyName::new("record").unwrap()),
        Some(&GraphProperty::Bytes(payload)),
    );
}

/// A restage of the same generation identity with different content is
/// refused with the typed sealed-store error, not a generic conflict.
#[test]
fn post_seal_conflicting_restage_gets_typed_immutable_refusal() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:refusal", "code");

    let g1 = rich_manifest(identity.clone(), "refused-g1", "original");
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:refused-g1",
        None,
        '4',
    );
    let commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &record.publication.key,
    );
    assert!(commit.snapshot.serves_from_sealed_store());

    // Same (projection, generation) identity, different source generation and
    // rows: an inadmissible rewrite of sealed content. The relational
    // authority is rolled back to its pre-publication state, the restored-
    // from-backup divergence that used to reach sealed rows as a stage-page
    // write, so the store itself is the last line refusing the rewrite.
    authority.heads.remove(&record.publication.key.projection);
    let foreign = GraphGenerationManifest::new(
        identity.clone(),
        GraphGenerationId::new("refused-g1").unwrap(),
        SourceGeneration::new("source:refused-g1-foreign").unwrap(),
        GraphWatermark::new("watermark:refused-g1").unwrap(),
        Vec::new(),
        vec![entity("entity:a", "foreign")],
        Vec::new(),
    )
    .unwrap();
    let foreign_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &foreign,
        "publish:refused-g1-foreign",
        None,
        '5',
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let error = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &foreign_record.publication.key,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(error, GraphDbError::SealedStoreImmutable { .. }),
        "a post-seal conflicting restage must get the typed refusal: {error:?}"
    );
    // The sealed store still serves the original rows.
    assert_snapshot_reads(&commit.snapshot, &identity, "original");
}

/// Restart recovery adopts the on-disk artifact instead of rebuilding it, and
/// a tampered receipt is discarded while reads fall back to the staging rows.
#[test]
fn restart_recovery_adopts_or_discards_the_on_disk_artifact() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:recovery", "code");

    let g1 = rich_manifest(identity.clone(), "recovered-g1", "durable");
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:recovered-g1",
        None,
        '6',
    );
    let commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &record.publication.key,
    );
    assert!(commit.snapshot.serves_from_sealed_store());
    drop(commit);
    assert!(registered.close().unwrap());
    drop(registered);

    // Restart: recovery must adopt the artifact from disk.
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let snapshot = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        )
        .unwrap();
    assert!(
        snapshot.serves_from_sealed_store(),
        "recovery must adopt the sealed artifact from disk"
    );
    assert_snapshot_reads(&snapshot, &identity, "durable");
    drop(snapshot);
    assert!(registered.close().unwrap());
    drop(registered);

    // Tamper with the receipt: recovery must discard the artifact and serve
    // from the staging database.
    let root = sealed_store_root(temp.path());
    let mut tampered = None;
    for entry in std::fs::read_dir(&root).unwrap().map(Result::unwrap) {
        let receipt = entry.path().join("sealed.json");
        if receipt.is_file() {
            let contents = std::fs::read_to_string(&receipt).unwrap();
            std::fs::write(&receipt, contents.replace("sha256:", "sha256-tampered:")).unwrap();
            tampered = Some(entry.path());
        }
    }
    let tampered = tampered.expect("the sealed artifact directory must exist");
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let snapshot = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        )
        .unwrap();
    assert!(
        !snapshot.serves_from_sealed_store(),
        "a tampered artifact must not be adopted"
    );
    assert!(
        !tampered.exists(),
        "a tampered artifact must be discarded from disk"
    );
    assert_snapshot_reads(&snapshot, &identity, "durable");
}

/// A seal that died between container write and rename leaves
/// `.staging-<digest>` under the sealed root. Nothing reads it, so the next
/// open of the store removes every such directory while leaving installed
/// artifacts untouched.
#[test]
fn reopening_the_store_sweeps_staging_left_by_an_interrupted_seal() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:staging-sweep", "code");

    let g1 = rich_manifest(identity.clone(), "swept-g1", "durable");
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:swept-g1",
        None,
        '6',
    );
    let commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &record.publication.key,
    );
    assert!(commit.snapshot.serves_from_sealed_store());
    drop(commit);
    assert!(registered.close().unwrap());
    drop(registered);

    // Two interrupted seals: one of a generation that never installed and one
    // whose digest matches the installed artifact but was started again.
    let root = sealed_store_root(temp.path());
    let installed = std::fs::read_dir(&root)
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .find(|path| path.join("sealed.json").is_file())
        .expect("the sealed artifact directory must exist");
    let abandoned = [
        root.join(format!(".staging-{}", "c".repeat(64))),
        root.join(format!(
            ".staging-{}",
            installed.file_name().unwrap().to_str().unwrap()
        )),
    ];
    for staging in &abandoned {
        std::fs::create_dir_all(staging).unwrap();
        std::fs::write(staging.join("generation.grafeo"), b"partial container").unwrap();
    }

    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let snapshot = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        )
        .unwrap();
    for staging in &abandoned {
        assert!(
            !staging.exists(),
            "opening the store removes abandoned staging {}",
            staging.display()
        );
    }
    assert!(
        installed.join("sealed.json").is_file(),
        "the installed sealed artifact is untouched by the sweep"
    );
    assert!(snapshot.serves_from_sealed_store());
    assert_snapshot_reads(&snapshot, &identity, "durable");
}

/// The sealed census splits the sealed root into what serves and what does
/// not: the head's artifact, a superseded generation's artifact, and staging
/// an interrupted seal left behind are each counted where they belong, and a
/// directory without a readable receipt is reported rather than classed.
#[test]
fn sealed_census_separates_heads_from_superseded_and_abandoned_bytes() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:census", "code");

    let g1 = rich_manifest(identity.clone(), "census-g1", "one");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:census-g1",
        None,
        '1',
    );
    let g1_commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &g1_record.publication.key,
    );
    let g1_head = g1_commit.head.clone();
    drop(g1_commit);
    let g2 = rich_manifest(identity.clone(), "census-g2", "two");
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:census-g2",
        Some(g1_head),
        '2',
    );
    let g2_commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &g2_record.publication.key,
    );
    drop(g2_commit);
    let root = sealed_store_root(temp.path());
    let staging = root.join(format!(".staging-{}", "d".repeat(64)));
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("generation.grafeo"), vec![0u8; 4096]).unwrap();
    let unreadable = root.join("e".repeat(64));
    std::fs::create_dir_all(&unreadable).unwrap();
    std::fs::write(unreadable.join("generation.grafeo"), b"no receipt").unwrap();

    let heads = std::collections::BTreeSet::from(["census-g2".to_owned()]);
    let census =
        tracedecay_graph_db::census_sealed_store(&support::graph_path(temp.path()), &heads)
            .unwrap();
    assert_eq!(census.head_count, 1);
    assert_eq!(
        census.superseded_count, 1,
        "g1 is sealed but no longer the head"
    );
    assert_eq!(census.abandoned_staging_count, 1);
    assert_eq!(census.abandoned_staging_bytes, 4096);
    assert_eq!(census.unrecognized_count, 1);
    assert!(census.head_bytes > 0 && census.superseded_bytes > 0);

    // With no heads journaled every sealed artifact reads as superseded, and
    // a store with no sealed root at all is an empty census, not an error.
    let none = tracedecay_graph_db::census_sealed_store(
        &support::graph_path(temp.path()),
        &std::collections::BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(none.superseded_count, 2);
    assert_eq!(none.head_count, 0);
    assert_eq!(
        tracedecay_graph_db::census_sealed_store(
            &temp.path().join("absent").join("graph.grafeo"),
            &heads,
        )
        .unwrap(),
        tracedecay_graph_db::SealedStoreCensusV1::default()
    );
}

/// Replaying an already-linearized publication is activation, not a new seal:
/// if an older installation has no derived sealed artifact, seating its
/// verified staging rows must not copy and compact the whole generation before
/// those rows can serve.
#[test]
fn historical_replay_does_not_rebuild_a_missing_sealed_artifact() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:historical-replay", "code");

    let g1 = rich_manifest(identity.clone(), "historical-g1", "durable");
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:historical-g1",
        None,
        '7',
    );
    let commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &record.publication.key,
    );
    assert!(commit.snapshot.serves_from_sealed_store());
    drop(commit);
    assert!(registered.close().unwrap());
    drop(registered);

    std::fs::remove_dir_all(sealed_store_root(temp.path())).unwrap();
    std::fs::remove_file(support::graph_path(temp.path()).with_extension("verified")).unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let replayed = publish(
        &registered,
        temp.path(),
        &mut authority,
        &record.publication.key,
    );

    assert!(
        !replayed.snapshot.serves_from_sealed_store(),
        "historical activation must serve verified staging rows without an eager sealed copy"
    );
    assert!(
        receipt_for_generation(temp.path(), "historical-g1").is_none(),
        "historical activation rebuilt the missing sealed artifact"
    );
    assert_snapshot_reads(&replayed.snapshot, &identity, "durable");
}

/// Retiring a sealed code generation deletes its artifact directory while the
/// successor's artifact stays.
#[test]
fn retirement_deletes_the_superseded_sealed_artifact() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:retire", "code");
    let sealed_generation = CodeGenerationId::new("code-generation.retire-g1").unwrap();
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "7".repeat(64))).unwrap();

    let g1 = rich_manifest(identity.clone(), "retire-g1", "old");
    let g1_record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:retire-g1",
        None,
        '7',
    );
    let g1_commit = publish_sealed(&registered, temp.path(), &mut authority, &g1_record, &g1);
    let g1_head = g1_commit.head.clone();
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    assert!(database.is_generation_sealed_only(&g1.identity()).unwrap());
    drop(g1_commit);

    let g2 = rich_manifest(identity.clone(), "retire-g2", "new");
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:retire-g2",
        Some(g1_head),
        '8',
    );
    let g2_commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &g2_record.publication.key,
    );
    drop(g2_commit);
    assert!(receipt_for_generation(temp.path(), "retire-g1").is_some());
    assert!(receipt_for_generation(temp.path(), "retire-g2").is_some());

    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        registered
            .registry
            .retire_one_code_generation_replay(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &sealed_generation,
                &sealed_digest,
            )
            .unwrap(),
        GraphReplayCollectionOutcome::Retired(_)
    ));
    assert!(
        receipt_for_generation(temp.path(), "retire-g1").is_none(),
        "retirement must delete the superseded sealed artifact"
    );
    assert!(
        receipt_for_generation(temp.path(), "retire-g2").is_some(),
        "the successor's sealed artifact must stay"
    );
    assert!(
        !database.is_generation_sealed_only(&g1.identity()).unwrap(),
        "retirement must remove the locator from sealed-only state"
    );
}

/// A live direct-sealed reader (recovered through
/// `recover_verified_sealed_snapshot`, which bypasses the staging database's
/// verified-generation state) must gate retirement of its generation exactly
/// like an ordinary live snapshot; retirement proceeds once it drops.
#[test]
fn retirement_waits_for_a_live_direct_sealed_reader() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:reader-gate", "code");
    let sealed_generation = CodeGenerationId::new("code-generation.sealed-reader-gate").unwrap();
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "9".repeat(64))).unwrap();

    let g1 = rich_manifest(identity.clone(), "gate-g1", "old");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:gate-g1",
        None,
        '7',
    );
    let g1_commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &g1_record.publication.key,
    );
    let g1_head = g1_commit.head.clone();
    drop(g1_commit);
    // Rewrite the journal row as a sealed code generation replay so both the
    // direct-sealed recovery and the production retirement path own it.
    let sealed_publication = g1
        .relational_sealed_replay(
            registered.binding.shard_id.clone(),
            GraphIdempotencyKey::new("publish:gate-g1").unwrap(),
            digest('7'),
            None,
            SealedCodeGenerationReplay {
                repository: RepositoryId::new("repository.sealed-reader-gate").unwrap(),
                generation: sealed_generation.clone(),
                sealed_state_digest: sealed_digest.clone(),
                projector_revision: GraphProjectorRevision::try_from(
                    "projector.sealed-reader-gate".to_owned(),
                )
                .unwrap(),
            },
            &|| Ok(()),
        )
        .unwrap();
    let rewritten =
        GraphPublicationReplayRecordV1::new(g1_record.sequence, sealed_publication).unwrap();
    let rewritten_head =
        GraphVerifiedHeadV1::from_replay(&rewritten, g1_head.recovered_digest.clone()).unwrap();
    authority
        .records
        .insert(g1_record.publication.key.clone(), rewritten);
    authority.heads.insert(
        g1_record.publication.key.projection.clone(),
        rewritten_head.clone(),
    );

    // Direct-sealed recovery serves cold starts: the staging shard is closed,
    // so the sealed artifact is the only open handle on this generation.
    assert!(
        registered
            .registry
            .close(&registration(registered.binding.clone(), temp.path()))
            .unwrap(),
        "the publishing runtime must close before cold direct recovery"
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let reader = registered
        .registry
        .recover_verified_sealed_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &g1_record.publication.key.projection,
        )
        .expect("direct-sealed recovery of the g1 head");
    assert_eq!(reader.generation(), &g1.generation);

    // The staging runtime comes back while the direct-sealed reader is live,
    // the successor generation publishes through it.
    registered.mount().unwrap();
    let g2 = rich_manifest(identity.clone(), "gate-g2", "new");
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:gate-g2",
        Some(rewritten_head),
        '8',
    );
    let g2_commit = publish(
        &registered,
        temp.path(),
        &mut authority,
        &g2_record.publication.key,
    );
    drop(g2_commit);
    assert!(receipt_for_generation(temp.path(), "gate-g1").is_some());

    assert!(
        matches!(
            registered
                .registry
                .retire_one_code_generation_replay(
                    registration(registered.binding.clone(), temp.path()),
                    &mut authority,
                    &context,
                    &sealed_generation,
                    &sealed_digest,
                )
                .unwrap(),
            GraphReplayCollectionOutcome::Retained
        ),
        "a live direct-sealed reader must retain its generation"
    );
    assert!(
        receipt_for_generation(temp.path(), "gate-g1").is_some(),
        "the sealed artifact must survive while the direct-sealed reader lives"
    );

    drop(reader);
    assert!(matches!(
        registered
            .registry
            .retire_one_code_generation_replay(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &sealed_generation,
                &sealed_digest,
            )
            .unwrap(),
        GraphReplayCollectionOutcome::Retired(_)
    ));
    assert!(
        receipt_for_generation(temp.path(), "gate-g1").is_none(),
        "retirement must delete the artifact once the reader drops"
    );
}

/// A generation shaped like a code graph: `symbol:<digest>` entities chained
/// by `edge:<digest>` relations, the identities `graph_stable_identity` mints.
fn stable_identity_manifest(
    projection_identity: GraphProjectionIdentity,
    rows: usize,
) -> GraphGenerationManifest {
    let symbol = |index: usize| {
        GraphEntityId::new(graph_stable_identity("symbol", &index.to_string())).unwrap()
    };
    let entities = (0..rows)
        .map(|index| GraphEntity::new(symbol(index), BTreeSet::new(), BTreeMap::new()).unwrap())
        .collect();
    let relations = (0..rows - 1)
        .map(|index| {
            GraphGenerationRelation::new(
                GraphRelationId::new(graph_stable_identity("edge", &index.to_string())).unwrap(),
                GraphEntityRef::new(projection_identity.clone(), symbol(index)),
                GraphEntityRef::new(projection_identity.clone(), symbol(index + 1)),
                GraphRelationKind::new("calls").unwrap(),
                BTreeMap::new(),
            )
            .unwrap()
        })
        .collect();
    GraphGenerationManifest::new(
        projection_identity,
        GraphGenerationId::new("key-budget-g1").unwrap(),
        SourceGeneration::new("source:key-budget-g1").unwrap(),
        GraphWatermark::new("watermark:key-budget-g1").unwrap(),
        Vec::new(),
        entities,
        relations,
    )
    .unwrap()
}

/// Keys and relation identities are the bulk of a sealed generation's
/// bytes. Hex keys sealed this 2,000-symbol generation to 3,150,837 bytes,
/// and binary keys, which the compact dictionary stores as marked hex, to
/// 1,786,870. Base64url keys plus each relation identity, source, and target
/// stored once, on its locator, in compact form seal it to 1,037,302, and
/// the rows still resolve through keys and edges.
#[test]
fn sealed_generation_bytes_stay_within_the_compact_identity_budget() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:key-budget", "code");
    let manifest = stable_identity_manifest(identity.clone(), 2_000);
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:key-budget-g1",
        None,
        '0',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    assert!(commit.snapshot.serves_from_sealed_store());

    let sealed_bytes = directory_bytes(&sealed_store_root(temp.path()));
    assert!(
        sealed_bytes <= 1_150_000,
        "sealed generation took {sealed_bytes} bytes"
    );
    let last = GraphEntityId::new(graph_stable_identity("symbol", "1999")).unwrap();
    assert_eq!(
        commit
            .snapshot
            .entity(
                &GraphEntityRef::new(identity.clone(), last.clone()),
                Arc::new(TestCancellation),
            )
            .unwrap(),
        Some(GraphEntity::new(last, BTreeSet::new(), BTreeMap::new()).unwrap())
    );
    let relation = commit
        .snapshot
        .relation(
            &GraphRelationRef::new(
                identity,
                GraphRelationId::new(graph_stable_identity("edge", "0")).unwrap(),
            ),
            Arc::new(TestCancellation),
        )
        .unwrap()
        .expect("sealed relation must resolve through its binary key");
    assert_eq!(
        relation.to.identity.as_str(),
        graph_stable_identity("symbol", "1")
    );
}

// ---------------------------------------------------------------------------
// At-rest measurement probe (ignored): sealed artifact open vs staging replay
// ---------------------------------------------------------------------------

fn probe_status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let prefix = format!("{field}:");
    status
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

fn probe_mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    };
    entries
        .map(Result::unwrap)
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                directory_bytes(&path)
            } else {
                std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn probe_entity_id(index: usize) -> GraphEntityId {
    GraphEntityId::new(format!("entity-{index:08}")).unwrap()
}

fn probe_manifest(
    projection_identity: GraphProjectionIdentity,
    rows: usize,
) -> GraphGenerationManifest {
    let entities = (0..rows)
        .map(|index| {
            GraphEntity::new(
                probe_entity_id(index),
                BTreeSet::new(),
                BTreeMap::from([(
                    GraphPropertyName::new("name").unwrap(),
                    GraphProperty::String(format!("symbol-{index:08}")),
                )]),
            )
            .unwrap()
        })
        .collect();
    let relations = (0..rows / 4)
        .map(|index| {
            GraphGenerationRelation::new(
                GraphRelationId::new(format!("relation-{index:08}")).unwrap(),
                GraphEntityRef::new(projection_identity.clone(), probe_entity_id(index)),
                GraphEntityRef::new(projection_identity.clone(), probe_entity_id(index + 1)),
                GraphRelationKind::new("calls").unwrap(),
                BTreeMap::new(),
            )
            .unwrap()
        })
        .collect();
    GraphGenerationManifest::new(
        projection_identity,
        GraphGenerationId::new("probe-g1").unwrap(),
        SourceGeneration::new("source:probe-g1").unwrap(),
        GraphWatermark::new("watermark:probe-g1").unwrap(),
        Vec::new(),
        entities,
        relations,
    )
    .unwrap()
}

/// Point reads and a bounded traversal against a raw graph handle in the
/// generation's physical namespace.
fn probe_reads(
    db: &tracedecay_graph_db::GraphDb,
    physical_namespace: &GraphNamespace,
    rows: usize,
) -> (Duration, usize, Duration, usize) {
    let stride = (rows / 64).max(1);
    let started = std::time::Instant::now();
    let mut hits = 0usize;
    for step in 0..64 {
        let index = (step * stride) % rows;
        if db
            .entity(
                physical_namespace,
                &probe_entity_id(index),
                Arc::new(TestCancellation),
            )
            .unwrap()
            .is_some()
        {
            hits += 1;
        }
    }
    let point_wall = started.elapsed();

    let started = std::time::Instant::now();
    let traversal = db
        .traverse(TraversalRequest {
            namespace: physical_namespace.clone(),
            start: probe_entity_id(0),
            relation_kinds: BTreeSet::from([GraphRelationKind::new("calls").unwrap()]),
            direction: GraphTraversalDirection::Outgoing,
            max_depth: 8,
            max_visits: 4096,
            max_results: 4096,
            cancellation: Arc::new(TestCancellation),
        })
        .unwrap();
    (point_wall, hits, started.elapsed(), traversal.visits.len())
}

/// Measurement harness, not a contract: seal one verified generation through
/// the production publish path, then compare activating it through the
/// staging database's full replay open against opening its sealed compact
/// artifact directly. One process; live-RSS deltas are reported per phase
/// (VmHWM is process-wide and stays polluted by the staging build).
///
/// ```text
/// TRACEDECAY_SEALED_PROBE_ROWS=500000 \
///   cargo test -p tracedecay-graph-db --features test-helpers --profile perf \
///   --test graph_db_suite -- --ignored --nocapture \
///   verified_generation_contract::sealed_store::sealed_artifact_open_probe
/// ```
#[test]
#[ignore = "at-rest measurement harness; see doc comment"]
fn sealed_artifact_open_probe() {
    let rows = std::env::var("TRACEDECAY_SEALED_PROBE_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(500_000usize);
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:probe", "code");

    // Production-shaped journaling: a generation of this size rides as a
    // sealed code generation replay (source reference in the journal, rows
    // supplied by the publication owner), exactly like the code-index
    // publisher.
    let manifest = probe_manifest(identity.clone(), rows);
    let record = authority.stage(
        manifest
            .relational_sealed_replay(
                registered.binding.shard_id.clone(),
                GraphIdempotencyKey::new("publish:probe-g1").unwrap(),
                digest('9'),
                None,
                SealedCodeGenerationReplay {
                    repository: RepositoryId::new("repository.sealed-probe").unwrap(),
                    generation: CodeGenerationId::new("code-generation.sealed-probe").unwrap(),
                    sealed_state_digest: SealedGraphStateDigest::try_from(format!(
                        "sha256:{}",
                        "5".repeat(64)
                    ))
                    .unwrap(),
                    projector_revision: GraphProjectorRevision::try_from(
                        "projector.sealed-probe".to_owned(),
                    )
                    .unwrap(),
                },
                &|| Ok(()),
            )
            .unwrap(),
    );

    let seal_started = std::time::Instant::now();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    // The shared fixture registration carries a 30s deadline; a 500k-row seal
    // legitimately outlives it, so the probe extends its own.
    let mut probe_registration = registration(registered.binding.clone(), temp.path());
    probe_registration.deadline = std::time::Instant::now() + Duration::from_secs(3_600);
    let commit = registered
        .registry
        .publish_verified(
            probe_registration,
            &mut authority,
            &context,
            &record.publication.key,
            Some(Arc::new(manifest).into()),
        )
        .unwrap();
    let seal_wall = seal_started.elapsed();
    assert!(commit.snapshot.serves_from_sealed_store());
    let receipt = receipt_for_generation(temp.path(), "probe-g1").unwrap();
    let physical_namespace = receipt
        .split("\"physical_namespace\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .map(|value| GraphNamespace::new(value).unwrap())
        .unwrap();
    let form = receipt
        .split("\"form\": \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();
    drop(commit);
    assert!(registered.close().unwrap());

    let staging_bytes = std::fs::metadata(support::graph_path(temp.path()))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let sealed_root = sealed_store_root(temp.path());
    let artifact_dir = std::fs::read_dir(&sealed_root)
        .unwrap()
        .map(Result::unwrap)
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .unwrap();
    let artifact_bytes = directory_bytes(&artifact_dir);

    // ---- activation via the staging database (full replay open) ----
    let rss_before = probe_status_kib("VmRSS").unwrap();
    let open_started = std::time::Instant::now();
    let staging_lease = registered.reopen_lease().unwrap();
    let staging_open_wall = open_started.elapsed();
    let rss_after_staging_open = probe_status_kib("VmRSS").unwrap();
    let (staging_points, staging_hits, staging_traversal, staging_visits) =
        probe_reads(&staging_lease, &physical_namespace, rows);
    drop(staging_lease);
    assert!(registered.close().unwrap());

    // ---- activation via the sealed artifact ----
    let rss_before_sealed = probe_status_kib("VmRSS").unwrap();
    let open_started = std::time::Instant::now();
    let sealed =
        tracedecay_graph_db::GraphDb::open_sealed_artifact_for_bench(&artifact_dir).unwrap();
    let sealed_open_wall = open_started.elapsed();
    let rss_after_sealed_open = probe_status_kib("VmRSS").unwrap();
    let (sealed_points, sealed_hits, sealed_traversal, sealed_visits) =
        probe_reads(&sealed, &physical_namespace, rows);

    println!("=== sealed artifact open probe ===");
    println!(
        "rows                 : {rows} entities + {} relations",
        rows / 4
    );
    println!("artifact form        : {form}");
    println!(
        "seal wall (publish)  : {:.2}s  <- stage+verify+close/reopen+artifact build",
        seal_wall.as_secs_f64()
    );
    println!(
        "staging store        : {staging_bytes} bytes ({:.1} MiB)",
        staging_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "sealed artifact      : {artifact_bytes} bytes ({:.1} MiB)",
        artifact_bytes as f64 / (1024.0 * 1024.0)
    );
    println!("--- staging replay activation ---");
    println!(
        "open wall            : {:.3}s",
        staging_open_wall.as_secs_f64()
    );
    println!(
        "VmRSS delta          : {} KiB ({:.1} MiB) [{} -> {}]",
        rss_after_staging_open.saturating_sub(rss_before),
        probe_mib(rss_after_staging_open.saturating_sub(rss_before)),
        rss_before,
        rss_after_staging_open
    );
    println!(
        "point reads          : 64 in {:.3}ms, {} hits",
        staging_points.as_secs_f64() * 1000.0,
        staging_hits
    );
    println!(
        "traversal            : depth 8 in {:.3}ms, {} visits",
        staging_traversal.as_secs_f64() * 1000.0,
        staging_visits
    );
    println!("--- sealed artifact activation ---");
    println!(
        "open wall            : {:.3}s",
        sealed_open_wall.as_secs_f64()
    );
    println!(
        "VmRSS delta          : {} KiB ({:.1} MiB) [{} -> {}]",
        rss_after_sealed_open.saturating_sub(rss_before_sealed),
        probe_mib(rss_after_sealed_open.saturating_sub(rss_before_sealed)),
        rss_before_sealed,
        rss_after_sealed_open
    );
    println!(
        "point reads          : 64 in {:.3}ms, {} hits",
        sealed_points.as_secs_f64() * 1000.0,
        sealed_hits
    );
    println!(
        "traversal            : depth 8 in {:.3}ms, {} visits",
        sealed_traversal.as_secs_f64() * 1000.0,
        sealed_visits
    );

    // Correctness gates: an open that cannot answer reads is not a faster
    // open, it is a broken one.
    assert_eq!(staging_hits, 64);
    assert_eq!(sealed_hits, 64);
    assert!(staging_visits > 1);
    assert_eq!(sealed_visits, staging_visits);
}

/// A generation wide enough that its rows span many spill batches: 300
/// entities with distinct labels and payloads, and 450 relations whose
/// endpoints cross every batch boundary.
fn spill_fixture_manifest(identity: GraphProjectionIdentity) -> GraphGenerationManifest {
    let entities = (0..300)
        .map(|index| {
            GraphEntity::new(
                GraphEntityId::new(format!("entity:{index:03}")).unwrap(),
                BTreeSet::from([
                    GraphLabel::new(if index % 3 == 0 { "File" } else { "Symbol" }).unwrap(),
                ]),
                BTreeMap::from([(
                    GraphPropertyName::new("marker").unwrap(),
                    GraphProperty::String(format!("payload-{}", index * 7919 % 1000)),
                )]),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let relations = (0..450)
        .map(|index| {
            GraphGenerationRelation::new(
                GraphRelationId::new(format!("relation:{index:03}")).unwrap(),
                GraphEntityRef::new(
                    identity.clone(),
                    GraphEntityId::new(format!("entity:{:03}", index % 300)).unwrap(),
                ),
                GraphEntityRef::new(
                    identity.clone(),
                    GraphEntityId::new(format!("entity:{:03}", (index * 37 + 11) % 300)).unwrap(),
                ),
                GraphRelationKind::new(if index % 2 == 0 { "calls" } else { "uses" }).unwrap(),
                BTreeMap::new(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    GraphGenerationManifest::new(
        identity,
        GraphGenerationId::new("spill-g1").unwrap(),
        SourceGeneration::new("source:spill-g1").unwrap(),
        GraphWatermark::new("watermark:spill-g1").unwrap(),
        Vec::new(),
        entities,
        relations,
    )
    .unwrap()
}

/// The manifest's rows pushed in three batches, in reverse and interleaved
/// order, with one entity and one relation repeated across batches.
fn spill_manifest_rows(
    spill: &mut GraphGenerationRowSpill,
    manifest: &GraphGenerationManifest,
) -> Result<(), GraphDbError> {
    let mut entities = manifest.entities.clone();
    entities.reverse();
    let mut relations = manifest.relations.clone();
    relations.reverse();
    let (first_entities, rest_entities) = entities.split_at(120);
    let (first_relations, rest_relations) = relations.split_at(200);
    spill.push_batch(first_entities.to_vec(), rest_relations.to_vec(), &|| Ok(()))?;
    let mut repeated_entities = rest_entities.to_vec();
    repeated_entities.push(first_entities[7].clone());
    spill.push_batch(repeated_entities, Vec::new(), &|| Ok(()))?;
    let mut repeated_relations = first_relations.to_vec();
    repeated_relations.push(rest_relations[3].clone());
    spill.push_batch(Vec::new(), repeated_relations, &|| Ok(()))
}

/// The sealed container's bytes past its headers. The file header and the
/// two checkpoint headers (the first 12,288 bytes) carry the wall-clock time
/// the container was written; every section after them is a pure function of
/// the rows.
fn sealed_container_sections(root: &Path) -> (u64, Vec<u8>) {
    const CONTAINER_DATA_OFFSET: usize = 3 * 4096;
    let entries = std::fs::read_dir(sealed_store_root(root))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.path().join("generation.grafeo").is_file())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "exactly one sealed generation");
    let bytes = std::fs::read(entries[0].path().join("generation.grafeo")).unwrap();
    (bytes.len() as u64, bytes[CONTAINER_DATA_OFFSET..].to_vec())
}

fn row_spills(root: &Path) -> Vec<String> {
    std::fs::read_dir(sealed_store_root(root))
        .map(|entries| {
            entries
                .map(Result::unwrap)
                .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                .filter(|name| name.starts_with(".rows-"))
                .collect()
        })
        .unwrap_or_default()
}

/// Rows pushed through a spill in shuffled batches, repeats included, seal
/// the same recovered digest, receipt, and container sections as the
/// manifest holding the same rows, and the spill leaves no scratch behind.
///
/// Fails if the merge drops or duplicates a row (the receipt counts and the
/// digest move), if relation endpoints resolve to the wrong sealed node (the
/// container sections differ), or if the spill directory outlives
/// publication.
#[test]
fn spilled_rows_seal_the_same_generation_as_their_manifest() {
    let identity = projection("sealed-store:spill", "code");
    let manifest = spill_fixture_manifest(identity.clone());

    let from_manifest = TempDir::new().unwrap();
    let manifest_graph = RegisteredGraph::new_mounted(from_manifest.path()).unwrap();
    let mut manifest_authority = RelationalAuthority::default();
    let record = stage_sealed_manifest(
        &mut manifest_authority,
        &manifest_graph.binding,
        &manifest,
        "publish:spill-g1",
        None,
        '5',
    );
    publish_sealed(
        &manifest_graph,
        from_manifest.path(),
        &mut manifest_authority,
        &record,
        &manifest,
    );

    let from_spill = TempDir::new().unwrap();
    let spill_graph = RegisteredGraph::new_mounted(from_spill.path()).unwrap();
    let mut spill = spill_graph
        .registry
        .generation_row_spill(
            registration(spill_graph.binding.clone(), from_spill.path()),
            identity.clone(),
        )
        .unwrap();
    assert_eq!(row_spills(from_spill.path()).len(), 1);
    spill_manifest_rows(&mut spill, &manifest).unwrap();
    assert_eq!(spill.distinct_entities(), 300);
    let spilled = spill.finish(manifest.identity(), &|| Ok(())).unwrap();
    assert_eq!(spilled.row_counts(), (300, 450));
    assert_eq!(
        spilled.expected_recovered_digest(),
        &manifest.expected_recovered_digest(&|| Ok(())).unwrap()
    );
    let rows = GraphGenerationRows::from(spilled);
    let mut spill_authority = RelationalAuthority::default();
    let source = SealedCodeGenerationReplay {
        repository: RepositoryId::new("repository.graph-staging-release").unwrap(),
        generation: CodeGenerationId::new("code-generation.spill-g1").unwrap(),
        sealed_state_digest: SealedGraphStateDigest::try_from(format!("sha256:{}", "5".repeat(64)))
            .unwrap(),
        projector_revision: GraphProjectorRevision::try_from(
            "projector.graph-staging-release".to_owned(),
        )
        .unwrap(),
    };
    let spill_record = spill_authority.stage(
        rows.relational_sealed_replay(
            spill_graph.binding.shard_id.clone(),
            GraphIdempotencyKey::new("publish:spill-g1").unwrap(),
            digest('5'),
            None,
            source,
            &|| Ok(()),
        )
        .unwrap(),
    );
    assert_eq!(
        spill_record.publication.canonical_replay_source,
        record.publication.canonical_replay_source
    );
    assert_eq!(
        spill_record.publication.expected_recovered_digest,
        record.publication.expected_recovered_digest
    );
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    spill_graph
        .registry
        .publish_verified(
            registration(spill_graph.binding.clone(), from_spill.path()),
            &mut spill_authority,
            &context,
            &spill_record.publication.key,
            Some(rows),
        )
        .unwrap();

    assert_eq!(
        receipt_for_generation(from_spill.path(), "spill-g1"),
        receipt_for_generation(from_manifest.path(), "spill-g1"),
    );
    assert!(
        receipt_for_generation(from_spill.path(), "spill-g1")
            .unwrap()
            .contains("\"relations\": 450")
    );
    assert_eq!(
        sealed_container_sections(from_spill.path()),
        sealed_container_sections(from_manifest.path())
    );
    assert_eq!(row_spills(from_spill.path()), Vec::<String>::new());
}

/// Two rows that share an identity with different content, or a relation
/// whose endpoint no batch pushed, refuse the spilled generation typed, the
/// same verdicts the manifest constructor gives the same rows.
#[test]
fn spilled_rows_refuse_conflicting_repeats_and_dangling_endpoints() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let identity = projection("sealed-store:spill-refusal", "code");
    let manifest = spill_fixture_manifest(identity.clone());
    let new_spill = || {
        registered
            .registry
            .generation_row_spill(
                registration(registered.binding.clone(), temp.path()),
                identity.clone(),
            )
            .unwrap()
    };

    let mut conflicting = new_spill();
    spill_manifest_rows(&mut conflicting, &manifest).unwrap();
    conflicting
        .push_batch(
            vec![entity("entity:042", "a different payload")],
            Vec::new(),
            &|| Ok(()),
        )
        .unwrap();
    let refused = conflicting
        .finish(manifest.identity(), &|| Ok(()))
        .unwrap_err();
    assert_eq!(
        refused,
        GraphDbError::invalid("a graph generation repeats an entity or relation identity")
    );

    let mut dangling = new_spill();
    spill_manifest_rows(&mut dangling, &manifest).unwrap();
    dangling
        .push_batch(
            Vec::new(),
            vec![
                GraphGenerationRelation::new(
                    GraphRelationId::new("relation:dangling").unwrap(),
                    GraphEntityRef::new(
                        identity.clone(),
                        GraphEntityId::new("entity:000").unwrap(),
                    ),
                    GraphEntityRef::new(
                        identity.clone(),
                        GraphEntityId::new("entity:999").unwrap(),
                    ),
                    GraphRelationKind::new("calls").unwrap(),
                    BTreeMap::new(),
                )
                .unwrap(),
            ],
            &|| Ok(()),
        )
        .unwrap();
    let refused = dangling
        .finish(manifest.identity(), &|| Ok(()))
        .unwrap_err();
    assert_eq!(
        refused,
        GraphDbError::invalid(
            "local relation endpoint `entity:999` is absent from the candidate generation"
        )
    );

    let mut exact = new_spill();
    spill_manifest_rows(&mut exact, &manifest).unwrap();
    assert_eq!(
        exact
            .finish(manifest.identity(), &|| Ok(()))
            .unwrap()
            .row_counts(),
        (300, 450)
    );
    assert_eq!(row_spills(temp.path()), Vec::<String>::new());
}

/// A row spill a killed process left under the sealed root is removed the
/// next time the store opens; a spill named for the opening process is its
/// own live publisher's and stays.
#[test]
fn store_open_sweeps_row_spills_abandoned_by_another_process() {
    let temp = TempDir::new().unwrap();
    let root = sealed_store_root(temp.path());
    std::fs::create_dir_all(root.join(".rows-4194305-0")).unwrap();
    std::fs::write(root.join(".rows-4194305-0/entities-0.run"), b"abandoned").unwrap();
    let own = format!(".rows-{}-999999", std::process::id());
    std::fs::create_dir_all(root.join(&own)).unwrap();

    let _registered = RegisteredGraph::new_mounted(temp.path()).unwrap();

    assert_eq!(row_spills(temp.path()), vec![own]);
}

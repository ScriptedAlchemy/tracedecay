//! Sealed per-generation compact store contract: seal builds an isolated,
//! digest-proven store; reads serve from it while the next generation stages
//! and seals; recovery adopts it from disk; retirement deletes it; and a
//! post-seal conflicting restage receives the typed immutable refusal.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tracedecay_graph_db::{GraphTraversalDirection, GraphVector, TraversalRequest, VectorMetric};

use super::*;

fn sealed_store_root(root: &Path) -> PathBuf {
    support::graph_path(root).with_extension("sealed")
}

/// Every sealed receipt currently on disk, as raw JSON strings.
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
            Some(Arc::new(manifest.clone())),
        )
        .unwrap()
}

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
            Some(Arc::new(manifest.clone())),
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

/// Small generations carrying Bytes properties stay in replay form and still
/// read exactly. Eager compact construction is reserved for generations above
/// the measured size threshold where its publication cost can be amortized.
#[test]
fn small_bytes_rows_seal_in_replay_form_and_read_exactly() {
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
        receipt.contains("\"form\": \"replay\""),
        "small Bytes generations must not pay eager compact construction: {receipt}"
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

/// Rows carrying Vector properties seal in replay form: the sealed lane never
/// serves vector search, so compacting these rows buys nothing, and
/// mixed-dimension vectors still fall back to a lossy display dictionary.
#[test]
fn vector_rows_seal_in_replay_form_and_read_exactly() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:vectors", "code");

    let mut g1 = rich_manifest(identity.clone(), "vectors-g1", "payload");
    let vector = GraphVector::new(vec![0.25_f32, -0.5, 0.75], 3, VectorMetric::Cosine).unwrap();
    for entity in &mut g1.entities {
        entity.properties.insert(
            GraphPropertyName::new("embedding").unwrap(),
            GraphProperty::Vector(vector.clone()),
        );
    }
    let record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:vectors-g1",
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
    let receipt = receipt_for_generation(temp.path(), "vectors-g1")
        .expect("seal must write the artifact receipt");
    assert!(
        receipt.contains("\"form\": \"replay\""),
        "vector rows must seal in replay form on the pinned engine: {receipt}"
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
            .get(&GraphPropertyName::new("embedding").unwrap()),
        Some(&GraphProperty::Vector(vector)),
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
    // authority is rolled back to its pre-publication state — the restored-
    // from-backup divergence that used to reach sealed rows as a stage-page
    // write — so the store itself is the last line refusing the rewrite.
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
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        registered
            .registry
            .release_sealed_generation_staging_rows(
                registration(registered.binding.clone(), temp.path()),
                &mut authority,
                &context,
                &g1_record.publication.key.projection,
            )
            .unwrap(),
        SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        }
    );
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

    // The staging runtime comes back while the direct-sealed reader is live —
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
///   cargo test -p tracedecay-graph-db --features test-helpers,graph-sealed-store \
///   --profile perf --test verified_generation_contract -- --ignored --nocapture \
///   sealed_store::sealed_artifact_open_probe
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
            Some(Arc::new(manifest)),
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

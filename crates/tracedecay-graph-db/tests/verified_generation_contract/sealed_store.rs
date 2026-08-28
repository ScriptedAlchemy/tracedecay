//! Sealed per-generation compact store contract: seal builds an isolated,
//! digest-proven store; reads serve from it while the next generation stages
//! and seals; recovery adopts it from disk; retirement deletes it; and a
//! post-seal conflicting restage receives the typed immutable refusal.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tracedecay_graph_db::{GraphTraversalDirection, TraversalRequest};

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
        entity.properties.get(&GraphPropertyName::new("marker").unwrap()),
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

/// Rows carrying Bytes properties seal in replay form (the pinned engine's
/// columnar Dict codec does not round-trip Bytes) and still read exactly.
#[test]
fn bytes_rows_seal_in_replay_form_and_read_exactly() {
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
        "Bytes rows must seal in replay form on the pinned engine: {receipt}"
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
        entity.properties.get(&GraphPropertyName::new("record").unwrap()),
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

/// Retiring a sealed code generation deletes its artifact directory while the
/// successor's artifact stays.
#[test]
fn retirement_deletes_the_superseded_sealed_artifact() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("sealed-store:retire", "code");
    let sealed_generation = CodeGenerationId::new("code-generation.sealed-retire").unwrap();
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "6".repeat(64))).unwrap();

    let g1 = rich_manifest(identity.clone(), "retire-g1", "old");
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:retire-g1",
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
    // Rewrite the journal row as a sealed code generation replay so the
    // production retirement path owns it.
    let sealed_publication = g1
        .relational_sealed_replay(
            registered.binding.shard_id.clone(),
            GraphIdempotencyKey::new("publish:retire-g1").unwrap(),
            digest('7'),
            None,
            SealedCodeGenerationReplay {
                repository: RepositoryId::new("repository.sealed-retire").unwrap(),
                generation: sealed_generation.clone(),
                sealed_state_digest: sealed_digest.clone(),
                projector_revision: GraphProjectorRevision::try_from(
                    "projector.sealed-retire".to_owned(),
                )
                .unwrap(),
            },
            &|| Ok(()),
        )
        .unwrap();
    authority.records.insert(
        g1_record.publication.key.clone(),
        GraphPublicationReplayRecordV1::new(g1_record.sequence, sealed_publication).unwrap(),
    );

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
}

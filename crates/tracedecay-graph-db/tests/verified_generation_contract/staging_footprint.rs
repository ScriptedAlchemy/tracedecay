//! Staging-row release, remount, and hibernated-retention contract.
//!
//! Each test names the production behaviour it pins; removing that behaviour
//! fails the assertion called out in the test's doc comment.

use std::path::{Path, PathBuf};

use tracedecay_graph_db::{
    GraphGenerationManifestProvider, GraphTraversalDirection, TraversalRequest,
};

use super::*;

fn sealed_store_root(root: &Path) -> PathBuf {
    support::graph_path(root).with_extension("sealed")
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

fn assert_projection_page_and_telemetry(
    snapshot: &tracedecay_graph_db::VerifiedGraphSnapshot,
    identity: &GraphProjectionIdentity,
) {
    let page = snapshot
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
    let telemetry = snapshot
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: identity.namespace.clone(),
            projection: identity.projection.clone(),
            cancellation: Arc::new(TestCancellation),
        })
        .unwrap()
        .expect("sealed projection telemetry must resolve");
    assert_eq!(telemetry.entity_count, 2);
    assert_eq!(telemetry.relation_count, 1);
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
        repository: RepositoryId::new("repository.graph-staging-footprint").unwrap(),
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
            "projector.graph-staging-footprint".to_owned(),
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

fn release_sealed_head(
    registered: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    projection: &tracedecay_store::GraphProjectionIdentityV1,
) -> SealedStagingRelease {
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    registered
        .registry
        .release_sealed_generation_staging_rows(
            registration(registered.binding.clone(), root),
            authority,
            &context,
            projection,
        )
        .unwrap()
}

fn retire_code_generation(
    registered: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    generation: &CodeGenerationId,
    digest: &SealedGraphStateDigest,
) -> GraphReplayCollectionOutcome {
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    registered
        .registry
        .retire_one_code_generation_replay(
            registration(registered.binding.clone(), root),
            authority,
            &context,
            generation,
            digest,
        )
        .unwrap()
}

fn recover_head(
    registered: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    projection: &tracedecay_store::GraphProjectionIdentityV1,
) -> tracedecay_graph_db::VerifiedGraphSnapshot {
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), root),
            authority,
            &context,
            projection,
        )
        .unwrap()
}

struct RemountSealedProvider {
    manifest: GraphGenerationManifest,
}

impl GraphGenerationManifestProvider for RemountSealedProvider {
    fn hydrate_sealed_code_generation(
        &self,
        _owner: &tracedecay_store::GraphProjectionIdentityV1,
        _source: &SealedCodeGenerationReplay,
        _check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphGenerationManifest, GraphDbError> {
        Ok(self.manifest.clone())
    }
}

fn probe_lease(registered: &RegisteredGraph, root: &Path) -> tracedecay_graph_db::GraphDbLeaseV1 {
    registered
        .registry
        .resolve(registration(registered.binding.clone(), root))
        .unwrap()
}

fn assert_engine_hibernated(registered: &RegisteredGraph, root: &Path) {
    let probe = probe_lease(registered, root);
    // Fails if retention / resolve reintroduced a lazy `ensure_opened` /
    // `read_guard` on this hibernated engine (the 11+ minute production open).
    assert!(
        !probe.staging_engine_is_open(),
        "the staging engine must stay hibernated"
    );
    drop(probe);
}

struct ReleasedGeneration {
    identity: GraphProjectionIdentity,
    generation: String,
    marker: String,
    manifest: GraphGenerationManifest,
    code_generation: CodeGenerationId,
    sealed_digest: SealedGraphStateDigest,
    snapshot: tracedecay_graph_db::VerifiedGraphSnapshot,
}

fn publish_and_release(
    registered: &RegisteredGraph,
    root: &Path,
    authority: &mut RelationalAuthority,
    database: &tracedecay_graph_db::GraphDbLeaseV1,
    n: usize,
    already_released: &[&ReleasedGeneration],
) -> ReleasedGeneration {
    let generation = format!("g{n}");
    let marker = format!("marker-{n}");
    let identity = projection(&format!("code:gen-{n}"), "code");
    let manifest = rich_manifest(identity.clone(), &generation, &marker);
    let input = char::from_digit(n as u32, 10).expect("generation index is a decimal digit");
    let record = stage_sealed_manifest(
        authority,
        &registered.binding,
        &manifest,
        &format!("publish:{generation}"),
        None,
        input,
    );
    let commit = publish_sealed(registered, root, authority, &record, &manifest);
    assert_snapshot_reads(&commit.snapshot, &identity, &marker);
    assert_projection_page_and_telemetry(&commit.snapshot, &identity);

    assert_eq!(
        release_sealed_head(
            registered,
            root,
            authority,
            &record.publication.key.projection
        ),
        SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        }
    );
    // Fails if `release_sealed_generation_staging_rows` stops deleting the
    // seated generation's staging rows (the slice-2 footprint contract).
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (0, 0)
    );
    // Fails if release mutates serveable content; the sealed store must keep
    // answering the pre-release entity / relation / telemetry shape.
    assert_snapshot_reads(&commit.snapshot, &identity, &marker);
    assert_projection_page_and_telemetry(&commit.snapshot, &identity);
    for prior in already_released {
        assert_eq!(
            database
                .staging_generation_row_counts(&prior.manifest.identity())
                .unwrap(),
            (0, 0)
        );
        assert_snapshot_reads(&prior.snapshot, &prior.identity, &prior.marker);
        assert_projection_page_and_telemetry(&prior.snapshot, &prior.identity);
    }

    ReleasedGeneration {
        identity,
        generation,
        marker,
        manifest,
        code_generation: CodeGenerationId::new(format!("code-generation.g{n}")).unwrap(),
        sealed_digest: SealedGraphStateDigest::try_from(format!(
            "sha256:{}",
            input.to_string().repeat(64)
        ))
        .unwrap(),
        snapshot: commit.snapshot,
    }
}

/// Staging holds rows only for generations still in flight; every seated
/// sealed head releases to `(0, 0)` and keeps serving, and retiring g1
/// deletes only that sealed artifact.
///
/// Fails if release stops clearing staging rows, if post-release reads
/// diverge, or if `retire_one_code_generation_replay` leaves g1's sealed
/// directory (or stops serving g2/g3).
#[test]
fn staging_holds_rows_for_at_most_active_and_in_flight_generations() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let database = probe_lease(&registered, temp.path());

    let g1 = publish_and_release(&registered, temp.path(), &mut authority, &database, 1, &[]);
    let g2 = publish_and_release(
        &registered,
        temp.path(),
        &mut authority,
        &database,
        2,
        &[&g1],
    );
    let g3 = publish_and_release(
        &registered,
        temp.path(),
        &mut authority,
        &database,
        3,
        &[&g1, &g2],
    );

    let g1_generation = g1.generation.clone();
    let g1_code = g1.code_generation.clone();
    let g1_digest = g1.sealed_digest.clone();
    let g1_identity = g1.manifest.identity();
    drop(g1);

    assert!(matches!(
        retire_code_generation(
            &registered,
            temp.path(),
            &mut authority,
            &g1_code,
            &g1_digest,
        ),
        GraphReplayCollectionOutcome::Retired(_)
    ));
    // Fails if retirement no longer deletes the superseded sealed artifact.
    assert!(
        receipt_for_generation(temp.path(), &g1_generation).is_none(),
        "retirement must delete g1's sealed directory"
    );
    assert!(
        !database.is_generation_sealed_only(&g1_identity).unwrap(),
        "retirement must clear sealed-only state for the deleted generation"
    );
    assert!(receipt_for_generation(temp.path(), &g2.generation).is_some());
    assert!(receipt_for_generation(temp.path(), &g3.generation).is_some());
    assert_snapshot_reads(&g2.snapshot, &g2.identity, &g2.marker);
    assert_projection_page_and_telemetry(&g2.snapshot, &g2.identity);
    assert_snapshot_reads(&g3.snapshot, &g3.identity, &g3.marker);
    assert_projection_page_and_telemetry(&g3.snapshot, &g3.identity);
}

/// A remount after release must adopt the sealed store and must not re-stage
/// rows into the shared container.
///
/// Fails if `recover_verified_snapshot` rebuilds staging rows, if remounted
/// reads diverge, or if `is_generation_sealed_only` is no longer true.
#[test]
fn remount_after_release_serves_from_the_sealed_store() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("code:gen-remount", "code");
    let manifest = rich_manifest(identity.clone(), "remount-g1", "remounted");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:remount-g1",
        None,
        '1',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    assert_eq!(
        release_sealed_head(
            &registered,
            temp.path(),
            &mut authority,
            &record.publication.key.projection,
        ),
        SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        }
    );
    assert_snapshot_reads(&commit.snapshot, &identity, "remounted");
    assert_projection_page_and_telemetry(&commit.snapshot, &identity);
    drop(commit);
    assert!(registered.close().unwrap());
    drop(registered);

    // Production remounts carry the code-index sealed-replay provider so
    // recover can decode the journaled identity and then adopt the on-disk
    // sealed store. The default test registry is inline-only.
    let registered = RegisteredGraph::new_mounted_with_manifest_provider(
        temp.path(),
        Arc::new(RemountSealedProvider {
            manifest: manifest.clone(),
        }),
    )
    .unwrap();
    let snapshot = recover_head(
        &registered,
        temp.path(),
        &mut authority,
        &record.publication.key.projection,
    );
    assert!(
        snapshot.serves_from_sealed_store(),
        "remount recovery must adopt the sealed artifact"
    );
    assert_snapshot_reads(&snapshot, &identity, "remounted");
    assert_projection_page_and_telemetry(&snapshot, &identity);
    let database = probe_lease(&registered, temp.path());
    // Fails if recovery restages the released generation into the container.
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (0, 0)
    );
    // Fails if recover treats a leftover staging projection commit as live
    // rows and skips `remember_sealed_only_generation` (durable sealed-only
    // is "no staging rows + matching sealed artifact").
    assert!(
        database
            .is_generation_sealed_only(&manifest.identity())
            .unwrap(),
        "remount recovery must keep the generation sealed-only"
    );
}

/// Retirement of a deleted code generation must not lazily open a hibernated
/// staging engine; it returns `RetentionPending` and retries once a later
/// lease has opened the engine.
///
/// Fails if `retire_one_code_generation_replay` calls `read_guard` /
/// `ensure_opened` on the hibernated engine (`staging_engine_is_open` becomes
/// true) or if the deferred outcome is no longer `RetentionPending`.
#[test]
fn hibernated_engine_defers_retirement_without_opening() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted_lazy(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("code:gen-hibernate-retire", "code");
    let manifest = rich_manifest(identity.clone(), "hibernate-retire-g1", "pending");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:hibernate-retire-g1",
        None,
        '1',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    assert_eq!(
        release_sealed_head(
            &registered,
            temp.path(),
            &mut authority,
            &record.publication.key.projection,
        ),
        SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        }
    );
    let open = probe_lease(&registered, temp.path());
    assert!(
        open.staging_engine_is_open(),
        "publish + release must have opened the lazy engine"
    );
    drop(open);
    drop(commit);
    assert_engine_hibernated(&registered, temp.path());

    let code_generation = CodeGenerationId::new("code-generation.hibernate-retire-g1").unwrap();
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "1".repeat(64))).unwrap();
    // Journaled sealed replay is the deleted code-generation identity the
    // production retention pass names.
    assert_eq!(
        retire_code_generation(
            &registered,
            temp.path(),
            &mut authority,
            &code_generation,
            &sealed_digest,
        ),
        GraphReplayCollectionOutcome::RetentionPending
    );
    assert_engine_hibernated(&registered, temp.path());
    assert!(
        receipt_for_generation(temp.path(), "hibernate-retire-g1").is_some(),
        "a deferred retirement must leave the sealed artifact queued"
    );

    let opener = probe_lease(&registered, temp.path());
    let _opened = opener.snapshot().unwrap();
    assert!(
        opener.staging_engine_is_open(),
        "taking a snapshot must open the hibernated engine"
    );
    let retried = retire_code_generation(
        &registered,
        temp.path(),
        &mut authority,
        &code_generation,
        &sealed_digest,
    );
    // Fails if the retry no longer completes the native delete once the
    // engine is already open (Retired from first linearization, Absent from
    // the tombstone-cleanup drain).
    assert!(
        matches!(
            retried,
            GraphReplayCollectionOutcome::Retired(_) | GraphReplayCollectionOutcome::Absent
        ),
        "retry after open must complete retirement: {retried:?}"
    );
    assert_eq!(
        opener
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        (0, 0)
    );
    assert!(
        receipt_for_generation(temp.path(), "hibernate-retire-g1").is_none(),
        "completed retirement must delete the sealed directory"
    );
}

/// The sealed-staging release sweep must retain with
/// `StagingEngineHibernated` and must not open the engine.
///
/// Fails if `release_sealed_generation_staging_rows` reintroduces
/// `recover_verified_snapshot` / `read_guard` on a hibernated engine, or if
/// the typed reason is removed.
#[test]
fn hibernated_engine_release_sweep_retains_without_opening() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted_lazy(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("code:gen-hibernate-release", "code");
    let manifest = rich_manifest(identity, "hibernate-release-g1", "retained");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:hibernate-release-g1",
        None,
        '2',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    assert_eq!(
        release_sealed_head(
            &registered,
            temp.path(),
            &mut authority,
            &record.publication.key.projection,
        ),
        SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        }
    );
    drop(commit);
    assert_engine_hibernated(&registered, temp.path());

    assert_eq!(
        release_sealed_head(
            &registered,
            temp.path(),
            &mut authority,
            &record.publication.key.projection,
        ),
        SealedStagingRelease::Retained(SealedStagingRetentionReason::StagingEngineHibernated)
    );
    assert_engine_hibernated(&registered, temp.path());
}

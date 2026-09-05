//! Staging-row release, remount, and hibernated-retention contract.
//!
//! Each test names the production behaviour it pins; removing that behaviour
//! fails the assertion called out in the test's doc comment.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

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

/// One entity in the shape the code-graph projector actually stages: a
/// readable marker plus an opaque `Bytes` payload (its packed symbol record).
/// The `Bytes` variant is what the recovered-digest proof has to read back
/// byte-for-byte out of a reopened container, so the fixtures that stand in
/// for a code generation carry one.
fn evidence_entity(identity: &str, marker: &str) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(identity).unwrap(),
        BTreeSet::new(),
        BTreeMap::from([
            (
                GraphPropertyName::new("marker").unwrap(),
                GraphProperty::String(marker.to_owned()),
            ),
            (
                GraphPropertyName::new("record").unwrap(),
                GraphProperty::Bytes((0..=u8::MAX).cycle().take(4_096).collect::<Vec<u8>>()),
            ),
        ]),
    )
    .unwrap()
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
        vec![
            evidence_entity("entity:a", marker),
            evidence_entity("entity:b", marker),
        ],
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

/// Publishes one already-journaled replay, exactly as
/// [`publish_sealed`] does but with no in-hand manifest — the inline arm the
/// memory graph uses.
fn publish_journaled(
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

/// A sealed generation whose verified head is not resident in this process
/// must still release its duplicate staging rows, and the container the next
/// open loads must not keep paying for them.
///
/// This is the live-daemon shape: at project open the daemon recovers the
/// memory graph's inline head and the one serving code generation, so every
/// other sealed code scope has no lease here. Release used to answer
/// `NoVerifiedLease` for all of them and log nothing, so the shared staging
/// container kept every scope's rows — 8.6 GB on disk, 20+ GB of heap on the
/// next open, with the release queue backed up for a day (#799).
///
/// Fails if the lease-independent authority is removed (release answers
/// `Retained` again), if a fail-closed check is skipped, if a released
/// generation stops being serveable from its sealed store, or if the
/// inline-head generation — which has no sealed artifact — is released.
#[test]
fn sealed_generations_release_staging_rows_without_a_resident_lease() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let database = probe_lease(&registered, temp.path());

    // The memory graph: an inline-manifest head whose rows live only in
    // staging and which has no sealed artifact at all.
    let inline_identity = projection("memory:journey", "memory");
    let inline_manifest = rich_manifest(inline_identity.clone(), "m1", "marker-memory");
    let inline_record = authority.stage(
        inline_manifest
            .relational_replay(
                registered.binding.shard_id.clone(),
                GraphIdempotencyKey::new("publish:m1").unwrap(),
                digest('9'),
                None,
                &|| Ok(()),
            )
            .unwrap(),
    );
    let inline_commit = publish_journaled(
        &registered,
        temp.path(),
        &mut authority,
        &inline_record.publication.key,
    );
    assert_snapshot_reads(&inline_commit.snapshot, &inline_identity, "marker-memory");

    // Two sealed code generations, each its own worktree-scope projection.
    let mut sealed = Vec::new();
    for n in 1..=2u32 {
        let generation = format!("g{n}");
        let marker = format!("marker-{n}");
        let identity = projection(&format!("code:scope-{n}"), "code");
        let manifest = rich_manifest(identity.clone(), &generation, &marker);
        let input = char::from_digit(n, 10).unwrap();
        let record = stage_sealed_manifest(
            &mut authority,
            &registered.binding,
            &manifest,
            &format!("publish:{generation}"),
            None,
            input,
        );
        let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
        sealed.push((identity, marker, manifest, commit));
    }

    for (_, _, manifest, _) in &sealed {
        assert_ne!(
            database
                .staging_generation_row_counts(&manifest.identity())
                .unwrap(),
            (0, 0),
            "a freshly sealed generation still has its staging rows"
        );
    }

    // Drop every snapshot and every resident lease: exactly what a daemon
    // that opened the project and activated nothing else looks like.
    let recovered_digests = sealed
        .iter()
        .map(|(_, _, _, commit)| commit.head.recovered_digest.as_str().to_owned())
        .collect::<Vec<_>>();
    let sealed_identities = sealed
        .iter()
        .map(|(_, _, manifest, _)| manifest.identity())
        .collect::<Vec<_>>();
    let inline_head_digest = inline_commit.head.recovered_digest.as_str().to_owned();
    let inline_manifest_identity = inline_manifest.identity();
    drop(inline_commit);
    for (_, _, _, commit) in sealed.drain(..) {
        drop(commit);
    }
    database.forget_resident_verified_leases().unwrap();

    for (identity, digest) in sealed_identities.iter().zip(&recovered_digests) {
        // Fails if a sealed generation with no resident lease is retained.
        let outcome = database
            .release_staging_rows_for_relational_head(identity, digest)
            .unwrap();
        assert_eq!(
            outcome,
            SealedStagingRelease::Released {
                entities: 2,
                relations: 1,
            },
            "generation {} with no resident lease must release its staging rows",
            identity.generation.as_str()
        );
        assert_eq!(
            database.staging_generation_row_counts(identity).unwrap(),
            (0, 0)
        );
        assert!(database.is_generation_sealed_only(identity).unwrap());
    }

    // Fails if a digest that does not match the installed artifact is allowed
    // to delete rows.
    assert_eq!(
        database
            .release_staging_rows_for_relational_head(
                &sealed_identities[0],
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            )
            .unwrap(),
        SealedStagingRelease::Retained(SealedStagingRetentionReason::SealedDigestMismatch)
    );

    // The memory graph with no sealed artifact installed: staging holds the
    // only copy of its rows, so no authority — lease or artifact — may
    // release them. Fails if the lease-independent arm ever releases rows
    // that nothing else can reconstruct.
    database
        .discard_sealed_generation_reader(&inline_manifest_identity)
        .unwrap();
    assert_eq!(
        database
            .release_staging_rows_for_relational_head(
                &inline_manifest_identity,
                &inline_head_digest,
            )
            .unwrap(),
        SealedStagingRelease::Retained(SealedStagingRetentionReason::NoSealedStore)
    );
    assert_ne!(
        database
            .staging_generation_row_counts(&inline_manifest_identity)
            .unwrap(),
        (0, 0),
        "a head with no sealed artifact keeps the only copy of its rows"
    );

    // What the next open has to load. Released rows must not survive in the
    // container the daemon replays at open; the inline head's rows must.
    drop(database);
    assert!(registered.close().unwrap());
    registered.mount().unwrap();
    let reopened = probe_lease(&registered, temp.path());
    for identity in &sealed_identities {
        assert_eq!(
            reopened.staging_generation_row_counts(identity).unwrap(),
            (0, 0),
            "a released generation must not come back on reopen"
        );
    }
    assert_ne!(
        reopened
            .staging_generation_row_counts(&inline_manifest_identity)
            .unwrap(),
        (0, 0),
        "the unreleased head's rows must survive the reopen"
    );
    // The row counts above are what bounds the next open: a released
    // generation contributes nothing to the replay. The container size is
    // reported rather than asserted because only a closed container has a
    // meaningful size, and this fixture's three two-row generations are far
    // too small for a byte bound to distinguish compaction from framing.
    println!(
        "TRACEDECAY_STAGING_CONTAINER_BYTES after_release_and_reopen={}",
        std::fs::metadata(support::graph_path(temp.path()))
            .map(|meta| meta.len())
            .unwrap_or(0)
    );
}

/// Every published generation is retained as a sealed reader, but at most one
/// of those readers may hold a materialized native engine.
///
/// grafeo's store is heap resident, so an eagerly opened sealed reader costs a
/// whole in-memory graph for as long as it is retained: five retained
/// generations meant five whole graphs (#799) and each published worktree
/// scope added one more (#830). Publication now installs sealed readers
/// lazily, releases the engine once the digest proof is filed, and steps every
/// other idle reader down when a newer generation installs.
///
/// Fails if a sealed reader is opened eagerly again, if the post-proof
/// hibernation is dropped, if installing a generation stops sweeping its
/// peers, or if any of that costs a retained generation its exact rows.
#[test]
fn retained_sealed_generations_hold_at_most_one_resident_engine() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let database = probe_lease(&registered, temp.path());

    let mut published = Vec::new();
    for n in 1..=3 {
        let generation = format!("g{n}");
        let marker = format!("marker-{n}");
        let identity = projection(&format!("code:gen-{n}"), "code");
        let manifest = rich_manifest(identity.clone(), &generation, &marker);
        let input = char::from_digit(n, 10).expect("generation index is a decimal digit");
        let record = stage_sealed_manifest(
            &mut authority,
            &registered.binding,
            &manifest,
            &format!("publish:{generation}"),
            None,
            input,
        );
        let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
        published.push((identity, marker, commit.snapshot));

        let (retained, resident) = database.sealed_generation_engine_census();
        // Fails if a published generation stops being retained as a sealed
        // reader — the identity every later read and release resolves through.
        assert_eq!(
            retained,
            published.len(),
            "every published generation must stay retained as a sealed reader"
        );
        // Fails if the retained set grows one materialized graph per
        // generation again.
        assert!(
            resident <= 1,
            "at most one retained sealed generation may hold a resident engine; \
             {resident} of {retained} were resident after publishing {generation}"
        );
    }

    // Reading a superseded generation legitimately reopens its container: it
    // is being served at that moment. What must not happen is that the
    // reopened engine stays resident once a newer generation installs.
    for (identity, marker, snapshot) in &published {
        assert_snapshot_reads(snapshot, identity, marker);
        assert_projection_page_and_telemetry(snapshot, identity);
    }

    let generation = "g4".to_owned();
    let marker = "marker-4".to_owned();
    let identity = projection("code:gen-4", "code");
    let manifest = rich_manifest(identity.clone(), &generation, &marker);
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:g4",
        None,
        '4',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    published.push((identity, marker, commit.snapshot));

    let (retained, resident) = database.sealed_generation_engine_census();
    assert_eq!(retained, 4);
    // Fails if installing a generation stops sweeping the readers that a
    // prior pass materialized — the case that leaves N whole graphs resident.
    assert!(
        resident <= 1,
        "installing a generation must step every other idle sealed reader down; \
         {resident} of {retained} stayed resident"
    );

    // Fails if hibernation cost a retained generation its exact rows: every
    // one of them must still serve the identity it was proven against.
    for (identity, marker, snapshot) in &published {
        assert_snapshot_reads(snapshot, identity, marker);
        assert_projection_page_and_telemetry(snapshot, identity);
    }
}

/// The bounded release sweep is also the retry point for a sealed reader
/// that stayed resident when an earlier non-blocking hibernation attempt met
/// an active reader. Once idle, the next sweep must step it down without
/// losing its exact sealed identity or reopening it afterward.
///
/// Fails if idle-reader hibernation remains install-only: the reopened engine
/// stays resident forever when no later generation is published.
#[test]
fn release_sweep_retries_idle_sealed_reader_hibernation() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("code:retry-idle-hibernation", "code");
    let manifest = rich_manifest(identity.clone(), "retry-idle-g1", "retry-idle");
    let replay = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:retry-idle-g1",
        None,
        '5',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &replay, &manifest);
    let database = probe_lease(&registered, temp.path());

    assert_snapshot_reads(&commit.snapshot, &identity, "retry-idle");
    assert_eq!(
        database.sealed_generation_engine_census(),
        (1, 1),
        "the read must materialize the retained sealed engine"
    );

    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let snapshot = commit.snapshot.clone();
    let read_identity = identity.clone();
    let read_entered = Arc::clone(&entered);
    let read_release = Arc::clone(&release);
    let reader = thread::spawn(move || {
        snapshot.entity(
            &GraphEntityRef::new(read_identity, GraphEntityId::new("entity:a").unwrap()),
            Arc::new(GateOnPoll {
                polls: AtomicUsize::new(0),
                // `GraphDb::entity` probes once before `read_database`, which
                // probes before and after acquiring the native database read
                // guard. Park on the latter so this is a real in-flight read.
                gate_on: 3,
                entered: read_entered,
                release: read_release,
            }),
        )
    });
    entered.wait();

    let registry = registered.registry.clone();
    let binding = registered.binding.clone();
    let root = temp.path().to_path_buf();
    let projection = replay.publication.key.projection.clone();
    let (result_tx, result_rx) = mpsc::channel();
    let sweep = thread::spawn(move || {
        let (control, probe) = control_and_probe();
        let result = registry.release_sealed_generation_staging_rows(
            registration(binding, &root),
            &mut authority,
            &GraphPublicationOperationContextV1::new(&control, &probe).unwrap(),
            &projection,
        );
        result_tx.send(()).unwrap();
        (authority, result)
    });
    let busy_completion = result_rx.recv_timeout(Duration::from_millis(100));
    release.wait();
    assert!(reader.join().unwrap().unwrap().is_some());
    assert_eq!(
        busy_completion,
        Err(mpsc::RecvTimeoutError::Timeout),
        "staging-row mutation remains serialized behind the snapshot's parent gate"
    );
    let (mut authority, sweep_result) = sweep.join().unwrap();
    assert_eq!(
        sweep_result,
        Ok(SealedStagingRelease::Released {
            entities: 2,
            relations: 1,
        })
    );
    assert_eq!(
        database.sealed_generation_engine_census(),
        (1, 1),
        "the child reaper must skip the active native reader rather than hibernate it once the read ends"
    );
    assert_eq!(
        release_sealed_head(
            &registered,
            temp.path(),
            &mut authority,
            &replay.publication.key.projection,
        ),
        SealedStagingRelease::AlreadyReleased
    );
    assert_eq!(
        database.sealed_generation_engine_census(),
        (1, 0),
        "the next bounded sweep must retry the now-idle reader"
    );
    assert_snapshot_reads(&commit.snapshot, &identity, "retry-idle");
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

/// The sealed-staging release sweep on a hibernated engine opens it once,
/// answers from the durable rows, and leaves it hibernated again.
///
/// A repeat sweep after the rows were released must report
/// `AlreadyReleased` from the absent rows, not re-report a release or hide
/// behind a `StagingEngineHibernated` refusal (which is what left fifteen
/// sealed generations' rows in one production container: the sweep only
/// ever ran while the engine happened to be open).
#[test]
fn hibernated_engine_release_sweep_opens_once_and_rehibernates() {
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
        SealedStagingRelease::AlreadyReleased,
        "a repeat sweep must answer from the absent rows, not refuse on hibernation"
    );
    assert_engine_hibernated(&registered, temp.path());
}

/// Corrupts every sealed generation container under `root`, leaving each
/// receipt exactly as the clean seal wrote it. This is the on-disk state the
/// daemon restart fixture injects: the derived artifact is unreadable while
/// the relational head, its journaled replay, and the released staging
/// projection commit are all intact.
fn corrupt_sealed_containers(root: &Path) -> usize {
    let mut corrupted = 0;
    let mut directories = vec![sealed_store_root(root)];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.map(Result::unwrap) {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                directories.push(path);
            } else if entry.file_name() == "generation.grafeo" {
                std::fs::write(&path, b"corrupt sealed graph").unwrap();
                corrupted += 1;
            }
        }
    }
    corrupted
}

/// A restart that finds its sealed artifact unreadable must repair the
/// released generation from its canonical replay and reproduce the exact
/// recovered digest its relational head names.
///
/// This is the graph-db half of the daemon's
/// `corrupt_graph_restart_repairs_through_canonical_serialized_activation`:
/// publish a sealed code generation, release its duplicate staging rows,
/// restart, and destroy the sealed container. Recovery then has neither
/// staging rows nor a usable artifact, so the canonical serialized activation
/// path republishes the same head with the hydrated manifest.
///
/// `retain_manifest` picks which staging arm runs: production hands
/// publication an `Arc::clone` of a manifest it still holds (the shared arm),
/// while a sole-owned `Arc` moves its rows into the owned arm. Both must
/// restore every released row.
///
/// Fails if that republication cannot restore the released rows - either
/// because the staged prefix is refused against the leftover projection
/// commit, or because the proof runs over a partial row set and reports a
/// recovered-digest mismatch that quarantines a healthy generation.
fn corrupt_sealed_artifact_repair_case(retain_manifest: bool, release_staging_rows: bool) {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("code:gen-corrupt-repair", "code");
    let manifest = rich_manifest(identity.clone(), "corrupt-repair-g1", "repaired");
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:corrupt-repair-g1",
        None,
        '3',
    );
    let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
    if release_staging_rows {
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
    }
    drop(commit);
    assert!(registered.close().unwrap());
    drop(registered);

    assert!(
        corrupt_sealed_containers(temp.path()) > 0,
        "the fixture must have sealed a generation to corrupt"
    );

    let registered = RegisteredGraph::new_mounted_with_manifest_provider(
        temp.path(),
        Arc::new(RemountSealedProvider {
            manifest: manifest.clone(),
        }),
    )
    .unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let recovered = registered.registry.recover_verified_snapshot(
        registration(registered.binding.clone(), temp.path()),
        &mut authority,
        &context,
        &record.publication.key.projection,
    );
    if release_staging_rows {
        assert!(
            recovered.is_err(),
            "a corrupt artifact with released rows has nothing to recover from"
        );
    } else {
        // The staging rows are still the authority: a corrupt derived
        // artifact must be discarded and the stored rows must prove.
        let snapshot = recovered.expect("retained staging rows must recover a corrupt artifact");
        assert_snapshot_reads(&snapshot, &identity, "repaired");
        drop(snapshot);
    }

    // The canonical serialized activation path: the same head, republished
    // from the hydrated manifest.
    let supplied = Arc::new(manifest.clone());
    let retained = retain_manifest.then(|| Arc::clone(&supplied));
    let repaired = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key,
            Some(supplied),
        )
        .expect("republishing the adopted head must repair the released rows");
    drop(retained);
    assert_snapshot_reads(&repaired.snapshot, &identity, "repaired");
    assert_projection_page_and_telemetry(&repaired.snapshot, &identity);
    drop(repaired);

    let database = probe_lease(&registered, temp.path());
    assert_eq!(
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap(),
        manifest.row_counts(),
        "the repair must restore every released row before it proves the digest"
    );
}

/// The production arm: the publisher keeps its own `Arc` to the manifest, so
/// staging borrows the rows instead of moving them.
#[test]
fn corrupt_sealed_artifact_repair_restages_shared_manifest_rows() {
    corrupt_sealed_artifact_repair_case(true, true);
}

/// The sole-owned arm: staging moves the manifest's rows page by page.
#[test]
fn corrupt_sealed_artifact_repair_restages_owned_manifest_rows() {
    corrupt_sealed_artifact_repair_case(false, true);
}

/// The retained-rows arm: the sealed artifact is destroyed while the staging
/// rows it was derived from are still there. Recovery must discard the
/// artifact and prove the stored rows, and the republish must be a no-op
/// reseat rather than a recovered-digest mismatch.
#[test]
fn corrupt_sealed_artifact_repair_proves_retained_staging_rows() {
    corrupt_sealed_artifact_repair_case(true, false);
}

/// A probe that answers `Cancelled` once it has been polled `cancel_after`
/// times. The staging-row release deletes rows in durable pages and polls the
/// operation between them, so this interrupts it at an exact page boundary.
struct CancelAfterPolls {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    polls: AtomicUsize,
    cancel_after: usize,
}

impl CancelAfterPolls {
    fn new(cancel_after: usize) -> Self {
        Self {
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new("release-cancel").unwrap(),
                generation: 1,
            },
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new("release-cancel-deadline").unwrap(),
            },
            polls: AtomicUsize::new(0),
            cancel_after,
        }
    }

    fn control(&self) -> RuntimeRequestControlV1 {
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline: self.deadline.clone(),
            cancellation: self.cancellation.clone(),
        }
    }
}

impl RuntimeRequestProbeV1 for CancelAfterPolls {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        (self.polls.fetch_add(1, Ordering::SeqCst) >= self.cancel_after)
            .then_some(RuntimeInterruptionV1::Cancelled)
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
    }
}

/// Interrupting a staging-row release must never leave a generation the
/// recovered-digest proof will read as corrupt.
///
/// The release deletes relation rows and entity rows in separate durable
/// pages and polls cancellation between them, so an interrupted sweep can
/// commit some pages and not others. Whatever row set that leaves behind, the
/// next mount has to end up serving the generation: either the rows are all
/// there and prove, or they are all gone and the sealed artifact serves. A
/// row set that is neither hashes to something the head never named, and the
/// generation is quarantined even though nothing about it is actually wrong.
///
/// Fails if any cancellation point leaves a row set neither recovery nor the
/// canonical republication can serve. The printed
/// `cancelled_release_partial_row_sets_observed` says whether the sweep
/// actually produced a partial set at this fixture's page geometry; the
/// contract is the same either way, and the loop starts catching the partial
/// case the moment a release page can commit without its siblings.
#[test]
fn a_cancelled_release_never_leaves_a_row_set_recovery_cannot_serve() {
    let mut observed_partial = false;
    for cancel_after in 0..24 {
        let temp = TempDir::new().unwrap();
        let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
        let mut authority = RelationalAuthority::default();
        let identity = projection("code:gen-release-cancel", "code");
        let manifest = rich_manifest(identity.clone(), "release-cancel-g1", "interrupted");
        let record = stage_sealed_manifest(
            &mut authority,
            &registered.binding,
            &manifest,
            "publish:release-cancel-g1",
            None,
            '4',
        );
        let commit = publish_sealed(&registered, temp.path(), &mut authority, &record, &manifest);
        drop(commit);

        let probe = CancelAfterPolls::new(cancel_after);
        let control = probe.control();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        let _ = registered.registry.release_sealed_generation_staging_rows(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        );
        let counts = {
            let database = probe_lease(&registered, temp.path());
            database
                .staging_generation_row_counts(&manifest.identity())
                .unwrap()
        };
        let full = manifest.row_counts();
        if counts != full && counts != (0, 0) {
            observed_partial = true;
        }
        assert!(registered.close().unwrap());
        drop(registered);
        // Destroy the derived artifact, as the daemon fixture does. Without
        // it the remount adopts the sealed copy and never reads the rows the
        // interrupted sweep left behind, which is exactly the row set this
        // test is about.
        assert!(corrupt_sealed_containers(temp.path()) > 0);

        // Remount exactly as a restarted daemon does and take the generation
        // back to a serving state.
        let registered = RegisteredGraph::new_mounted_with_manifest_provider(
            temp.path(),
            Arc::new(RemountSealedProvider {
                manifest: manifest.clone(),
            }),
        )
        .unwrap();
        let (control, probe) = control_and_probe();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        let recovered = registered.registry.recover_verified_snapshot(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        );
        match recovered {
            Ok(snapshot) => {
                assert_snapshot_reads(&snapshot, &identity, "interrupted");
                drop(snapshot);
            }
            Err(error) => {
                // Recovery may legitimately refuse (the rows are gone and the
                // artifact alone cannot answer the walk); the canonical
                // republish is then the repair, and it must succeed.
                let repaired = registered
                    .registry
                    .publish_verified(
                        registration(registered.binding.clone(), temp.path()),
                        &mut authority,
                        &context,
                        &record.publication.key,
                        Some(Arc::new(manifest.clone())),
                    )
                    .unwrap_or_else(|republish| {
                        panic!(
                            "a release cancelled after {cancel_after} polls left \
                             {counts:?} of {full:?} rows that neither recovery \
                             ({error}) nor republication ({republish}) could serve"
                        )
                    });
                assert_snapshot_reads(&repaired.snapshot, &identity, "interrupted");
                drop(repaired);
            }
        }
    }
    // Not an assertion about the fixture's size, only a record of what the
    // sweep can actually produce here; the contract above holds either way.
    eprintln!("cancelled_release_partial_row_sets_observed={observed_partial}");
}

/// Mirrors the crate-private `limits::MAX_NATIVE_GENERATION_STAGE_MUTATIONS`,
/// the mutation ceiling one native staging page flushes at. Kept in sync by
/// hand; the geometry assertion below fails loudly if it drifts.
const NATIVE_STAGE_PAGE_MUTATIONS: usize = 65_536;

/// Mirrors `limits::MAX_VERIFIED_GENERATION_BATCH_MUTATIONS`, the rows one
/// durable release transaction removes.
const RELEASE_PAGE_MUTATIONS: usize = 4_096;

/// A manifest in the two-page staging geometry an interrupted release can
/// misrepresent: one full native page plus a second page wider than a single
/// release transaction. Carries the same readable evidence entities and
/// relation as [`rich_manifest`] so a recovered snapshot can still be walked.
fn two_page_manifest(
    projection_identity: GraphProjectionIdentity,
    generation: &str,
    marker: &str,
    entity_count: usize,
) -> GraphGenerationManifest {
    let from = GraphEntityRef::new(
        projection_identity.clone(),
        GraphEntityId::new("entity:a").unwrap(),
    );
    let to = GraphEntityRef::new(
        projection_identity.clone(),
        GraphEntityId::new("entity:b").unwrap(),
    );
    // `entity:a` and `entity:b` sort below every `entity:f…` filler, so the
    // release's ordered deletion pages start inside the first stage page --
    // the range whose rows the count heuristic claimed were still present.
    let mut entities = vec![
        evidence_entity("entity:a", marker),
        evidence_entity("entity:b", marker),
    ];
    entities.extend((0..entity_count - 2).map(|index| {
        GraphEntity::new(
            GraphEntityId::new(format!("entity:f{index:06}")).unwrap(),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .unwrap()
    }));
    GraphGenerationManifest::new(
        projection_identity,
        GraphGenerationId::new(generation).unwrap(),
        SourceGeneration::new(format!("source:{generation}")).unwrap(),
        GraphWatermark::new(format!("watermark:{generation}")).unwrap(),
        Vec::new(),
        entities,
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

/// Restaging a generation whose staging-row release was interrupted must
/// restore every released row, not just the pages a row count still misses.
///
/// The release deletes bounded pages of arbitrary identities, so the rows it
/// leaves behind are not the ordered prefix the staging pipeline builds. At a
/// one-page geometry any deletion drops the count below the only page's
/// `range.end` and the replay is forced anyway; at this two-page geometry the
/// survivors still out-count the *first* page, and treating that count as
/// proof of the page's presence skipped the exact rows the release had
/// removed. The replay then reported success while the recovered digest no
/// longer matched the head, so the generation could not be served at all.
///
/// Fails if a remount after an interrupted release cannot serve the full row
/// set, or if the restage leaves fewer rows than the manifest.
#[test]
fn a_partially_released_generation_restages_every_row_it_lost() {
    let entity_count = NATIVE_STAGE_PAGE_MUTATIONS + 2 * RELEASE_PAGE_MUTATIONS;
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = projection("code:gen-release-partial", "code");
    let manifest = two_page_manifest(
        identity.clone(),
        "release-partial-g1",
        "interrupted",
        entity_count,
    );
    let full = manifest.row_counts();
    assert_eq!(full, (entity_count, 1));
    let record = stage_sealed_manifest(
        &mut authority,
        &registered.binding,
        &manifest,
        "publish:release-partial-g1",
        None,
        '5',
    );
    drop(publish_sealed(
        &registered,
        temp.path(),
        &mut authority,
        &record,
        &manifest,
    ));

    // Walk the cancellation budget upward until a durable entity release page
    // has committed. The release is resumable, so each attempt resumes where
    // the last stopped and the first attempt that removes rows removes one
    // bounded page.
    //
    // The budget is measured in cancellation *polls*, and a release page polls
    // once per row it enumerates before it commits `RELEASE_PAGE_MUTATIONS` of
    // them -- so the first durable page costs on the order of the whole row
    // count in polls, not the few dozen a short linear walk reaches. Start at
    // that scale and double: the first budget that commits cannot reach a
    // second page (which costs another full enumeration), so the fixture
    // still lands in the one-page-removed geometry the assertion below reads.
    let mut partial = None;
    let mut cancel_after = entity_count;
    while cancel_after <= 4 * entity_count {
        let probe = CancelAfterPolls::new(cancel_after);
        let control = probe.control();
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        let _ = registered.registry.release_sealed_generation_staging_rows(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &context,
            &record.publication.key.projection,
        );
        let counts = {
            let database = probe_lease(&registered, temp.path());
            database
                .staging_generation_row_counts(&manifest.identity())
                .unwrap()
        };
        if counts.0 < full.0 {
            partial = Some(counts);
            break;
        }
        cancel_after *= 2;
    }
    let counts =
        partial.expect("an interrupted release must commit at least one durable entity page");
    assert!(
        counts.0 >= NATIVE_STAGE_PAGE_MUTATIONS && counts.0 < full.0,
        "this fixture must reach the state the count heuristic misreads: \
         {counts:?} of {full:?}, first stage page ends at {NATIVE_STAGE_PAGE_MUTATIONS}"
    );

    assert!(registered.close().unwrap());
    drop(registered);
    // Destroy the derived artifact, as the daemon fixture does, so the remount
    // has to read the row set the interrupted release left behind instead of
    // adopting the sealed copy.
    assert!(corrupt_sealed_containers(temp.path()) > 0);

    let registered = RegisteredGraph::new_mounted_with_manifest_provider(
        temp.path(),
        Arc::new(RemountSealedProvider {
            manifest: manifest.clone(),
        }),
    )
    .unwrap();
    let (control, probe) = control_and_probe();
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let recovered = registered.registry.recover_verified_snapshot(
        registration(registered.binding.clone(), temp.path()),
        &mut authority,
        &context,
        &record.publication.key.projection,
    );
    match recovered {
        Ok(snapshot) => {
            assert_snapshot_reads(&snapshot, &identity, "interrupted");
            drop(snapshot);
        }
        Err(error) => {
            let repaired = registered
                .registry
                .publish_verified(
                    registration(registered.binding.clone(), temp.path()),
                    &mut authority,
                    &context,
                    &record.publication.key,
                    Some(Arc::new(manifest.clone())),
                )
                .unwrap_or_else(|republish| {
                    panic!(
                        "a release interrupted at {counts:?} of {full:?} rows left a row set \
                         neither recovery ({error}) nor republication ({republish}) could serve"
                    )
                });
            assert_snapshot_reads(&repaired.snapshot, &identity, "interrupted");
            drop(repaired);
        }
    }
    let restaged = {
        let database = probe_lease(&registered, temp.path());
        database
            .staging_generation_row_counts(&manifest.identity())
            .unwrap()
    };
    assert_eq!(
        restaged, full,
        "every released row must be restaged, not skipped by a row count"
    );
}

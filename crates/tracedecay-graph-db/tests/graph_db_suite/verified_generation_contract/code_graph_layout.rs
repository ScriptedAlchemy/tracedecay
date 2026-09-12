//! Code-graph namespace layout contract (issue #836).
//!
//! The canonical code-graph namespace is derived from the code shard alone, so
//! every generation of one scope publishes into a single projection. These
//! tests pin the two consequences that make the layout worth having:
//!
//! * publishing generation N+1 supersedes N as an ordinary verified-head
//!   replacement, and N is then reclaimed through the ordinary
//!   `retire_replay` path — never through the head-retirement escape hatch;
//! * a store persisted under the retired per-generation layout still opens,
//!   and its immortal per-generation head is drained through the existing
//!   superseded-head retirement path without disturbing the canonical
//!   projection the code index republished into.

use rusqlite::Savepoint;
use tracedecay_graph_db::{
    LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX, SupersededReplayRetirement,
    VerifiedGraphCommit, code_graph_shard_namespace, is_code_graph_shard_namespace,
    is_legacy_per_generation_code_graph_namespace,
};
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    exact_sql::ExactSqlHandle,
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
    repository::{GRAPH_PUBLICATION_SCHEMA_V1, GraphPublicationExactSqlStorage},
};
use tracedecay_store::{
    AdmissionConfigV1, CodeShardScopeV1, RepositoryWritePayloadV1, RuntimeReadOutcomeV1,
    RuntimeReadRequestV1, StorageRuntimeErrorV1, StoreShardScopeV1, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

use super::*;

struct NoPublicationWrites;

impl StorageOperationExecutor for NoPublicationWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoPublicationReads;

impl ReaderQueryExecutor for NoPublicationReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        unreachable!("exact SQL graph publication queries bypass product reads")
    }
}

struct ExactPublicationAuthority {
    _writer: PersistentWriter,
    _readers: ReaderPool<NoPublicationReads>,
    storage: GraphPublicationExactSqlStorage,
}

impl ExactPublicationAuthority {
    fn new(root: &std::path::Path, binding: &tracedecay_store::StoreRuntimeBindingV1) -> Self {
        let path = root.join("graph-publication-authority.sqlite3");
        drop(rusqlite::Connection::open(&path).unwrap());
        let path = path.canonicalize().unwrap();
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(&path).unwrap(),
        );
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
            AdmissionConfigV1::default(),
            NoPublicationWrites,
        )
        .unwrap();
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding.clone(), locator, path).unwrap(),
            AdmissionConfigV1::default().readers,
            NoPublicationReads,
        )
        .unwrap();
        let handle = ExactSqlHandle::attach(&writer, &readers).unwrap();
        handle
            .execute_batch(GRAPH_PUBLICATION_SCHEMA_V1.to_owned())
            .unwrap();
        let storage = GraphPublicationExactSqlStorage::from_authorized_handle(handle).unwrap();
        Self {
            _writer: writer,
            _readers: readers,
            storage,
        }
    }
}

fn code_shard(worktree: &str) -> StoreShardIdV1 {
    StoreShardIdV1::new(
        tracedecay_domain::BrainId::new("brain.code-graph-layout").unwrap(),
        tracedecay_domain::UserProfileId::new("profile.code-graph-layout").unwrap(),
        StoreShardScopeV1::Code {
            project_id: tracedecay_domain::ProjectId::new("project.code-graph-layout").unwrap(),
            repository_id: RepositoryId::new("repository.code-graph-layout").unwrap(),
            scope: CodeShardScopeV1::Worktree {
                worktree_id: tracedecay_domain::WorktreeId::new(worktree).unwrap(),
            },
        },
    )
}

/// The projection the code index publishes into after the cutover: one
/// namespace per code shard, shared by every generation of that shard.
fn canonical_projection(worktree: &str) -> GraphProjectionIdentity {
    GraphProjectionIdentity::new(
        code_graph_shard_namespace(&code_shard(worktree)).unwrap(),
        GraphProjectionId::new("code-graph").unwrap(),
    )
}

/// A projection exactly as a pre-cutover store persisted it: the code
/// generation hashed into the namespace, so the generation owns the projection.
fn legacy_per_generation_projection(digest_byte: char) -> GraphProjectionIdentity {
    GraphProjectionIdentity::new(
        GraphNamespace::new(format!(
            "{LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX}{}",
            digest_byte.to_string().repeat(64)
        ))
        .unwrap(),
        GraphProjectionId::new("code-graph").unwrap(),
    )
}

fn sealed_source(
    generation: &CodeGenerationId,
    digest: &SealedGraphStateDigest,
) -> SealedCodeGenerationReplay {
    SealedCodeGenerationReplay {
        repository: RepositoryId::new("repository.code-graph-layout").unwrap(),
        generation: generation.clone(),
        sealed_state_digest: digest.clone(),
        projector_revision: GraphProjectorRevision::try_from(
            "projector.code-graph-layout".to_owned(),
        )
        .unwrap(),
    }
}

/// Rewrites the already-journaled replay of `record` so it names a sealed code
/// generation, keeping its sequence. The registry selects retirement
/// candidates by decoding the journaled replay source, so this is what makes a
/// published generation visible to the code-generation retirement sweep.
#[allow(clippy::too_many_arguments)]
fn bind_sealed_source(
    authority: &mut RelationalAuthority,
    binding: &tracedecay_store::StoreRuntimeBindingV1,
    manifest: &GraphGenerationManifest,
    record: &GraphPublicationReplayRecordV1,
    idempotency: &str,
    expected: Option<GraphVerifiedHeadV1>,
    input: char,
    generation: &CodeGenerationId,
    sealed_digest: &SealedGraphStateDigest,
) {
    let sealed = manifest
        .relational_sealed_replay(
            binding.shard_id.clone(),
            GraphIdempotencyKey::new(idempotency).unwrap(),
            digest(input),
            expected,
            sealed_source(generation, sealed_digest),
            &|| Ok(()),
        )
        .unwrap();
    authority.records.insert(
        record.publication.key.clone(),
        GraphPublicationReplayRecordV1::new(record.sequence, sealed).unwrap(),
    );
}

fn fresh_context<'a>(
    control: &'a RuntimeRequestControlV1,
    probe: &'a Probe,
) -> GraphPublicationOperationContextV1<'a> {
    GraphPublicationOperationContextV1::new(control, probe).unwrap()
}

/// The canonical namespace is generation-agnostic and never collides with the
/// retired per-generation layout it replaced.
#[test]
fn canonical_code_graph_namespace_is_per_shard_and_disjoint_from_the_legacy_layout() {
    let primary = canonical_projection("worktree.primary");
    let linked = canonical_projection("worktree.linked");
    assert_eq!(primary, canonical_projection("worktree.primary"));
    assert_ne!(primary, linked);
    assert!(is_code_graph_shard_namespace(&primary.namespace));
    assert!(!is_legacy_per_generation_code_graph_namespace(
        &primary.namespace
    ));
    assert!(is_legacy_per_generation_code_graph_namespace(
        &legacy_per_generation_projection('a').namespace
    ));
}

/// Publishing a second generation of one code shard supersedes the first head,
/// and the superseded generation is then reclaimed by the ordinary
/// `retire_replay` path — the head-retirement escape hatch is never used.
#[test]
fn second_generation_supersedes_the_head_and_the_first_retires_without_head_retirement() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = canonical_projection("worktree.primary");
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "7".repeat(64))).unwrap();
    let alpha = CodeGenerationId::new("code-generation.alpha").unwrap();
    let beta = CodeGenerationId::new("code-generation.beta").unwrap();

    let g1 = manifest(identity.clone(), "layout-g1", "g1", vec![], vec![]);
    let g1_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g1,
        "publish:layout-g1",
        None,
        '1',
    );
    let (control, probe) = control_and_probe();
    let g1_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &fresh_context(&control, &probe),
            &g1_record.publication.key,
            None,
        )
        .unwrap();
    let g1_head = g1_commit.head.clone();
    drop(g1_commit);
    bind_sealed_source(
        &mut authority,
        &registered.binding,
        &g1,
        &g1_record,
        "publish:layout-g1",
        None,
        '1',
        &alpha,
        &sealed_digest,
    );

    // The second generation lands in the same projection, so its publication
    // is an ordinary compare-and-swap against the first generation's head.
    let g2 = manifest(identity.clone(), "layout-g2", "g2", vec![], vec![]);
    let g2_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &g2,
        "publish:layout-g2",
        Some(g1_head.clone()),
        '2',
    );
    assert_eq!(
        g2_record.publication.key.projection, g1_record.publication.key.projection,
        "both generations of one code shard share a single projection",
    );
    let (control, probe) = control_and_probe();
    let g2_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &fresh_context(&control, &probe),
            &g2_record.publication.key,
            None,
        )
        .unwrap();
    let g2_head = g2_commit.head.clone();
    drop(g2_commit);
    bind_sealed_source(
        &mut authority,
        &registered.binding,
        &g2,
        &g2_record,
        "publish:layout-g2",
        Some(g1_head),
        '2',
        &beta,
        &sealed_digest,
    );

    assert_eq!(
        authority.heads.get(&g2_record.publication.key.projection),
        Some(&g2_head),
        "publishing the second generation supersedes the first head",
    );
    assert_eq!(
        authority.head_retirement_calls, 0,
        "supersession never reaches the head-retirement path",
    );

    // The superseded generation is historical replay: the ordinary retirement
    // path reclaims it, and the current head is left standing.
    let (control, probe) = control_and_probe();
    assert_eq!(
        registered.registry.retire_one_code_generation_replay(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &fresh_context(&control, &probe),
            &alpha,
            &sealed_digest,
        ),
        Ok(GraphReplayCollectionOutcome::Retired(Box::new(
            tracedecay_graph_db::GraphGenerationReplaySource::SealedCodeGeneration(sealed_source(
                &alpha,
                &sealed_digest,
            ))
        )))
    );
    assert_eq!(
        authority.head_retirement_calls, 0,
        "a superseded generation must retire through retire_replay, not the \
         per-generation head-retirement escape hatch",
    );
    assert!(
        authority.retired.contains_key(&g1_record.publication.key),
        "the superseded generation is tombstoned",
    );
    assert_eq!(
        authority.heads.get(&g2_record.publication.key.projection),
        Some(&g2_head),
        "retiring the superseded generation leaves the current head standing",
    );
}

/// A store persisted under the retired per-generation layout still opens, and
/// its immortal per-generation head is drained through the existing
/// superseded-head retirement path without touching the canonical projection
/// the code index republished into after the cutover.
#[test]
fn a_store_persisted_under_the_legacy_layout_opens_and_drains_its_per_generation_head() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let legacy_identity = legacy_per_generation_projection('c');
    let canonical_identity = canonical_projection("worktree.primary");
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "6".repeat(64))).unwrap();
    let legacy_generation = CodeGenerationId::new("code-generation.pre-cutover").unwrap();
    let current_generation = CodeGenerationId::new("code-generation.post-cutover").unwrap();

    // Pre-cutover state: the generation owns a projection of its own and is
    // its permanent verified head.
    let legacy = manifest(
        legacy_identity.clone(),
        "legacy-g1",
        "legacy",
        vec![],
        vec![],
    );
    let legacy_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &legacy,
        "publish:legacy-g1",
        None,
        '3',
    );
    let (control, probe) = control_and_probe();
    let legacy_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &fresh_context(&control, &probe),
            &legacy_record.publication.key,
            None,
        )
        .unwrap();
    drop(legacy_commit);
    bind_sealed_source(
        &mut authority,
        &registered.binding,
        &legacy,
        &legacy_record,
        "publish:legacy-g1",
        None,
        '3',
        &legacy_generation,
        &sealed_digest,
    );

    // Post-cutover: the canonical per-shard projection has no head, so the
    // code index republishes the live generation into it. The legacy
    // projection is untouched by that publication.
    let current = manifest(
        canonical_identity.clone(),
        "canonical-g1",
        "canonical",
        vec![],
        vec![],
    );
    let current_record = stage_manifest(
        &mut authority,
        &registered.binding,
        &current,
        "publish:canonical-g1",
        None,
        '4',
    );
    let (control, probe) = control_and_probe();
    let current_commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &fresh_context(&control, &probe),
            &current_record.publication.key,
            None,
        )
        .unwrap();
    let current_head = current_commit.head.clone();
    drop(current_commit);
    bind_sealed_source(
        &mut authority,
        &registered.binding,
        &current,
        &current_record,
        "publish:canonical-g1",
        None,
        '4',
        &current_generation,
        &sealed_digest,
    );

    // Remount: the legacy-layout rows must survive a close and reopen.
    assert!(registered.close().unwrap());
    drop(registered);
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();

    // Draining the pre-cutover generation goes through the superseded-head
    // retirement path, because a legacy projection's only replay is its head.
    let (control, probe) = control_and_probe();
    assert_eq!(
        registered.registry.retire_one_code_generation_replay(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &fresh_context(&control, &probe),
            &legacy_generation,
            &sealed_digest,
        ),
        Ok(GraphReplayCollectionOutcome::Retired(Box::new(
            tracedecay_graph_db::GraphGenerationReplaySource::SealedCodeGeneration(sealed_source(
                &legacy_generation,
                &sealed_digest,
            ))
        )))
    );
    assert_eq!(
        authority.head_retirement_calls, 1,
        "a legacy per-generation head is reclaimed by the head-retirement path",
    );
    assert!(
        !authority
            .heads
            .contains_key(&legacy_record.publication.key.projection),
        "the legacy per-generation head is gone",
    );
    assert_eq!(
        authority
            .heads
            .get(&current_record.publication.key.projection),
        Some(&current_head),
        "draining legacy-layout residue leaves the canonical head standing",
    );

    // Nothing legacy-layout is left for the sweep to find.
    let (control, probe) = control_and_probe();
    assert_eq!(
        registered.registry.retire_one_code_generation_replay(
            registration(registered.binding.clone(), temp.path()),
            &mut authority,
            &fresh_context(&control, &probe),
            &legacy_generation,
            &sealed_digest,
        ),
        Ok(GraphReplayCollectionOutcome::Absent)
    );
}

/// A retired legacy replay remains sufficient authority to release its
/// duplicate staging rows after head retirement. The sealed artifact must
/// still reproduce the tombstone's exact digest; an active, dependency-
/// bearing, ambiguous, or non-legacy replay stays fail-closed.
///
/// Fails if release checks only `verified_head` and returns
/// `NoVerifiedLease` before inspecting the durable cleanup tombstone.
#[test]
fn retired_legacy_replay_without_a_head_releases_its_verified_sealed_staging_rows() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = ExactPublicationAuthority::new(temp.path(), &registered.binding);
    let identity = legacy_per_generation_projection('d');
    let code_generation = CodeGenerationId::new("code-generation.legacy-no-head").unwrap();
    let sealed_digest =
        SealedGraphStateDigest::try_from(format!("sha256:{}", "8".repeat(64))).unwrap();
    let generation = manifest(
        identity,
        "legacy-no-head-g1",
        "legacy-no-head",
        vec![],
        vec![],
    );
    let publication = generation
        .relational_sealed_replay(
            registered.binding.shard_id.clone(),
            GraphIdempotencyKey::new("publish:legacy-no-head-g1").unwrap(),
            digest('8'),
            None,
            sealed_source(&code_generation, &sealed_digest),
            &|| Ok(()),
        )
        .unwrap();
    let (control, probe) = control_and_probe();
    let replay = match authority
        .storage
        .append_replay(&publication, &fresh_context(&control, &probe))
        .unwrap()
    {
        GraphReplayAppendOutcomeV1::Appended(replay) => replay,
        outcome => panic!("fresh exact-SQL authority must append: {outcome:?}"),
    };
    let (control, probe) = control_and_probe();
    assert_eq!(
        registered.registry.release_sealed_generation_staging_rows(
            registration(registered.binding.clone(), temp.path()),
            &mut authority.storage,
            &fresh_context(&control, &probe),
            &replay.publication.key.projection,
        ),
        Ok(SealedStagingRelease::Retained(
            SealedStagingRetentionReason::NoVerifiedLease,
        )),
        "production no-head replay semantics classify the sole active replay as pending"
    );
    // The legacy shape under test: the generation's rows already sit in the
    // staging database when its sealed artifact is built.
    registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap()
        .stage_generation_rows_unpublished(Arc::new(generation.clone()))
        .unwrap();
    let (control, probe) = control_and_probe();
    let commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), temp.path()),
            &mut authority.storage,
            &fresh_context(&control, &probe),
            &replay.publication.key,
            Some(Arc::new(generation.clone())),
        )
        .unwrap();
    let retirement = GraphPublicationReplayRetirementV1::new(
        replay.publication.key.clone(),
        replay.publication.input_digest.clone(),
        replay
            .publication
            .dependency_generation_closure_digest
            .clone(),
        replay.publication.direct_dependency_generations.clone(),
        replay.publication.expected_prior_head.clone(),
        replay.publication.expected_recovered_digest.clone(),
        replay.publication.canonical_replay_source_digest.clone(),
    )
    .unwrap();
    let (control, probe) = control_and_probe();
    assert!(matches!(
        authority
            .storage
            .retire_verified_head_replay(
                &retirement,
                &commit.head,
                &fresh_context(&control, &probe),
            )
            .unwrap(),
        GraphReplayRetirementOutcomeV1::Retired(_)
    ));
    drop(commit);

    assert!(registered.close().unwrap());
    drop(registered);

    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let (control, probe) = control_and_probe();
    assert_eq!(
        registered.registry.release_sealed_generation_staging_rows(
            registration(registered.binding.clone(), temp.path()),
            &mut authority.storage,
            &fresh_context(&control, &probe),
            &replay.publication.key.projection,
        ),
        Ok(SealedStagingRelease::Released {
            entities: 1,
            relations: 0,
        })
    );
    let database = registered
        .registry
        .resolve(registration(registered.binding.clone(), temp.path()))
        .unwrap();
    assert_eq!(
        database
            .staging_generation_row_counts(&generation.identity())
            .unwrap(),
        (0, 0),
        "the replay-verified sealed artifact makes the staging copy redundant"
    );
}

/// Number of sealed generation artifacts currently on disk under the store.
fn sealed_generation_count(root: &std::path::Path) -> usize {
    std::fs::read_dir(support::graph_path(root).with_extension("sealed"))
        .map(|entries| {
            entries
                .map(Result::unwrap)
                .filter(|entry| entry.path().join("sealed.json").is_file())
                .count()
        })
        .unwrap_or(0)
}

fn publish_generation(
    registered: &RegisteredGraph,
    root: &std::path::Path,
    authority: &mut RelationalAuthority,
    identity: &GraphProjectionIdentity,
    generation: &str,
    expected: Option<GraphVerifiedHeadV1>,
    input: char,
) -> (GraphPublicationReplayRecordV1, VerifiedGraphCommit) {
    let manifest = manifest(identity.clone(), generation, generation, vec![], vec![]);
    let record = stage_manifest(
        authority,
        &registered.binding,
        &manifest,
        &format!("publish:{generation}"),
        expected,
        input,
    );
    let (control, probe) = control_and_probe();
    let commit = registered
        .registry
        .publish_verified(
            registration(registered.binding.clone(), root),
            authority,
            &fresh_context(&control, &probe),
            &record.publication.key,
            None,
        )
        .unwrap();
    (record, commit)
}

/// Installing a new head is the ordinary reclaim of every generation it
/// superseded: their journal rows are tombstoned and finalized, and their
/// sealed artifacts leave the disk, while the head — and any generation a
/// live reader still holds — stays.
#[test]
fn installing_a_head_retires_every_superseded_generation_it_no_longer_needs() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = canonical_projection("worktree.superseded");
    let root = temp.path();

    let (g1_record, g1_commit) = publish_generation(
        &registered,
        root,
        &mut authority,
        &identity,
        "sup-g1",
        None,
        '1',
    );
    let g1_head = g1_commit.head.clone();
    let (g2_record, g2_commit) = publish_generation(
        &registered,
        root,
        &mut authority,
        &identity,
        "sup-g2",
        Some(g1_head.clone()),
        '2',
    );
    let g2_head = g2_commit.head.clone();
    drop(g2_commit);
    let (g3_record, g3_commit) = publish_generation(
        &registered,
        root,
        &mut authority,
        &identity,
        "sup-g3",
        Some(g2_head),
        '3',
    );
    let g3_head = g3_commit.head.clone();
    drop(g3_commit);
    assert_eq!(
        sealed_generation_count(root),
        3,
        "every publish sealed its generation"
    );

    // g1's publication snapshot is still alive: it is a live reader, so the
    // first pass retires only g2.
    let (control, probe) = control_and_probe();
    let receipt = registered
        .registry
        .retire_superseded_projection_replays(
            registration(registered.binding.clone(), root),
            &mut authority,
            &fresh_context(&control, &probe),
            &g3_record.publication.key.projection,
        )
        .unwrap();
    assert_eq!(
        receipt,
        SupersededReplayRetirement {
            retired: 1,
            retained: 1,
            pending: 0,
        },
        "the superseded generation a live reader holds is retained"
    );
    assert!(authority.records.contains_key(&g1_record.publication.key));
    assert!(!authority.records.contains_key(&g2_record.publication.key));
    assert_eq!(
        authority
            .retired
            .get(&g2_record.publication.key)
            .map(|tombstone| tombstone.canonical_replay_source.is_none()),
        Some(true),
        "the retired replay is tombstoned and its cleanup finalized"
    );
    assert_eq!(sealed_generation_count(root), 2);
    assert_eq!(authority.head_retirement_calls, 0);

    // Once the reader is gone the remaining superseded generation goes too.
    drop(g1_commit);
    let (control, probe) = control_and_probe();
    let receipt = registered
        .registry
        .retire_superseded_projection_replays(
            registration(registered.binding.clone(), root),
            &mut authority,
            &fresh_context(&control, &probe),
            &g3_record.publication.key.projection,
        )
        .unwrap();
    assert_eq!(
        receipt,
        SupersededReplayRetirement {
            retired: 1,
            retained: 0,
            pending: 0,
        }
    );
    assert!(!authority.records.contains_key(&g1_record.publication.key));
    assert!(authority.records.contains_key(&g3_record.publication.key));
    assert_eq!(
        authority.heads.get(&g3_record.publication.key.projection),
        Some(&g3_head),
        "the installed head is never a retirement candidate"
    );
    assert_eq!(
        sealed_generation_count(root),
        1,
        "only the head's sealed artifact remains"
    );

    // A clean projection is a no-op, and the head still serves.
    let (control, probe) = control_and_probe();
    assert_eq!(
        registered
            .registry
            .retire_superseded_projection_replays(
                registration(registered.binding.clone(), root),
                &mut authority,
                &fresh_context(&control, &probe),
                &g3_record.publication.key.projection,
            )
            .unwrap(),
        SupersededReplayRetirement::default()
    );
    let (control, probe) = control_and_probe();
    let recovered = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), root),
            &mut authority,
            &fresh_context(&control, &probe),
            &g3_record.publication.key.projection,
        )
        .unwrap();
    assert_eq!(recovered.generation().as_str(), "sup-g3");
}

/// A generation large enough that its rows dominate the container.
fn bulk_manifest(
    identity: &GraphProjectionIdentity,
    generation: &str,
    entities: usize,
) -> GraphGenerationManifest {
    let payload = "x".repeat(512);
    GraphGenerationManifest::new(
        identity.clone(),
        GraphGenerationId::new(generation).unwrap(),
        SourceGeneration::new(format!("source:{generation}")).unwrap(),
        GraphWatermark::new(format!("watermark:{generation}")).unwrap(),
        vec![],
        (0..entities)
            .map(|index| entity(&format!("entity:{generation}:{index}"), &payload))
            .collect(),
        vec![],
    )
    .unwrap()
}

fn staging_container_bytes(root: &std::path::Path) -> u64 {
    let container = support::graph_path(root);
    [container.clone(), container.with_extension("grafeo.wal")]
        .into_iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Deleting a superseded generation's rows is what reclaims the staging
/// container: Grafeo writes each checkpoint out of place and truncates the
/// dead generation, so once retirement has removed the rows the file
/// converges to the live rows within two checkpoints. No compaction or
/// vacuum is involved; this is the mechanism live-container reclaim rests on.
#[test]
fn retiring_superseded_generations_shrinks_the_staging_container_on_checkpoint() {
    let temp = TempDir::new().unwrap();
    let registered = RegisteredGraph::new_mounted(temp.path()).unwrap();
    let mut authority = RelationalAuthority::default();
    let identity = canonical_projection("worktree.shrink");
    let root = temp.path();

    let mut expected = None;
    let mut last_record = None;
    for (generation, input) in [("shrink-g1", '1'), ("shrink-g2", '2'), ("shrink-g3", '3')] {
        let manifest = bulk_manifest(&identity, generation, 3_000);
        let record = stage_manifest(
            &mut authority,
            &registered.binding,
            &manifest,
            &format!("publish:{generation}"),
            expected.clone(),
            input,
        );
        let (control, probe) = control_and_probe();
        let commit = registered
            .registry
            .publish_verified(
                registration(registered.binding.clone(), root),
                &mut authority,
                &fresh_context(&control, &probe),
                &record.publication.key,
                None,
            )
            .unwrap();
        expected = Some(commit.head.clone());
        drop(commit);
        last_record = Some(record);
    }
    let head_record = last_record.unwrap();

    // Checkpoint with every generation's rows still present.
    assert!(registered.close().unwrap());
    let with_superseded_rows = staging_container_bytes(root);
    let lease = registered.reopen_lease().unwrap();
    drop(lease);

    let (control, probe) = control_and_probe();
    let receipt = registered
        .registry
        .retire_superseded_projection_replays(
            registration(registered.binding.clone(), root),
            &mut authority,
            &fresh_context(&control, &probe),
            &head_record.publication.key.projection,
        )
        .unwrap();
    assert_eq!(
        receipt,
        SupersededReplayRetirement {
            retired: 2,
            retained: 0,
            pending: 0,
        },
        "both superseded generations retire while the engine is open"
    );

    // Two checkpoints: the first may append the new generation past the dead
    // one, the second lands below it and truncates.
    assert!(registered.close().unwrap());
    let lease = registered.reopen_lease().unwrap();
    drop(lease);
    assert!(registered.close().unwrap());
    let after_retirement = staging_container_bytes(root);
    println!(
        "staging container: {with_superseded_rows} bytes with three generations, {after_retirement} bytes after retiring two"
    );
    assert!(
        after_retirement * 2 < with_superseded_rows,
        "retiring two of three generations must give back more than half of the container: \
         {with_superseded_rows} -> {after_retirement}"
    );

    // The head still serves after the rewrite.
    registered.mount().unwrap();
    let (control, probe) = control_and_probe();
    let recovered = registered
        .registry
        .recover_verified_snapshot(
            registration(registered.binding.clone(), root),
            &mut authority,
            &fresh_context(&control, &probe),
            &head_record.publication.key.projection,
        )
        .unwrap();
    assert_eq!(recovered.generation().as_str(), "shrink-g3");
}

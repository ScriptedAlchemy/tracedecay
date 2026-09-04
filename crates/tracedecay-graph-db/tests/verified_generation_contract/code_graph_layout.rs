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

use tracedecay_graph_db::{
    LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX, code_graph_shard_namespace,
    is_code_graph_shard_namespace, is_legacy_per_generation_code_graph_namespace,
};
use tracedecay_store::{CodeShardScopeV1, StoreShardScopeV1};

use super::*;

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
        authority
            .heads
            .get(&legacy_record.publication.key.projection)
            .is_none(),
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

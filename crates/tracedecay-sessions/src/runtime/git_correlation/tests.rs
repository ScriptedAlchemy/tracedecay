use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_graph_db::{
    GraphDbError, GraphGenerationId, GraphIdempotencyKey, GraphNamespace, GraphProjectorRevision,
    NeverCancelled, VerifiedGraphSnapshot,
};

use super::test_support::MemoryEvidenceGraphRuntime;
use super::*;

fn span(
    id: &str,
    provider: &str,
    session_id: &str,
    branch: Option<&str>,
    worktree: &str,
    first_ts: i64,
    last_ts: i64,
) -> SessionGitSpan {
    SessionGitSpan {
        span_id: id.to_owned(),
        provider: provider.to_owned(),
        session_id: session_id.to_owned(),
        thread_id: None,
        branch: branch.map(str::to_owned),
        worktree: worktree.to_owned(),
        first_ts,
        last_ts,
        event_count: 2,
        source: SpanSource::Ingest,
    }
}

fn commit(
    sha: &str,
    provider: &str,
    session_id: &str,
    relation: CommitRelation,
    confidence: i64,
) -> CommitSessionRecord {
    CommitSessionRecord {
        commit_sha: sha.to_owned(),
        provider: provider.to_owned(),
        session_id: session_id.to_owned(),
        branch: Some("main".to_owned()),
        worktree: Some("/repo".to_owned()),
        committed_at: 150,
        span_overlap_kind: SpanOverlapKind::Direct,
        span_id: None,
        relation,
        evidence: if relation == CommitRelation::Produced {
            CommitEvidence::ToolResult
        } else {
            CommitEvidence::HeadObservation
        },
        confidence,
        evidence_message_id: Some("message-1".to_owned()),
    }
}

fn projection() -> GitEvidenceProjectionV1 {
    GitEvidenceProjectionV1::new(
        "watermark-7",
        vec![
            span("span-a", "", "session-a", Some("main"), "/repo/", 100, 200),
            span(
                "span-b",
                "claude",
                "session-a",
                Some("feature"),
                "/repo",
                210,
                240,
            ),
            span(
                "span-c",
                "codex",
                "session-b",
                Some("main"),
                "/other",
                120,
                180,
            ),
        ],
        vec![
            commit(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "claude",
                "session-a",
                CommitRelation::Produced,
                100,
            ),
            commit(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
                "codex",
                "session-b",
                CommitRelation::Observed,
                60,
            ),
        ],
    )
    .unwrap()
}

#[test]
fn normalize_worktree_preserves_root_and_removes_aliases() {
    assert_eq!(normalize_worktree("/repo///"), "/repo");
    assert_eq!(normalize_worktree(r"C:\repo\\"), "C:/repo");
    assert_eq!(normalize_worktree("/"), "/");
    assert_eq!(normalize_worktree("/private/var/tmp/repo"), "/var/tmp/repo");
}

#[test]
fn git_ref_filter_parses_and_validates_kinds() {
    assert_eq!(
        GitRefFilter::parse("worktree", r"C:\repo\\").unwrap(),
        GitRefFilter::Worktree("C:/repo".to_owned())
    );
    assert!(GitRefFilter::parse("commit", "abc").is_err());
    assert!(GitRefFilter::parse("tag", "v1").is_err());
}

#[test]
fn projection_is_canonical_and_normalizes_evidence() {
    let projection = projection();
    assert_eq!(projection.source_watermark(), "watermark-7");
    assert_eq!(
        projection
            .spans()
            .iter()
            .map(|span| span.span_id.as_str())
            .collect::<Vec<_>>(),
        vec!["span-a", "span-b", "span-c"]
    );
    assert_eq!(projection.spans()[0].worktree, "/repo");
    // span-a arrives with an empty provider; the canonical session map
    // must settle it from the sibling span of the same session.
    assert_eq!(projection.spans()[0].provider, "claude");
    assert_eq!(projection.commit_sessions()[0].provider, "claude");
}

#[test]
fn projection_rejects_duplicate_or_dangling_evidence() {
    let duplicate = span("same", "claude", "session-a", Some("main"), "/repo", 1, 2);
    assert!(
        GitEvidenceProjectionV1::new("watermark", vec![duplicate.clone(), duplicate], Vec::new())
            .is_err()
    );

    let mut dangling = commit(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "claude",
        "session-a",
        CommitRelation::Produced,
        100,
    );
    dangling.span_id = Some("missing".to_owned());
    assert!(GitEvidenceProjectionV1::new("watermark", Vec::new(), vec![dangling]).is_err());
}

#[test]
fn branch_query_aggregates_spans_per_session() {
    let hits = projection().sessions_for(
        &SessionsForQuery {
            git_ref: GitRefFilter::Branch("main".to_owned()),
            since: Some(110),
            until: Some(210),
            limit: 10,
        },
        CommitRelationFilter::Produced,
    );
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].session_id, "session-a");
    assert_eq!(hits[0].provider, "claude");
    assert_eq!(hits[0].event_count, 2);
    assert_eq!(hits[0].span_count, 1);
}

#[test]
fn commit_query_defaults_to_producer_evidence() {
    let projection = projection();
    let query = SessionsForQuery {
        git_ref: GitRefFilter::Commit("aaaaaa".to_owned()),
        since: None,
        until: None,
        limit: 10,
    };
    let produced = projection.sessions_for(&query, CommitRelationFilter::Produced);
    assert_eq!(produced.len(), 1);
    assert_eq!(produced[0].session_id, "session-a");

    let all = projection.sessions_for(&query, CommitRelationFilter::All);
    assert_eq!(all.len(), 2);
}

#[test]
fn compound_scope_intersects_branch_worktree_and_commit() {
    let ids = projection()
        .session_ids_for_scope(&GitScopeFilter {
            branch: Some("main".to_owned()),
            worktree: Some("/repo".to_owned()),
            commit: Some("aaaaaa".to_owned()),
        })
        .unwrap();
    assert_eq!(ids, vec![("claude".to_owned(), "session-a".to_owned())]);
}

#[test]
fn commit_scope_falls_back_to_observed_when_no_producer_matches() {
    let ids = projection()
        .session_ids_for_scope(&GitScopeFilter {
            branch: None,
            worktree: None,
            commit: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab".to_owned()),
        })
        .unwrap();
    assert_eq!(ids, vec![("codex".to_owned(), "session-b".to_owned())]);
}

#[test]
fn manifest_encodes_sessions_spans_commits_and_evidence_relations() {
    let projection = projection();
    let identity =
        git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap();
    let revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned()).unwrap();
    let manifest =
        build_git_evidence_manifest_checked(identity, &projection, &revision, &|| Ok(())).unwrap();

    assert_eq!(manifest.entities.len(), 8);
    assert_eq!(manifest.relations.len(), 5);
    assert_eq!(manifest.source_generation.as_str(), "watermark-7");
    assert_eq!(manifest.watermark.as_str(), "watermark-7");
}

#[test]
fn manifest_generation_is_content_addressed() {
    let projection = projection();
    let revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned()).unwrap();
    let first = git_evidence_generation_id(&projection, &revision).unwrap();
    let second = git_evidence_generation_id(&projection, &revision).unwrap();
    assert_eq!(first, second);

    let changed = GitEvidenceProjectionV1::new(
        "watermark-8",
        projection.spans().to_vec(),
        projection.commit_sessions().to_vec(),
    )
    .unwrap();
    assert_ne!(
        first,
        git_evidence_generation_id(&changed, &revision).unwrap()
    );
}

#[test]
fn cancellation_check_is_observed_while_building_manifest() {
    let projection = projection();
    let identity =
        git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap();
    let revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned()).unwrap();
    let cancelled = Arc::new(AtomicBool::new(true));
    let result = build_git_evidence_manifest_checked(identity, &projection, &revision, &|| {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            Err(GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    });
    assert_eq!(result.unwrap_err(), GitCorrelationError::Cancelled);
}

#[test]
fn publication_returns_committed_projection_after_commit_point_cancellation() {
    let runtime = MemoryEvidenceGraphRuntime::default();
    runtime.cancel_request_after_next_publish();
    let projection = projection();
    let revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned()).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let published = publish_git_evidence_projection(
        &runtime,
        git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap(),
        &projection,
        &revision,
        GraphIdempotencyKey::new("git-evidence-post-commit-cancellation").unwrap(),
        Arc::clone(&cancelled),
    )
    .expect("verified-head publication already committed");

    assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(published.projection(), &projection);
}

#[test]
fn recovery_rejects_a_generation_not_bound_to_canonical_projection_bytes() {
    let projection = projection();
    let identity =
        git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap();
    let revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned()).unwrap();
    let mut manifest =
        build_git_evidence_manifest_checked(identity, &projection, &revision, &|| Ok(())).unwrap();
    manifest.generation = GraphGenerationId::new("foreign-git-evidence-generation").unwrap();
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled)).unwrap();

    let error =
        GitEvidenceProjectionStore::from_verified_snapshot(snapshot, Arc::new(NeverCancelled))
            .unwrap_err();
    assert!(
        matches!(error, GitCorrelationError::Corrupt(detail) if detail.contains("generation mismatch"))
    );
}

#[test]
fn recovery_rejects_a_foreign_namespace_with_canonical_projection_bytes() {
    let projection = projection();
    let identity =
        git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap();
    let revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned()).unwrap();
    let mut manifest =
        build_git_evidence_manifest_checked(identity, &projection, &revision, &|| Ok(())).unwrap();
    let foreign_identity =
        git_evidence_projection_identity(GraphNamespace::new("foreign").unwrap()).unwrap();
    manifest.projection.clone_from(&foreign_identity);
    for relation in &mut manifest.relations {
        relation.from.projection.clone_from(&foreign_identity);
        relation.to.projection.clone_from(&foreign_identity);
    }
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled)).unwrap();

    let error =
        GitEvidenceProjectionStore::from_verified_snapshot(snapshot, Arc::new(NeverCancelled))
            .unwrap_err();
    assert!(
        matches!(error, GitCorrelationError::Corrupt(detail) if detail.contains("foreign projection identity"))
    );
}

#[test]
fn publication_rejects_a_foreign_namespace_before_verified_head_cas() {
    let runtime = MemoryEvidenceGraphRuntime::default();
    let projection = projection();
    let foreign_identity =
        git_evidence_projection_identity(GraphNamespace::new("foreign").unwrap()).unwrap();
    let revision =
        GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION_V1.to_owned()).unwrap();

    let error = publish_git_evidence_projection(
        &runtime,
        foreign_identity.clone(),
        &projection,
        &revision,
        GraphIdempotencyKey::new("foreign-git-evidence-publication").unwrap(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err();
    assert!(matches!(error, GitCorrelationError::Contract(_)));
    assert!(
        recover_git_evidence_projection(
            &runtime,
            &foreign_identity,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
        .is_none(),
        "foreign verified head must not be published"
    );
}

#[test]
fn recovery_observes_request_cancellation_before_reading_a_snapshot() {
    let runtime = MemoryEvidenceGraphRuntime::default();
    let identity =
        git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap();
    let result =
        recover_git_evidence_projection(&runtime, &identity, Arc::new(AtomicBool::new(true)));
    assert_eq!(result.unwrap_err(), GitCorrelationError::Cancelled);
}

#[test]
fn recovery_observes_request_cancellation_while_reading_a_snapshot() {
    let runtime = Arc::new(MemoryEvidenceGraphRuntime::default());
    runtime.gate_snapshot_reads();
    let cancelled = Arc::new(AtomicBool::new(false));
    let reader_runtime = Arc::clone(&runtime);
    let reader_cancelled = Arc::clone(&cancelled);
    let reader = std::thread::spawn(move || {
        let identity =
            git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap();
        recover_git_evidence_projection(reader_runtime.as_ref(), &identity, reader_cancelled)
    });
    runtime.await_gated_snapshot_reader();
    assert_eq!(
        runtime.gated_snapshot_readers_entered(),
        1,
        "the reader must be inside the snapshot read before cancellation"
    );
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    runtime.release_gated_snapshot_reads();

    assert_eq!(
        reader.join().unwrap().unwrap_err(),
        GitCorrelationError::Cancelled
    );
}

//! The indexed graph view must answer exactly what the complete in-memory
//! projection answers, while touching rows in proportion to the answer.
//!
//! Every query below is evaluated twice — through
//! [`GitEvidenceProjectionV1`] over the full projection (the oracle the
//! facades used to hydrate on every read) and through
//! [`GitEvidenceGraphView`] over the published graph — and the results must be
//! identical, ordering included. Row-touch scaling is asserted with the graph
//! store's thread-scoped decode counters rather than wall time.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_graph_db::{
    GraphDbError, GraphIdempotencyKey, GraphNamespace, GraphProjectionIdentity,
    GraphProjectorRevision, GraphProperty, GraphPropertyName, NeverCancelled,
    VerifiedGraphSnapshot, take_graph_db_traversal_counters,
};
use tracedecay_runtime_core::shard_runtime::VerifiedGraphRuntimePortV1;

use super::test_support::MemoryEvidenceGraphRuntime;
use super::*;

const BASE_TS: i64 = 1_700_000_000;

fn identity() -> GraphProjectionIdentity {
    git_evidence_projection_identity(GraphNamespace::new("project").unwrap()).unwrap()
}

fn revision() -> GraphProjectorRevision {
    GraphProjectorRevision::try_from(GIT_EVIDENCE_PROJECTOR_REVISION.to_owned()).unwrap()
}

fn never_cancelled() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

struct FlagCancellation(Arc<AtomicBool>);

impl tracedecay_graph_db::GraphCancellation for FlagCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn sha(bucket: usize, commit_in_bucket: usize) -> String {
    format!("{bucket:06x}{commit_in_bucket}{}", "a".repeat(33))
}

/// `sessions` sessions with several spans each and commit records that share
/// six-digit prefixes, so every bounded path (newest-first paging with ties,
/// per-session aggregation, prefix buckets holding several commits, producer
/// fallback) has real work to do.
fn seeded_projection(sessions: usize) -> GitEvidenceProjectionV1 {
    let mut spans = Vec::new();
    let mut commits = Vec::new();
    for index in 0..sessions {
        let session_id = format!("s{index:05}");
        let provider = if index % 2 == 0 { "codex" } else { "claude" };
        // Groups of three sessions end their `main` activity at the same
        // second so limit boundaries fall inside ties.
        let main_first = BASE_TS + 100 * (index / 3) as i64;
        spans.push(SessionGitSpan {
            span_id: format!("span-main-{index}"),
            // Unattributed hook spans are settled by the canonical provider.
            provider: if index % 6 == 0 {
                String::new()
            } else {
                provider.to_owned()
            },
            session_id: session_id.clone(),
            thread_id: None,
            branch: Some("main".to_owned()),
            worktree: "/repo".to_owned(),
            first_ts: main_first,
            last_ts: main_first + 50,
            event_count: 2,
            source: SpanSource::Ingest,
        });
        if index % 4 == 0 {
            spans.push(SessionGitSpan {
                span_id: format!("span-main-early-{index}"),
                provider: provider.to_owned(),
                session_id: session_id.clone(),
                thread_id: None,
                branch: Some("main".to_owned()),
                worktree: "/repo".to_owned(),
                first_ts: main_first - 500,
                last_ts: main_first - 480,
                event_count: 3,
                source: SpanSource::Backfill,
            });
        }
        spans.push(SessionGitSpan {
            span_id: format!("span-feature-{index}"),
            provider: provider.to_owned(),
            session_id: session_id.clone(),
            thread_id: Some(format!("thread-{index}")),
            branch: (index % 5 != 0).then(|| format!("feature-{}", index % 7)),
            worktree: format!("/wt-{}", index % 3),
            first_ts: BASE_TS + 100 * index as i64 + 10,
            last_ts: BASE_TS + 100 * index as i64 + 40,
            event_count: 1,
            source: SpanSource::HookRoute,
        });
        let commit_sha = sha(index / 8, (index / 4) % 2);
        let produced = index % 4 == 0;
        commits.push(CommitSessionRecord {
            commit_sha: commit_sha.clone(),
            provider: provider.to_owned(),
            session_id: session_id.clone(),
            branch: Some("main".to_owned()),
            worktree: Some("/repo".to_owned()),
            committed_at: BASE_TS + 100 * index as i64,
            span_overlap_kind: if produced {
                SpanOverlapKind::Direct
            } else {
                SpanOverlapKind::WithinSpan
            },
            span_id: Some(format!("span-main-{index}")),
            relation: if produced {
                CommitRelation::Produced
            } else {
                CommitRelation::Observed
            },
            evidence: if produced {
                CommitEvidence::ToolResult
            } else {
                CommitEvidence::TimeOverlap
            },
            confidence: if produced {
                100
            } else {
                10 + (index % 4) as i64 * 10
            },
            evidence_message_id: produced.then(|| format!("message-{index}")),
        });
        if index % 10 == 0 && index + 8 < sessions {
            // A second, weaker record for the same session on a different
            // commit in the neighbouring bucket.
            commits.push(CommitSessionRecord {
                commit_sha: sha(index / 8 + 1, 0),
                provider: provider.to_owned(),
                session_id,
                branch: Some("main".to_owned()),
                worktree: Some("/repo".to_owned()),
                committed_at: BASE_TS + 100 * index as i64 + 5,
                span_overlap_kind: SpanOverlapKind::ExtendedWindow,
                span_id: None,
                relation: CommitRelation::Observed,
                evidence: CommitEvidence::ReflogOverlap,
                confidence: 30,
                evidence_message_id: None,
            });
        }
    }
    GitEvidenceProjectionV1::new(format!("watermark-{sessions}"), spans, commits).unwrap()
}

fn publish(projection: &GitEvidenceProjectionV1) -> MemoryEvidenceGraphRuntime {
    let runtime = MemoryEvidenceGraphRuntime::default();
    publish_git_evidence_projection(
        &runtime,
        identity(),
        projection,
        &revision(),
        GraphIdempotencyKey::new(format!("graph-view-{}", projection.source_watermark())).unwrap(),
        never_cancelled(),
    )
    .unwrap();
    runtime
}

fn open_indexed(runtime: &MemoryEvidenceGraphRuntime) -> GitEvidenceGraphView {
    match open_git_evidence_graph_view(runtime, &identity(), Arc::new(NeverCancelled)).unwrap() {
        GitEvidenceGraphHead::Indexed(view) => view,
        GitEvidenceGraphHead::Unpublished => panic!("projection was published"),
        GitEvidenceGraphHead::Legacy { generation } => {
            panic!("current publication reported legacy generation {generation}")
        }
    }
}

fn query(
    git_ref: GitRefFilter,
    since: Option<i64>,
    until: Option<i64>,
    limit: usize,
) -> SessionsForQuery {
    SessionsForQuery {
        git_ref,
        since,
        until,
        limit,
    }
}

fn sessions_for_queries(sessions: usize) -> Vec<(SessionsForQuery, CommitRelationFilter)> {
    let mid = BASE_TS + 100 * (sessions / 6) as i64;
    let late = BASE_TS + 100 * (sessions / 3) as i64;
    let mut queries = Vec::new();
    for git_ref in [
        GitRefFilter::Branch("main".to_owned()),
        GitRefFilter::Branch("feature-3".to_owned()),
        GitRefFilter::Branch("never-seen".to_owned()),
        GitRefFilter::Worktree("/repo".to_owned()),
        GitRefFilter::Worktree("/wt-1".to_owned()),
        GitRefFilter::Worktree("/elsewhere".to_owned()),
    ] {
        for limit in [1, 2, 4, 7, 25, MAX_SESSIONS_FOR_LIMIT, 10_000] {
            for (since, until) in [
                (None, None),
                (Some(mid), None),
                (None, Some(mid)),
                (Some(mid), Some(late)),
                (Some(late + 1), Some(late + 1)),
                (Some(BASE_TS - 10_000), Some(BASE_TS - 9_000)),
            ] {
                queries.push((
                    query(git_ref.clone(), since, until, limit),
                    CommitRelationFilter::Produced,
                ));
            }
        }
    }
    let bucket = sha(1, 0);
    for commit in [
        bucket[..6].to_owned(),
        bucket[..7].to_owned(),
        bucket.clone(),
        sha(0, 1),
        sha(sessions / 8 + 4, 0)[..6].to_owned(),
        format!("{:06x}", 0xfffff0),
    ] {
        for relation in [
            CommitRelationFilter::Produced,
            CommitRelationFilter::Observed,
            CommitRelationFilter::All,
        ] {
            for limit in [1, 3, 50] {
                for (since, until) in [(None, None), (Some(mid), None), (None, Some(mid))] {
                    queries.push((
                        query(GitRefFilter::Commit(commit.clone()), since, until, limit),
                        relation,
                    ));
                }
            }
        }
    }
    queries
}

fn scope_filters(sessions: usize) -> Vec<GitScopeFilter> {
    let bucket = sha(1, 0);
    let mut filters = Vec::new();
    for branch in [None, Some("main"), Some("feature-2"), Some("never-seen")] {
        for worktree in [None, Some("/repo"), Some("/wt-2"), Some("/elsewhere")] {
            for commit in [
                None,
                Some(bucket[..6].to_owned()),
                Some(bucket.clone()),
                Some(sha(sessions / 8 + 9, 1)),
            ] {
                filters.push(GitScopeFilter {
                    branch: branch.map(str::to_owned),
                    worktree: worktree.map(str::to_owned),
                    commit: commit.clone(),
                });
            }
        }
    }
    filters
}

#[test]
fn graph_view_matches_full_projection_for_every_query_shape() {
    const SESSIONS: usize = 240;
    let projection = seeded_projection(SESSIONS);
    let runtime = publish(&projection);
    let view = open_indexed(&runtime);

    for (query, relation) in sessions_for_queries(SESSIONS) {
        let expected = projection.sessions_for(&query, relation);
        let observed = view.sessions_for(&query, relation).unwrap();
        assert_eq!(observed, expected, "query {query:?} relation {relation:?}");
    }
    for filter in scope_filters(SESSIONS) {
        let expected = projection.session_ids_for_scope(&filter);
        let observed = view.session_ids_for_scope(&filter).unwrap();
        assert_eq!(observed, expected, "scope {filter:?}");
    }
    assert_eq!(
        view.health(Some(7)).span_count,
        projection.spans().len() as u64
    );
    assert_eq!(
        view.health(Some(7)).commit_count,
        projection.commit_sessions().len() as u64
    );
    assert_eq!(
        view.health(Some(7)).source_watermark.as_deref(),
        Some("watermark-240")
    );
    assert_eq!(view.health(Some(7)).backfill_watermark, Some(7));
    let presence = view.presence(None);
    assert!(presence.projection_available && presence.spans_present && presence.commits_present);
    assert_eq!(
        presence.generation.as_deref(),
        Some(view.verified_snapshot().generation().as_str())
    );
}

#[test]
fn graph_view_reports_bounded_empty_row_families() {
    let spans_only = GitEvidenceProjectionV1::new(
        "spans-only",
        vec![SessionGitSpan {
            span_id: "only".to_owned(),
            provider: "codex".to_owned(),
            session_id: "s".to_owned(),
            thread_id: None,
            branch: Some("main".to_owned()),
            worktree: "/repo".to_owned(),
            first_ts: 1,
            last_ts: 2,
            event_count: 1,
            source: SpanSource::Ingest,
        }],
        Vec::new(),
    )
    .unwrap();
    let view = open_indexed(&publish(&spans_only));
    let presence = view.presence(None);
    assert!(presence.spans_present);
    assert!(!presence.commits_present);
    assert_eq!(view.health(None).commit_count, 0);
    assert_eq!(
        view.sessions_for(
            &query(GitRefFilter::Commit("abcdef".to_owned()), None, None, 5),
            CommitRelationFilter::All,
        )
        .unwrap(),
        Vec::new()
    );
}

/// Property decodes are the graph store's unit of "rows hydrated": every entity
/// or relation whose payload is materialised counts once. A bounded read must
/// hydrate the same rows on a store ten times larger.
fn decodes<T>(read: impl FnOnce() -> T) -> (T, u64) {
    take_graph_db_traversal_counters();
    let value = read();
    (value, take_graph_db_traversal_counters().property_decodes)
}

#[test]
fn bounded_reads_hydrate_rows_in_proportion_to_the_answer_not_the_store() {
    let small = seeded_projection(240);
    let large = seeded_projection(2_400);
    let small_runtime = publish(&small);
    let large_runtime = publish(&large);

    // The full recovery every facade used to run: it hydrates the store.
    let (_, small_full) = decodes(|| {
        recover_git_evidence_projection(&small_runtime, &identity(), never_cancelled())
            .unwrap()
            .unwrap()
    });
    let (_, large_full) = decodes(|| {
        recover_git_evidence_projection(&large_runtime, &identity(), never_cancelled())
            .unwrap()
            .unwrap()
    });
    assert!(
        small_full >= 240 && large_full >= 2_400 && large_full >= small_full * 9,
        "full recovery must scale with the store: {small_full} vs {large_full}"
    );

    let (small_view, small_open) = decodes(|| open_indexed(&small_runtime));
    let (large_view, large_open) = decodes(|| open_indexed(&large_runtime));
    assert_eq!(
        small_open, large_open,
        "opening the view reads only projection metadata"
    );
    assert_eq!(small_open, 1);

    let (small_health, _) = decodes(|| small_view.health(None));
    assert_eq!(small_health.span_count, small.spans().len() as u64);

    let bounded = [
        (
            query(GitRefFilter::Branch("main".to_owned()), None, None, 5),
            CommitRelationFilter::Produced,
        ),
        (
            query(GitRefFilter::Worktree("/repo".to_owned()), None, None, 3),
            CommitRelationFilter::Produced,
        ),
        (
            query(
                GitRefFilter::Branch("main".to_owned()),
                None,
                Some(BASE_TS + 100 * 20),
                4,
            ),
            CommitRelationFilter::Produced,
        ),
        (
            query(GitRefFilter::Commit(sha(3, 1)), None, None, 10),
            CommitRelationFilter::All,
        ),
        (
            query(
                GitRefFilter::Commit(sha(3, 1)[..6].to_owned()),
                None,
                None,
                10,
            ),
            CommitRelationFilter::Observed,
        ),
    ];
    for (query, relation) in bounded {
        let (small_hits, small_decodes) = decodes(|| small_view.sessions_for(&query, relation));
        let (large_hits, large_decodes) = decodes(|| large_view.sessions_for(&query, relation));
        let small_hits = small_hits.unwrap();
        let large_hits = large_hits.unwrap();
        assert_eq!(small_hits, small.sessions_for(&query, relation));
        assert_eq!(large_hits, large.sessions_for(&query, relation));
        assert_eq!(small_hits.len(), large_hits.len(), "{query:?}");
        assert_eq!(
            small_decodes, large_decodes,
            "{query:?} hydrated {small_decodes} rows on 240 sessions but {large_decodes} on 2400"
        );
        assert!(
            large_decodes < large_full / 20,
            "{query:?} hydrated {large_decodes} rows against {large_full} for full recovery"
        );
    }

    // Scope resolution returns every matching session, so its row budget is
    // the answer itself: the intersected sessions, never spans, unrelated
    // evidence, or the sessions a single selector would have matched alone.
    for filter in [
        GitScopeFilter {
            branch: Some("feature-3".to_owned()),
            worktree: None,
            commit: None,
        },
        GitScopeFilter {
            branch: Some("feature-3".to_owned()),
            worktree: Some("/wt-1".to_owned()),
            commit: None,
        },
    ] {
        let (small_ids, small_decodes) = decodes(|| small_view.session_ids_for_scope(&filter));
        let (large_ids, large_decodes) = decodes(|| large_view.session_ids_for_scope(&filter));
        let small_ids = small_ids.unwrap().unwrap();
        let large_ids = large_ids.unwrap().unwrap();
        assert_eq!(small_ids, small.session_ids_for_scope(&filter).unwrap());
        assert_eq!(large_ids, large.session_ids_for_scope(&filter).unwrap());
        assert_eq!(small_decodes, small_ids.len() as u64, "{filter:?}");
        assert_eq!(large_decodes, large_ids.len() as u64, "{filter:?}");
    }
    // A commit-scoped intersection hydrates the commit's records and nothing
    // else: the branch selector contributes index keys only.
    let filter = GitScopeFilter {
        branch: Some("main".to_owned()),
        worktree: None,
        commit: Some(sha(2, 0)[..6].to_owned()),
    };
    let (large_ids, large_decodes) = decodes(|| large_view.session_ids_for_scope(&filter));
    assert_eq!(
        large_ids.unwrap().unwrap(),
        large.session_ids_for_scope(&filter).unwrap()
    );
    let bucket_records = large
        .commit_sessions()
        .iter()
        .filter(|record| record.commit_sha.starts_with(&sha(2, 0)[..6]))
        .count() as u64;
    // The bucket's two commits (edge plus target each) and one decode per
    // record relation.
    assert_eq!(large_decodes, 2 * 2 + bucket_records, "{filter:?}");
}

#[test]
fn unpublished_projection_is_a_typed_absent_head() {
    let runtime = MemoryEvidenceGraphRuntime::default();
    assert!(matches!(
        open_git_evidence_graph_view(&runtime, &identity(), Arc::new(NeverCancelled)).unwrap(),
        GitEvidenceGraphHead::Unpublished
    ));
}

#[test]
fn single_selector_scope_resolution_stops_after_the_caller_bound() {
    let projection = seeded_projection(2_400);
    let runtime = publish(&projection);
    let view = open_indexed(&runtime);
    let filter = GitScopeFilter {
        branch: Some("feature-3".to_owned()),
        worktree: None,
        commit: None,
    };

    let (ids, decodes) = decodes(|| view.session_ids_for_scope_bounded(&filter, 101));
    let ids = ids.unwrap().unwrap();
    assert_eq!(ids.len(), 101);
    assert_eq!(decodes, 101);
}

#[test]
fn legacy_head_is_fully_recoverable_but_serves_no_bounded_reads() {
    let projection = seeded_projection(24);
    let runtime = MemoryEvidenceGraphRuntime::default();
    let manifest = legacy_git_evidence_manifest_for_test(identity(), &projection).unwrap();
    let generation = manifest.generation.clone();
    runtime
        .publish_verified_manifest(
            &manifest,
            GraphIdempotencyKey::new("legacy-head").unwrap(),
            never_cancelled(),
        )
        .unwrap();

    match open_git_evidence_graph_view(&runtime, &identity(), Arc::new(NeverCancelled)).unwrap() {
        GitEvidenceGraphHead::Legacy {
            generation: observed,
        } => assert_eq!(observed, generation),
        GitEvidenceGraphHead::Unpublished => panic!("legacy head was published"),
        GitEvidenceGraphHead::Indexed(_) => panic!("legacy head carries no index"),
    }

    let recovered = recover_git_evidence_projection(&runtime, &identity(), never_cancelled())
        .unwrap()
        .expect("legacy rows recover in full");
    assert_eq!(
        recovered.projector_revision(),
        GitEvidenceProjectorRevision::LegacyV1
    );
    assert_eq!(recovered.projection(), &projection);

    // Re-projecting the same content publishes an indexed successor.
    let republished = publish_git_evidence_projection(
        &runtime,
        identity(),
        recovered.projection(),
        &revision(),
        GraphIdempotencyKey::new("legacy-head-reprojected").unwrap(),
        never_cancelled(),
    )
    .unwrap();
    assert_ne!(republished.verified_snapshot().generation(), &generation);
    let view = open_indexed(&runtime);
    assert_eq!(
        view.verified_snapshot().generation(),
        republished.verified_snapshot().generation()
    );
    assert_eq!(
        view.health(None).span_count,
        projection.spans().len() as u64
    );
}

#[test]
fn unknown_recorded_projector_revision_is_corrupt_on_both_read_paths() {
    let projection = seeded_projection(6);
    let mut manifest =
        build_git_evidence_manifest_checked(identity(), &projection, &revision(), &|| Ok(()))
            .unwrap();
    for entity in &mut manifest.entities {
        if entity.identity.as_str() == "projection:session-git-evidence" {
            entity.properties.insert(
                GraphPropertyName::new("projector-revision").unwrap(),
                GraphProperty::String("session-git-evidence-projector.v9".to_owned()),
            );
        }
    }
    let runtime = MemoryEvidenceGraphRuntime::default();
    runtime
        .publish_verified_manifest(
            &manifest,
            GraphIdempotencyKey::new("future-revision").unwrap(),
            never_cancelled(),
        )
        .unwrap();

    let view_error =
        open_git_evidence_graph_view(&runtime, &identity(), Arc::new(NeverCancelled)).unwrap_err();
    assert!(
        matches!(&view_error, GitCorrelationError::Corrupt(detail) if detail.contains("projector.v9")),
        "{view_error}"
    );
    let recovery_error =
        recover_git_evidence_projection(&runtime, &identity(), never_cancelled()).unwrap_err();
    assert!(
        matches!(&recovery_error, GitCorrelationError::Corrupt(detail) if detail.contains("projector.v9")),
        "{recovery_error}"
    );
}

#[test]
fn missing_count_metadata_is_corrupt_not_empty() {
    let projection = seeded_projection(6);
    let mut manifest =
        build_git_evidence_manifest_checked(identity(), &projection, &revision(), &|| Ok(()))
            .unwrap();
    for entity in &mut manifest.entities {
        if entity.identity.as_str() == "projection:session-git-evidence" {
            entity
                .properties
                .remove(&GraphPropertyName::new("span-count").unwrap());
        }
    }
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled)).unwrap();
    let runtime = MemoryEvidenceGraphRuntime::default();
    runtime.install_snapshot(snapshot);
    let error =
        open_git_evidence_graph_view(&runtime, &identity(), Arc::new(NeverCancelled)).unwrap_err();
    assert!(
        matches!(&error, GitCorrelationError::Corrupt(detail) if detail.contains("span-count")),
        "{error}"
    );
}

#[test]
fn foreign_projection_identity_is_corrupt_for_the_graph_view() {
    let projection = seeded_projection(6);
    let mut manifest =
        build_git_evidence_manifest_checked(identity(), &projection, &revision(), &|| Ok(()))
            .unwrap();
    let foreign =
        git_evidence_projection_identity(GraphNamespace::new("foreign").unwrap()).unwrap();
    manifest.projection.clone_from(&foreign);
    for relation in &mut manifest.relations {
        relation.from.projection.clone_from(&foreign);
        relation.to.projection.clone_from(&foreign);
    }
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled)).unwrap();
    let runtime = MemoryEvidenceGraphRuntime::default();
    runtime.install_snapshot(snapshot);
    let error =
        open_git_evidence_graph_view(&runtime, &foreign, Arc::new(NeverCancelled)).unwrap_err();
    assert!(
        matches!(&error, GitCorrelationError::Corrupt(detail) if detail.contains("foreign projection identity")),
        "{error}"
    );
}

#[test]
fn graph_view_observes_cancellation_before_and_during_reads() {
    let projection = seeded_projection(24);
    let runtime = publish(&projection);
    assert_eq!(
        open_git_evidence_graph_view(
            &runtime,
            &identity(),
            Arc::new(FlagCancellation(Arc::new(AtomicBool::new(true)))),
        )
        .unwrap_err(),
        GitCorrelationError::Cancelled
    );

    let cancelled = never_cancelled();
    let GitEvidenceGraphHead::Indexed(view) = open_git_evidence_graph_view(
        &runtime,
        &identity(),
        Arc::new(FlagCancellation(Arc::clone(&cancelled))),
    )
    .unwrap() else {
        panic!("published projection must open as indexed");
    };
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        view.sessions_for(
            &query(GitRefFilter::Branch("main".to_owned()), None, None, 5),
            CommitRelationFilter::Produced,
        )
        .unwrap_err(),
        GitCorrelationError::Cancelled
    );
    assert_eq!(
        view.session_ids_for_scope(&GitScopeFilter {
            branch: Some("main".to_owned()),
            worktree: None,
            commit: None,
        })
        .unwrap_err(),
        GitCorrelationError::Cancelled
    );
    // Metadata was read at open time and needs no further graph access.
    assert!(view.presence(None).spans_present);
}

#[test]
fn graph_view_retains_its_generation_across_a_racing_publication() {
    let first = seeded_projection(12);
    let runtime = publish(&first);
    let view = open_indexed(&runtime);
    let first_generation = view.verified_snapshot().generation().clone();

    let second = seeded_projection(36);
    publish_git_evidence_projection(
        &runtime,
        identity(),
        &second,
        &revision(),
        GraphIdempotencyKey::new("successor").unwrap(),
        never_cancelled(),
    )
    .unwrap();

    // The admitted reader keeps answering from the generation it opened.
    assert_eq!(view.verified_snapshot().generation(), &first_generation);
    assert_eq!(view.health(None).span_count, first.spans().len() as u64);
    let query = query(GitRefFilter::Branch("main".to_owned()), None, None, 50);
    assert_eq!(
        view.sessions_for(&query, CommitRelationFilter::Produced)
            .unwrap(),
        first.sessions_for(&query, CommitRelationFilter::Produced)
    );
    // New readers see the successor.
    let successor = open_indexed(&runtime);
    assert_ne!(
        successor.verified_snapshot().generation(),
        &first_generation
    );
    assert_eq!(
        successor
            .sessions_for(&query, CommitRelationFilter::Produced)
            .unwrap(),
        second.sessions_for(&query, CommitRelationFilter::Produced)
    );
}

#[test]
fn span_index_key_orders_newest_activity_first_across_the_i64_range() {
    let span = |first_ts: i64, last_ts: i64| SessionGitSpan {
        span_id: format!("span-{first_ts}-{last_ts}"),
        provider: "codex".to_owned(),
        session_id: "s".to_owned(),
        thread_id: None,
        branch: None,
        worktree: "/repo".to_owned(),
        first_ts,
        last_ts,
        event_count: 1,
        source: SpanSource::Ingest,
    };
    let keys = [
        span(i64::MIN, i64::MIN),
        span(-5, -1),
        span(-5, 0),
        span(0, 7),
        span(3, 7),
        span(i64::MAX - 1, i64::MAX),
    ]
    .iter()
    .map(|span| {
        tracedecay_graph_db::GraphRelationId::new(super::store::SpanIndexKey::encode(
            "branch-span",
            span,
        ))
        .unwrap()
    })
    .collect::<Vec<_>>();
    let mut sorted = keys.clone();
    sorted.sort();
    let decoded = sorted
        .iter()
        .map(|key| {
            let key = super::store::SpanIndexKey::parse(key).unwrap();
            (key.last_ts, key.first_ts)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        decoded,
        vec![
            (i64::MAX, i64::MAX - 1),
            (7, 0),
            (7, 3),
            (0, -5),
            (-1, -5),
            (i64::MIN, i64::MIN),
        ]
    );
    let malformed = tracedecay_graph_db::GraphRelationId::new("branch-span:zz:0:a:b").unwrap();
    assert!(matches!(
        super::store::SpanIndexKey::parse(&malformed),
        Err(GitCorrelationError::Corrupt(_))
    ));
}

#[test]
fn seeded_publication_rejects_cancelled_manifest_builds() {
    let projection = seeded_projection(6);
    let error = build_git_evidence_manifest_checked(identity(), &projection, &revision(), &|| {
        Err(GraphDbError::Cancelled)
    })
    .unwrap_err();
    assert_eq!(error, GitCorrelationError::Cancelled);
}

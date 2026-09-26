//! The bounded row reads must answer exactly what the complete-scan oracle
//! answers over every stored row.
//!
//! Every query below is evaluated twice, through [`GitEvidenceProjectionV1`]
//! over all rows and through [`GitEvidenceView`] over the stored rows. The
//! results must be identical, ordering included.

use tracedecay_runtime_core::db::engine::{TestConnection, TransactionBehavior};

use super::*;

const BASE_TS: i64 = 1_700_000_000;

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

struct SeededStore {
    _directory: tempfile::TempDir,
    connection: TestConnection,
}

async fn empty_store() -> SeededStore {
    let directory = tempfile::tempdir().unwrap();
    let connection = TestConnection::open(&directory.path().join("sessions.db"));
    ensure_git_correlation_receipt_schema_in_transaction(&connection)
        .await
        .unwrap();
    SeededStore {
        _directory: directory,
        connection,
    }
}

async fn write(
    store: &SeededStore,
    batch: GitEvidenceBatch,
) -> (GitEvidenceWrite, Option<GitEvidenceGeneration>) {
    let transaction = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .unwrap();
    let mut writer = GitEvidenceWriter::open(&transaction).await.unwrap();
    let written = writer.apply(batch).await.unwrap();
    let generation = writer.finish().await.unwrap();
    transaction.commit().await.unwrap();
    (written, generation)
}

async fn seeded_store(projection: &GitEvidenceProjectionV1) -> SeededStore {
    let store = empty_store().await;
    write(
        &store,
        GitEvidenceBatch {
            spans: projection.spans().to_vec(),
            commits: projection.commit_sessions().to_vec(),
            merge_gap_secs: DEFAULT_SPAN_MERGE_GAP_SECS,
            ..GitEvidenceBatch::default()
        },
    )
    .await;
    store
}

#[tokio::test]
async fn rows_view_matches_full_projection_for_every_query_shape() {
    const SESSIONS: usize = 240;
    let projection = seeded_projection(SESSIONS);
    let store = seeded_store(&projection).await;
    let view = open_git_evidence_view(&store.connection)
        .await
        .unwrap()
        .expect("evidence was written");

    for (query, relation) in sessions_for_queries(SESSIONS) {
        let expected = projection.sessions_for(&query, relation);
        let observed = view.sessions_for(&query, relation).await.unwrap();
        assert_eq!(observed, expected, "query {query:?} relation {relation:?}");
    }
    for filter in scope_filters(SESSIONS) {
        let expected = projection.session_ids_for_scope(&filter);
        let observed = view.session_ids_for_scope(&filter).await.unwrap();
        assert_eq!(observed, expected, "scope {filter:?}");
    }
    let health = view.health(Some(7));
    assert_eq!(health.span_count, projection.spans().len() as u64);
    assert_eq!(
        health.commit_count,
        projection.commit_sessions().len() as u64
    );
    assert_eq!(
        health.source_watermark.as_deref(),
        Some("git-evidence-sequence:1")
    );
    assert_eq!(health.backfill_watermark, Some(7));
    let presence = view.presence(None);
    assert!(presence.projection_available && presence.spans_present && presence.commits_present);
    assert_eq!(presence.generation, health.generation);

    let sessions = ["s00000", "s00004", "s00011", "never-seen"]
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let (spans, commits) = view.session_evidence(&sessions).await.unwrap();
    let mut expected_spans = projection
        .spans()
        .iter()
        .filter(|span| sessions.contains(&span.session_id))
        .cloned()
        .collect::<Vec<_>>();
    expected_spans.sort_by(|left, right| {
        (
            &left.provider,
            &left.session_id,
            left.first_ts,
            &left.span_id,
        )
            .cmp(&(
                &right.provider,
                &right.session_id,
                right.first_ts,
                &right.span_id,
            ))
    });
    assert_eq!(spans, expected_spans);
    assert_eq!(
        commits,
        projection
            .commit_sessions()
            .iter()
            .filter(|record| sessions.contains(&record.session_id))
            .cloned()
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn rows_view_reports_empty_row_families() {
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
    let store = seeded_store(&spans_only).await;
    let view = open_git_evidence_view(&store.connection)
        .await
        .unwrap()
        .unwrap();
    let presence = view.presence(None);
    assert!(presence.spans_present);
    assert!(!presence.commits_present);
    assert_eq!(view.health(None).commit_count, 0);
    assert_eq!(
        view.sessions_for(
            &query(GitRefFilter::Commit("abcdef".to_owned()), None, None, 5),
            CommitRelationFilter::All,
        )
        .await
        .unwrap(),
        Vec::new()
    );
}

#[tokio::test]
async fn a_store_that_never_recorded_evidence_has_no_view() {
    let store = empty_store().await;
    assert!(
        open_git_evidence_view(&store.connection)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn single_selector_scope_resolution_stops_after_the_caller_bound() {
    let projection = seeded_projection(2_400);
    let store = seeded_store(&projection).await;
    let view = open_git_evidence_view(&store.connection)
        .await
        .unwrap()
        .unwrap();
    let filter = GitScopeFilter {
        branch: Some("feature-3".to_owned()),
        worktree: None,
        commit: None,
    };
    let unbounded = view.session_ids_for_scope(&filter).await.unwrap().unwrap();
    let bounded = view
        .session_ids_for_scope_bounded(&filter, 101)
        .await
        .unwrap()
        .unwrap();
    assert!(unbounded.len() > 101);
    assert_eq!(bounded.len(), 101);
    assert!(bounded.iter().all(|id| unbounded.contains(id)));
}

fn observation(session_id: &str, provider: &str, ts: i64) -> SpanObservation {
    SpanObservation {
        provider: provider.to_owned(),
        session_id: session_id.to_owned(),
        thread_id: None,
        branch: Some("main".to_owned()),
        worktree: "/repo".to_owned(),
        ts,
        source: SpanSource::HookRoute,
    }
}

/// Writes touch only the sessions they name: an unchanged batch installs no
/// generation, an extending observation updates exactly its span row, and a
/// later provider settles the session's unattributed rows.
#[tokio::test]
async fn writes_append_per_session_and_install_a_generation_only_on_change() {
    let store = empty_store().await;
    let batch = || GitEvidenceBatch {
        observations: vec![observation("hook-session", "", 100)],
        merge_gap_secs: 60,
        ..GitEvidenceBatch::default()
    };
    let (first, first_generation) = write(&store, batch()).await;
    assert_eq!(
        first,
        GitEvidenceWrite {
            spans_changed: 1,
            commits_changed: 0
        }
    );
    let first_generation = first_generation.expect("a new span installs a generation");
    assert_eq!(
        (first_generation.sequence, first_generation.span_count),
        (1, 1)
    );

    let (repeated, repeated_generation) = write(&store, batch()).await;
    assert_eq!(repeated, GitEvidenceWrite::default());
    assert_eq!(
        repeated_generation, None,
        "unchanged evidence installs nothing"
    );

    let (extended, extended_generation) = write(
        &store,
        GitEvidenceBatch {
            observations: vec![observation("hook-session", "codex", 130)],
            merge_gap_secs: 60,
            ..GitEvidenceBatch::default()
        },
    )
    .await;
    assert_eq!(
        extended,
        GitEvidenceWrite {
            spans_changed: 1,
            commits_changed: 0
        }
    );
    let extended_generation = extended_generation.unwrap();
    assert_eq!(
        (extended_generation.sequence, extended_generation.span_count),
        (2, 1)
    );
    assert_ne!(extended_generation.digest, first_generation.digest);

    let view = open_git_evidence_view(&store.connection)
        .await
        .unwrap()
        .unwrap();
    let (spans, _) = view
        .session_evidence(&BTreeSet::from(["hook-session".to_owned()]))
        .await
        .unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].first_ts, spans[0].last_ts), (100, 130));
    assert_eq!(spans[0].provider, "codex");
    assert_eq!(spans[0].event_count, 2);
}

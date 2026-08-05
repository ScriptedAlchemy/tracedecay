use std::sync::Mutex;

use tracedecay_domain::{
    ConfigurationRevisionId, GitOidV1, ManifestDigest, RepositoryId, SessionCursorKeyIdV1,
    SessionCursorVersionV1, SignedCursorKeyRefV1, WorktreeId,
};
use tracedecay_temporal_query::ports::InMemoryCursorAuthenticator;

use super::*;

#[derive(Default)]
struct FakeGraph {
    commit: Mutex<Option<String>>,
    search: Vec<BranchSearchMatchV1>,
    nodes: Vec<BranchGraphSymbol>,
}

impl BranchGraphReadPort for FakeGraph {
    fn search<'a>(
        &'a self,
        _query: &'a str,
        _limit: usize,
    ) -> BranchGraphFuture<'a, Vec<BranchSearchMatchV1>> {
        let result = self.search.clone();
        Box::pin(async move { Ok(result) })
    }

    fn all_nodes(&self) -> BranchGraphFuture<'_, Vec<BranchGraphSymbol>> {
        let result = self.nodes.clone();
        Box::pin(async move { Ok(result) })
    }

    fn source_commit(&self) -> BranchMarkerFuture<'_> {
        let commit = self.commit.lock().expect("commit").clone();
        Box::pin(async move { commit })
    }
}

#[derive(Clone, Copy)]
enum FakeDisposition {
    Ready,
    Denied,
    Pending,
    Unavailable(BranchQueryUnavailableReasonV1),
}

struct FakeResolver {
    scope: ResolvedScope,
    graph: Arc<FakeGraph>,
    disposition: FakeDisposition,
    revalidation: Mutex<BranchRevalidationOutcome>,
}

impl FakeResolver {
    fn ready(worktree: &str) -> Self {
        let scope = ResolvedScope::new(
            ProjectId::new("project.fixture").expect("project"),
            RepositoryId::new("repository.fixture").expect("repository"),
            WorktreeId::new(worktree).expect("worktree"),
            Some(RefId::new("refs/heads/main").expect("ref")),
        )
        .expect("scope");
        Self {
            scope,
            graph: Arc::new(FakeGraph {
                commit: Mutex::new(Some("a".repeat(40))),
                ..FakeGraph::default()
            }),
            disposition: FakeDisposition::Ready,
            revalidation: Mutex::new(BranchRevalidationOutcome::Current),
        }
    }

    fn snapshot(&self, branch: &str) -> ResolvedBranchSnapshot {
        let reference = RefId::new(format!("refs/heads/{branch}")).expect("ref");
        let scope = ResolvedScope::new(
            self.scope.project_id.clone(),
            self.scope.repository_id.clone(),
            self.scope.worktree_id.clone(),
            Some(reference.clone()),
        )
        .expect("scope");
        let generation = BranchGraphGenerationV1 {
            graph_scope_id: format!("scope.{branch}"),
            source_oid: GitOidV1::new("a".repeat(40)).expect("commit"),
            content_digest: ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
                .expect("digest"),
            recorded_sync_at: Some(UtcMicros(1)),
            generation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("digest"),
        };
        ResolvedBranchSnapshot {
            identity: BranchSnapshotIdentityV1 {
                project_id: scope.project_id,
                repository_id: scope.repository_id,
                worktree_id: scope.worktree_id,
                reference,
                scope_digest: scope.scope_digest,
                authorization: authorization_epoch(),
                generation,
            },
            registered_scope: GraphScopeRecord {
                graph_scope_id: format!("scope.{branch}"),
                project_id: "project.fixture".to_owned(),
                store_id: "store.fixture".to_owned(),
                branch_name: branch.to_owned(),
                db_relpath: format!("branches/{branch}.db"),
                parent_scope_id: None,
                last_synced_at: Some(1),
                writable: false,
            },
            authorization: authorization_epoch(),
            worktree_root: PathBuf::from("/fixture"),
            symbols: self.graph.nodes.clone(),
            graph: Arc::clone(&self.graph) as Arc<dyn BranchGraphReadPort>,
        }
    }
}

impl BranchSnapshotResolver for FakeResolver {
    fn comparison_targets<'a>(
        &'a self,
        request: &'a BranchDiffRequestV1,
    ) -> ComparisonTargetsFuture<'a> {
        Box::pin(async move {
            Ok((
                request.base.clone().unwrap_or_else(|| "main".to_owned()),
                request.head.clone().unwrap_or_else(|| "main".to_owned()),
            ))
        })
    }

    fn resolve<'a>(
        &'a self,
        branch: &'a str,
        _capability: &'static str,
    ) -> BranchResolutionFuture<'a> {
        Box::pin(async move {
            match self.disposition {
                FakeDisposition::Ready => BranchResolutionOutcome::Resolved(self.snapshot(branch)),
                FakeDisposition::Denied => BranchResolutionOutcome::Denied,
                FakeDisposition::Pending => std::future::pending().await,
                FakeDisposition::Unavailable(reason) => {
                    BranchResolutionOutcome::Unavailable(reason)
                }
            }
        })
    }

    fn revalidate<'a>(
        &'a self,
        _snapshot: &'a ResolvedBranchSnapshot,
        _capability: &'static str,
    ) -> BranchRevalidationFuture<'a> {
        Box::pin(async move {
            match &*self.revalidation.lock().expect("revalidation") {
                BranchRevalidationOutcome::Current => BranchRevalidationOutcome::Current,
                BranchRevalidationOutcome::Denied => BranchRevalidationOutcome::Denied,
                BranchRevalidationOutcome::Stale(reason) => {
                    BranchRevalidationOutcome::Stale(*reason)
                }
                BranchRevalidationOutcome::Unavailable(reason) => {
                    BranchRevalidationOutcome::Unavailable(*reason)
                }
            }
        })
    }
}

fn search_request() -> BranchQueryRequestV1 {
    BranchQueryRequestV1::Search(BranchSearchRequestV1 {
        branch: "main".to_owned(),
        query: "needle".to_owned(),
        limit: 10,
        cursor: None,
    })
}

fn authorization_epoch() -> BranchAuthorizationEpochV1 {
    BranchAuthorizationEpochV1 {
        configuration_revision: ConfigurationRevisionId::new("configuration.revision.fixture")
            .expect("revision"),
        configuration_digest: ManifestDigest::new(format!("sha256:{}", "d".repeat(64)))
            .expect("digest"),
        configuration_provenance_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
            .expect("digest"),
        grant_expires_at: UtcMicros(i64::MAX),
    }
}

fn executor(resolver: FakeResolver) -> DaemonBranchQueryExecutor {
    let cursor_key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new("branch-query-test").expect("key"),
        version: SessionCursorVersionV1::new(1).expect("version"),
    };
    let cursor_authenticator =
        InMemoryCursorAuthenticator::new(cursor_key.clone(), vec![7; 32]).expect("authenticator");
    DaemonBranchQueryExecutor {
        resolver: Arc::new(resolver),
        cursor_key,
        cursor_authenticator: Arc::new(cursor_authenticator),
    }
}

fn search_match(id: &str) -> BranchSearchMatchV1 {
    BranchSearchMatchV1 {
        id: id.to_owned(),
        name: id.to_owned(),
        kind: "function".to_owned(),
        file: "src/lib.rs".to_owned(),
        line: 1,
        signature: None,
        score: 1.0,
    }
}

#[tokio::test]
async fn unauthorized_branch_is_denied_without_a_graph_result() {
    let mut resolver = FakeResolver::ready("worktree.main");
    resolver.disposition = FakeDisposition::Denied;
    let executor = executor(resolver);
    assert!(matches!(
        executor
            .execute(search_request(), BranchQueryControlsV1::default())
            .await,
        BranchQueryOutcomeV1::Denied
    ));
}

#[tokio::test]
async fn linked_worktree_identity_is_retained_in_the_snapshot() {
    let executor = executor(FakeResolver::ready("worktree.linked"));
    let outcome = executor
        .execute(search_request(), BranchQueryControlsV1::default())
        .await;
    let BranchQueryOutcomeV1::Complete {
        result: BranchQueryResultV1::Search(result),
    } = outcome
    else {
        panic!("expected complete search");
    };
    assert_eq!(result.snapshot.worktree_id.as_str(), "worktree.linked");
}

#[tokio::test]
async fn search_cursor_pages_total_order_without_repeating_items() {
    let mut exact = FakeResolver::ready("worktree.main");
    Arc::get_mut(&mut exact.graph).expect("unique graph").search = vec![search_match("one")];
    let exact_outcome = executor(exact)
        .execute(
            BranchQueryRequestV1::Search(BranchSearchRequestV1 {
                branch: "main".to_owned(),
                query: "needle".to_owned(),
                limit: 1,
                cursor: None,
            }),
            BranchQueryControlsV1::default(),
        )
        .await;
    assert!(matches!(
        exact_outcome,
        BranchQueryOutcomeV1::Complete { .. }
    ));

    let mut truncated = FakeResolver::ready("worktree.main");
    Arc::get_mut(&mut truncated.graph)
        .expect("unique graph")
        .search = vec![search_match("one"), search_match("two")];
    let executor = executor(truncated);
    let first_outcome = executor
        .execute(
            BranchQueryRequestV1::Search(BranchSearchRequestV1 {
                branch: "main".to_owned(),
                query: "needle".to_owned(),
                limit: 1,
                cursor: None,
            }),
            BranchQueryControlsV1::default(),
        )
        .await;
    let BranchQueryOutcomeV1::Complete {
        result: BranchQueryResultV1::Search(result),
    } = first_outcome
    else {
        panic!("expected first search page");
    };
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].id, "one");
    assert_eq!(result.total, 2);
    let cursor = result.next_cursor.expect("cursor");

    let second_outcome = executor
        .execute(
            BranchQueryRequestV1::Search(BranchSearchRequestV1 {
                branch: "main".to_owned(),
                query: "needle".to_owned(),
                limit: 1,
                cursor: Some(cursor),
            }),
            BranchQueryControlsV1::default(),
        )
        .await;
    let BranchQueryOutcomeV1::Complete {
        result: BranchQueryResultV1::Search(result),
    } = second_outcome
    else {
        panic!("expected second search page");
    };
    assert_eq!(result.items[0].id, "two");
    assert!(result.next_cursor.is_none());
}

#[tokio::test]
async fn same_branch_diff_reuses_one_exact_snapshot() {
    let outcome = executor(FakeResolver::ready("worktree.main"))
        .execute(
            BranchQueryRequestV1::Diff(BranchDiffRequestV1 {
                base: Some("main".to_owned()),
                head: Some("main".to_owned()),
                file: None,
                kind: None,
                limit: 10,
                cursor: None,
            }),
            BranchQueryControlsV1::default(),
        )
        .await;
    let BranchQueryOutcomeV1::Complete {
        result: BranchQueryResultV1::Diff(result),
    } = outcome
    else {
        panic!("expected same-branch diff");
    };
    assert_eq!(result.base, result.head);
    assert_eq!(result.summary.added, 0);
    assert_eq!(result.summary.removed, 0);
    assert_eq!(result.summary.changed, 0);
    assert!(result.note.is_some());
}

#[tokio::test]
async fn missing_graph_authority_is_not_an_empty_success() {
    let mut resolver = FakeResolver::ready("worktree.main");
    resolver.disposition =
        FakeDisposition::Unavailable(BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable);
    let executor = executor(resolver);
    assert!(matches!(
        executor
            .execute(search_request(), BranchQueryControlsV1::default())
            .await,
        BranchQueryOutcomeV1::Unavailable {
            reason: BranchQueryUnavailableReasonV1::GraphAuthorityUnavailable
        }
    ));
}

#[tokio::test]
async fn generation_drift_discards_query_values_as_stale() {
    let mut resolver = FakeResolver::ready("worktree.main");
    Arc::get_mut(&mut resolver.graph)
        .expect("unique graph")
        .search = vec![search_match("must-not-escape")];
    *resolver.revalidation.lock().expect("revalidation") =
        BranchRevalidationOutcome::Stale(BranchQueryStaleReasonV1::GraphGenerationChanged);
    let executor = executor(resolver);
    assert!(matches!(
        executor
            .execute(search_request(), BranchQueryControlsV1::default())
            .await,
        BranchQueryOutcomeV1::Stale {
            reason: BranchQueryStaleReasonV1::GraphGenerationChanged
        }
    ));
}

#[tokio::test]
async fn authorization_epoch_drift_discards_query_values() {
    let mut resolver = FakeResolver::ready("worktree.main");
    Arc::get_mut(&mut resolver.graph)
        .expect("unique graph")
        .search = vec![search_match("must-not-escape")];
    *resolver.revalidation.lock().expect("revalidation") =
        BranchRevalidationOutcome::Stale(BranchQueryStaleReasonV1::AuthorizationEpochChanged);
    assert!(matches!(
        executor(resolver)
            .execute(search_request(), BranchQueryControlsV1::default())
            .await,
        BranchQueryOutcomeV1::Stale {
            reason: BranchQueryStaleReasonV1::AuthorizationEpochChanged
        }
    ));
}

#[tokio::test]
async fn tampered_search_cursor_is_rejected() {
    let mut resolver = FakeResolver::ready("worktree.main");
    Arc::get_mut(&mut resolver.graph)
        .expect("unique graph")
        .search = vec![search_match("one"), search_match("two")];
    let executor = executor(resolver);
    let first = executor
        .execute(
            BranchQueryRequestV1::Search(BranchSearchRequestV1 {
                branch: "main".to_owned(),
                query: "needle".to_owned(),
                limit: 1,
                cursor: None,
            }),
            BranchQueryControlsV1::default(),
        )
        .await;
    let BranchQueryOutcomeV1::Complete {
        result: BranchQueryResultV1::Search(result),
    } = first
    else {
        panic!("expected first page");
    };
    let mut cursor = result.next_cursor.expect("cursor");
    cursor.push('0');
    assert!(matches!(
        executor
            .execute(
                BranchQueryRequestV1::Search(BranchSearchRequestV1 {
                    branch: "main".to_owned(),
                    query: "needle".to_owned(),
                    limit: 1,
                    cursor: Some(cursor),
                }),
                BranchQueryControlsV1::default(),
            )
            .await,
        BranchQueryOutcomeV1::Unavailable {
            reason: BranchQueryUnavailableReasonV1::CursorUnavailable
        }
    ));
}

#[test]
fn diff_page_is_bounded_across_change_categories() {
    let symbol = |name: &str| BranchDiffSymbolV1 {
        name: name.to_owned(),
        qualified_name: name.to_owned(),
        kind: "function".to_owned(),
        file: "src/lib.rs".to_owned(),
        line: 1,
        signature: None,
    };
    let changed = BranchChangedSymbolV1 {
        name: "changed".to_owned(),
        qualified_name: "changed".to_owned(),
        kind: "function".to_owned(),
        file: "src/lib.rs".to_owned(),
        line: 1,
        base_signature: Some("old".to_owned()),
        head_signature: Some("new".to_owned()),
    };
    let (added, removed, changed) = paginate_diff(
        vec![symbol("added")],
        vec![symbol("removed")],
        vec![changed],
        1,
        3,
    );
    assert!(added.is_empty());
    assert_eq!(removed.len(), 1);
    assert_eq!(changed.len(), 1);
}

#[tokio::test]
async fn live_cancellation_stops_before_graph_resolution() {
    let cancellation = tracedecay_application::CancellationSignal::active("cancel.branch-query")
        .expect("cancellation");
    let mut resolver = FakeResolver::ready("worktree.main");
    resolver.disposition = FakeDisposition::Pending;
    let executor = executor(resolver);
    let execution = executor.execute(
        search_request(),
        BranchQueryControlsV1 {
            cancellation: Some(cancellation.clone()),
            ..BranchQueryControlsV1::default()
        },
    );
    let cancellation_request = async {
        tokio::task::yield_now().await;
        assert!(cancellation.cancel(tracedecay_application::now_micros()));
    };
    let (outcome, ()) = tokio::join!(execution, cancellation_request);
    assert!(matches!(outcome, BranchQueryOutcomeV1::Cancelled));
}

#[tokio::test]
async fn elapsed_deadline_stops_before_graph_resolution() {
    let executor = executor(FakeResolver::ready("worktree.main"));
    assert!(matches!(
        executor
            .execute(
                search_request(),
                BranchQueryControlsV1 {
                    deadline: Some(
                        tracedecay_application::Deadline::new(UtcMicros(1)).expect("deadline"),
                    ),
                    ..BranchQueryControlsV1::default()
                },
            )
            .await,
        BranchQueryOutcomeV1::TimedOut
    ));
}

#[test]
fn invalid_registered_generation_is_unavailable() {
    let resolver = FakeResolver::ready("worktree.main");
    let mut scope = resolver.snapshot("main").registered_scope;
    scope.last_synced_at = Some(-1);
    assert!(matches!(
        RegisteredBranchSnapshotResolver::generation(
            &scope,
            GitOidV1::new("a".repeat(40)).expect("oid"),
            ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("digest"),
        ),
        Err(BranchQueryUnavailableReasonV1::GenerationUnavailable)
    ));
}

#[test]
fn symbolic_text_is_not_accepted_as_a_native_source_oid() {
    assert!(GitOidV1::new("banana").is_err());
}

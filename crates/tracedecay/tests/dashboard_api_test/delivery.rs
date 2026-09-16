use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::delivery::{
    ProjectDeliveryCiSourceV1, ProjectDeliveryCiTimelineV1,
    ProjectDeliveryFailureLocalizationSourceV1, ProjectDeliveryGitHubOperationSnapshotV1,
    ProjectDeliveryGitHubSourceV1, ProjectDeliveryGitHubTimelineV1, ProjectDeliveryPullRequestIdentityV1,
    ProjectDeliveryPullRequestOperationV1, ProjectDeliveryPullRequestStateV1,
    ProjectDeliveryPullRequestV1, ProjectDeliveryReadOutcomeV1, ProjectDeliveryReadRequestV1,
    ProjectDeliveryReleaseSourceV1, ProjectDeliverySnapshotV1,
};
use tracedecay_contracts::code_index_freshness::{
    CodeIndexFreshnessReadFuture, CodeIndexFreshnessReader, CodeIndexWorktreeFreshnessV1,
};
use tracedecay_dashboard_api::{
    DashboardDeliveryProjectV1, DashboardDeliveryReadFutureV1, DashboardDeliveryReadPortV1,
    DashboardHttpRequestControlV1,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCoverageV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewReadCheckpointV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{CommitId, ProjectId, ProviderId, RepositoryId, UtcMicros, WorktreeId};

use crate::dashboard_api_support::*;

const DELIVERY_HTTP_ADMISSION_MATCHED_PR: &str = "42";
const DELIVERY_HTTP_ADMISSION_UNMATCHED_PR: &str = "99";
const DELIVERY_HTTP_ADMISSION_INDEXED_HEAD: &str = "commit.delivery-http-admission.indexed";
const DELIVERY_HTTP_ADMISSION_UNMATCHED_HEAD: &str = "commit.delivery-http-admission.unmatched";

/// A fake `DashboardDeliveryReadPortV1` that always returns two provider pull
/// requests: one whose retained head matches the fixture's indexed head, one
/// that does not. This proves `GET /api/delivery/inbox` admits only the
/// head-matched pull request over real HTTP, not just in the application's
/// own unit tests.
struct FakeDeliveryReadPortV1;

impl DashboardDeliveryReadPortV1 for FakeDeliveryReadPortV1 {
    fn read(
        &self,
        _control: DashboardHttpRequestControlV1,
        _project: DashboardDeliveryProjectV1,
        _request: ProjectDeliveryReadRequestV1,
    ) -> DashboardDeliveryReadFutureV1<'_> {
        Box::pin(async move {
            ProjectDeliveryReadOutcomeV1::Ready {
                snapshot: Box::new(delivery_http_admission_snapshot()),
            }
        })
    }
}

fn delivery_http_admission_pull_request(id: &str, retained_head: &str) -> ProjectDeliveryPullRequestV1 {
    ProjectDeliveryPullRequestV1 {
        provider: ProviderId::new("github").unwrap(),
        pull_request_id: GitHubPullRequestIdV1::new(id).unwrap(),
        identity: Some(ProjectDeliveryPullRequestIdentityV1 {
            title: format!("Pull request {id}"),
            state: ProjectDeliveryPullRequestStateV1::Open,
            draft: false,
            additions: 4,
            deletions: 1,
            changed_files: 1,
        }),
        operations: vec![ProjectDeliveryPullRequestOperationV1 {
            operation: GitHubReviewReadOperationV1::RestGetPullRequest,
            latest_attempt: Some(ProjectDeliveryGitHubOperationSnapshotV1 {
                provider_base_commit_id: CommitId::new("commit.delivery-http-admission.base").unwrap(),
                provider_head_commit_id: CommitId::new(retained_head).unwrap(),
                merge_base_commit_id: CommitId::new("commit.delivery-http-admission.merge-base")
                    .unwrap(),
                outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
                coverage: GitHubReviewCoverageV1::Complete,
                fetched_at: UtcMicros(20),
                checkpoint: GitHubReviewReadCheckpointV1 {
                    etag: None,
                    next_cursor: None,
                    rate_limit: None,
                },
            }),
            last_complete: None,
        }],
    }
}

fn delivery_http_admission_snapshot() -> ProjectDeliverySnapshotV1 {
    let head = CommitId::new(DELIVERY_HTTP_ADMISSION_INDEXED_HEAD).unwrap();
    ProjectDeliverySnapshotV1 {
        scope: FeedbackScopeV1 {
            project_id: ProjectId::new("project.delivery-http-admission").unwrap(),
            repository_id: RepositoryId::new("repository.delivery-http-admission").unwrap(),
            worktree_id: WorktreeId::new("worktree.delivery-http-admission").unwrap(),
            branch_ref: "refs/heads/feature".to_owned(),
            head_commit_id: head.clone(),
        },
        expected_head_commit_id: head,
        github_reviews: ProjectDeliveryGitHubSourceV1::Ready {
            timeline: ProjectDeliveryGitHubTimelineV1 {
                pull_requests: vec![
                    delivery_http_admission_pull_request(
                        DELIVERY_HTTP_ADMISSION_MATCHED_PR,
                        DELIVERY_HTTP_ADMISSION_INDEXED_HEAD,
                    ),
                    delivery_http_admission_pull_request(
                        DELIVERY_HTTP_ADMISSION_UNMATCHED_PR,
                        DELIVERY_HTTP_ADMISSION_UNMATCHED_HEAD,
                    ),
                ],
                review_items: Vec::new(),
                pull_requests_total: 2,
                review_items_total: 0,
                pull_requests_truncated: false,
                review_items_truncated: false,
            },
        },
        ci_checks: ProjectDeliveryCiSourceV1::Ready {
            timeline: ProjectDeliveryCiTimelineV1 {
                checks: Vec::new(),
                total_retained: 0,
                truncated: false,
            },
        },
        failure_localization: ProjectDeliveryFailureLocalizationSourceV1::NotConfigured,
        releases: ProjectDeliveryReleaseSourceV1::Unavailable,
    }
}

fn delivery_http_admission_freshness_reader() -> CodeIndexFreshnessReader {
    Arc::new(|_project_root: PathBuf| -> CodeIndexFreshnessReadFuture {
        Box::pin(async move {
            Some(CodeIndexWorktreeFreshnessV1 {
                worktree_root: "/tmp/delivery-http-admission".to_owned(),
                repository_id: Some("repository.delivery-http-admission".to_owned()),
                worktree_id: Some("worktree.delivery-http-admission".to_owned()),
                source_reference: Some("refs/heads/feature".to_owned()),
                source_revision: Some(DELIVERY_HTTP_ADMISSION_INDEXED_HEAD.to_owned()),
                latest_generation_id: Some("generation.delivery-http-admission.1".to_owned()),
                coverage: "complete".to_owned(),
                staleness_state: Some("fresh".to_owned()),
                sealed_at_micros: Some(30),
                ..CodeIndexWorktreeFreshnessV1::default()
            })
        })
    })
}

#[test]
fn delivery_inbox_admits_only_indexed_pull_requests_over_http() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let mut fixture = start_dashboard_fixture_with_delivery_authority(FakeDeliveryAuthority {
            delivery_read_authority: Arc::new(FakeDeliveryReadPortV1),
            code_index_freshness_reader: delivery_http_admission_freshness_reader(),
        })
        .await;
        // The shared fixture registers the project with no git common dir
        // (`initialize_project_graph_for_test` passes `None`), but the
        // Delivery inbox admission gate requires one to attach the indexed
        // head; `pin_fixture_repository_identity` already ran `git init` on
        // the fixture root, so resolve and register the real one here.
        let git_common_dir =
            tracedecay_runtime_core::worktree::git_common_dir(&fixture.project_root);
        fixture
            .host_runtime
            .upsert_code_project(
                fixture.host_runtime.project_id().as_str(),
                &fixture.project_root,
                git_common_dir.as_deref(),
                None,
                None,
            )
            .await
            .unwrap_or_else(|error| {
                panic!("register delivery http admission git common dir: {error}")
            });

        let agent = http_agent();
        let (status, body) = get_json(
            &agent,
            &format!("{}/api/delivery/inbox", fixture.base_url),
        );
        assert_eq!(status, 200, "delivery inbox should resolve over HTTP: {body}");

        let pull_requests = body["payload"]["pull_requests"]
            .as_array()
            .unwrap_or_else(|| panic!("expected admitted pull requests: {body}"));
        assert_eq!(
            pull_requests.len(),
            1,
            "only the head-matched provider pull request should be admitted: {body}"
        );
        assert_eq!(
            pull_requests[0]["pull_request"]["pull_request_id"],
            DELIVERY_HTTP_ADMISSION_MATCHED_PR
        );
        assert_eq!(body["payload"]["excluded_pull_requests"], 1);

        let membership_edges = body["payload"]["membership_edges"]
            .as_array()
            .unwrap_or_else(|| panic!("expected membership edges: {body}"));
        assert!(
            membership_edges.iter().any(|edge| {
                edge["pull_request_id"] == DELIVERY_HTTP_ADMISSION_MATCHED_PR
                    && edge["basis"]["kind"] == "branch_pull_request_reference"
            }),
            "the admitted pull request must carry a branch_pull_request_reference membership edge: {body}"
        );

        let attention = pull_requests[0]["attention"]
            .as_array()
            .unwrap_or_else(|| panic!("expected attention array: {body}"));
        for source in [
            "ci_failure",
            "unresolved_review",
            "new_review_comment",
            "contradiction",
            "unsafe_pattern",
            "test_risk",
            "unreviewed_changed_code",
            "weak_evidence",
            "evidence_gap",
            "overlapping_edit",
            "confirmed_conflict",
            "divergent_shared_implementation",
            "stale_provider_state",
        ] {
            assert!(
                attention.iter().any(|item| item["source"] == source),
                "attention must name every source, missing {source}: {body}"
            );
        }

        fixture.server.stop();
    });
}

#[test]
fn delivery_overview_serves_real_git_reads_and_typed_unmounted_authority() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        // The fixture root is already a registered git repository carrying
        // the authoritative identity marker; the server resolved its exact
        // scope from it at startup. This test only adds real history.
        let mut fixture = start_dashboard_fixture_without_memory().await;
        let project_root = fixture.project_root.clone();
        write_file(
            &project_root.join("src/lib.rs"),
            "pub fn delivery_fixture() -> &'static str { \"initial\" }\n",
        );
        commit_all(&project_root, "initial delivery fixture");
        // Host `git init` still defaults to `master` on Ubuntu CI images;
        // this test owns an attached `main` so the live-head assertion is
        // not host-default-branch noise.
        git(&project_root, &["branch", "-M", "main"]);
        write_file(
            &project_root.join("src/review.rs"),
            "pub fn review_context() -> bool { true }\n",
        );
        commit_all(&project_root, "serve delivery review context");
        write_file(
            &project_root.join("src/lib.rs"),
            "pub fn delivery_fixture() -> &'static str { \"working tree\" }\n",
        );

        let agent = http_agent();

        let (status, body) = get_json(
            &agent,
            &format!("{}/api/delivery/overview", fixture.base_url),
        );
        assert_eq!(status, 200, "delivery overview should resolve: {body}");
        assert_eq!(body["schema_revision"], 1);
        assert_eq!(body["domain_state"], "partial");

        assert_eq!(body["payload"]["changes"]["state"], "ready");
        assert_eq!(
            body["payload"]["changes"]["value"]["head"]["state"],
            "attached"
        );
        assert_eq!(
            body["payload"]["changes"]["value"]["head"]["branch"],
            "main"
        );
        assert_eq!(body["payload"]["changes"]["value"]["unstaged"], 1);
        assert!(
            body["payload"]["changes"]["value"]["changed_paths"]
                .as_array()
                .is_some_and(|paths| paths.iter().any(|path| path == "src/lib.rs"))
        );

        assert_eq!(body["payload"]["commits"]["state"], "ready");
        let commits = body["payload"]["commits"]["value"]["items"]
            .as_array()
            .unwrap_or_else(|| panic!("expected delivery commit items: {body}"));
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0]["subject"], "serve delivery review context");
        assert!(
            commits[0]["commit"]
                .as_str()
                .is_some_and(|value| value.len() == 40)
        );
        assert!(commits[0]["author_at_micros"].is_i64());

        assert_eq!(
            body["payload"]["generation_freshness"]["state"],
            "unavailable"
        );
        assert_eq!(
            body["payload"]["generation_freshness"]["required_authority"],
            "daemon code-index generation freshness authority"
        );

        for source in [
            "pull_requests",
            "review_comments",
            "ci_checks",
            "failure_localization",
            "releases",
        ] {
            assert_eq!(
                body["payload"][source]["state"], "unavailable",
                "{source} must be typed unavailable rather than empty: {body}"
            );
            assert!(
                body["payload"][source]["required_authority"]
                    .as_str()
                    .is_some_and(|authority| authority.contains("authority")),
                "{source} must name the missing composition seam: {body}"
            );
        }

        fixture.server.stop();
    });
}

#[test]
fn delivery_contract_exposes_typed_provider_rows_and_source_states() {
    let schema: serde_json::Value = serde_json::from_str(
        &dashboard::contract_schema::render_dashboard_contract_schema()
            .expect("render dashboard contract schema"),
    )
    .expect("parse dashboard contract schema");
    let definitions = schema["$defs"]
        .as_object()
        .expect("dashboard schema definitions");

    for definition in [
        "DeliveryPullRequestV1",
        "DeliveryPullRequestOperationV1",
        "DeliveryGitHubOperationSnapshotV1",
        "DeliveryReviewItemV1",
        "DeliveryReviewObservationV1",
        "DeliveryCiCheckV1",
        "DeliveryCiRunIdentityV1",
        "DeliveryReleaseV1",
        "DeliveryRateLimitCheckpointV1",
        "DeliveryInboxV1",
        "DeliveryAttentionItemV1",
        "DeliveryMembershipEdgeV1",
        "DeliverySharedCodeRefV1",
    ] {
        assert!(
            definitions.contains_key(definition),
            "missing typed Delivery schema {definition}"
        );
    }

    assert!(
        definitions["DeliveryPullRequestV1"]["properties"]
            .get("operations")
            .is_some(),
        "pull requests must retain provider-qualified operation evidence"
    );
    assert!(
        definitions["DeliveryReviewItemV1"]["properties"]
            .get("observations")
            .is_some(),
        "review rows must retain latest-attempt and last-complete observations"
    );
    for property in ["observation_id", "run"] {
        assert!(
            definitions["DeliveryCiCheckV1"]["properties"]
                .get(property)
                .is_some(),
            "CI rows must retain opaque {property} identity"
        );
    }
    for private_field in ["checkpoint", "body_anchor", "body_digest", "failure_anchor"] {
        assert!(
            definitions["DeliveryReviewObservationV1"]["properties"]
                .get(private_field)
                .is_none()
                && definitions["DeliveryCiCheckV1"]["properties"]
                    .get(private_field)
                    .is_none(),
            "private retained-source field {private_field} must not cross the dashboard wire"
        );
    }

    let membership = definitions["DeliveryMembershipBasisV1"].to_string();
    for basis in [
        "shared_work_objective",
        "session_git_relation",
        "explicit_handoff",
        "shared_agent",
        "branch_pull_request_reference",
    ] {
        assert!(
            membership.contains(basis),
            "Delivery membership must retain the explicit {basis} basis: {membership}"
        );
    }
    let attention = definitions["DeliveryAttentionSourceV1"].to_string();
    for source in [
        "ci_failure",
        "unresolved_review",
        "new_review_comment",
        "contradiction",
        "unsafe_pattern",
        "test_risk",
        "unreviewed_changed_code",
        "weak_evidence",
        "evidence_gap",
        "overlapping_edit",
        "confirmed_conflict",
        "divergent_shared_implementation",
        "stale_provider_state",
    ] {
        assert!(
            attention.contains(source),
            "Delivery attention must retain the typed {source} source: {attention}"
        );
    }

    let projection = definitions
        .get("DeliveryProjectionV1_for_DeliveryPullRequestTimelineV1")
        .unwrap_or_else(|| {
            definitions
                .iter()
                .find_map(|(name, schema)| {
                    (name.starts_with("DeliveryProjectionV1")
                        && schema.to_string().contains("not_published"))
                    .then_some(schema)
                })
                .expect("typed Delivery projection schema")
        });
    let projection_schema = projection.to_string();
    for state in [
        "ready",
        "partial",
        "stale",
        "rate_limited",
        "failed",
        "denied",
        "not_published",
        "empty_measured",
        "unavailable",
    ] {
        assert!(
            projection_schema.contains(state),
            "Delivery projection schema must retain {state}: {projection_schema}"
        );
    }
}

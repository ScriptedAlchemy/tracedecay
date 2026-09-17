use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::delivery::{
    ProjectDeliveryCiSourceV1, ProjectDeliveryCiTimelineV1,
    ProjectDeliveryFailureLocalizationSourceV1, ProjectDeliveryGitHubOperationSnapshotV1,
    ProjectDeliveryGitHubSourceV1, ProjectDeliveryGitHubTimelineV1,
    ProjectDeliveryPullRequestIdentityV1, ProjectDeliveryPullRequestOperationV1,
    ProjectDeliveryPullRequestStateV1, ProjectDeliveryPullRequestV1, ProjectDeliveryReadOutcomeV1,
    ProjectDeliveryReadRequestV1, ProjectDeliveryReleaseSourceV1, ProjectDeliverySnapshotV1,
};
use tracedecay_contracts::code_index_freshness::{
    CodeIndexFreshnessReadFuture, CodeIndexFreshnessReader, CodeIndexWorktreeFreshnessV1,
};
use tracedecay_contracts::feedback::{
    FeedbackProximityAccessKindV1, FeedbackProximityEncounterV1, FeedbackProximityIntervalV1,
    FeedbackProximityParticipantV1, FeedbackProximityReadPageV1, FeedbackProximityReadResultV1,
    FeedbackProximityRelationV1, PROXIMITY_CAPABILITY_ID_V1, PROXIMITY_USE_CASE_ID_V1,
};
use tracedecay_contracts::{
    CapabilityGrantId, CapabilityGrantSnapshot, DisclosureClass, RequestContext, ResolvedScope,
};
use tracedecay_daemon_service::{
    DaemonAdvisoryCycleInvocationFuture, DaemonAdvisoryCycleInvocationOwner,
    DaemonAdvisoryCycleInvocationPort, DaemonAdvisoryCycleInvocationRequest,
    DaemonFeedbackProximityInvocationFuture, DaemonFeedbackProximityInvocationRequest,
    DaemonInvocationService, feedback_proximity_invocation_result,
};
use tracedecay_dashboard_api::{
    DashboardDeliveryProjectV1, DashboardDeliveryReadFutureV1, DashboardDeliveryReadPortV1,
    DashboardHttpRequestControlV1,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCoverageV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewReadCheckpointV1,
    GitHubReviewReadOperationV1, ProximityAddressV1, ProximityCoverageV1, ProximityWarningClassV1,
};
use tracedecay_domain::{
    ActorId, AgentInstanceId, CodeGenerationId, CommitId, FileOccurrenceId, ManifestDigest,
    ObservationSourceIdentityV1, ProjectId, ProviderId, RefId, RepositoryId, SessionId, SourceSpan,
    SymbolOccurrenceId, UtcMicros, WorktreeId,
};
use tracedecay_mcp::handlers::dashboard_delivery::DashboardProximityAttentionReadAdapter;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::dashboard_api_support::*;

const DELIVERY_HTTP_ADMISSION_MATCHED_PR: &str = "42";
const DELIVERY_HTTP_ADMISSION_UNMATCHED_PR: &str = "99";
const DELIVERY_HTTP_ADMISSION_INDEXED_HEAD: &str = "commit.delivery-http-admission.indexed";
const DELIVERY_HTTP_ADMISSION_UNMATCHED_HEAD: &str = "commit.delivery-http-admission.unmatched";
const DELIVERY_HTTP_PROXIMITY_ENCOUNTER: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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

/// Daemon proximity owner that returns one OverlappingEdit encounter naming
/// the admitted indexed head. Mounted under the fixture root so the
/// production `DashboardProximityAttentionReadAdapter` must look it up,
/// invoke, and fold evidence — not a pre-folded Ready stub.
struct DeliveryHttpProximityOwner {
    project_id: ProjectId,
}

impl DaemonAdvisoryCycleInvocationPort for DeliveryHttpProximityOwner {
    fn invoke(
        &self,
        _request: DaemonAdvisoryCycleInvocationRequest,
    ) -> DaemonAdvisoryCycleInvocationFuture<'_> {
        Box::pin(async {
            Err(tracedecay_contracts::ApplicationProblem::unavailable(
                tracedecay_contracts::SafeDiagnostic {
                    code: "delivery.http.proximity.advisory-unused".to_owned(),
                    message: "Advisory cycle is unused by the delivery HTTP proximity proof"
                        .to_owned(),
                },
            ))
        })
    }

    fn invoke_proximity(
        &self,
        request: DaemonFeedbackProximityInvocationRequest,
    ) -> DaemonFeedbackProximityInvocationFuture<'_> {
        let project_id = self.project_id.clone();
        Box::pin(async move {
            let scope = FeedbackScopeV1 {
                project_id: project_id.clone(),
                repository_id: RepositoryId::new("repository.delivery-http-admission").unwrap(),
                worktree_id: WorktreeId::new("worktree.delivery-http-admission").unwrap(),
                branch_ref: "refs/heads/feature".to_owned(),
                head_commit_id: CommitId::new(DELIVERY_HTTP_ADMISSION_INDEXED_HEAD).unwrap(),
            };
            let participant = |provider: &str, session: &str, agent: &str, head: &str| {
                FeedbackProximityParticipantV1 {
                    source: ObservationSourceIdentityV1::for_provider(
                        ProviderId::new(provider).unwrap(),
                        SessionId::new(session).unwrap(),
                    )
                    .unwrap(),
                    agent_id: AgentInstanceId::new(agent).unwrap(),
                    worktree_id: Some(scope.worktree_id.clone()),
                    worktree_root: format!("/tmp/{session}"),
                    branch_ref: Some(RefId::new(scope.branch_ref.clone()).unwrap()),
                    head_revision: Some(CommitId::new(head).unwrap()),
                    access: FeedbackProximityAccessKindV1::Write,
                    activity: FeedbackProximityIntervalV1 {
                        start: UtcMicros(10),
                        end: UtcMicros(40),
                    },
                    address: ProximityAddressV1 {
                        scope: scope.clone(),
                        file: FileOccurrenceId::new("file.delivery-http-admission").unwrap(),
                        span: Some(SourceSpan {
                            start_byte: 0,
                            end_byte: 8,
                        }),
                        symbol: Some(
                            SymbolOccurrenceId::new("symbol.delivery-http-admission").unwrap(),
                        ),
                    },
                }
            };
            let encounter = FeedbackProximityEncounterV1 {
                encounter_id: ManifestDigest::new(DELIVERY_HTTP_PROXIMITY_ENCOUNTER.to_owned())
                    .unwrap(),
                scope: scope.clone(),
                interval: FeedbackProximityIntervalV1 {
                    start: UtcMicros(20),
                    end: UtcMicros(40),
                },
                participants: vec![
                    participant(
                        "codex",
                        "session.delivery-http-left",
                        "agent.delivery-http-left",
                        DELIVERY_HTTP_ADMISSION_INDEXED_HEAD,
                    ),
                    participant(
                        "cursor",
                        "session.delivery-http-right",
                        "agent.delivery-http-right",
                        "commit.delivery-http-admission.sibling",
                    ),
                ],
                relation: FeedbackProximityRelationV1::OverlappingEdit {
                    warning_class: ProximityWarningClassV1::SameFile,
                },
                observed_at: UtcMicros(40),
                expires_at: UtcMicros(400),
                coverage: ProximityCoverageV1::Complete,
            };
            let read = FeedbackProximityReadResultV1::Complete {
                page: FeedbackProximityReadPageV1 {
                    scope: scope.clone(),
                    source_generation: CodeGenerationId::new(
                        "generation.delivery-http-admission.1",
                    )
                    .unwrap(),
                    observed_at: UtcMicros(40),
                    expires_at: UtcMicros(400),
                    encounters: vec![encounter],
                },
            };
            let resolved = ResolvedScope::new(
                project_id,
                scope.repository_id.clone(),
                scope.worktree_id.clone(),
                Some(RefId::new(scope.branch_ref.clone()).unwrap()),
            )
            .unwrap();
            let now = request.request.observed_at;
            let grant = CapabilityGrantSnapshot::new(
                CapabilityGrantId::new("grant.delivery-http-proximity").unwrap(),
                1,
                ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                ActorId::new("actor.delivery-http-proximity.issuer").unwrap(),
                UtcMicros(now.0.saturating_sub(1_000_000)),
                UtcMicros(now.0.saturating_add(60_000_000)),
                resolved.clone(),
                BTreeSet::from([CapabilityId::new(PROXIMITY_CAPABILITY_ID_V1).unwrap()]),
                BTreeSet::from([UseCaseId::new(PROXIMITY_USE_CASE_ID_V1).unwrap()]),
                DisclosureClass::Evidence,
            )
            .unwrap();
            let context = RequestContext::new(
                ActorId::new("actor.delivery-http-proximity").unwrap(),
                resolved,
                grant,
                request.request_id.clone(),
                request.deadline.clone(),
                request.cancellation.clone(),
            )
            .unwrap();
            feedback_proximity_invocation_result(
                &context,
                now,
                request.deadline,
                request.cancellation,
                read,
            )
        })
    }
}

fn delivery_http_admission_pull_request(
    id: &str,
    retained_head: &str,
) -> ProjectDeliveryPullRequestV1 {
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
                provider_base_commit_id: CommitId::new("commit.delivery-http-admission.base")
                    .unwrap(),
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
            Ok(Some(CodeIndexWorktreeFreshnessV1 {
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
            }))
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
        // Production adapter first; the advisory-cycle owner is published
        // after the fixture owns a real project root/id. Lookup happens on
        // the HTTP request, so a pre-start empty registry is fine.
        let service = DaemonInvocationService::default();
        let proximity_adapter =
            Arc::new(DashboardProximityAttentionReadAdapter::new(service.clone()));
        let mut fixture = start_dashboard_fixture_with_delivery_authority(FakeDeliveryAuthority {
            delivery_read_authority: Arc::new(FakeDeliveryReadPortV1),
            proximity_attention_read_authority: Some(proximity_adapter),
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

        let project_id = fixture.host_runtime.project_id().clone();
        let owner = DaemonAdvisoryCycleInvocationOwner::new(
            project_id.clone(),
            Arc::new(DeliveryHttpProximityOwner { project_id }),
        );
        service
            .project_runtimes
            .publish(fixture.project_root.clone(), owner)
            .await
            .unwrap_or_else(|error| panic!("publish delivery HTTP proximity owner: {error}"));

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

        let overlapping = attention
            .iter()
            .find(|item| item["source"] == "overlapping_edit")
            .unwrap_or_else(|| panic!("expected overlapping_edit attention: {body}"));
        assert_eq!(
            overlapping["state"], "active",
            "server-owned proximity join must activate overlapping_edit: {body}"
        );
        assert_eq!(overlapping["coverage"], "complete");
        let evidence = overlapping["evidence"]
            .as_array()
            .unwrap_or_else(|| panic!("expected proximity evidence: {body}"));
        assert!(
            evidence.iter().any(|item| {
                item["kind"] == "proximity_encounter"
                    && item["relation"] == "overlapping_edit"
                    && item["encounter_id"] == DELIVERY_HTTP_PROXIMITY_ENCOUNTER
            }),
            "HTTP inbox must carry typed proximity_encounter evidence: {body}"
        );
        let confirmed = attention
            .iter()
            .find(|item| item["source"] == "confirmed_conflict")
            .unwrap();
        assert_eq!(
            confirmed["state"], "clear",
            "mounted proximity with zero conflict matches measures Clear, not Unsupported: {body}"
        );

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

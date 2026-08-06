use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use static_assertions::assert_not_impl_any;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestId, ResolvedScope,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCoverageV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1,
    GitHubReviewReadCheckpointV1,
};
use tracedecay_domain::{
    ActorId, CommitId, ManifestDigest, ProjectId, ProviderId, RefId, RepositoryId, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::super::store::ProjectGitHubReviewStoreV1;
use super::super::{
    GitHubReadResumeV1, GitHubReviewAtomicRefreshStoreV1, GitHubReviewReadResponseV1,
    GitHubReviewRefreshStateV1, GitHubReviewRefreshStoreCommitOutcomeV1,
};
use super::*;
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const THREAD_CAPTURE: &str =
    include_str!("../../../fixtures/pr13_branch_pr/review_thread.graphql.json");

fn test_http_transport(config: GitHubHttpReadConfigV1) -> GitHubHttpReadClientV1 {
    GitHubHttpReadClientV1::build(config, false).unwrap()
}

#[derive(Clone, Copy)]
enum FixtureCredentialAuthorityModeV1 {
    Verified,
    NotConfigured,
    WriteCapable,
    Indeterminate,
}

struct FixtureCredentialAuthorityV1 {
    mode: FixtureCredentialAuthorityModeV1,
}

impl GitHubReadOnlyCredentialAuthorityV1 for FixtureCredentialAuthorityV1 {
    fn resolve(
        &self,
        _repository_owner: &str,
        _repository_name: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
        match self.mode {
            FixtureCredentialAuthorityModeV1::Verified => {
                GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                    secret: GitHubReadOnlyCredentialSecretV1::new(
                        "github_pat_fixture_private_read",
                    )
                    .unwrap(),
                    exact_permissions: BTreeSet::from([
                        GitHubReadPermissionV1::PullRequests,
                        GitHubReadPermissionV1::Actions,
                        GitHubReadPermissionV1::Checks,
                    ]),
                }
            }
            FixtureCredentialAuthorityModeV1::NotConfigured => {
                GitHubReadOnlyCredentialAuthorityOutcomeV1::NotConfigured
            }
            FixtureCredentialAuthorityModeV1::WriteCapable => {
                GitHubReadOnlyCredentialAuthorityOutcomeV1::WriteCapable
            }
            FixtureCredentialAuthorityModeV1::Indeterminate => {
                GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate
            }
        }
    }
}

struct MutableFixtureCredentialAuthorityV1 {
    mode: Mutex<FixtureCredentialAuthorityModeV1>,
}

impl MutableFixtureCredentialAuthorityV1 {
    fn new(mode: FixtureCredentialAuthorityModeV1) -> Self {
        Self {
            mode: Mutex::new(mode),
        }
    }

    fn set_mode(&self, mode: FixtureCredentialAuthorityModeV1) {
        *self.mode.lock().unwrap() = mode;
    }
}

impl GitHubReadOnlyCredentialAuthorityV1 for MutableFixtureCredentialAuthorityV1 {
    fn resolve(
        &self,
        repository_owner: &str,
        repository_name: &str,
    ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
        FixtureCredentialAuthorityV1 {
            mode: *self.mode.lock().unwrap(),
        }
        .resolve(repository_owner, repository_name)
    }
}

fn registered_fixture_credential(
    repository: &str,
    mode: FixtureCredentialAuthorityModeV1,
) -> (
    Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    RegisteredGitHubReadOnlyCredentialV1,
) {
    let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
        Arc::new(FixtureCredentialAuthorityV1 { mode });
    assert!(register_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        repository,
        &authority,
    ));
    let resolution =
        resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", repository);
    (authority, resolution)
}

#[test]
fn credential_remount_receives_a_new_opaque_generation() {
    let repository = "credential-generation";
    let (first_authority, first) =
        registered_fixture_credential(repository, FixtureCredentialAuthorityModeV1::Verified);
    let RegisteredGitHubReadOnlyCredentialV1::Verified(first) = first else {
        panic!("first credential must resolve");
    };
    let first_generation = first.generation();
    assert!(unregister_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        repository,
        &first_authority,
    ));

    let (second_authority, second) =
        registered_fixture_credential(repository, FixtureCredentialAuthorityModeV1::Verified);
    let RegisteredGitHubReadOnlyCredentialV1::Verified(second) = second else {
        panic!("second credential must resolve");
    };
    assert_ne!(first_generation, second.generation());
    assert!(unregister_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        repository,
        &second_authority,
    ));
}

async fn captured_get_headers(credential: GitHubReadOnlyCredentialV1, repository: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        )
        .unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: format!("http://{address}"),
        graphql_uri: format!("http://{address}/graphql"),
        ..GitHubHttpReadConfigV1::default()
    };
    let client = GitHubReadOnlyClientV1 {
        target: GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: repository.to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        credential,
        transport: test_http_transport(config),
    };
    let request_scope = scope("captured-headers");
    let _ = client
        .get(
            &context(&request_scope),
            &format!("http://{address}/fixture"),
            None,
            GitHubReadPermissionV1::PullRequests,
        )
        .await;
    server.join().unwrap()
}

#[tokio::test]
async fn retry_after_without_primary_rate_headers_is_not_authorization_denial() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
    });
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: format!("http://{address}"),
        graphql_uri: format!("http://{address}/graphql"),
        ..GitHubHttpReadConfigV1::default()
    };
    let client = GitHubReadOnlyClientV1 {
        target: GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "retry-after-only".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        credential: GitHubReadOnlyCredentialV1::anonymous(),
        transport: test_http_transport(config),
    };
    let request_scope = scope("retry-after");
    let response = client
        .get(
            &context(&request_scope),
            &format!("http://{address}/fixture"),
            None,
            GitHubReadPermissionV1::PullRequests,
        )
        .await;
    server.join().unwrap();

    let outcome = network_failure(response);
    assert!(
        matches!(
            outcome,
            GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                metadata: GitHubReadNetworkMetadataV1 {
                    status: GitHubReadNetworkStatusV1::RateLimited,
                    rate_limit: None,
                    retry_at: Some(_),
                    ..
                },
                ..
            })
        ),
        "Retry-After is rate-limit evidence, not authorization denial"
    );
}

#[test]
fn github_credentials_are_not_debuggable_or_serializable() {
    assert_not_impl_any!(
        GitHubReadOnlyCredentialSecretV1:
            std::fmt::Debug,
            serde::Serialize,
            serde::de::DeserializeOwned
    );
    assert_not_impl_any!(
        GitHubReadOnlyCredentialV1:
            std::fmt::Debug,
            serde::Serialize,
            serde::de::DeserializeOwned
    );
    assert_not_impl_any!(
        ProfileGitHubReadOnlyCredentialAuthorityV1:
            std::fmt::Debug,
            serde::Serialize,
            serde::de::DeserializeOwned
    );
}

#[tokio::test]
async fn exact_profile_configuration_mount_authenticates_project_open_review_read() {
    struct PullRequestReadCredential;

    impl GitHubReadOnlyCredentialAuthorityV1 for PullRequestReadCredential {
        fn resolve(
            &self,
            repository_owner: &str,
            repository_name: &str,
        ) -> GitHubReadOnlyCredentialAuthorityOutcomeV1 {
            if repository_owner != "ScriptedAlchemy" || repository_name != "profile-mounted-private"
            {
                return GitHubReadOnlyCredentialAuthorityOutcomeV1::Indeterminate;
            }
            GitHubReadOnlyCredentialAuthorityOutcomeV1::Verified {
                secret: GitHubReadOnlyCredentialSecretV1::new("github_pat_exact_profile_fixture")
                    .unwrap(),
                exact_permissions: BTreeSet::from([GitHubReadPermissionV1::PullRequests]),
            }
        }
    }

    let profile_root = tempfile::tempdir().unwrap();
    let exact_profile = UserProfileId::new("profile.github.exact").unwrap();
    let other_profile = UserProfileId::new("profile.github.other").unwrap();
    let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
        Arc::new(PullRequestReadCredential);
    assert!(register_profile_github_read_only_credential_authority_v1(
        exact_profile.clone(),
        "ScriptedAlchemy",
        "profile-mounted-private",
        &authority,
    ));
    assert_eq!(
        mount_profile_github_read_only_credential_authority_v1(
            &other_profile,
            "ScriptedAlchemy",
            "profile-mounted-private",
        ),
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::NotConfigured
    );
    assert!(matches!(
        resolve_registered_github_read_only_credential_v1(
            "ScriptedAlchemy",
            "profile-mounted-private",
        ),
        RegisteredGitHubReadOnlyCredentialV1::Missing
    ));
    assert_eq!(
        mount_profile_github_read_only_credential_authority_v1(
            &exact_profile,
            "ScriptedAlchemy",
            "profile-mounted-private",
        ),
        ProfileGitHubReadOnlyCredentialMountOutcomeV1::Mounted
    );
    let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
        resolve_registered_github_read_only_credential_v1(
            "ScriptedAlchemy",
            "profile-mounted-private",
        )
    else {
        panic!("exact-profile project-open mount must resolve");
    };
    assert!(credential.permits(GitHubReadPermissionV1::PullRequests));
    assert!(!credential.permits(GitHubReadPermissionV1::Actions));
    assert!(!credential.permits(GitHubReadPermissionV1::Checks));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (headers, _) = read_http_request_with_headers(&mut stream);
        let fixture: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
        write_http_json(&mut stream, &fixture["response"]);
        headers
    });
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: format!("http://{address}"),
        graphql_uri: format!("http://{address}/graphql"),
        ..GitHubHttpReadConfigV1::default()
    };
    let client = GitHubReadOnlyClientV1 {
        target: GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "profile-mounted-private".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        credential,
        transport: test_http_transport(config),
    };
    let request_scope = scope("exact-profile-project-open");
    let outcome = client
        .execute_graphql(
            &context(&request_scope),
            &GitHubGraphQlReadRequestV1 {
                scope: request_scope,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
                resume: GitHubReadResumeV1::empty(),
            },
        )
        .await;
    let GitHubReadNetworkOutcomeV1::Response(response) = outcome else {
        panic!("exact-profile project-open review read must contribute a response");
    };
    let envelope: GraphQlResponseV1 = serde_json::from_slice(&response.body).unwrap();
    assert!(
        !envelope
            .data
            .unwrap()
            .repository
            .unwrap()
            .pull_request
            .unwrap()
            .review_threads
            .nodes
            .is_empty()
    );
    let headers = server.join().unwrap().to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer github_pat_exact_profile_fixture\r\n"));
    assert!(
        std::fs::read_dir(profile_root.path())
            .unwrap()
            .next()
            .is_none(),
        "credential mount must not persist token material",
    );
    assert!(unregister_profile_github_read_only_credential_authority_v1(
        &exact_profile,
        "ScriptedAlchemy",
        "profile-mounted-private",
        &authority,
    ));
}

#[test]
fn application_registration_retains_and_exactly_unregisters_authority() {
    let authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> =
        Arc::new(FixtureCredentialAuthorityV1 {
            mode: FixtureCredentialAuthorityModeV1::Verified,
        });
    let weak = Arc::downgrade(&authority);
    assert!(register_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        "retained-private",
        &authority,
    ));
    drop(authority);
    let retained = weak
        .upgrade()
        .expect("application registry retains authority");
    let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
        resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", "retained-private")
    else {
        panic!("registered authority must issue a verified credential");
    };
    assert!(credential.permits(GitHubReadPermissionV1::PullRequests));
    assert!(unregister_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        "retained-private",
        &retained,
    ));
    assert!(!credential.permits(GitHubReadPermissionV1::PullRequests));
    assert!(
        credential
            .authorization_header_for(GitHubReadPermissionV1::PullRequests)
            .is_err()
    );
    drop(credential);
    drop(retained);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn anonymous_requests_never_emit_authorization() {
    let headers = captured_get_headers(GitHubReadOnlyCredentialV1::anonymous(), "tracedecay").await;
    assert!(!headers.to_ascii_lowercase().contains("authorization:"));
}

#[tokio::test]
async fn verified_private_requests_emit_secret_only_as_authorization() {
    let (_authority, resolution) =
        registered_fixture_credential("private-read", FixtureCredentialAuthorityModeV1::Verified);
    let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
        panic!("verified authority must resolve");
    };
    let target = GitHubRepositoryTargetV1 {
        owner: "ScriptedAlchemy".to_owned(),
        repository: "private-read".to_owned(),
        pull_request_number: 421,
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    assert!(
        GitHubReadOnlyClientV1::new(
            target.clone(),
            credential.clone(),
            GitHubHttpReadClientV1::new(GitHubHttpReadConfigV1::default()).unwrap(),
        )
        .is_some()
    );
    assert!(
        GitHubCiReadOnlyClientV1::new(
            GitHubCiRepositoryTargetV1 {
                owner: target.owner,
                repository: target.repository,
            },
            credential.clone(),
            GitHubHttpReadClientV1::new(GitHubHttpReadConfigV1::default()).unwrap(),
        )
        .is_some()
    );
    let headers = captured_get_headers(credential, "private-read")
        .await
        .to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer github_pat_fixture_private_read"));
}

#[test]
fn unavailable_write_capable_and_indeterminate_authorities_reject_before_network() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    for (repository, mode) in [
        (
            "not-configured",
            FixtureCredentialAuthorityModeV1::NotConfigured,
        ),
        (
            "write-capable",
            FixtureCredentialAuthorityModeV1::WriteCapable,
        ),
        (
            "indeterminate",
            FixtureCredentialAuthorityModeV1::Indeterminate,
        ),
    ] {
        let (_authority, resolution) = registered_fixture_credential(repository, mode);
        assert!(matches!(
            resolution,
            RegisteredGitHubReadOnlyCredentialV1::Rejected
        ));
    }
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
fn cached_private_credential_re_resolves_permission_and_repository_binding() {
    let authority = Arc::new(MutableFixtureCredentialAuthorityV1::new(
        FixtureCredentialAuthorityModeV1::Verified,
    ));
    let registered: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> = authority.clone();
    assert!(register_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        "permission-drift",
        &registered,
    ));
    let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
        resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", "permission-drift")
    else {
        panic!("initial verified authority must resolve");
    };
    let wrong_repository = GitHubRepositoryTargetV1 {
        owner: "ScriptedAlchemy".to_owned(),
        repository: "other-repository".to_owned(),
        pull_request_number: 421,
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    };
    assert!(
        GitHubReadOnlyClientV1::new(
            wrong_repository,
            credential.clone(),
            GitHubHttpReadClientV1::new(GitHubHttpReadConfigV1::default()).unwrap(),
        )
        .is_none()
    );

    authority.set_mode(FixtureCredentialAuthorityModeV1::WriteCapable);
    assert!(!credential.permits(GitHubReadPermissionV1::PullRequests));
    assert!(
        credential
            .authorization_header_for(GitHubReadPermissionV1::PullRequests)
            .is_err()
    );
}

#[test]
fn cached_private_credential_fails_closed_when_authority_expires() {
    let authority = Arc::new(MutableFixtureCredentialAuthorityV1::new(
        FixtureCredentialAuthorityModeV1::Verified,
    ));
    let registered: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> = authority.clone();
    assert!(register_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        "expired-private",
        &registered,
    ));
    let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
        resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", "expired-private")
    else {
        panic!("initial verified authority must resolve");
    };

    authority.set_mode(FixtureCredentialAuthorityModeV1::NotConfigured);

    assert!(!credential.permits(GitHubReadPermissionV1::PullRequests));
    assert!(
        credential
            .authorization_header_for(GitHubReadPermissionV1::PullRequests)
            .is_err()
    );
}

#[tokio::test]
async fn permission_drift_after_rest_response_blocks_response_publication() {
    let authority = Arc::new(MutableFixtureCredentialAuthorityV1::new(
        FixtureCredentialAuthorityModeV1::Verified,
    ));
    let registered: Arc<dyn GitHubReadOnlyCredentialAuthorityV1> = authority.clone();
    assert!(register_github_read_only_credential_authority_v1(
        "ScriptedAlchemy",
        "publication-drift",
        &registered,
    ));
    let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) =
        resolve_registered_github_read_only_credential_v1("ScriptedAlchemy", "publication-drift")
    else {
        panic!("initial verified authority must resolve");
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let authority_for_server = Arc::clone(&authority);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut stream);
        authority_for_server.set_mode(FixtureCredentialAuthorityModeV1::WriteCapable);
        write_http_json(
            &mut stream,
            &json!({
                "id": 4_026_204_542_u64,
                "number": 421,
                "base": {"sha": "commit.github.base"},
                "head": {"sha": "commit.github.head"}
            }),
        );
    });
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: format!("http://{address}"),
        graphql_uri: format!("http://{address}/graphql"),
        ..GitHubHttpReadConfigV1::default()
    };
    let client = GitHubReadOnlyClientV1 {
        target: GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "publication-drift".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        credential,
        transport: test_http_transport(config),
    };
    let request_scope = scope("publication-drift");
    let outcome = client
        .execute_rest(
            &context(&request_scope),
            &GitHubRestReadRequestV1 {
                descriptor: super::super::GitHubRestDescriptorV1 {
                    operation: GitHubReviewReadOperationV1::RestGetPullRequest,
                },
                scope: request_scope,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
                resume: GitHubReadResumeV1::empty(),
            },
        )
        .await;
    server.join().unwrap();

    assert_eq!(outcome, GitHubReadNetworkOutcomeV1::Denied);
}

fn scope(suffix: &str) -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new(format!("project.github.{suffix}")).unwrap(),
        repository_id: RepositoryId::new(format!("repository.github.{suffix}")).unwrap(),
        worktree_id: WorktreeId::new(format!("worktree.github.{suffix}")).unwrap(),
        branch_ref: format!("refs/heads/github-{suffix}"),
        head_commit_id: CommitId::new(format!("commit.github.{suffix}.head")).unwrap(),
    }
}

fn context(scope: &FeedbackScopeV1) -> RequestContext {
    let resolved = ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.github.owner-bound").unwrap(),
        1,
        ManifestDigest::new(SHA).unwrap(),
        ActorId::new("actor.github.issuer").unwrap(),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        resolved.clone(),
        BTreeSet::from([
            CapabilityId::new("capability.application.feedback.github-review-ingest").unwrap(),
        ]),
        BTreeSet::from([
            UseCaseId::new("use-case.application.feedback.github-review-ingest").unwrap(),
        ]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.github.owner-bound").unwrap(),
        resolved,
        grant,
        RequestId::new("request.github.owner-bound").unwrap(),
        Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
        CancellationContext::active("cancel.github.owner-bound").unwrap(),
    )
    .unwrap()
}

fn request(scope: FeedbackScopeV1) -> GitHubReviewReadRequestV1 {
    GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        scope,
        pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
    }
}

fn complete_response(request: &GitHubReviewReadRequestV1) -> GitHubReviewReadResponseV1 {
    GitHubReviewReadResponseV1 {
        ingress: GitHubReviewIngressResultV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            scope: request.scope.clone(),
            pull_request_id: request.pull_request_id.clone(),
            provider_base_commit_id: CommitId::new("commit.github.base").unwrap(),
            provider_head_commit_id: request.scope.head_commit_id.clone(),
            merge_base_commit_id: CommitId::new("commit.github.merge-base").unwrap(),
            operation: request.operation,
            outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
            coverage: GitHubReviewCoverageV1::Complete,
            items: Vec::new(),
            fetched_at: UtcMicros(10),
        },
        checkpoint: GitHubReviewReadCheckpointV1 {
            etag: None,
            next_cursor: None,
            rate_limit: None,
        },
    }
}

mod async_transport;
mod reads;

use reads::{read_http_request, read_http_request_with_headers, write_http_json};

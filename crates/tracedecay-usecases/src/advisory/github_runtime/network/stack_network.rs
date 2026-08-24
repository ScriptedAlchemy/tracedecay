//! Authenticated, read-only GitHub stack acquisition and exact compare binding.

use serde_json::json;
use tracedecay_application::feedback::GitHubReviewReadRequestV1;
use tracedecay_application::{RequestContext, now_micros};
use tracedecay_domain::{CommitId, ProviderId, canonical_sha256};
use url::Url;

use super::{GitHubReadOnlyClientV1, GitHubReadPermissionV1, HttpResponseV1, parse_bounded};
use crate::advisory::github_runtime::dto::{GraphQlStackResponseV1, RestComparisonV1};
use crate::advisory::github_runtime::stack::{GITHUB_STACK_QUERY_V1, decode_stack_snapshot};
use crate::advisory::github_runtime::stack_anchors::GitHubStackReadAuthorityV1;
use crate::advisory::github_runtime::{GitHubGraphQlReadRequestV1, GitHubReadResumeV1};
use crate::stack_coordinator::GitHubStackProviderOutcomeV1;

impl GitHubReadOnlyClientV1 {
    /// Reads the optional GitHub stack through an authenticated static GraphQL
    /// query and fixed compare GETs for exact merge-base evidence. No query
    /// text, HTTP verb, or provider mutation is supplied by the caller.
    pub(in crate::advisory::github_runtime) fn read_stack(
        &self,
        context: &RequestContext,
        request: &GitHubGraphQlReadRequestV1,
        review_request: &GitHubReviewReadRequestV1,
        provider: &ProviderId,
        anchors: &dyn GitHubStackReadAuthorityV1,
    ) -> GitHubStackProviderOutcomeV1 {
        if !super::request_context_admitted(context)
            || request.pull_request_id != self.target.pull_request_id
            || request.scope != review_request.scope
            || request.pull_request_id != review_request.pull_request_id
            || request.resume != GitHubReadResumeV1::empty()
        {
            return GitHubStackProviderOutcomeV1::Unavailable;
        }
        let payload = json!({
            "query": GITHUB_STACK_QUERY_V1,
            "variables": {
                "owner": self.target.owner,
                "repository": self.target.repository,
                "number": self.target.pull_request_number,
            },
        });
        let HttpResponseV1::Ok { body, .. } = self.post_static_graphql(&payload) else {
            return GitHubStackProviderOutcomeV1::Unavailable;
        };
        if !super::request_context_admitted(context) {
            return GitHubStackProviderOutcomeV1::Unavailable;
        }
        let Ok(response_digest) = canonical_sha256(&(
            "tracedecay.github-stack.graphql-response.v1",
            &self.target.owner,
            &self.target.repository,
            self.target.pull_request_number,
            &body,
        )) else {
            return GitHubStackProviderOutcomeV1::Unavailable;
        };
        let Some(envelope) = serde_json::from_slice::<GraphQlStackResponseV1>(&body).ok() else {
            return GitHubStackProviderOutcomeV1::Degraded { response_digest };
        };
        if !envelope.errors.is_empty() {
            return GitHubStackProviderOutcomeV1::Unavailable;
        }
        let Some(pull_request) = envelope
            .data
            .and_then(|data| data.repository)
            .and_then(|repository| repository.pull_request)
        else {
            return GitHubStackProviderOutcomeV1::Degraded { response_digest };
        };
        let Some(stack) = pull_request.stack.clone() else {
            return GitHubStackProviderOutcomeV1::EnabledWithoutStack { response_digest };
        };
        let selected_position = pull_request.stack_entry.map(|entry| entry.position);
        match decode_stack_snapshot(&self.target, selected_position, stack, response_digest) {
            Ok(decoded) => {
                let mut merge_base_commit_ids = Vec::with_capacity(decoded.layers.len());
                for layer in &decoded.layers {
                    let Some(merge_base_commit_id) = self.read_compare_merge_base(
                        context,
                        &layer.base_commit_id,
                        &layer.head_commit_id,
                    ) else {
                        return GitHubStackProviderOutcomeV1::Degraded {
                            response_digest: decoded.response_digest.clone(),
                        };
                    };
                    merge_base_commit_ids.push(merge_base_commit_id);
                }
                match anchors.bind_provider_snapshot(
                    context,
                    review_request,
                    provider,
                    decoded,
                    merge_base_commit_ids,
                    now_micros(),
                ) {
                    Some(snapshot) => GitHubStackProviderOutcomeV1::Enabled(snapshot),
                    None => GitHubStackProviderOutcomeV1::Unavailable,
                }
            }
            Err(response_digest) => GitHubStackProviderOutcomeV1::Degraded { response_digest },
        }
    }

    fn read_compare_merge_base(
        &self,
        context: &RequestContext,
        base_commit_id: &CommitId,
        head_commit_id: &CommitId,
    ) -> Option<CommitId> {
        if !super::request_context_admitted(context) {
            return None;
        }
        let mut url = Url::parse(&self.config.rest_base_uri).ok()?;
        let comparison = format!("{}...{}", base_commit_id.as_str(), head_commit_id.as_str());
        url.path_segments_mut().ok()?.extend([
            "repos",
            self.target.owner.as_str(),
            self.target.repository.as_str(),
            "compare",
            comparison.as_str(),
        ]);
        let response = self.get(url.as_str(), None, GitHubReadPermissionV1::PullRequests);
        if !super::request_context_admitted(context) {
            return None;
        }
        let HttpResponseV1::Ok { body, .. } = response else {
            return None;
        };
        let comparison = parse_bounded::<RestComparisonV1>(&body)?;
        CommitId::new(comparison.merge_base_commit.sha).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use serde_json::json;
    use tracedecay_application::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
    use tracedecay_application::retrieval::{
        GitTopologyAnchorAuthorityV2, GitTopologyAnchorResolutionOutcomeV2,
        GitTopologyAnchorResolutionV2,
    };
    use tracedecay_application::{RequestContext, now_micros};
    use tracedecay_domain::ObservationScopeV1;
    use tracedecay_domain::{CommitId, ProviderId, UtcMicros};
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;

    use super::super::test_support::{
        read_http_request, read_http_request_with_headers, write_http_json,
    };
    use super::super::tests::{
        FixtureCredentialAuthorityModeV1, context, registered_fixture_credential, request, scope,
    };
    use super::super::{
        GitHubHttpReadConfigV1, GitHubRepositoryTargetV1, RegisteredGitHubReadOnlyCredentialV1,
    };
    use super::*;
    use crate::advisory::github_runtime::{
        GitHubGraphQlReadRequestV1, GitHubProviderLifecycleV1, GitHubReadResumeV1,
        GitHubSourceAccessAuthorityV1, ProjectGitHubStackAnchorAuthorityV1,
    };
    use crate::stack_coordinator::{DaemonGitHubStackCoordinatorV1, GitHubStackProviderSnapshotV1};

    struct ReadyStackSourceAccess;

    impl GitHubSourceAccessAuthorityV1 for ReadyStackSourceAccess {
        fn authorize<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
            Box::pin(async { GitHubProviderLifecycleV1::Ready })
        }
    }

    struct RejectStackBinding;

    impl GitHubStackReadAuthorityV1 for RejectStackBinding {
        fn bind_provider_snapshot(
            &self,
            _context: &RequestContext,
            _request: &GitHubReviewReadRequestV1,
            _provider: &ProviderId,
            _decoded: crate::advisory::github_runtime::stack::DecodedGitHubStackSnapshotV1,
            _merge_base_commit_ids: Vec<CommitId>,
            _observed_at: UtcMicros,
        ) -> Option<GitHubStackProviderSnapshotV1> {
            None
        }
    }

    #[test]
    fn unavailable_compare_degrades_decoded_stack_without_enabling_snapshot() {
        let mut scope = scope("stack-compare-unavailable");
        scope.head_commit_id = CommitId::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let context = context(&scope);
        let request = request(scope.clone());
        let provider = ProviderId::new("provider.github").unwrap();
        let (_credential_authority, resolution) = registered_fixture_credential(
            "stack-compare-unavailable",
            FixtureCredentialAuthorityModeV1::Verified,
        );
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
            panic!("verified fixture credential must resolve");
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let head_commit = scope.head_commit_id.as_str().to_owned();
        let server = std::thread::spawn(move || {
            let (mut graphql, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut graphql);
            write_http_json(
                &mut graphql,
                &json!({
                    "data": { "repository": { "pullRequest": {
                        "stackEntry": { "position": 1 },
                        "stack": {
                            "id": "stack-node-compare-unavailable",
                            "number": 421,
                            "baseRefName": "main",
                            "size": 1,
                            "entries": {
                                "totalCount": 1,
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": [{
                                    "position": 1,
                                    "pullRequest": {
                                        "number": 421,
                                        "baseRefName": "main",
                                        "headRefName": "github-stack-compare-unavailable",
                                        "baseRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                                        "headRefOid": head_commit,
                                        "baseRef": null,
                                        "statusCheckRollup": { "state": "SUCCESS" },
                                        "mergeQueueEntry": null
                                    }
                                }]
                            }
                        }
                    }}}
                }),
            );
            let (mut compare, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut compare);
            compare
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "stack-compare-unavailable".to_owned(),
                pull_request_number: 421,
                pull_request_id: request.pull_request_id.clone(),
            },
            credential,
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let outcome = client.read_stack(
            &context,
            &GitHubGraphQlReadRequestV1 {
                scope: scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                resume: GitHubReadResumeV1::empty(),
            },
            &request,
            &provider,
            &RejectStackBinding,
        );
        server.join().unwrap();

        assert!(matches!(
            outcome,
            GitHubStackProviderOutcomeV1::Degraded { .. }
        ));
    }

    #[tokio::test]
    async fn authenticated_stack_graphql_and_compare_publish_restart_safe_v3_lineage() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let mut scope = scope("stack-anchor-http");
        scope.head_commit_id = CommitId::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let context = context(&scope);
        let request = request(scope.clone());
        let profile = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let runtime = RegisteredGlobalDbTestRuntime::project(
            profile.path(),
            project.path(),
            scope.project_id.clone(),
        )
        .await
        .unwrap();
        let database = runtime.project_database_arc().unwrap();
        let anchors =
            ProjectGitHubStackAnchorAuthorityV1::new(database.clone(), scope.clone()).unwrap();
        let provider = ProviderId::new("provider.github").unwrap();
        let (_credential_authority, resolution) = registered_fixture_credential(
            "stack-anchor-http",
            FixtureCredentialAuthorityModeV1::Verified,
        );
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
            panic!("verified fixture credential must resolve");
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let head_commit = scope.head_commit_id.as_str().to_owned();
        let server = std::thread::spawn(move || {
            let (mut graphql, _) = listener.accept().unwrap();
            let (headers, payload) = read_http_request_with_headers(&mut graphql);
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer github_pat_fixture_private_read")
            );
            assert!(payload["query"].as_str().unwrap().contains("stackEntry"));
            write_http_json(
                &mut graphql,
                &json!({
                    "data": { "repository": { "pullRequest": {
                        "stackEntry": { "position": 1 },
                        "stack": {
                            "id": "stack-node-private-1",
                            "number": 421,
                            "baseRefName": "main",
                            "size": 1,
                            "entries": {
                                "totalCount": 1,
                                "pageInfo": { "hasNextPage": false, "endCursor": null },
                                "nodes": [{
                                    "position": 1,
                                    "pullRequest": {
                                        "number": 421,
                                        "baseRefName": "main",
                                        "headRefName": "github-stack-anchor-http",
                                        "baseRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                                        "headRefOid": head_commit,
                                        "baseRef": null,
                                        "statusCheckRollup": { "state": "SUCCESS" },
                                        "mergeQueueEntry": {
                                            "id": "queue-private-1",
                                            "position": 1,
                                            "state": "QUEUED"
                                        }
                                    }
                                }]
                            }
                        }
                    }}}
                }),
            );
            let (mut compare, _) = listener.accept().unwrap();
            let (headers, _) = read_http_request_with_headers(&mut compare);
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer github_pat_fixture_private_read")
            );
            assert!(headers.contains("/compare/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa..."));
            write_http_json(
                &mut compare,
                &json!({
                    "merge_base_commit": { "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
                }),
            );
        });
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "stack-anchor-http".to_owned(),
                pull_request_number: 421,
                pull_request_id: request.pull_request_id.clone(),
            },
            credential,
            config: GitHubHttpReadConfigV1 {
                rest_base_uri: format!("http://{address}"),
                graphql_uri: format!("http://{address}/graphql"),
                ..GitHubHttpReadConfigV1::default()
            },
        };
        let provider_outcome = client.read_stack(
            &context,
            &GitHubGraphQlReadRequestV1 {
                scope: scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                resume: GitHubReadResumeV1::empty(),
            },
            &request,
            &provider,
            &anchors,
        );
        server.join().unwrap();
        let GitHubStackProviderOutcomeV1::Enabled(ref provider_snapshot) = provider_outcome else {
            panic!("authenticated GraphQL and compare must yield enabled stack");
        };
        assert_eq!(provider_snapshot.layers.len(), 1);
        assert_eq!(
            provider_snapshot.layers[0]
                .pull_request
                .merge_base_commit_id
                .as_str(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let observed_at = now_micros();
        let source_binding = anchors
            .source_binding(&context, &request, &provider_outcome, observed_at)
            .unwrap();
        let coordinator = DaemonGitHubStackCoordinatorV1::default();
        coordinator
            .register_scope(
                context.scope(),
                tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
            )
            .unwrap();
        let observation = coordinator
            .observe_provider(
                context.scope().clone(),
                provider,
                provider_outcome,
                source_binding.clone(),
                observed_at,
            )
            .unwrap();
        assert_eq!(
            anchors
                .publish(&context, &request, &observation, &ReadyStackSourceAccess)
                .await,
            crate::advisory::github_runtime::GitHubStackAnchorPublicationOutcomeV1::Published
        );
        drop(anchors);
        drop(database);
        drop(runtime);
        let restarted = RegisteredGlobalDbTestRuntime::project(
            profile.path(),
            project.path(),
            scope.project_id.clone(),
        )
        .await
        .unwrap();
        let database = restarted.project_database_arc().unwrap();
        let anchors =
            ProjectGitHubStackAnchorAuthorityV1::new(database.clone(), scope.clone()).unwrap();
        let durable = anchors
            .resolve_published_observation(context.scope(), observation)
            .await
            .unwrap();
        let snapshot = durable.snapshot_anchor.as_ref().unwrap();
        let mut lineage = durable.capability_anchor.source_anchors().to_vec();
        lineage.extend_from_slice(snapshot.source_anchors());
        for source in lineage {
            let store =
                tracedecay_global_db::RegisteredGitTopologyAnchorAuthorityV2::new(database.clone());
            let owner = ObservationScopeV1::Project {
                project_id: scope.project_id.clone(),
            };
            let Ok(GitTopologyAnchorResolutionOutcomeV2::Resolved(source_record)) = store
                .resolve(
                    GitTopologyAnchorResolutionV2::new(owner.clone(), source.anchor_id().clone())
                        .unwrap(),
                )
                .await
            else {
                panic!("every stack lineage source must resolve after restart");
            };
            for nested in source_record.source_anchors() {
                assert!(matches!(
                    store
                        .resolve(
                            GitTopologyAnchorResolutionV2::new(
                                owner.clone(),
                                nested.anchor_id().clone(),
                            )
                            .unwrap(),
                        )
                        .await,
                    Ok(GitTopologyAnchorResolutionOutcomeV2::Resolved(_))
                ));
            }
        }
    }
}

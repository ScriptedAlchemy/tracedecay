use tracedecay_domain::{
    CommitId, GitHubPullRequestIdV1, ManifestDigest, PrivacyDomainBoundLocatorDigest, RefId,
    canonical_sha256,
};

use super::dto::GraphQlPullRequestStackV1;
use super::network::GitHubRepositoryTargetV1;
use crate::stack_coordinator::MAX_GITHUB_STACK_LAYERS_V1;

pub(super) const GITHUB_STACK_QUERY_V1: &str = r"
query TraceDecayGitHubStack($owner: String!, $repository: String!, $number: Int!) {
  repository(owner: $owner, name: $repository) {
    pullRequest(number: $number) {
      stackEntry { position }
      stack {
        id number baseRefName size
        entries(first: 100) {
          totalCount
          pageInfo { hasNextPage endCursor }
          nodes {
            position
            pullRequest {
              number baseRefName headRefName baseRefOid headRefOid
              baseRef {
                name target { ... on Commit { oid } }
                branchProtectionRule {
                  id pattern requiresApprovingReviews requiresCodeOwnerReviews
                  requiresStatusChecks requiresStrictStatusChecks
                  requiredApprovingReviewCount requiredStatusCheckContexts
                }
              }
              statusCheckRollup { state }
              mergeQueueEntry { id position state }
            }
          }
        }
      }
    }
  }
}
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DecodedGitHubStackLayerV1 {
    pub provider_position: u32,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub base_ref_id: RefId,
    pub head_ref_id: RefId,
    pub base_commit_id: CommitId,
    pub head_commit_id: CommitId,
    pub protection_digest: ManifestDigest,
    pub ci_digest: ManifestDigest,
    pub merge_queue_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DecodedGitHubStackSnapshotV1 {
    pub response_digest: ManifestDigest,
    pub provider_stack_id_digest: tracedecay_domain::PrivacyDomainBoundLocatorDigest,
    pub final_target_ref_id: RefId,
    pub final_target_commit_id: CommitId,
    pub layers: Vec<DecodedGitHubStackLayerV1>,
}

pub(super) fn decode_stack_snapshot(
    target: &GitHubRepositoryTargetV1,
    selected_position: Option<u32>,
    stack: GraphQlPullRequestStackV1,
    response_digest: ManifestDigest,
) -> Result<DecodedGitHubStackSnapshotV1, ManifestDigest> {
    let entries = stack.entries;
    if stack.id.is_empty()
        || stack.number == 0
        || stack.base_ref_name.is_empty()
        || usize::try_from(stack.size).ok() != Some(entries.nodes.len())
        || stack.size != entries.total_count
        || entries.page_info.has_next_page
        || entries.nodes.is_empty()
        || entries.nodes.len() > MAX_GITHUB_STACK_LAYERS_V1
    {
        return Err(response_digest);
    }
    let mut layers = Vec::with_capacity(entries.nodes.len());
    let mut selected_found = false;
    for (index, entry) in entries.nodes.into_iter().enumerate() {
        let expected_position = u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1));
        let Some(pull_request) = entry.pull_request else {
            return Err(response_digest);
        };
        if Some(entry.position) != expected_position
            || pull_request.number == 0
            || pull_request.base_ref_name.is_empty()
            || pull_request.head_ref_name.is_empty()
            || pull_request.base_ref_oid.is_empty()
            || pull_request.head_ref_oid.is_empty()
            || pull_request
                .base_ref
                .as_ref()
                .and_then(|reference| reference.target.as_ref())
                .is_some_and(|commit| commit.oid != pull_request.base_ref_oid)
        {
            return Err(response_digest);
        }
        if pull_request.number == target.pull_request_number {
            selected_found = selected_position == Some(entry.position);
        }
        let pull_request_id = if pull_request.number == target.pull_request_number {
            target.pull_request_id.clone()
        } else {
            let Ok(pull_request_id) = GitHubPullRequestIdV1::new(pull_request.number.to_string())
            else {
                return Err(response_digest);
            };
            pull_request_id
        };
        let Some(base_ref_id) = github_head_ref(&pull_request.base_ref_name) else {
            return Err(response_digest);
        };
        let Some(head_ref_id) = github_head_ref(&pull_request.head_ref_name) else {
            return Err(response_digest);
        };
        let Ok(base_commit_id) = CommitId::new(pull_request.base_ref_oid) else {
            return Err(response_digest);
        };
        let Ok(head_commit_id) = CommitId::new(pull_request.head_ref_oid) else {
            return Err(response_digest);
        };
        let Ok(protection_digest) = canonical_sha256(&(
            "tracedecay.github-stack.protection.v1",
            pull_request
                .base_ref
                .as_ref()
                .and_then(|reference| reference.branch_protection_rule.as_ref()),
        )) else {
            return Err(response_digest);
        };
        let Ok(ci_digest) = canonical_sha256(&(
            "tracedecay.github-stack.ci.v1",
            pull_request.status_check_rollup.as_ref(),
        )) else {
            return Err(response_digest);
        };
        let Ok(merge_queue_digest) = canonical_sha256(&(
            "tracedecay.github-stack.merge-queue.v1",
            pull_request.merge_queue_entry.as_ref(),
        )) else {
            return Err(response_digest);
        };
        layers.push(DecodedGitHubStackLayerV1 {
            provider_position: entry.position,
            pull_request_id,
            base_ref_id,
            head_ref_id,
            base_commit_id,
            head_commit_id,
            protection_digest,
            ci_digest,
            merge_queue_digest,
        });
    }
    if !selected_found
        || layers.first().is_none_or(|layer| {
            layer.base_ref_id.as_str() != github_head_ref_text(&stack.base_ref_name)
        })
        || layers.windows(2).any(|pair| {
            pair[1].base_ref_id != pair[0].head_ref_id
                || pair[1].base_commit_id != pair[0].head_commit_id
        })
    {
        return Err(response_digest);
    }
    let Some(final_layer) = layers.first() else {
        return Err(response_digest);
    };
    let Ok(provider_stack_id_manifest) = canonical_sha256(&(
        "tracedecay.github-stack.provider-id.v1",
        &target.owner,
        &target.repository,
        stack.number,
        &stack.id,
    )) else {
        return Err(response_digest);
    };
    let Ok(provider_stack_id_digest) =
        PrivacyDomainBoundLocatorDigest::new(provider_stack_id_manifest.as_str())
    else {
        return Err(response_digest);
    };
    Ok(DecodedGitHubStackSnapshotV1 {
        response_digest,
        provider_stack_id_digest,
        final_target_ref_id: final_layer.base_ref_id.clone(),
        final_target_commit_id: final_layer.base_commit_id.clone(),
        layers,
    })
}

fn github_head_ref(name: &str) -> Option<RefId> {
    (!name.is_empty()
        && name.len() <= 255
        && !name.bytes().any(|byte| byte.is_ascii_control())
        && !name.contains("..")
        && !name.starts_with('/')
        && !name.ends_with('/'))
    .then(|| RefId::new(github_head_ref_text(name)).ok())
    .flatten()
}

fn github_head_ref_text(name: &str) -> String {
    format!("refs/heads/{name}")
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::GitHubPullRequestIdV1;

    use super::super::network::GITHUB_REVIEW_THREADS_QUERY_V1;
    use super::*;

    #[test]
    fn static_read_query_decodes_exact_linear_stack_without_write_authority() {
        assert!(!GITHUB_REVIEW_THREADS_QUERY_V1.contains("stackEntry { position }"));
        assert!(GITHUB_STACK_QUERY_V1.contains("stackEntry { position }"));
        assert!(GITHUB_STACK_QUERY_V1.contains("statusCheckRollup { state }"));
        assert!(!GITHUB_STACK_QUERY_V1.contains("mutation "));
        let target = GitHubRepositoryTargetV1 {
            owner: "octo-org".to_owned(),
            repository: "stack-repository".to_owned(),
            pull_request_number: 42,
            pull_request_id: GitHubPullRequestIdV1::new("42").unwrap(),
        };
        let pull_request: super::super::dto::GraphQlSelectedStackPullRequestV1 = serde_json::from_value(
            serde_json::json!({
                "stackEntry": { "position": 2 },
                "stack": {
                    "id": "stack-node-1",
                    "number": 7,
                    "baseRefName": "main",
                    "size": 2,
                    "entries": {
                        "totalCount": 2,
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
                        "nodes": [
                            {
                                "position": 1,
                                "pullRequest": {
                                    "number": 41,
                                    "baseRefName": "main",
                                    "headRefName": "lower",
                                    "baseRefOid": "commit.main",
                                    "headRefOid": "commit.lower",
                                    "baseRef": null,
                                    "statusCheckRollup": { "state": "SUCCESS" },
                                    "mergeQueueEntry": null
                                }
                            },
                            {
                                "position": 2,
                                "pullRequest": {
                                    "number": 42,
                                    "baseRefName": "lower",
                                    "headRefName": "upper",
                                    "baseRefOid": "commit.lower",
                                    "headRefOid": "commit.upper",
                                    "baseRef": null,
                                    "statusCheckRollup": { "state": "PENDING" },
                                    "mergeQueueEntry": { "id": "queue-1", "position": 1, "state": "QUEUED" }
                                }
                            }
                        ]
                    }
                }
            }),
        )
        .unwrap();
        let snapshot = decode_stack_snapshot(
            &target,
            pull_request.stack_entry.map(|entry| entry.position),
            pull_request.stack.clone().unwrap(),
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.layers.len(), 2);
        assert_eq!(snapshot.layers[0].provider_position, 1);
        assert_eq!(snapshot.layers[1].pull_request_id.as_str(), "42");
        assert_eq!(snapshot.final_target_ref_id.as_str(), "refs/heads/main");
        assert_eq!(snapshot.final_target_commit_id.as_str(), "commit.main");
    }
}

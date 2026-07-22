//! PR13 GitHub adapter for the retained external-source vertical.
//!
//! GitHub identifiers are used transiently to derive privacy-bound digests.
//! The resulting source commit contains no endpoint, credential, URL, path,
//! title, body, or provider payload.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::SourceCaptureAdmissionV1;
use tracedecay_application::feedback::GitHubReviewReadRequestV1;
use tracedecay_domain::feedback::{GitHubReviewCoverageV1, GitHubReviewLifecycleV1};
use tracedecay_domain::{
    ComponentVersion, LocatorDigest, PrivacyDomainId, SourceAggregateFrontierV1,
    SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1, SourceContentStateV1,
    SourceCoverageV1, SourceCursorV1, SourceDefinitionV1, SourceDeletionSemanticsV1,
    SourceInstanceId, SourceNativeObjectIdV1, SourceObjectObservationV1, SourceObjectRevisionV1,
    SourcePartitionFrontierV1, SourcePartitionIdV1, SourceRefetchStrategyV1,
    SourceSnapshotCompletionV1, SourceSnapshotIdV1, canonical_sha256,
};
use tracedecay_store::SourceCommitV1;

use super::GitHubReviewRefreshStateV1;

const GITHUB_NATIVE_ROOT_DOMAIN_V1: &str = "tracedecay.pr13.github.native-root.v1";
const GITHUB_PRIVACY_DOMAIN_V1: &str = "tracedecay.pr13.github.privacy-domain.v1";
const GITHUB_PARTITION_DOMAIN_V1: &str = "tracedecay.pr13.github.partition.v1";
const GITHUB_CURSOR_DOMAIN_V1: &str = "tracedecay.pr13.github.cursor.v1";
const GITHUB_SNAPSHOT_DOMAIN_V1: &str = "tracedecay.pr13.github.snapshot.v1";
const GITHUB_OBJECT_DOMAIN_V1: &str = "tracedecay.pr13.github.native-object.v1";
const GITHUB_OBJECT_DIGEST_DOMAIN_V1: &str = "tracedecay.pr13.github.object-digest.v1";
const GITHUB_REQUEST_DOMAIN_V1: &str = "tracedecay.pr13.github.source-request.v1";
const GITHUB_IDEMPOTENCY_DOMAIN_V1: &str = "tracedecay.pr13.github.source-idempotency.v1";
const GITHUB_PROJECTOR_VERSION_V1: &str = "github-review-source-projector-v1";

pub(super) fn source_commit_for_refresh(
    request: &GitHubReviewReadRequestV1,
    next: &GitHubReviewRefreshStateV1,
    expected_frontier: Option<&SourceAggregateFrontierV1>,
) -> Option<SourceCommitV1> {
    request.validate().ok()?;
    next.validate_for(request).then_some(())?;
    let source_id = SourceInstanceId::new("github-review-pr13-v1").ok()?;
    let definition = SourceDefinitionV1::new(
        source_id,
        next.latest_attempt.ingress.provider.clone(),
        1,
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
        4,
    )
    .ok()?;
    let privacy_domain_digest = canonical_sha256(&(
        GITHUB_PRIVACY_DOMAIN_V1,
        &request.scope.project_id,
        &definition.source_id,
    ))
    .ok()?;
    let privacy_domain = PrivacyDomainId::new(format!(
        "github-review.{}",
        privacy_domain_digest.as_str().trim_start_matches("sha256:")
    ))
    .ok()?;
    let native_root = LocatorDigest::new(
        canonical_sha256(&(
            GITHUB_NATIVE_ROOT_DOMAIN_V1,
            &privacy_domain,
            &definition.source_id,
            &request.scope.project_id,
            &request.scope.repository_id,
            &request.pull_request_id,
        ))
        .ok()?
        .as_str(),
    )
    .ok()?;
    let binding = SourceBindingV1::new(
        &definition,
        SourceBindingOwnerV1::Project(request.scope.project_id.clone()),
        privacy_domain,
        native_root,
        1,
    )
    .ok()?;
    let binding_identity = binding.immutable_identity().ok()?;
    let partition = SourcePartitionIdV1::new(
        canonical_sha256(&(GITHUB_PARTITION_DOMAIN_V1, request.operation)).ok()?,
    );
    let expected_partition = expected_frontier.and_then(|frontier| frontier.partition(&partition));
    let coverage = source_coverage(next.latest_attempt.ingress.coverage);
    let cursor = match &next.latest_attempt.checkpoint.next_cursor {
        Some(cursor) => Some(SourceCursorV1::new(
            canonical_sha256(&(GITHUB_CURSOR_DOMAIN_V1, cursor)).ok()?,
        )),
        None => None,
    };
    let continuation = if coverage == SourceCoverageV1::Partial {
        Some(cursor.clone().unwrap_or(SourceCursorV1::new(
            canonical_sha256(&(GITHUB_CURSOR_DOMAIN_V1, &next.revision)).ok()?,
        )))
    } else {
        None
    };
    let snapshot = if coverage == SourceCoverageV1::Complete {
        Some(SourceSnapshotIdV1::new(
            canonical_sha256(&(GITHUB_SNAPSHOT_DOMAIN_V1, &next.revision)).ok()?,
        ))
    } else {
        None
    };
    let observations = source_observations(&binding_identity, next)?;
    let input_digest = canonical_sha256(&(
        GITHUB_REQUEST_DOMAIN_V1,
        request,
        &next.revision,
        &next.latest_attempt.ingress.coverage,
    ))
    .ok()?;
    let next_partition = SourcePartitionFrontierV1::new(
        binding_identity,
        partition.clone(),
        cursor,
        snapshot.clone(),
        continuation,
        coverage,
        expected_partition.map_or(0, SourcePartitionFrontierV1::sequence) + 1,
        expected_partition.and_then(SourcePartitionFrontierV1::last_complete_snapshot),
        input_digest.clone(),
    )
    .ok()?;
    let snapshot_completion = match snapshot {
        Some(snapshot) => Some(
            SourceSnapshotCompletionV1::new(
                partition.clone(),
                snapshot,
                observations
                    .iter()
                    .filter(|observation| {
                        observation.content_state() != SourceContentStateV1::AuthoritativeDeleted
                    })
                    .map(|observation| observation.native_object().clone())
                    .collect::<BTreeSet<_>>(),
            )
            .ok()?,
        ),
        None => None,
    };
    let idempotency_key = canonical_sha256(&(
        GITHUB_IDEMPOTENCY_DOMAIN_V1,
        &input_digest,
        &next.revision,
        &binding.binding_digest,
    ))
    .ok()?;
    let admission = SourceCaptureAdmissionV1::new(
        definition,
        binding,
        expected_frontier.cloned(),
        next_partition,
        observations,
        snapshot_completion,
        idempotency_key,
        input_digest,
    )
    .ok()?;
    let (
        definition,
        binding,
        expected_frontier,
        next_frontier,
        observations,
        snapshot_completion,
        idempotency_key,
        request_digest,
    ) = admission.into_parts();
    SourceCommitV1::new(
        definition,
        binding,
        partition,
        ComponentVersion::new(GITHUB_PROJECTOR_VERSION_V1).ok()?,
        idempotency_key,
        request_digest,
        expected_frontier,
        next_frontier,
        observations,
        snapshot_completion,
    )
    .ok()
}

fn source_coverage(coverage: GitHubReviewCoverageV1) -> SourceCoverageV1 {
    match coverage {
        GitHubReviewCoverageV1::Complete => SourceCoverageV1::Complete,
        GitHubReviewCoverageV1::Partial => SourceCoverageV1::Partial,
        GitHubReviewCoverageV1::Unavailable
        | GitHubReviewCoverageV1::Denied
        | GitHubReviewCoverageV1::Stale => SourceCoverageV1::Unknown,
    }
}

fn source_observations(
    binding: &tracedecay_domain::SourceBindingIdentityV1,
    next: &GitHubReviewRefreshStateV1,
) -> Option<Vec<SourceObjectObservationV1>> {
    let mut observations = BTreeMap::new();
    for item in &next.latest_attempt.ingress.items {
        let native_object = SourceNativeObjectIdV1::new(
            canonical_sha256(&(GITHUB_OBJECT_DOMAIN_V1, binding, &item.comment_id)).ok()?,
        );
        let content_state = if item.lifecycle == GitHubReviewLifecycleV1::Deleted {
            SourceContentStateV1::AuthoritativeDeleted
        } else {
            SourceContentStateV1::Live
        };
        let observation = SourceObjectObservationV1::new(
            native_object.clone(),
            SourceObjectRevisionV1::new(item.body_digest.clone()),
            canonical_sha256(&(
                GITHUB_OBJECT_DIGEST_DOMAIN_V1,
                binding,
                &item.body_digest,
                item.lifecycle,
                content_state,
            ))
            .ok()?,
            content_state,
        )
        .ok()?;
        match observations.insert(native_object, observation.clone()) {
            Some(previous) if previous != observation => return None,
            Some(_) | None => {}
        }
    }
    Some(observations.into_values().collect())
}

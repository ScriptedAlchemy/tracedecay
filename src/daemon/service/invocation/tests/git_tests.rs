//! `git` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[test]
fn git_read_output_limit_maps_to_a_stable_saturated_problem() {
    let problem = git_read_output_limit_problem();
    assert!(matches!(
        problem,
        ApplicationProblem::Saturated {
            diagnostic: SafeDiagnostic { ref code, .. },
            retry: RetryDirective::Never,
            ..
        } if code == "git_read.output_limit_exceeded"
    ));
}

#[test]
fn git_read_packet_binds_catalog_authority_and_native_coverage() {
    let scope = ResolvedScope::new(
        ProjectId::new("project.git-read-packet").expect("project"),
        tracedecay_domain::RepositoryId::new("repository.git-read-packet").expect("repository"),
        tracedecay_domain::WorktreeId::new("worktree.git-read-packet").expect("worktree"),
        Some(tracedecay_domain::RefId::new("refs/heads/main").expect("reference")),
    )
    .expect("scope");
    let request = crate::application::git_reads::GitReadRequestV1::Status;
    let capability =
        tracedecay_tool_catalog::CapabilityId::new(request.capability_id()).expect("capability");
    let digest =
        || ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("manifest digest");
    let authority = DaemonGitAuthorityStateV1 {
        scope: scope.clone(),
        requester: ActorId::new("actor.git-read-packet").expect("actor"),
        effective_capabilities: std::collections::BTreeSet::from([capability]),
        grant_expires_at: UtcMicros(i64::MAX),
        policy_revision: 1,
        policy_digest: digest(),
        configuration_digest: digest(),
        catalog_digest: digest(),
        privacy_digest: digest(),
        evaluated_at: UtcMicros(1),
    };
    let result = crate::application::git_reads::GitReadResultV1::Status(
        crate::git_query::GitQueryEnvelopeV1 {
            value: crate::git_query::GitStatusSummaryV1 {
                repository: scope.repository_id.clone(),
                head: GitHeadStateV1::Unborn {
                    branch: "refs/heads/main".to_owned(),
                },
                operation: tracedecay_domain::git::GitOperationStateV1::None,
                staged: 0,
                unstaged: 0,
                conflicted: 0,
                untracked: 0,
                ignored: 0,
                changed_paths: Vec::new(),
                schema_version: crate::git_query::GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
            },
            coverage: tracedecay_domain::git::GitCoverageV1::complete(),
            truncated_by_bound: false,
        },
    );

    let packet = git_read_evidence_packet(
        "request.git-read-packet",
        &request,
        &authority,
        result,
        UtcMicros(2),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active("cancel.git-read-packet").expect("cancellation"),
    )
    .expect("Git read packet");

    assert_eq!(packet.authority.authorized_scope_digest, scope.scope_digest);
    assert_eq!(
        packet.coverage.completeness,
        tracedecay_application::CoverageCompleteness::Complete
    );
    assert_eq!(packet.page.returned, 1);
    assert!(packet.payload.is_some());
    assert!(matches!(
        packet.evidence_authorities.as_slice(),
        [EvidenceAuthority { source_kind, .. }] if source_kind == "native_git"
    ));
    let complete_evidence_id = packet.evidence_authorities[0].evidence_id.clone();

    let partial = git_read_evidence_packet(
        "request.git-read-packet-partial",
        &request,
        &authority,
        crate::application::git_reads::GitReadResultV1::Status(
            crate::git_query::GitQueryEnvelopeV1 {
                value: crate::git_query::GitStatusSummaryV1 {
                    repository: scope.repository_id,
                    head: GitHeadStateV1::Unborn {
                        branch: "refs/heads/main".to_owned(),
                    },
                    operation: tracedecay_domain::git::GitOperationStateV1::None,
                    staged: 0,
                    unstaged: 0,
                    conflicted: 0,
                    untracked: 0,
                    ignored: 0,
                    changed_paths: Vec::new(),
                    schema_version: crate::git_query::GIT_QUERY_SCHEMA_VERSION_V1.to_owned(),
                },
                coverage: tracedecay_domain::git::GitCoverageV1::degraded(vec![
                    tracedecay_domain::git::GitDegradationV1::TruncatedOutput,
                    tracedecay_domain::git::GitDegradationV1::ConflictedState,
                ]),
                truncated_by_bound: true,
            },
        ),
        UtcMicros(3),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active("cancel.git-read-packet-partial").expect("cancellation"),
    )
    .expect("partial Git read packet");
    assert_eq!(
        partial.coverage.completeness,
        tracedecay_application::CoverageCompleteness::Partial
    );
    assert!(matches!(
        partial.omissions.as_slice(),
        [
            Omission {
                domain: EvidenceDomain::Source,
                reason: OmissionReason::Budget,
                ..
            },
            Omission {
                domain: EvidenceDomain::Source,
                reason: OmissionReason::Conflict,
                ..
            }
        ]
    ));
    assert_ne!(
        partial.evidence_authorities[0].evidence_id, complete_evidence_id,
        "native Git evidence identity must bind the captured result"
    );
}

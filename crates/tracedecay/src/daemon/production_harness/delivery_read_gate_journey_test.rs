//! Delivery read gate journey over the production project composition.
//!
//! Project open must register the daemon-owned Delivery read authority even
//! when no GitHub credential is configured, so the dashboard reads the exact
//! typed mount gate instead of a generic missing-authority answer. The gate
//! must stay readable while the feedback/advisory owners remain deferred.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{ActorId, CommitId, ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::advisory::GitHubReleaseReadControlV1;
use tracedecay_usecases::delivery::{
    ProjectDeliveryProviderMountGateV1, ProjectDeliveryReadOutcomeV1, ProjectDeliveryReadRequestV1,
};

use super::journey_test_support::git;
use super::*;

#[tokio::test]
async fn project_open_registers_the_typed_delivery_gate_without_a_github_credential() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(&project).expect("project root");
    std::fs::write(
        project.join("lib.rs"),
        "pub fn delivery_gate_journey_symbol() {}\n",
    )
    .expect("project source");
    git(&project, &["init", "--quiet", "-b", "main"]);
    // A recognizable GitHub remote with no registered credential is the exact
    // production shape behind the GitHubCredentialNotConfigured gate.
    git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/tracedecay-fixture/delivery-gate-journey.git",
        ],
    );
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "seed delivery gate journey",
        ],
    );
    let head_commit = git(&project, &["rev-parse", "HEAD"]);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition opens without a GitHub credential");
    let resources = harness.resources.as_ref().expect("live harness resources");
    let canonical_project = resources
        .servers
        .keys()
        .next()
        .expect("composed project route")
        .clone();
    let authority = resources
        .invocation
        .service
        .delivery_read_authority(Some(&canonical_project))
        .await
        .expect("project open must register the Delivery read authority even without a credential");

    let scope = authority.scope().clone();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.delivery-gate-journey").expect("grant id"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        ActorId::new("actor.delivery-gate-journey.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([
            CapabilityId::new(CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1).expect("CI capability")
        ]),
        BTreeSet::from([UseCaseId::new(CI_FAILURE_LOCALIZE_USE_CASE_ID_V1).expect("CI use case")]),
        DisclosureClass::Evidence,
    )
    .expect("delivery read grant");
    let context = RequestContext::new(
        ActorId::new("actor.delivery-gate-journey").expect("actor"),
        scope,
        grant,
        RequestId::new("request.delivery-gate-journey").expect("request id"),
        Deadline::new(UtcMicros(i64::MAX - 1)).expect("deadline"),
        CancellationContext::active("cancel.delivery-gate-journey").expect("cancellation"),
    )
    .expect("delivery read context");
    let request = ProjectDeliveryReadRequestV1 {
        expected_head_commit_id: CommitId::new(head_commit).expect("head commit"),
        max_pull_requests: 1,
        max_review_items: 1,
        max_ci_checks: 1,
        max_releases: 1,
    };
    let control = GitHubReleaseReadControlV1::bounded(Instant::now() + Duration::from_secs(5));

    assert_eq!(
        authority.handle().read(&context, &request, &control).await,
        ProjectDeliveryReadOutcomeV1::NotMounted {
            gate: ProjectDeliveryProviderMountGateV1::GitHubCredentialNotConfigured,
        },
        "the registered Delivery read must answer with the exact typed credential gate"
    );

    harness.shutdown().await;
}

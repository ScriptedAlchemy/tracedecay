//! Shared fixtures for the verified graph query deadline and source suites.

use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_application::{
    ApplicationOperation, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot,
    DisclosureClass, RequestContext, ResolvedScope,
};
use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
};
use tracedecay_domain::{
    ActorId, CodeGenerationId, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros,
    WorktreeId,
};
use tracedecay_graph_db::NeverCancelled;

use super::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionFuture, CodeGraphReadAdmissionPort,
    CodeGraphReadAdmissionRequest, CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    VerifiedCodeGraphRead,
};

pub(super) fn graph_operation() -> ApplicationOperation {
    tracedecay_application::retrieval::catalog::primitive_read_operation("node")
        .expect("graph operation catalog")
        .expect("registered node operation")
}

pub(super) fn fixture_scope(tag: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new(format!("project.{tag}").as_str()).expect("project"),
        RepositoryId::new(format!("repository.{tag}").as_str()).expect("repository"),
        WorktreeId::new(format!("worktree.{tag}").as_str()).expect("worktree"),
        Some(RefId::new(format!("refs/heads/{tag}").as_str()).expect("reference")),
    )
    .expect("scope")
}

pub(super) fn admit_context(
    request: &CodeGraphReadAdmissionRequest<'_>,
    scope: &ResolvedScope,
) -> RequestContext {
    let actor = ActorId::new("actor.verified-query-fixture").expect("actor");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.verified-query-fixture").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
        actor.clone(),
        request.observed_at,
        // Grant validity is independent of the request deadline so a delayed
        // admission can still return a context; TimedOut comes from the
        // post-wait deadline re-check, not from an unconstructable grant.
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([request.operation.capability_id().clone()]),
        BTreeSet::from([request.operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        actor,
        scope.clone(),
        grant,
        request.request_id.clone(),
        request.deadline.clone(),
        request.cancellation.context(),
    )
    .expect("context")
}

pub(super) struct ImmediateAdmission {
    pub(super) scope: ResolvedScope,
}

impl CodeGraphReadAdmissionPort for ImmediateAdmission {
    fn admit<'a>(
        &'a self,
        request: CodeGraphReadAdmissionRequest<'a>,
    ) -> CodeGraphReadAdmissionFuture<'a> {
        let scope = self.scope.clone();
        Box::pin(async move { Ok(admit_context(&request, &scope)) })
    }
}

pub(super) struct ImmediateProjection {
    pub(super) scope: ResolvedScope,
    pub(super) store: Arc<CodeGraphProjectionStore>,
}

impl CodeGraphProjectionReadPort for ImmediateProjection {
    fn open<'a>(&'a self, _request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        let scope = self.scope.clone();
        let store = Arc::clone(&self.store);
        Box::pin(async move {
            VerifiedCodeGraphRead::new(scope, store, super::CodeGraphReadFreshnessV1::Current)
        })
    }
}

pub(super) fn fixture_store(tag: &str) -> Arc<CodeGraphProjectionStore> {
    let cancellation =
        CancellationSignal::active(format!("cancel.{tag}.store")).expect("cancellation");
    let projection = HermeticCodeGraphProjectionStore::memory(&cancellation).expect("projection");
    let generation =
        CodeGenerationId::new(format!("generation.{tag}.1").as_str()).expect("generation");
    projection
        .publish_with_cancellation(&generation, &[], &[], Arc::new(NeverCancelled))
        .expect("publish");
    Arc::new(projection.verified_store(&generation).expect("store"))
}

pub(super) fn assert_route(error: tracedecay_domain::errors::TraceDecayError, reason_code: &str) {
    let (code, _, detail) = error
        .project_route_context()
        .expect("typed graph project route");
    assert_eq!(code, reason_code);
    match reason_code {
        "code-graph-timed-out" => {
            assert_eq!(detail, CodeGraphReadError::TimedOut.to_string());
        }
        "code-graph-cancelled" => {
            assert_eq!(detail, CodeGraphReadError::Cancelled.to_string());
        }
        other => panic!("unexpected graph reason {other}"),
    }
}

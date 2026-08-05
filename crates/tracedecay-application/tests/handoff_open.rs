use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    HandoffAuthoritySnapshotV1, HandoffOpenAuthorityError, HandoffOpenAuthorityPort,
    HandoffOpenBindingV1, HandoffOpenConsumeOutcomeV1, HandoffOpenContextV1, HandoffOpenError,
    HandoffOpenGrantV1, HandoffOpenService, HandoffOpenTargetError, HandoffOpenTargetPort,
    HandoffOpenToken, HandoffSessionId, OpenInvestigationHandoffRequestV1,
    OpenTaskHandoffRequestV1, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::feedback::FeedbackFindingId;
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, TaskId, UtcMicros, WorkVersion, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const ISSUED_AT: UtcMicros = UtcMicros(1_000_000);
const EXPIRES_AT: UtcMicros = UtcMicros(61_000_000);

#[derive(Clone, Default)]
struct MemoryAuthority {
    state: Arc<Mutex<MemoryAuthorityState>>,
}

#[derive(Default)]
struct MemoryAuthorityState {
    grants: BTreeMap<ManifestDigest, HandoffOpenGrantV1>,
    consumptions: BTreeMap<ManifestDigest, HandoffOpenConsumeOutcomeV1>,
}

impl HandoffOpenAuthorityPort for MemoryAuthority {
    fn issue(&self, grant: &HandoffOpenGrantV1) -> Result<(), HandoffOpenAuthorityError> {
        let mut state = self.state.lock().unwrap();
        if state
            .grants
            .insert(grant.token_digest().clone(), grant.clone())
            .is_some()
        {
            return Err(HandoffOpenAuthorityError::Conflict);
        }
        Ok(())
    }

    fn resolve(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenContextV1,
        observed_at: UtcMicros,
    ) -> Result<Option<HandoffOpenGrantV1>, HandoffOpenAuthorityError> {
        let state = self.state.lock().unwrap();
        Ok(state
            .grants
            .get(token_digest)
            .filter(|grant| grant.context() == expected && observed_at < *grant.expires_at())
            .cloned())
    }

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected: &HandoffOpenContextV1,
        request_id: &RequestId,
        input_digest: &ManifestDigest,
        consumed_at: UtcMicros,
    ) -> Result<HandoffOpenConsumeOutcomeV1, HandoffOpenAuthorityError> {
        let mut state = self.state.lock().unwrap();
        if let Some(outcome) = state.consumptions.get(token_digest) {
            return Ok(match outcome {
                HandoffOpenConsumeOutcomeV1::Consumed(consumption)
                    if consumption.request_id() == request_id
                        && consumption.input_digest() == input_digest =>
                {
                    HandoffOpenConsumeOutcomeV1::Consumed(consumption.clone())
                }
                _ => HandoffOpenConsumeOutcomeV1::Concealed,
            });
        }
        let Some(grant) = state
            .grants
            .get(token_digest)
            .filter(|grant| grant.context() == expected && consumed_at < *grant.expires_at())
            .cloned()
        else {
            return Ok(HandoffOpenConsumeOutcomeV1::Concealed);
        };
        let consumption = grant
            .consume(request_id.clone(), input_digest.clone(), consumed_at)
            .map_err(|_| HandoffOpenAuthorityError::Unavailable)?;
        let outcome = HandoffOpenConsumeOutcomeV1::Consumed(consumption);
        state
            .consumptions
            .insert(token_digest.clone(), outcome.clone());
        Ok(outcome)
    }
}

#[derive(Clone)]
struct CurrentTargets {
    current: Arc<Mutex<BTreeSet<ManifestDigest>>>,
}

impl CurrentTargets {
    fn all_current(bindings: &[HandoffOpenBindingV1]) -> Self {
        Self {
            current: Arc::new(Mutex::new(
                bindings
                    .iter()
                    .map(|binding| binding.target().owner_version_digest().clone())
                    .collect(),
            )),
        }
    }

    fn retire(&self, version: &ManifestDigest) {
        self.current.lock().unwrap().remove(version);
    }
}

impl HandoffOpenTargetPort for CurrentTargets {
    fn is_current<'a>(
        &'a self,
        _context: &'a RequestContext,
        binding: &'a HandoffOpenBindingV1,
    ) -> Pin<Box<dyn Future<Output = Result<bool, HandoffOpenTargetError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(self
                .current
                .lock()
                .unwrap()
                .contains(binding.target().owner_version_digest()))
        })
    }
}

fn digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
}

fn scope() -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.handoff").unwrap(),
        RepositoryId::new("repository.handoff").unwrap(),
        WorktreeId::new("worktree.handoff").unwrap(),
        None,
    )
    .unwrap()
}

fn context(request_id: &str) -> RequestContext {
    let scope = scope();
    let capability_ids = [
        "capability.handoff.issue",
        "capability.handoff.open_investigation_handoff",
        "capability.handoff.open_task_handoff",
    ];
    let use_case_ids = [
        "use-case.handoff.issue",
        "use-case.handoff.open_investigation_handoff",
        "use-case.handoff.open_task_handoff",
    ];
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.handoff").unwrap(),
        7,
        digest('a'),
        ActorId::new("actor.handoff").unwrap(),
        UtcMicros(1),
        UtcMicros(120_000_000),
        scope.clone(),
        capability_ids
            .into_iter()
            .map(|id| CapabilityId::new(id).unwrap())
            .collect(),
        use_case_ids
            .into_iter()
            .map(|id| UseCaseId::new(id).unwrap())
            .collect(),
        DisclosureClass::Metadata,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.handoff").unwrap(),
        scope,
        grant,
        RequestId::new(request_id).unwrap(),
        Deadline::new(UtcMicros(90_000_000)).unwrap(),
        CancellationContext::active(format!("cancel.{request_id}")).unwrap(),
    )
    .unwrap()
}

fn authority() -> HandoffAuthoritySnapshotV1 {
    HandoffAuthoritySnapshotV1::new(digest('b'), digest('c')).unwrap()
}

fn investigation_binding(context: &RequestContext) -> HandoffOpenBindingV1 {
    HandoffOpenBindingV1::investigation(
        context,
        HandoffSessionId::new("lsp-session.investigation").unwrap(),
        FeedbackFindingId::new("feedback.finding.investigation").unwrap(),
        digest('d'),
        authority(),
    )
    .unwrap()
}

fn task_binding(context: &RequestContext) -> HandoffOpenBindingV1 {
    HandoffOpenBindingV1::task(
        context,
        HandoffSessionId::new("lsp-session.task").unwrap(),
        TaskId::new("task.handoff").unwrap(),
        WorkVersion::new(9).unwrap(),
        authority(),
    )
    .unwrap()
}

fn token(fill: char) -> HandoffOpenToken {
    HandoffOpenToken::new(fill.to_string().repeat(48)).unwrap()
}

#[tokio::test]
async fn issue_then_open_returns_only_the_bound_surface_and_atomic_receipt() {
    let issue_context = context("request.issue");
    let investigation = investigation_binding(&issue_context);
    let task = task_binding(&issue_context);
    let target_port = CurrentTargets::all_current(&[investigation.clone(), task.clone()]);
    let service = HandoffOpenService::new(MemoryAuthority::default(), target_port);

    service
        .issue(
            &issue_context,
            investigation.clone(),
            &token('i'),
            ISSUED_AT,
            EXPIRES_AT,
        )
        .await
        .unwrap();
    service
        .issue(
            &issue_context,
            task.clone(),
            &token('t'),
            ISSUED_AT,
            EXPIRES_AT,
        )
        .await
        .unwrap();

    let investigation_result = service
        .open_investigation(
            &context("request.open-investigation"),
            OpenInvestigationHandoffRequestV1 {
                token: "i".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.investigation").unwrap(),
            },
            authority(),
            UtcMicros(2_000_000),
        )
        .await
        .unwrap();
    assert_eq!(
        investigation_result.surface.finding_id.as_str(),
        "feedback.finding.investigation"
    );
    assert_eq!(
        investigation_result.surface.owner_version_digest,
        digest('d')
    );
    assert_eq!(
        investigation_result.receipt.request_id.as_str(),
        "request.open-investigation"
    );

    let task_result = service
        .open_task(
            &context("request.open-task"),
            OpenTaskHandoffRequestV1 {
                token: "t".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.task").unwrap(),
            },
            authority(),
            UtcMicros(2_000_000),
        )
        .await
        .unwrap();
    assert_eq!(task_result.surface.task_id.as_str(), "task.handoff");
    assert_eq!(task_result.surface.version, WorkVersion::new(9).unwrap());
    assert_eq!(task_result.receipt.request_id.as_str(), "request.open-task");

    let encoded = serde_json::to_value(task_result).unwrap();
    assert!(encoded.pointer("/surface/task_id").is_some());
    assert!(encoded.get("token").is_none());
    assert!(encoded.get("task_body").is_none());
    assert!(encoded.get("edit").is_none());
}

#[tokio::test]
async fn wrong_kind_scope_session_authority_expiry_and_replay_are_indistinguishable() {
    let issue_context = context("request.issue");
    let binding = investigation_binding(&issue_context);
    let service = HandoffOpenService::new(
        MemoryAuthority::default(),
        CurrentTargets::all_current(std::slice::from_ref(&binding)),
    );
    service
        .issue(&issue_context, binding, &token('s'), ISSUED_AT, EXPIRES_AT)
        .await
        .unwrap();

    let wrong_kind = service
        .open_task(
            &context("request.wrong-kind"),
            OpenTaskHandoffRequestV1 {
                token: "s".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.investigation").unwrap(),
            },
            authority(),
            UtcMicros(2_000_000),
        )
        .await
        .unwrap_err();
    let wrong_session = service
        .open_investigation(
            &context("request.wrong-session"),
            OpenInvestigationHandoffRequestV1 {
                token: "s".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.other").unwrap(),
            },
            authority(),
            UtcMicros(2_000_000),
        )
        .await
        .unwrap_err();
    let wrong_authority = service
        .open_investigation(
            &context("request.wrong-authority"),
            OpenInvestigationHandoffRequestV1 {
                token: "s".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.investigation").unwrap(),
            },
            HandoffAuthoritySnapshotV1::new(digest('e'), digest('c')).unwrap(),
            UtcMicros(2_000_000),
        )
        .await
        .unwrap_err();
    let expired = service
        .open_investigation(
            &context("request.expired"),
            OpenInvestigationHandoffRequestV1 {
                token: "s".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.investigation").unwrap(),
            },
            authority(),
            EXPIRES_AT,
        )
        .await
        .unwrap_err();

    assert_eq!(wrong_kind, HandoffOpenError::NotFoundOrNotAuthorized);
    assert_eq!(wrong_session, wrong_kind);
    assert_eq!(wrong_authority, wrong_kind);
    assert_eq!(expired, wrong_kind);

    let success = service
        .open_investigation(
            &context("request.success"),
            OpenInvestigationHandoffRequestV1 {
                token: "s".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.investigation").unwrap(),
            },
            authority(),
            UtcMicros(3_000_000),
        )
        .await
        .unwrap();
    let same_request = service
        .open_investigation(
            &context("request.success"),
            OpenInvestigationHandoffRequestV1 {
                token: "s".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.investigation").unwrap(),
            },
            authority(),
            UtcMicros(4_000_000),
        )
        .await
        .unwrap();
    assert_eq!(same_request.receipt, success.receipt);

    let replay = service
        .open_investigation(
            &context("request.replay"),
            OpenInvestigationHandoffRequestV1 {
                token: "s".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.investigation").unwrap(),
            },
            authority(),
            UtcMicros(4_000_000),
        )
        .await
        .unwrap_err();
    assert_eq!(replay, HandoffOpenError::NotFoundOrNotAuthorized);
}

#[tokio::test]
async fn owner_version_is_rechecked_before_and_after_single_use_commit() {
    let issue_context = context("request.issue");
    let binding = task_binding(&issue_context);
    let current = CurrentTargets::all_current(std::slice::from_ref(&binding));
    let service = HandoffOpenService::new(MemoryAuthority::default(), current.clone());
    service
        .issue(
            &issue_context,
            binding.clone(),
            &token('v'),
            ISSUED_AT,
            EXPIRES_AT,
        )
        .await
        .unwrap();

    current.retire(binding.target().owner_version_digest());
    let stale = service
        .open_task(
            &context("request.stale"),
            OpenTaskHandoffRequestV1 {
                token: "v".repeat(48),
                session_id: HandoffSessionId::new("lsp-session.task").unwrap(),
            },
            authority(),
            UtcMicros(2_000_000),
        )
        .await
        .unwrap_err();

    assert_eq!(stale, HandoffOpenError::NotFoundOrNotAuthorized);
}

#[test]
fn token_debug_and_request_debug_never_expose_the_secret() {
    let token = token('z');
    assert_eq!(format!("{token:?}"), "HandoffOpenToken([REDACTED])");
    let request = OpenTaskHandoffRequestV1 {
        token: "z".repeat(48),
        session_id: HandoffSessionId::new("lsp-session.task").unwrap(),
    };
    let debug = format!("{request:?}");
    assert!(!debug.contains(&"z".repeat(48)));
    assert!(debug.contains("[REDACTED]"));
}

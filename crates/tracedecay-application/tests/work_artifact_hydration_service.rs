//! Artifact hydration contract: topology-pinned paging, typed absence,
//! stale-cursor refusal, typed evidence coverage, and page-consistency
//! refusal — all without ever carrying artifact bytes.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    ApplicationProblemKind, CancellationContext, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, MAX_WORK_ATTEMPT_LIST_PAGE_SIZE, RequestContext, RequestId, ResolvedScope,
    WorkArtifactHydrationRequestV1, WorkArtifactHydrationService, WorkArtifactHydrationV1,
    WorkAttemptEvidencePageV1, WorkAttemptEvidenceReadPort, WorkAttemptEvidenceRecordV1,
    WorkAttemptEvidenceRowV1, WorkAttemptEvidenceStateV1, WorkAttemptListCoverageV1,
    WorkAttemptListCursorV1, WorkAttemptProviderOutcomeV1, WorkAttemptStorageError,
    WorkAttemptTopologyBindingV1, WorkAttemptTopologyStateV1,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProviderId, RepositoryId, UtcMicros, WorkArtifactRefV1,
    WorkAttemptIdentityV1, WorkAuthority, WorkProviderRouteId, WorkProviderRouteV1, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context(project: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.hydration.fixture"),
        id::<WorktreeId>("worktree.hydration.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.fixture").unwrap();
    let use_case = UseCaseId::new("use-case.work.fixture").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.fixture"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.hydration.owner"),
        scope,
        grant,
        RequestId::new(format!("request.{project}.hydration")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{project}.hydration")).unwrap(),
    )
    .unwrap()
}

fn identity(task: &str, attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(id(task), id(&format!("run.{task}")), id(attempt)).unwrap()
}

fn artifact(name: &str, byte: char, byte_length: u64) -> WorkArtifactRefV1 {
    WorkArtifactRefV1::new(id(name), digest(byte), byte_length).unwrap()
}

fn sealed_evidence(identity: &WorkAttemptIdentityV1) -> WorkAttemptEvidenceRecordV1 {
    let route = WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.hydration.claude-code.v1"),
    )
    .unwrap();
    WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: route.clone(),
        actual_route: Some(route),
        outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
        stdout: None,
        stderr: None,
        provider_fallback: None,
        observed_at: UtcMicros(500),
    }
}

/// In-memory evidence rows in the same stable identity order the registered
/// store answers, with an overridable remaining count so the page-consistency
/// refusal is falsifiable.
#[derive(Clone, Default)]
struct RowStore {
    rows: Arc<Mutex<Vec<WorkAttemptEvidenceRowV1>>>,
    remaining_override: Arc<Mutex<Option<u32>>>,
}

impl WorkAttemptEvidenceReadPort for RowStore {
    fn evidence_page(
        &self,
        _authority: &WorkAuthority,
        start_after: Option<&WorkAttemptIdentityV1>,
        limit: u32,
    ) -> Result<WorkAttemptEvidencePageV1, WorkAttemptStorageError> {
        let rows = self.rows.lock().unwrap();
        let after: Vec<WorkAttemptEvidenceRowV1> = rows
            .iter()
            .filter(|row| match start_after {
                None => true,
                Some(start_after) => {
                    (
                        row.identity.task_id().as_str(),
                        row.identity.run_id().as_str(),
                        row.identity.attempt_id().as_str(),
                    ) > (
                        start_after.task_id().as_str(),
                        start_after.run_id().as_str(),
                        start_after.attempt_id().as_str(),
                    )
                }
            })
            .cloned()
            .collect();
        let remaining = self
            .remaining_override
            .lock()
            .unwrap()
            .unwrap_or(u32::try_from(after.len()).unwrap());
        Ok(WorkAttemptEvidencePageV1 {
            rows: after.into_iter().take(limit as usize).collect(),
            remaining,
        })
    }
}

fn verified(generation: &str) -> WorkAttemptTopologyStateV1 {
    WorkAttemptTopologyStateV1::Verified(WorkAttemptTopologyBindingV1 {
        generation: generation.to_owned(),
        task_count: 3,
    })
}

fn request(
    page_size: u32,
    cursor: Option<WorkAttemptListCursorV1>,
) -> WorkArtifactHydrationRequestV1 {
    WorkArtifactHydrationRequestV1 { page_size, cursor }
}

fn seeded() -> (WorkArtifactHydrationService<RowStore>, RowStore) {
    let store = RowStore::default();
    let first = identity("task.hydration.a", "attempt.1");
    let second = identity("task.hydration.b", "attempt.1");
    let third = identity("task.hydration.c", "attempt.1");
    *store.rows.lock().unwrap() = vec![
        WorkAttemptEvidenceRowV1 {
            identity: first.clone(),
            artifacts: vec![
                artifact("artifact.hydration.log", 'b', 128),
                artifact("artifact.hydration.patch", 'c', 4_096),
            ],
            evidence: Some(sealed_evidence(&first)),
        },
        WorkAttemptEvidenceRowV1 {
            identity: second,
            artifacts: Vec::new(),
            evidence: None,
        },
        WorkAttemptEvidenceRowV1 {
            identity: third.clone(),
            artifacts: vec![artifact("artifact.hydration.report", 'd', 512)],
            evidence: Some(sealed_evidence(&third)),
        },
    ];
    (WorkArtifactHydrationService::new(store.clone()), store)
}

#[test]
fn page_size_bounds_are_refused_as_invalid() {
    let (service, _) = seeded();
    let context = context("project.hydration.bounds");
    for page_size in [0, MAX_WORK_ATTEMPT_LIST_PAGE_SIZE + 1] {
        let refused = service
            .hydrate(&context, &request(page_size, None), |_| {
                Ok(verified("generation.hydration.1"))
            })
            .unwrap_err();
        assert_eq!(refused.kind(), ApplicationProblemKind::InvalidRequest);
    }
}

#[test]
fn an_absent_scope_is_typed_and_a_cursor_against_it_is_stale() {
    let (service, _) = seeded();
    let context = context("project.hydration.absent");
    let absent = service
        .hydrate(&context, &request(10, None), |_| {
            Ok(WorkAttemptTopologyStateV1::Absent)
        })
        .unwrap();
    assert_eq!(absent, WorkArtifactHydrationV1::Absent);

    let cursor = WorkAttemptListCursorV1 {
        generation: "generation.hydration.gone".to_owned(),
        start_after: identity("task.hydration.a", "attempt.1"),
    };
    let stale = service
        .hydrate(&context, &request(10, Some(cursor)), |_| {
            Ok(WorkAttemptTopologyStateV1::Absent)
        })
        .unwrap_err();
    assert_eq!(stale.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn hydration_pages_under_one_generation_and_types_evidence_coverage() {
    let (service, _) = seeded();
    let context = context("project.hydration.paging");
    let first_page = service
        .hydrate(&context, &request(2, None), |_| {
            Ok(verified("generation.hydration.1"))
        })
        .unwrap();
    let WorkArtifactHydrationV1::Hydrated {
        topology,
        attempts,
        coverage,
    } = first_page
    else {
        panic!("a populated scope must hydrate");
    };
    assert_eq!(topology.generation, "generation.hydration.1");
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0].artifacts.len(),
        2,
        "artifact references are served in their canonical stored order"
    );
    assert!(matches!(
        attempts[0].evidence,
        WorkAttemptEvidenceStateV1::Sealed { .. }
    ));
    assert_eq!(
        attempts[1].evidence,
        WorkAttemptEvidenceStateV1::Pending,
        "an attempt that has not reported is a typed pending state"
    );
    let WorkAttemptListCoverageV1::Capped {
        returned,
        remaining,
        resume,
    } = coverage
    else {
        panic!("a capped page must say so");
    };
    assert_eq!((returned, remaining), (2, 1));
    assert_eq!(resume.generation, "generation.hydration.1");

    let second_page = service
        .hydrate(&context, &request(2, Some(resume)), |_| {
            Ok(verified("generation.hydration.1"))
        })
        .unwrap();
    let WorkArtifactHydrationV1::Hydrated {
        attempts, coverage, ..
    } = second_page
    else {
        panic!("the resumed page must hydrate");
    };
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].identity,
        identity("task.hydration.c", "attempt.1")
    );
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 1 }
    );
}

#[test]
fn a_cursor_from_a_superseded_generation_is_refused_stale() {
    let (service, _) = seeded();
    let context = context("project.hydration.stale");
    let cursor = WorkAttemptListCursorV1 {
        generation: "generation.hydration.old".to_owned(),
        start_after: identity("task.hydration.a", "attempt.1"),
    };
    let stale = service
        .hydrate(&context, &request(2, Some(cursor)), |_| {
            Ok(verified("generation.hydration.2"))
        })
        .unwrap_err();
    assert_eq!(stale.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn an_inconsistent_storage_page_is_refused_not_served() {
    let (service, store) = seeded();
    let context = context("project.hydration.inconsistent");
    // The store claims fewer remaining rows than it returned; serving that
    // page would fabricate coverage, so the read refuses instead.
    *store.remaining_override.lock().unwrap() = Some(1);
    let refused = service
        .hydrate(&context, &request(2, None), |_| {
            Ok(verified("generation.hydration.1"))
        })
        .unwrap_err();
    assert_eq!(refused.kind(), ApplicationProblemKind::Unavailable);
}

use std::collections::BTreeSet;
use std::fmt;

use schemars::schema_for;
use tracedecay_application::{
    AuthorizedMultiRootQueryService, AuthorizedScopeSet, AuthorizedScopeSetAuthority,
    CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, MultiRootQueryError,
    MultiRootQueryPort, MultiRootQueryRequestV1, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, CollectionRevision, ManifestDigest, ProjectId, RefId, RepositoryId, RootGenerationV1,
    RootScopeOutcomeV1, ScopeOutcome, ScopeSetId, ScopeSetRevision, ScopeUnavailableReasonV1,
    StackRevision, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const CAPABILITY: &str = "capability.multi-root.query";
const USE_CASE: &str = "use-case.multi-root.query";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn context(worktree: &str, suffix: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        id::<WorktreeId>(worktree),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id(&format!("grant.{suffix}")),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new(CAPABILITY).unwrap()]),
        BTreeSet::from([UseCaseId::new(USE_CASE).unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.requester"),
        scope,
        grant,
        RequestId::new(format!("request.{suffix}")).unwrap(),
        Deadline::new(UtcMicros(900)).unwrap(),
        CancellationContext::active(format!("cancel.{suffix}")).unwrap(),
    )
    .unwrap()
}

fn setup() -> (AuthorizedScopeSet, Vec<RequestContext>) {
    let contexts = vec![
        context("worktree.main", "main"),
        context("worktree.linked", "linked"),
    ];
    let set = AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new("scope-set.fixture").unwrap(),
        ScopeSetRevision::new(1).unwrap(),
        contexts.clone(),
        &CapabilityId::new(CAPABILITY).unwrap(),
        &UseCaseId::new(USE_CASE).unwrap(),
        UtcMicros(10),
    )
    .unwrap();
    (set, contexts)
}

fn generation(scope: &ResolvedScope, byte: char) -> RootScopeOutcomeV1<RootGenerationV1> {
    RootScopeOutcomeV1::new(
        scope.scope_digest.clone(),
        ScopeOutcome::Exact(
            RootGenerationV1::new(
                scope.scope_digest.clone(),
                CollectionRevision::new(digest(byte)).unwrap(),
                StackRevision::new(digest(byte)).unwrap(),
            )
            .unwrap(),
        ),
    )
    .unwrap()
}

#[derive(Clone, Copy)]
enum LinkedOutcome {
    Unavailable,
    Denied,
}

struct Port(LinkedOutcome);

impl MultiRootQueryPort<String, String> for Port {
    fn query_root(
        &self,
        context: &RequestContext,
        _generation: &RootGenerationV1,
        query: &String,
        page: u64,
    ) -> ScopeOutcome<Vec<String>> {
        if context.scope().worktree_id.as_str() == "worktree.linked" {
            return match self.0 {
                LinkedOutcome::Unavailable => ScopeOutcome::Unavailable {
                    reason: ScopeUnavailableReasonV1::StoreUnavailable,
                },
                LinkedOutcome::Denied => ScopeOutcome::Denied,
            };
        }
        ScopeOutcome::Exact(vec![format!(
            "{}:{query}:{page}",
            context.scope().worktree_id.as_str(),
        )])
    }
}

fn request(
    scope_set: AuthorizedScopeSet,
    contexts: Vec<RequestContext>,
    query_digest: ManifestDigest,
    page: u64,
    continuation: Option<tracedecay_application::MultiRootContinuationV1>,
) -> MultiRootQueryRequestV1<String> {
    let generations = scope_set
        .roots()
        .iter()
        .enumerate()
        .map(|(index, root)| generation(root.scope(), if index == 0 { 'b' } else { 'c' }))
        .collect();
    MultiRootQueryRequestV1 {
        scope_set,
        contexts,
        root_generations: generations,
        capability_id: CapabilityId::new(CAPABILITY).unwrap(),
        use_case_id: UseCaseId::new(USE_CASE).unwrap(),
        observed_at: UtcMicros(10),
        query: "needle".to_owned(),
        query_digest,
        order_digest: digest('e'),
        page,
        continuation,
    }
}

#[test]
fn two_root_query_returns_partial_truth_and_frozen_continuation() {
    let (set, contexts) = setup();
    let page = AuthorizedMultiRootQueryService::new(Port(LinkedOutcome::Unavailable))
        .execute(request(set.clone(), contexts.clone(), digest('d'), 0, None))
        .unwrap();

    assert!(matches!(page.aggregate, ScopeOutcome::Partial { .. }));
    assert_eq!(page.roots.len(), 2);
    assert!(matches!(
        page.roots[0].outcome,
        ScopeOutcome::Unavailable {
            reason: ScopeUnavailableReasonV1::StoreUnavailable
        }
    ));
    assert!(matches!(page.roots[1].outcome, ScopeOutcome::Exact(_)));
    assert_eq!(page.continuation.root_generations().len(), 2);
    assert_eq!(page.continuation.next_page(), 1);

    let next = AuthorizedMultiRootQueryService::new(Port(LinkedOutcome::Unavailable))
        .execute(request(
            set,
            contexts,
            digest('d'),
            1,
            Some(page.continuation),
        ))
        .unwrap();
    let ScopeOutcome::Partial { value, .. } = next.aggregate else {
        panic!("one available root must keep the continuation partial");
    };
    assert_eq!(value, ["worktree.main:needle:1"]);
}

#[test]
fn cursor_mismatch_and_denied_root_never_become_empty_success() {
    let (set, contexts) = setup();
    let first = AuthorizedMultiRootQueryService::new(Port(LinkedOutcome::Denied))
        .execute(request(set.clone(), contexts.clone(), digest('d'), 0, None))
        .unwrap();
    assert!(matches!(first.aggregate, ScopeOutcome::Partial { .. }));
    assert!(matches!(first.roots[0].outcome, ScopeOutcome::Denied));

    let mismatch = AuthorizedMultiRootQueryService::new(Port(LinkedOutcome::Denied))
        .execute(request(
            set,
            contexts,
            digest('f'),
            1,
            Some(first.continuation),
        ))
        .unwrap_err();
    assert_eq!(
        mismatch,
        MultiRootQueryError::CursorMismatch {
            field: "query digest"
        }
    );
}

#[test]
fn denied_generation_does_not_require_a_current_root_context() {
    let (set, mut contexts) = setup();
    let denied_index = set
        .roots()
        .iter()
        .position(|root| root.scope().worktree_id.as_str() == "worktree.linked")
        .unwrap();
    let denied_scope = set.roots()[denied_index].scope().scope_digest.clone();
    contexts.retain(|context| context.scope().scope_digest != denied_scope);
    let mut request = request(set, contexts, digest('d'), 0, None);
    request.root_generations[denied_index] =
        RootScopeOutcomeV1::new(denied_scope, ScopeOutcome::Denied).unwrap();

    let page = AuthorizedMultiRootQueryService::new(Port(LinkedOutcome::Unavailable))
        .execute(request)
        .unwrap();

    assert!(matches!(
        page.roots[denied_index].outcome,
        ScopeOutcome::Denied
    ));
    assert!(page.roots.iter().enumerate().any(
        |(index, root)| index != denied_index && matches!(root.outcome, ScopeOutcome::Exact(_))
    ));
    assert!(matches!(page.aggregate, ScopeOutcome::Partial { .. }));
}

#[test]
fn continuation_schema_and_runtime_reject_page_zero() {
    let schema =
        serde_json::to_value(schema_for!(tracedecay_application::MultiRootContinuationV1)).unwrap();
    assert_eq!(schema["properties"]["next_page"]["minimum"], 1);

    let generations = vec![generation(
        &context("worktree.main", "schema").scope().clone(),
        'b',
    )];
    assert!(
        tracedecay_application::MultiRootContinuationV1::new(
            digest('a'),
            generations.clone(),
            digest('c'),
            digest('d'),
            0,
        )
        .is_err()
    );

    let continuation = tracedecay_application::MultiRootContinuationV1::new(
        digest('a'),
        generations,
        digest('c'),
        digest('d'),
        1,
    )
    .unwrap();
    let mut wire = serde_json::to_value(continuation).unwrap();
    wire["next_page"] = serde_json::json!(0);
    assert!(
        serde_json::from_value::<tracedecay_application::MultiRootContinuationV1>(wire).is_err()
    );
}

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::fmt;

use tracedecay_application::{
    ApplicationOperation, AuthorityReceipt, AuthorizationPort, AuthorizationPortOutcome,
    AuthorizationRequest, CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    EvidenceCoverage, EvidenceDomain, PageState, PolicyDecisionRef, RequestContext, RequestId,
    ResolvedScope, ResultContractRef, RetrievalEvidence, SourceAuthorizationSnapshot,
    TemporalState,
};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros,
    WorktreeId,
};
use tracedecay_policy::authorization::{
    SourceAuthorizationInputV1, SourceAuthorizationTruthTableV1,
};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, SortContractId, UseCaseId};

pub const SHA256_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const SHA256_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SOURCE_AUTHORIZATION_TRUTH_TABLES: &str =
    include_str!("../../../tracedecay-policy/tests/fixtures/source_authorization/core.json");

pub fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity is canonical")
}

pub fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).expect("fixture digest is canonical")
}

pub fn result_contract() -> ResultContractRef {
    ResultContractRef::new(
        SchemaId::new("schema.application.fixture.result").unwrap(),
        1,
    )
    .unwrap()
}

pub fn operation() -> ApplicationOperation {
    ApplicationOperation::new(
        CapabilityId::new("capability.retrieval.symbol-search").unwrap(),
        UseCaseId::new("use-case.retrieval.symbol-search").unwrap(),
        result_contract(),
        true,
    )
}

pub fn scope() -> ResolvedScope {
    ResolvedScope::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        id::<WorktreeId>("worktree.fixture"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap()
}

pub fn context(operation: &ApplicationOperation) -> RequestContext {
    let scope = scope();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.fixture"),
        1,
        digest(SHA256_A),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.requester"),
        scope,
        grant,
        RequestId::new("request.fixture").unwrap(),
        Deadline::new(UtcMicros(500)).unwrap(),
        CancellationContext::active("cancel.fixture").unwrap(),
    )
    .unwrap()
}

pub fn authority(context: &RequestContext) -> AuthorityReceipt {
    AuthorityReceipt::from_context(
        context,
        PolicyDecisionRef::new(
            "policy.fixture",
            1,
            digest(SHA256_B),
            ComponentVersion::new("policy.evaluator.v1").unwrap(),
        )
        .unwrap(),
        UtcMicros(2),
    )
    .unwrap()
}

pub fn source_authorization_input(name: &str) -> SourceAuthorizationInputV1 {
    serde_json::from_str::<Vec<SourceAuthorizationTruthTableV1>>(SOURCE_AUTHORIZATION_TRUTH_TABLES)
        .expect("checked-in source authorization truth tables deserialize")
        .into_iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("source authorization fixture {name} exists"))
        .input
}

pub fn authorized_source_input() -> SourceAuthorizationInputV1 {
    source_authorization_input("project_authorized_live")
}

pub fn source_snapshot(input: SourceAuthorizationInputV1) -> SourceAuthorizationSnapshot {
    SourceAuthorizationSnapshot::new(input, true)
}

pub struct StaticAuthorizationPort {
    outcome: AuthorizationPortOutcome,
}

impl StaticAuthorizationPort {
    pub fn authorized() -> Self {
        Self::new(AuthorizationPortOutcome::Snapshot(Box::new(
            source_snapshot(authorized_source_input()),
        )))
    }

    pub fn new(outcome: AuthorizationPortOutcome) -> Self {
        Self { outcome }
    }
}

impl AuthorizationPort for StaticAuthorizationPort {
    fn source_authorization_snapshot(
        &self,
        _request: &AuthorizationRequest<'_>,
    ) -> AuthorizationPortOutcome {
        self.outcome.clone()
    }
}

pub struct SequencedAuthorizationPort {
    outcomes: RefCell<VecDeque<AuthorizationPortOutcome>>,
}

impl SequencedAuthorizationPort {
    pub fn snapshots(snapshots: impl IntoIterator<Item = SourceAuthorizationSnapshot>) -> Self {
        Self {
            outcomes: RefCell::new(
                snapshots
                    .into_iter()
                    .map(|snapshot| AuthorizationPortOutcome::Snapshot(Box::new(snapshot)))
                    .collect(),
            ),
        }
    }
}

impl AuthorizationPort for SequencedAuthorizationPort {
    fn source_authorization_snapshot(
        &self,
        _request: &AuthorizationRequest<'_>,
    ) -> AuthorizationPortOutcome {
        self.outcomes
            .borrow_mut()
            .pop_front()
            .expect("authorization snapshot sequence is not exhausted")
    }
}

pub fn evidence<T>(payload: T) -> RetrievalEvidence<T> {
    RetrievalEvidence {
        payload: Some(payload),
        temporal: TemporalState::current(UtcMicros(2)),
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 1, 1, 1).unwrap(),
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.symbol.fixture.v1").unwrap(),
            1,
            Some(1),
            1,
        )
        .unwrap(),
        finished_at: UtcMicros(3),
        budget: Default::default(),
        cancellation: None,
    }
}

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use tracedecay_application::memory::{
    CommitFactCommandV1, CommitFactDispositionV1, CommitFactPort, CommitFactPortResultV1,
    CurrentFactsPort, CurrentFactsPortResultV1, CurrentFactsQueryV1, MemoryApplication,
    MemoryApplicationInvariantError, MemoryContradictionStateV1, MemoryFactSnapshotV1,
    MemoryReadCoverageV1, MemoryReadResultV1, MemoryUseCaseError, PromoteFactProposalCommandV1,
    PromoteFactProposalPort, PromoteFactProposalPortResultV1,
};
use tracedecay_domain::{
    DomainError, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, ProjectId,
    ProvenanceId, UtcMicros,
};

fn project_owner() -> FactOwnerV1 {
    FactOwnerV1::Project {
        project_id: ProjectId::new("project.memory.application").unwrap(),
    }
}

fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            owner,
            FactIdentitySourceV1::Application {
                operation_id: ProvenanceId::new(operation).unwrap(),
            },
        )
        .unwrap(),
    )
    .unwrap()
}

struct CommitPort {
    calls: Arc<Mutex<Vec<&'static str>>>,
    result_owner: FactOwnerV1,
    result_fact_id: FactId,
}

impl CommitFactPort for CommitPort {
    type Command = &'static str;
    type Error = DomainError;
    type Output = &'static str;

    async fn commit_fact(
        &self,
        command: Self::Command,
    ) -> Result<CommitFactPortResultV1<Self::Output>, Self::Error> {
        self.calls.lock().unwrap().push(command);
        Ok(CommitFactPortResultV1::new(
            "committed",
            CommitFactDispositionV1::Committed,
            Some(self.result_owner.clone()),
            Some(self.result_fact_id.clone()),
        ))
    }
}

#[test]
fn project_wide_commit_is_owner_bound_and_returns_the_port_output() {
    let owner = project_owner();
    let fact_id = fact_id(owner.clone(), "operation.memory.commit");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let application = MemoryApplication::new(
        owner.clone(),
        CommitPort {
            calls: Arc::clone(&calls),
            result_owner: owner.clone(),
            result_fact_id: fact_id.clone(),
        },
    )
    .unwrap();

    let output =
        block_on(application.commit_fact(CommitFactCommandV1::new(owner, fact_id, "write")))
            .unwrap();

    assert_eq!(output, "committed");
    assert_eq!(*calls.lock().unwrap(), vec!["write"]);
}

#[test]
fn owner_mismatch_is_rejected_before_the_commit_port_runs() {
    let owner = project_owner();
    let fact_id = fact_id(owner.clone(), "operation.memory.commit");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let application = MemoryApplication::new(
        owner.clone(),
        CommitPort {
            calls: Arc::clone(&calls),
            result_owner: owner,
            result_fact_id: fact_id.clone(),
        },
    )
    .unwrap();

    let error = block_on(application.commit_fact(CommitFactCommandV1::new(
        FactOwnerV1::Profile,
        fact_id,
        "write",
    )))
    .unwrap_err();

    assert!(matches!(
        error,
        MemoryUseCaseError::Invariant(MemoryApplicationInvariantError::OwnerMismatch { .. })
    ));
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn commit_rejects_cross_owner_authority_receipts() {
    let owner = project_owner();
    let fact_id = fact_id(owner.clone(), "operation.memory.commit");
    let application = MemoryApplication::new(
        owner.clone(),
        CommitPort {
            calls: Arc::new(Mutex::new(Vec::new())),
            result_owner: FactOwnerV1::Profile,
            result_fact_id: fact_id.clone(),
        },
    )
    .unwrap();

    let error =
        block_on(application.commit_fact(CommitFactCommandV1::new(owner, fact_id, "write")))
            .unwrap_err();

    assert!(matches!(
        error,
        MemoryUseCaseError::Invariant(MemoryApplicationInvariantError::InvalidAuthorityResult {
            invariant: "fact commit identity"
        })
    ));
}

struct CurrentFactsPortFixture {
    snapshots: Vec<MemoryFactSnapshotV1>,
}

impl CurrentFactsPort for CurrentFactsPortFixture {
    type Error = DomainError;
    type Output = &'static str;
    type Query = ();

    async fn query_current_facts(
        &self,
        _query: Self::Query,
    ) -> Result<CurrentFactsPortResultV1<Self::Output>, Self::Error> {
        Ok(CurrentFactsPortResultV1::new(
            "facts",
            self.snapshots.clone(),
        ))
    }
}

#[test]
fn current_fact_pages_must_remain_owner_bound_ordered_and_bounded() {
    let owner = project_owner();
    let mut fact_ids = [
        fact_id(owner.clone(), "operation.memory.a"),
        fact_id(owner.clone(), "operation.memory.b"),
    ];
    fact_ids.sort();
    let [first, second] = fact_ids;
    let application = MemoryApplication::new(
        owner.clone(),
        CurrentFactsPortFixture {
            snapshots: vec![
                MemoryFactSnapshotV1::new(owner.clone(), second.clone(), UtcMicros(2)),
                MemoryFactSnapshotV1::new(owner.clone(), first, UtcMicros(1)),
            ],
        },
    )
    .unwrap();

    let error =
        block_on(application.query_current_facts(CurrentFactsQueryV1::new(owner, None, 2, ())))
            .unwrap_err();

    assert!(matches!(
        error,
        MemoryUseCaseError::Invariant(MemoryApplicationInvariantError::InvalidAuthorityResult {
            invariant: "current fact bounds, owner, cursor, and ordering"
        })
    ));
}

#[test]
fn current_fact_pages_reject_cross_owner_cursor_and_limit_violations() {
    let owner = project_owner();
    let mut fact_ids = [
        fact_id(owner.clone(), "operation.memory.page-a"),
        fact_id(owner.clone(), "operation.memory.page-b"),
    ];
    fact_ids.sort();
    let [first, second] = fact_ids;
    let cases = [
        (
            vec![MemoryFactSnapshotV1::new(
                FactOwnerV1::Profile,
                second.clone(),
                UtcMicros(2),
            )],
            None,
            1,
        ),
        (
            vec![MemoryFactSnapshotV1::new(
                owner.clone(),
                first.clone(),
                UtcMicros(1),
            )],
            Some(first.clone()),
            1,
        ),
        (
            vec![
                MemoryFactSnapshotV1::new(owner.clone(), first.clone(), UtcMicros(1)),
                MemoryFactSnapshotV1::new(owner.clone(), second, UtcMicros(2)),
            ],
            None,
            1,
        ),
    ];

    for (snapshots, after, limit) in cases {
        let application =
            MemoryApplication::new(owner.clone(), CurrentFactsPortFixture { snapshots }).unwrap();
        let error = block_on(application.query_current_facts(CurrentFactsQueryV1::new(
            owner.clone(),
            after,
            limit,
            (),
        )))
        .unwrap_err();
        assert!(matches!(
            error,
            MemoryUseCaseError::Invariant(
                MemoryApplicationInvariantError::InvalidAuthorityResult {
                    invariant: "current fact bounds, owner, cursor, and ordering"
                }
            )
        ));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProposalState {
    Pending,
    Applied,
}

struct ProposalPortFixture {
    owner: FactOwnerV1,
    fact_id: FactId,
    proposal_id: ProvenanceId,
    previous_state: ProposalState,
    disposition: CommitFactDispositionV1,
}

impl PromoteFactProposalPort for ProposalPortFixture {
    type Command = &'static str;
    type Error = DomainError;
    type Output = ProposalState;
    type State = ProposalState;

    async fn promote_fact_proposal(
        &self,
        _command: Self::Command,
    ) -> Result<PromoteFactProposalPortResultV1<Self::Output, Self::State>, Self::Error> {
        Ok(PromoteFactProposalPortResultV1::new(
            ProposalState::Applied,
            self.proposal_id.clone(),
            self.previous_state,
            self.disposition,
            Some(self.owner.clone()),
            Some(self.fact_id.clone()),
        ))
    }
}

#[test]
fn proposal_progress_and_committed_fact_identity_survive_the_port_boundary() {
    let owner = project_owner();
    let fact_id = fact_id(owner.clone(), "operation.memory.proposal");
    let application = MemoryApplication::new(
        owner.clone(),
        ProposalPortFixture {
            owner: owner.clone(),
            fact_id: fact_id.clone(),
            proposal_id: ProvenanceId::new("proposal.memory").unwrap(),
            previous_state: ProposalState::Pending,
            disposition: CommitFactDispositionV1::Committed,
        },
    )
    .unwrap();

    let output = block_on(
        application.promote_fact_proposal(PromoteFactProposalCommandV1::new(
            owner,
            ProvenanceId::new("proposal.memory").unwrap(),
            ProposalState::Pending,
            fact_id,
            "promote",
        )),
    )
    .unwrap();

    assert_eq!(output, ProposalState::Applied);
}

#[test]
fn proposal_previous_state_mismatch_is_rejected_as_a_cas_identity_violation() {
    let owner = project_owner();
    let fact_id = fact_id(owner.clone(), "operation.memory.proposal-mismatch");
    let application = MemoryApplication::new(
        owner.clone(),
        ProposalPortFixture {
            owner: owner.clone(),
            fact_id: fact_id.clone(),
            proposal_id: ProvenanceId::new("proposal.memory").unwrap(),
            previous_state: ProposalState::Applied,
            disposition: CommitFactDispositionV1::Committed,
        },
    )
    .unwrap();

    let error = block_on(
        application.promote_fact_proposal(PromoteFactProposalCommandV1::new(
            owner,
            ProvenanceId::new("proposal.memory").unwrap(),
            ProposalState::Pending,
            fact_id,
            "promote",
        )),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        MemoryUseCaseError::Invariant(MemoryApplicationInvariantError::InvalidAuthorityResult {
            invariant: "proposal CAS identity"
        })
    ));
}

#[test]
fn empty_payload_with_unknown_coverage_remains_truthfully_incomplete() {
    let result = MemoryReadResultV1::new(
        Vec::<FactId>::new(),
        MemoryReadCoverageV1::new(0, 0, 1, 0),
        MemoryContradictionStateV1::Unknown,
    );

    assert!(result.payload().is_empty());
    assert!(!result.coverage().is_complete());
    assert_eq!(result.contradiction(), &MemoryContradictionStateV1::Unknown);
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

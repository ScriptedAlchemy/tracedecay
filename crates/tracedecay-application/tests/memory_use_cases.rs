use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use tracedecay_application::memory::{
    CommitFactPort, CurrentFactsPort, MemoryApplication, MemoryApplicationInvariantError,
    MemoryCommitFactCommand, MemoryCommitFactDisposition, MemoryCommitFactPortResult,
    MemoryContradictionState, MemoryCurrentFactsPortResult, MemoryCurrentFactsQuery,
    MemoryFactSnapshot, MemoryReadCoverage, MemoryReadResult, MemoryUseCaseError,
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
    ) -> Result<MemoryCommitFactPortResult<Self::Output>, Self::Error> {
        self.calls.lock().unwrap().push(command);
        Ok(MemoryCommitFactPortResult::new(
            "committed",
            MemoryCommitFactDisposition::Committed,
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
        block_on(application.commit_fact(MemoryCommitFactCommand::new(owner, fact_id, "write")))
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

    let error = block_on(application.commit_fact(MemoryCommitFactCommand::new(
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
        block_on(application.commit_fact(MemoryCommitFactCommand::new(owner, fact_id, "write")))
            .unwrap_err();

    assert!(matches!(
        error,
        MemoryUseCaseError::Invariant(MemoryApplicationInvariantError::InvalidAuthorityResult {
            invariant: "fact commit identity"
        })
    ));
}

struct CurrentFactsPortFixture {
    snapshots: Vec<MemoryFactSnapshot>,
}

impl CurrentFactsPort for CurrentFactsPortFixture {
    type Error = DomainError;
    type Output = &'static str;
    type Query = ();

    async fn query_current_facts(
        &self,
        _query: Self::Query,
    ) -> Result<MemoryCurrentFactsPortResult<Self::Output>, Self::Error> {
        Ok(MemoryCurrentFactsPortResult::new(
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
                MemoryFactSnapshot::new(owner.clone(), second.clone(), UtcMicros(2)),
                MemoryFactSnapshot::new(owner.clone(), first, UtcMicros(1)),
            ],
        },
    )
    .unwrap();

    let error =
        block_on(application.query_current_facts(MemoryCurrentFactsQuery::new(owner, None, 2, ())))
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
            vec![MemoryFactSnapshot::new(
                FactOwnerV1::Profile,
                second.clone(),
                UtcMicros(2),
            )],
            None,
            1,
        ),
        (
            vec![MemoryFactSnapshot::new(
                owner.clone(),
                first.clone(),
                UtcMicros(1),
            )],
            Some(first.clone()),
            1,
        ),
        (
            vec![
                MemoryFactSnapshot::new(owner.clone(), first.clone(), UtcMicros(1)),
                MemoryFactSnapshot::new(owner.clone(), second, UtcMicros(2)),
            ],
            None,
            1,
        ),
    ];

    for (snapshots, after, limit) in cases {
        let application =
            MemoryApplication::new(owner.clone(), CurrentFactsPortFixture { snapshots }).unwrap();
        let error = block_on(
            application.query_current_facts(MemoryCurrentFactsQuery::new(
                owner.clone(),
                after,
                limit,
                (),
            )),
        )
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

#[test]
fn empty_payload_with_unknown_coverage_remains_truthfully_incomplete() {
    let result = MemoryReadResult::new(
        Vec::<FactId>::new(),
        MemoryReadCoverage::new(0, 0, 1, 0),
        MemoryContradictionState::Unknown,
    );

    assert!(result.payload().is_empty());
    assert!(!result.coverage().is_complete());
    assert_eq!(result.contradiction(), &MemoryContradictionState::Unknown);
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

//! Equivalence proof for the incremental Work projection fold.
//!
//! `reference_rebuild` is the full-history rebuild exactly as it was written
//! before the fold existed. Every test here asserts that folding one event at
//! a time through [`WorkProjectionStateV1::apply`] produces the same
//! projection value *and* the same serialized bytes, and that both reject the
//! same malformed histories with the same error.

use std::collections::BTreeSet;

use crate::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, TaskId, UtcMicros, WorktreeId,
};

use super::{
    WORK_PROJECTION_STATE_VERSION_V1, WorkAuthority, WorkContractError, WorkEvent, WorkEventKind,
    WorkProjection, WorkProjectionStateV1, WorkVersion,
};

/// The pre-fold implementation, kept verbatim as the equivalence oracle.
fn reference_rebuild(history: &[WorkEvent]) -> Result<WorkProjection, WorkContractError> {
    let first = history.first().ok_or(WorkContractError::EmptyHistory)?;
    let WorkEventKind::Created {
        title,
        dependencies,
    } = first.event()
    else {
        return Err(WorkContractError::MissingCreation);
    };
    if first.version() != WorkVersion::initial() {
        return Err(WorkContractError::NonContiguousVersion);
    }

    let mut projection = WorkProjection {
        task_id: first.task_id().clone(),
        version: first.version(),
        authority: first.authority().clone(),
        title: title.clone(),
        dependencies: dependencies.clone(),
        accepted_proposal: None,
        execution_admitted: false,
        task_accepted: false,
        history_len: 0,
    };
    let mut expected_version = WorkVersion::initial();
    let mut previous_time = first.occurred_at();
    let mut commands = BTreeSet::new();

    for event in history {
        if event.task_id() != &projection.task_id || event.authority() != &projection.authority {
            return Err(WorkContractError::MixedAuthority);
        }
        if event.version() != expected_version {
            return Err(WorkContractError::NonContiguousVersion);
        }
        if event.occurred_at() < previous_time {
            return Err(WorkContractError::NonMonotonicTime);
        }
        if !commands.insert(event.command_id().clone()) {
            return Err(WorkContractError::DuplicateCommand);
        }
        if projection.task_accepted && event.version() != WorkVersion::initial() {
            return Err(WorkContractError::InvalidTransition);
        }

        match event.event() {
            WorkEventKind::Created { .. } if event.version() != WorkVersion::initial() => {
                return Err(WorkContractError::InvalidTransition);
            }
            WorkEventKind::Created { .. } => {}
            WorkEventKind::DependenciesReplanned { dependencies } => {
                projection.dependencies = dependencies.clone();
            }
            WorkEventKind::ProposalAccepted { proposal_id, .. } => {
                projection.accepted_proposal = Some(proposal_id.clone());
            }
            WorkEventKind::ProposalRejected { proposal_id, .. }
            | WorkEventKind::ProposalSuperseded { proposal_id, .. } => {
                if projection.accepted_proposal.as_ref() == Some(proposal_id) {
                    projection.accepted_proposal = None;
                }
            }
            WorkEventKind::ExecutionAdmitted => {
                if projection.accepted_proposal.is_none() {
                    return Err(WorkContractError::InvalidTransition);
                }
                projection.execution_admitted = true;
            }
            WorkEventKind::TaskAccepted => projection.task_accepted = true,
        }

        projection.version = event.version();
        projection.history_len += 1;
        previous_time = event.occurred_at();
        expected_version = event.version().next()?;
    }

    Ok(projection)
}

/// Folds one event at a time, the way storage does on each append.
fn incremental(history: &[WorkEvent]) -> Result<WorkProjection, WorkContractError> {
    let (first, rest) = history
        .split_first()
        .ok_or(WorkContractError::EmptyHistory)?;
    let mut state = WorkProjectionStateV1::rebuild(std::slice::from_ref(first))?;
    for event in rest {
        let carried = serde_json::to_string(&state).expect("fold state serializes");
        let reloaded: WorkProjectionStateV1 =
            serde_json::from_str(&carried).expect("fold state round-trips");
        assert_eq!(reloaded, state, "persisted fold state must round-trip");
        state = reloaded.apply(event)?;
    }
    Ok(state.into_projection())
}

/// Asserts the incremental fold and the full rebuild agree on value, on
/// serialized bytes, and on rejection.
fn assert_equivalent(history: &[WorkEvent]) {
    let reference = reference_rebuild(history);
    let folded = incremental(history);
    let shipped = WorkProjection::rebuild(history);
    assert_eq!(folded, reference, "incremental fold diverged from rebuild");
    assert_eq!(
        shipped, reference,
        "shipped rebuild diverged from reference"
    );
    if let (Ok(reference), Ok(folded)) = (&reference, &folded) {
        assert_eq!(
            serde_json::to_vec(reference).unwrap(),
            serde_json::to_vec(folded).unwrap(),
            "incremental fold produced different bytes"
        );
    }
}

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

fn authority() -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.work.fold"),
        id::<RepositoryId>("repository.work.fold"),
        id::<WorktreeId>("worktree.work.fold"),
        id::<ActorId>("actor.work.fold"),
        digest('a'),
    )
    .unwrap()
}

fn event(version: u64, kind: WorkEventKind) -> WorkEvent {
    event_at(
        version,
        version as i64,
        &format!("command.work.fold.{version}"),
        kind,
    )
}

fn event_at(version: u64, occurred_at: i64, command: &str, kind: WorkEventKind) -> WorkEvent {
    WorkEvent::new(
        id::<TaskId>("task.work.fold"),
        WorkVersion::new(version).unwrap(),
        authority(),
        UtcMicros(occurred_at),
        id(command),
        digest('b'),
        kind,
    )
    .unwrap()
}

fn created() -> WorkEventKind {
    WorkEventKind::Created {
        title: "fold the projection".to_owned(),
        dependencies: BTreeSet::new(),
    }
}

fn accepted(proposal: &str) -> WorkEventKind {
    WorkEventKind::ProposalAccepted {
        proposal_id: id::<ProposalId>(proposal),
        proposal_digest: digest('c'),
    }
}

#[test]
fn folding_a_full_lifecycle_matches_the_full_rebuild() {
    assert_equivalent(&[
        event(1, created()),
        event(
            2,
            WorkEventKind::DependenciesReplanned {
                dependencies: BTreeSet::from([id::<TaskId>("task.work.fold.dependency")]),
            },
        ),
        event(3, accepted("proposal.work.fold.first")),
        event(4, WorkEventKind::ExecutionAdmitted),
        event(5, WorkEventKind::TaskAccepted),
    ]);
}

#[test]
fn folding_proposal_churn_matches_the_full_rebuild() {
    assert_equivalent(&[
        event(1, created()),
        event(2, accepted("proposal.work.fold.first")),
        event(
            3,
            WorkEventKind::ProposalSuperseded {
                proposal_id: id::<ProposalId>("proposal.work.fold.first"),
                proposal_digest: digest('c'),
            },
        ),
        event(4, accepted("proposal.work.fold.second")),
        event(
            5,
            WorkEventKind::ProposalRejected {
                proposal_id: id::<ProposalId>("proposal.work.fold.other"),
                proposal_digest: digest('c'),
            },
        ),
        event(6, WorkEventKind::ExecutionAdmitted),
    ]);
}

#[test]
fn folding_rejects_every_history_the_full_rebuild_rejects() {
    assert_equivalent(&[]);
    assert_equivalent(&[event(1, WorkEventKind::TaskAccepted)]);
    assert_equivalent(&[event(2, created())]);
    assert_equivalent(&[event(1, created()), event(3, WorkEventKind::TaskAccepted)]);
    assert_equivalent(&[event(1, created()), event(2, created())]);
    assert_equivalent(&[
        event(1, created()),
        event(2, WorkEventKind::ExecutionAdmitted),
    ]);
    assert_equivalent(&[
        event(1, created()),
        event(2, WorkEventKind::TaskAccepted),
        event(3, accepted("proposal.work.fold.after-acceptance")),
    ]);
    assert_equivalent(&[
        event_at(1, 20, "command.work.fold.1", created()),
        event_at(2, 10, "command.work.fold.2", WorkEventKind::TaskAccepted),
    ]);
    assert_equivalent(&[
        event_at(1, 1, "command.work.fold.shared", created()),
        event_at(
            2,
            2,
            "command.work.fold.shared",
            WorkEventKind::TaskAccepted,
        ),
    ]);
}

#[test]
fn every_prefix_of_a_history_folds_to_its_own_rebuild() {
    let history = [
        event(1, created()),
        event(2, accepted("proposal.work.fold.first")),
        event(3, WorkEventKind::ExecutionAdmitted),
        event(4, WorkEventKind::TaskAccepted),
    ];

    for length in 1..=history.len() {
        let prefix = &history[..length];
        assert_equivalent(prefix);
        let state = WorkProjectionStateV1::rebuild(prefix).unwrap();
        assert_eq!(state.command_ids().len(), length);
        assert_eq!(state.version().get(), length as u64);
        assert_eq!(state.projection().history_len(), length);
        assert_eq!(state.occurred_at(), UtcMicros(length as i64));
    }
}

#[test]
fn persisted_fold_state_refuses_an_unrecognised_version() {
    let state = WorkProjectionStateV1::rebuild(&[event(1, created())]).unwrap();
    assert_eq!(state.state_version(), WORK_PROJECTION_STATE_VERSION_V1);

    let mut payload: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&state).expect("fold state serializes"))
            .unwrap();
    payload["state_version"] = serde_json::json!(WORK_PROJECTION_STATE_VERSION_V1 + 1);

    assert!(serde_json::from_value::<WorkProjectionStateV1>(payload).is_err());
}

#[test]
fn persisted_fold_state_refuses_a_frontier_that_contradicts_its_projection() {
    let state = WorkProjectionStateV1::rebuild(&[
        event(1, created()),
        event(2, WorkEventKind::TaskAccepted),
    ])
    .unwrap();
    let encoded = serde_json::to_string(&state).expect("fold state serializes");

    let mut short_frontier: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    short_frontier["command_ids"] = serde_json::json!(["command.work.fold.1"]);
    assert!(serde_json::from_value::<WorkProjectionStateV1>(short_frontier).is_err());

    let mut skewed_version: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    skewed_version["next_version"] = serde_json::json!(9);
    assert!(serde_json::from_value::<WorkProjectionStateV1>(skewed_version).is_err());

    let mut unknown_field: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    unknown_field["run_ids"] = serde_json::json!(["run.work.fold.unexpected"]);
    assert!(serde_json::from_value::<WorkProjectionStateV1>(unknown_field).is_err());
}

use std::collections::BTreeSet;

use serde_json::json;
use tracedecay_domain::{
    ActorId, MAX_WORK_DEPENDENCIES, MAX_WORK_TITLE_BYTES, ManifestDigest, ProjectId, ProposalId,
    RepositoryId, RunId, RuntimeEvidenceRef, TaskId, UtcMicros, WorkAuthority, WorkEvent,
    WorkEventKind, WorkProjection, WorkVersion, WorktreeId,
};

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
        id::<ProjectId>("project.work.fixture"),
        id::<RepositoryId>("repository.work.fixture"),
        id::<WorktreeId>("worktree.work.fixture"),
        id::<ActorId>("actor.work.fixture"),
        digest('a'),
    )
    .unwrap()
}

fn event(version: u64, kind: WorkEventKind) -> WorkEvent {
    WorkEvent::new(
        id::<TaskId>("task.work.fixture"),
        WorkVersion::new(version).unwrap(),
        authority(),
        UtcMicros(version as i64),
        id(&format!("command.work.fixture.{version}")),
        digest('b'),
        kind,
    )
    .unwrap()
}

#[test]
fn task_and_version_identity_are_validated() {
    assert!(TaskId::new("task.stable").is_ok());
    assert!(TaskId::new(" task.unstable").is_err());
    assert!(RunId::new("run.stable").is_ok());
    assert!(RunId::new("run\nunstable").is_err());
    assert!(WorkVersion::new(0).is_err());
    assert!(serde_json::from_value::<WorkVersion>(json!(0)).is_err());
    assert_eq!(WorkVersion::initial().get(), 1);
    assert_eq!(WorkVersion::initial().next().unwrap().get(), 2);
}

#[test]
fn created_work_is_bounded() {
    let oversized_title = "x".repeat(MAX_WORK_TITLE_BYTES + 1);
    assert!(
        WorkEvent::new(
            id("task.work.oversized-title"),
            WorkVersion::initial(),
            authority(),
            UtcMicros(1),
            id("command.work.oversized-title"),
            digest('b'),
            WorkEventKind::Created {
                title: oversized_title,
                dependencies: BTreeSet::new(),
            },
        )
        .is_err()
    );

    let dependencies = (0..=MAX_WORK_DEPENDENCIES)
        .map(|ordinal| id::<TaskId>(&format!("task.work.dependency.{ordinal}")))
        .collect();
    assert!(
        WorkEvent::new(
            id("task.work.oversized-dependencies"),
            WorkVersion::initial(),
            authority(),
            UtcMicros(1),
            id("command.work.oversized-dependencies"),
            digest('b'),
            WorkEventKind::Created {
                title: "Bound dependencies".to_owned(),
                dependencies,
            },
        )
        .is_err()
    );
}

#[test]
fn projection_rebuild_is_deterministic_and_runtime_evidence_does_not_accept_work() {
    let proposal_id = id::<ProposalId>("proposal.work.fixture");
    let history = vec![
        event(
            1,
            WorkEventKind::Created {
                title: "Implement bounded work authority".to_owned(),
                dependencies: BTreeSet::new(),
            },
        ),
        event(
            2,
            WorkEventKind::ProposalAccepted {
                proposal_id: proposal_id.clone(),
                proposal_digest: digest('c'),
            },
        ),
        event(
            3,
            WorkEventKind::RuntimeEvidenceAttached {
                evidence: RuntimeEvidenceRef::new(id("run.work.fixture"), digest('d'), true)
                    .unwrap(),
            },
        ),
    ];

    let first = WorkProjection::rebuild(&history).unwrap();
    let second = WorkProjection::rebuild(&history).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.version(), WorkVersion::new(3).unwrap());
    assert_eq!(first.accepted_proposal(), Some(&proposal_id));
    assert_eq!(first.runtime_evidence().len(), 1);
    assert!(!first.is_task_accepted());
}

#[test]
fn projection_rebuild_rejects_non_contiguous_or_wrong_task_history() {
    let mut history = vec![event(
        1,
        WorkEventKind::Created {
            title: "One task".to_owned(),
            dependencies: BTreeSet::new(),
        },
    )];
    history.push(
        WorkEvent::new(
            id::<TaskId>("task.other"),
            WorkVersion::new(2).unwrap(),
            authority(),
            UtcMicros(2),
            id("command.other"),
            digest('e'),
            WorkEventKind::TaskAccepted,
        )
        .unwrap(),
    );

    assert!(WorkProjection::rebuild(&history).is_err());
}

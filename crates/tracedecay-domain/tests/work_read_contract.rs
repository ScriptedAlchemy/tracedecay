use std::collections::BTreeSet;

use serde_json::json;
use tracedecay_domain::{
    ActorId, MAX_WORK_PROJECTION_READ_ITEMS, ManifestDigest, ProjectId, ProjectionGenerationId,
    RepositoryId, TaskId, UtcMicros, WorkAuthority, WorkEvent, WorkEventKind, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1,
    WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1, WorkVersion,
    WorktreeId,
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

fn projection(task: &str, title: &str) -> WorkProjection {
    WorkProjection::rebuild(&[WorkEvent::new(
        id::<TaskId>(task),
        WorkVersion::initial(),
        WorkAuthority::new(
            id::<ProjectId>("project.work.read"),
            id::<RepositoryId>("repository.work.read"),
            id::<WorktreeId>("worktree.work.read"),
            id::<ActorId>("actor.work.read"),
            digest('a'),
        )
        .unwrap(),
        UtcMicros(1),
        id(&format!("command.{task}")),
        digest('b'),
        WorkEventKind::Created {
            title: title.to_owned(),
            dependencies: BTreeSet::new(),
        },
    )
    .unwrap()])
    .unwrap()
}

fn generation(value: &str) -> ProjectionGenerationId {
    id(value)
}

fn cursor(value: &str) -> WorkProjectionResumeCursorV1 {
    WorkProjectionResumeCursorV1::new(generation("generation.work.read.1"), value).unwrap()
}

#[test]
fn complete_snapshot_is_canonical_and_round_trips() {
    let snapshot = WorkProjectionSnapshotV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(7),
        vec![
            projection("task.work.read.b", "Second"),
            projection("task.work.read.a", "First"),
        ],
        WorkProjectionCoverageV1::complete(2, 2).unwrap(),
    )
    .unwrap();

    assert_eq!(
        snapshot.projections()[0].task_id().as_str(),
        "task.work.read.a"
    );
    assert!(snapshot.coverage().resume_cursor().is_none());
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(wire["coverage"]["state"], "complete");
    assert!(wire["coverage"].get("cursor").is_none());
    assert_eq!(
        serde_json::from_value::<WorkProjectionSnapshotV1>(wire.clone()).unwrap(),
        snapshot
    );

    let mut future_wire = wire;
    future_wire["future_response_metadata"] = json!({"revision": 2});
    assert_eq!(
        serde_json::from_value::<WorkProjectionSnapshotV1>(future_wire).unwrap(),
        snapshot
    );
    assert!(
        WorkProjectionSnapshotV1::new(
            generation("generation.work.read.1"),
            WorkProjectionSequenceV1::new(7),
            vec![
                projection("task.work.read.duplicate", "First"),
                projection("task.work.read.duplicate", "Second"),
            ],
            WorkProjectionCoverageV1::complete(2, 2).unwrap(),
        )
        .is_err()
    );
}

#[test]
fn partial_and_capped_coverage_require_truthful_counts_ranges_and_cursors() {
    let range = WorkProjectionSequenceRangeV1::new(
        WorkProjectionSequenceV1::new(4),
        WorkProjectionSequenceV1::new(7),
    )
    .unwrap();
    let resume = cursor("opaque.application.authenticated.token");

    assert!(WorkProjectionCoverageV1::partial(1, 2, range, resume.clone()).is_ok());
    assert!(WorkProjectionCoverageV1::partial(2, 2, range, resume.clone()).is_err());
    assert!(WorkProjectionCoverageV1::capped(1, 3, 1, range, resume).is_ok());
    assert!(WorkProjectionCoverageV1::capped(1, 1, 1, range, cursor("other")).is_err());
    assert!(WorkProjectionCoverageV1::complete(1, 2).is_err());

    assert!(
        serde_json::from_value::<WorkProjectionCoverageV1>(json!({
            "state": "complete",
            "returned": 1,
            "total": 1,
            "cursor": {
                "generation_id": "generation.work.read.1",
                "token": "opaque.forbidden"
            }
        }))
        .is_err()
    );

    let wrong_generation_coverage = WorkProjectionCoverageV1::partial(
        1,
        2,
        range,
        WorkProjectionResumeCursorV1::new(
            generation("generation.work.read.other"),
            "opaque.wrong-generation",
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        WorkProjectionSnapshotV1::new(
            generation("generation.work.read.1"),
            WorkProjectionSequenceV1::new(7),
            vec![projection("task.work.read.a", "First")],
            wrong_generation_coverage,
        )
        .is_err()
    );

    let partial_snapshot = WorkProjectionSnapshotV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(7),
        vec![projection("task.work.read.a", "First")],
        WorkProjectionCoverageV1::partial(1, 2, range, cursor("opaque.partial")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        partial_snapshot
            .coverage()
            .resume_cursor()
            .unwrap()
            .generation_id(),
        partial_snapshot.generation_id()
    );
}

#[test]
fn resume_cursor_wire_is_opaque_and_validated() {
    let resume = cursor("opaque.application.authenticated.token");
    assert_eq!(
        serde_json::to_value(&resume).unwrap(),
        json!({
            "generation_id": "generation.work.read.1",
            "token": "opaque.application.authenticated.token"
        })
    );
    assert!(WorkProjectionResumeCursorV1::new(generation("generation.work.read.1"), "").is_err());
    assert!(
        WorkProjectionResumeCursorV1::new(generation("generation.work.read.1"), " offset=4")
            .is_err()
    );
    assert!(
        serde_json::from_value::<WorkProjectionResumeCursorV1>(json!({
            "generation_id": "generation.work.read.1",
            "token": "line\nbreak"
        }))
        .is_err()
    );
}

#[test]
fn delta_is_generation_bound_monotonic_and_disjoint() {
    let generation_id = generation("generation.work.read.1");
    let snapshot = WorkProjectionSnapshotV1::new(
        generation_id.clone(),
        WorkProjectionSequenceV1::new(7),
        vec![projection("task.work.read.a", "First")],
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    let range = WorkProjectionSequenceRangeV1::new(
        WorkProjectionSequenceV1::new(7),
        WorkProjectionSequenceV1::new(9),
    )
    .unwrap();
    let delta = WorkProjectionDeltaV1::new(
        generation_id,
        WorkProjectionSequenceV1::new(7),
        WorkProjectionSequenceV1::new(9),
        vec![projection("task.work.read.b", "Changed")],
        BTreeSet::from([id::<TaskId>("task.work.read.removed")]),
        WorkProjectionCoverageV1::partial(2, 3, range, cursor("opaque.next")).unwrap(),
    )
    .unwrap();

    delta.validate_after(&snapshot).unwrap();
    assert_eq!(delta.changed().len(), 1);
    assert_eq!(delta.removed().len(), 1);

    let overlap = WorkProjectionDeltaV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(7),
        WorkProjectionSequenceV1::new(9),
        vec![projection("task.work.read.same", "Changed")],
        BTreeSet::from([id::<TaskId>("task.work.read.same")]),
        WorkProjectionCoverageV1::partial(2, 3, range, cursor("opaque.overlap")).unwrap(),
    );
    assert!(overlap.is_err());

    let too_many = (0..=MAX_WORK_PROJECTION_READ_ITEMS)
        .map(|ordinal| projection(&format!("task.work.read.changed.{ordinal:04}"), "Changed"))
        .collect();
    assert!(
        WorkProjectionDeltaV1::new(
            generation("generation.work.read.1"),
            WorkProjectionSequenceV1::new(7),
            WorkProjectionSequenceV1::new(9),
            too_many,
            BTreeSet::new(),
            WorkProjectionCoverageV1::complete(
                (MAX_WORK_PROJECTION_READ_ITEMS + 1) as u32,
                (MAX_WORK_PROJECTION_READ_ITEMS + 1) as u32,
            )
            .unwrap(),
        )
        .is_err()
    );

    let other_generation = WorkProjectionSnapshotV1::new(
        generation("generation.work.read.other"),
        WorkProjectionSequenceV1::new(7),
        vec![projection("task.work.read.a", "First")],
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    assert!(delta.validate_after(&other_generation).is_err());

    let wrong_sequence = WorkProjectionSnapshotV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(6),
        vec![projection("task.work.read.a", "First")],
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    assert!(delta.validate_after(&wrong_sequence).is_err());
}

#[test]
fn deserialization_rejects_forged_snapshot_and_delta_states() {
    let snapshot = WorkProjectionSnapshotV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(7),
        vec![projection("task.work.read.a", "First")],
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    let mut forged_snapshot = serde_json::to_value(snapshot).unwrap();
    forged_snapshot["coverage"]["returned"] = json!(0);
    assert!(serde_json::from_value::<WorkProjectionSnapshotV1>(forged_snapshot).is_err());

    let range = WorkProjectionSequenceRangeV1::new(
        WorkProjectionSequenceV1::new(7),
        WorkProjectionSequenceV1::new(9),
    )
    .unwrap();
    let delta = WorkProjectionDeltaV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(7),
        WorkProjectionSequenceV1::new(9),
        vec![projection("task.work.read.b", "Changed")],
        BTreeSet::new(),
        WorkProjectionCoverageV1::partial(1, 2, range, cursor("opaque.next")).unwrap(),
    )
    .unwrap();
    let mut forged_delta = serde_json::to_value(delta).unwrap();
    forged_delta["to_sequence"] = forged_delta["from_sequence"].clone();
    assert!(serde_json::from_value::<WorkProjectionDeltaV1>(forged_delta).is_err());

    let valid_delta = WorkProjectionDeltaV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(7),
        WorkProjectionSequenceV1::new(9),
        vec![projection("task.work.read.b", "Changed")],
        BTreeSet::new(),
        WorkProjectionCoverageV1::partial(1, 2, range, cursor("opaque.next")).unwrap(),
    )
    .unwrap();
    let mut wrong_cursor_generation = serde_json::to_value(valid_delta).unwrap();
    wrong_cursor_generation["generation_id"] = json!("generation.work.read.other");
    assert!(serde_json::from_value::<WorkProjectionDeltaV1>(wrong_cursor_generation).is_err());

    let removed_delta = WorkProjectionDeltaV1::new(
        generation("generation.work.read.1"),
        WorkProjectionSequenceV1::new(7),
        WorkProjectionSequenceV1::new(9),
        Vec::new(),
        BTreeSet::from([id::<TaskId>("task.work.read.removed")]),
        WorkProjectionCoverageV1::complete(1, 1).unwrap(),
    )
    .unwrap();
    let mut duplicate_removed = serde_json::to_value(removed_delta).unwrap();
    duplicate_removed["removed"]
        .as_array_mut()
        .unwrap()
        .push(json!("task.work.read.removed"));
    assert!(serde_json::from_value::<WorkProjectionDeltaV1>(duplicate_removed).is_err());
}

/// A newer writer may add fields a older reader has never heard of. Dropping
/// them must not change the projection the older reader reconstructs.
#[test]
fn unknown_projection_fields_are_ignored_on_read() {
    let projection = projection("task.work.read.legacy", "Legacy projection");
    let mut value = serde_json::to_value(&projection).unwrap();
    value["future_projection_metadata"] = json!({"revision": 2});

    assert_eq!(
        serde_json::from_value::<WorkProjection>(value).unwrap(),
        projection
    );
}

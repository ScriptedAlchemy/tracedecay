mod work_registered_store;

use rusqlite::params;
use std::num::NonZeroU16;

use tracedecay_application::{
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkDuplicateAdjudicationPortV1,
    WorkDuplicateAdjudicationStorageErrorV1, WorkDuplicateAdjudicationWriteV1,
    WorkOwnerObservationKindV1, WorkOwnerObservationMarkOutcomeV1, WorkOwnerObservationReceiptV1,
    WorkOwnerObservationStoragePortV1, work_duplicate_adjudication_input_digest,
};
use tracedecay_domain::{
    ActorId, AttemptId, CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
    ManifestDigest, ProjectId, ProjectionGenerationId, QuantityEvidenceClassV1, RepositoryId,
    RunId, TaskId, UtcMicros, WorkAttemptIdentityV1, WorkAuthority, WorkCommandId,
    WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationEvidenceV1,
    WorkDuplicateAdjudicationQuantitiesV1, WorkDuplicateAdjudicationRevisionV1,
    WorkTopologyGenerationRefV1, WorktreeId,
};

use work_registered_store::RegisteredWorkStore;

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

fn topology_ref(byte: char) -> WorkTopologyGenerationRefV1 {
    WorkTopologyGenerationRefV1::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn authority() -> WorkAuthority {
    authority_for("duplicate")
}

fn authority_for(suffix: &str) -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.duplicate"),
        id::<RepositoryId>("repository.duplicate"),
        id::<WorktreeId>(&format!("worktree.{suffix}")),
        id::<ActorId>("actor.duplicate"),
        digest('a'),
    )
    .unwrap()
}

fn attempt(task: &str, run: &str, attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>(task),
        id::<RunId>(run),
        id::<AttemptId>(attempt),
    )
    .unwrap()
}

fn command(
    command_id: &str,
    expected_revision: Option<WorkDuplicateAdjudicationRevisionV1>,
    verdict: DuplicateEffortKindV1,
) -> WorkDuplicateAdjudicationCommandV1 {
    WorkDuplicateAdjudicationCommandV1 {
        expected_revision,
        first_attempt: attempt("task.duplicate.1", "run.duplicate.1", "attempt.duplicate.1"),
        second_attempt: attempt("task.duplicate.2", "run.duplicate.2", "attempt.duplicate.2"),
        evidence: WorkDuplicateAdjudicationEvidenceV1 {
            work_generation: id::<ProjectionGenerationId>("generation.work.1"),
            topology_generation: topology_ref('1'),
        },
        verdict,
        quantities: WorkDuplicateAdjudicationQuantitiesV1 {
            wall_micros: Some(10),
            token_count: None,
            cost_micros: None,
            test_count: None,
            effect_count: None,
            evidence: QuantityEvidenceClassV1::OwnerReceipt,
            effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
            coverage: if verdict == DuplicateEffortKindV1::Unknown {
                CoverageStateV1::Unknown
            } else {
                CoverageStateV1::Known
            },
        },
        reason: "independent review".to_owned(),
        command_id: id::<WorkCommandId>(command_id),
        occurred_at: UtcMicros(100),
    }
}

fn write(command: WorkDuplicateAdjudicationCommandV1) -> WorkDuplicateAdjudicationWriteV1 {
    let canonical_input_digest = work_duplicate_adjudication_input_digest(&command).unwrap();
    WorkDuplicateAdjudicationWriteV1 {
        actor_id: id::<ActorId>("actor.duplicate"),
        command,
        canonical_input_digest,
    }
}

fn insert_attempts(connection: &rusqlite::Connection) {
    insert_attempts_for(connection, &authority());
}

fn insert_attempts_for(connection: &rusqlite::Connection, authority: &WorkAuthority) {
    for identity in [
        attempt("task.duplicate.1", "run.duplicate.1", "attempt.duplicate.1"),
        attempt("task.duplicate.2", "run.duplicate.2", "attempt.duplicate.2"),
        attempt("task.duplicate.3", "run.duplicate.3", "attempt.duplicate.3"),
    ] {
        connection
            .execute(
                "INSERT INTO work_attempts_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, state, lease_id, fence_epoch,
                    terminal, attempt_payload, evidence_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'succeeded', 'lease.fixture', 1,
                    1, '{}', NULL)",
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    identity.task_id().as_str(),
                    identity.run_id().as_str(),
                    identity.attempt_id().as_str(),
                ],
            )
            .unwrap();
    }
}

#[test]
fn pending_scan_cursor_does_not_skip_the_same_command_in_another_authority() {
    let second_authority = authority_for("duplicate-second");
    let store = RegisteredWorkStore::start_with_setup(
        "duplicate-adjudication-observation-authority-cursor",
        |connection| {
            insert_attempts(connection);
            insert_attempts_for(connection, &second_authority);
        },
    );
    let write = write(command(
        "command.duplicate.same-cursor",
        None,
        DuplicateEffortKindV1::ExactDuplicate,
    ));
    store
        .storage()
        .compare_and_record_duplicate_adjudication(&authority(), &write)
        .unwrap();
    store
        .storage()
        .compare_and_record_duplicate_adjudication(&second_authority, &write)
        .unwrap();

    let first = store
        .storage()
        .pending_owner_observations(None, NonZeroU16::new(1).unwrap())
        .unwrap();
    assert_eq!(first.len(), 1);
    let second = store
        .storage()
        .pending_owner_observations(Some(&first[0].scan_cursor), NonZeroU16::new(1).unwrap())
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_ne!(
        first[0].marker.authority, second[0].marker.authority,
        "the exact cursor must retain both authority-scoped receipts"
    );
}

#[test]
fn adjudication_is_revision_cas_and_exact_replay_on_the_registered_store() {
    let store = RegisteredWorkStore::start_with_setup("duplicate-adjudication", insert_attempts);
    let first = write(command(
        "command.duplicate.1",
        None,
        DuplicateEffortKindV1::ExactDuplicate,
    ));
    let appended = store
        .storage()
        .compare_and_record_duplicate_adjudication(&authority(), &first)
        .unwrap();
    let WorkDuplicateAdjudicationAppendOutcomeV1::Appended(receipt) = appended else {
        panic!("first adjudication must append")
    };
    assert_eq!(receipt.revision().get(), 1);

    assert!(matches!(
        store
            .storage()
            .compare_and_record_duplicate_adjudication(&authority(), &first)
            .unwrap(),
        WorkDuplicateAdjudicationAppendOutcomeV1::Replayed(_)
    ));
    assert_eq!(store.count("work_duplicate_adjudications_v1"), 1);

    let stale = write(command(
        "command.duplicate.stale",
        None,
        DuplicateEffortKindV1::NotDuplicate,
    ));
    assert_eq!(
        store
            .storage()
            .compare_and_record_duplicate_adjudication(&authority(), &stale)
            .unwrap_err(),
        WorkDuplicateAdjudicationStorageErrorV1::RevisionConflict
    );

    let mut changed_pair = command(
        "command.duplicate.changed-pair",
        Some(WorkDuplicateAdjudicationRevisionV1::initial()),
        DuplicateEffortKindV1::NotDuplicate,
    );
    changed_pair.second_attempt =
        attempt("task.duplicate.3", "run.duplicate.3", "attempt.duplicate.3");
    assert_eq!(
        store
            .storage()
            .compare_and_record_duplicate_adjudication(&authority(), &write(changed_pair))
            .unwrap_err(),
        WorkDuplicateAdjudicationStorageErrorV1::RevisionConflict,
        "an adjudication revision cannot change its exact attempt pair"
    );

    let correction = write(command(
        "command.duplicate.2",
        Some(WorkDuplicateAdjudicationRevisionV1::initial()),
        DuplicateEffortKindV1::NotDuplicate,
    ));
    let corrected = store
        .storage()
        .compare_and_record_duplicate_adjudication(&authority(), &correction)
        .unwrap();
    let WorkDuplicateAdjudicationAppendOutcomeV1::Appended(receipt) = corrected else {
        panic!("correction must append")
    };
    assert_eq!(receipt.revision().get(), 2);
    assert_eq!(store.count("work_duplicate_adjudications_v1"), 2);

    let attempts = [
        attempt("task.duplicate.1", "run.duplicate.1", "attempt.duplicate.1"),
        attempt("task.duplicate.2", "run.duplicate.2", "attempt.duplicate.2"),
    ];
    let latest = store
        .storage()
        .latest_duplicate_adjudications_for_attempts(
            &authority(),
            &id::<ProjectionGenerationId>("generation.work.1"),
            &topology_ref('1'),
            &attempts,
        )
        .unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].revision().get(), 2);
    assert_eq!(
        latest[0].command().verdict,
        DuplicateEffortKindV1::NotDuplicate
    );
}

#[test]
fn adjudication_refuses_attempts_outside_the_exact_work_authority() {
    let store = RegisteredWorkStore::start("duplicate-adjudication-missing-attempts");
    let write = write(command(
        "command.duplicate.missing",
        None,
        DuplicateEffortKindV1::Unknown,
    ));
    assert_eq!(
        store
            .storage()
            .compare_and_record_duplicate_adjudication(&authority(), &write)
            .unwrap_err(),
        WorkDuplicateAdjudicationStorageErrorV1::NotFoundOrNotAuthorized
    );
    assert_eq!(store.count("work_duplicate_adjudications_v1"), 0);
}

#[test]
fn adjudication_persists_a_pending_observation_marker_with_the_receipt() {
    let store = RegisteredWorkStore::start_with_setup(
        "duplicate-adjudication-observation-marker",
        insert_attempts,
    );
    store
        .storage()
        .compare_and_record_duplicate_adjudication(
            &authority(),
            &write(command(
                "command.duplicate.observation-marker",
                None,
                DuplicateEffortKindV1::ExactDuplicate,
            )),
        )
        .unwrap();

    let (state, digest): (String, String) = store.inspect(|connection| {
        connection
            .query_row(
                "SELECT observation_state, receipt_digest
                 FROM work_duplicate_adjudications_v1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    });
    assert_eq!(state, "pending");
    assert!(ManifestDigest::new(digest).is_ok());
}

#[test]
fn one_exact_attempt_pair_has_one_revisioned_adjudication_identity() {
    let store = RegisteredWorkStore::start_with_setup(
        "duplicate-adjudication-pair-identity",
        insert_attempts,
    );
    store
        .storage()
        .compare_and_record_duplicate_adjudication(
            &authority(),
            &write(command(
                "command.duplicate.pair-identity.first",
                None,
                DuplicateEffortKindV1::ExactDuplicate,
            )),
        )
        .unwrap();

    let duplicate_identity = command(
        "command.duplicate.pair-identity.second",
        None,
        DuplicateEffortKindV1::ExactDuplicate,
    );
    assert_eq!(
        store
            .storage()
            .compare_and_record_duplicate_adjudication(&authority(), &write(duplicate_identity),)
            .unwrap_err(),
        WorkDuplicateAdjudicationStorageErrorV1::RevisionConflict,
        "a second command cannot create a second relation for one exact attempt pair"
    );
    assert_eq!(store.count("work_duplicate_adjudications_v1"), 1);
}

#[test]
fn pending_duplicate_receipt_is_recoverable_and_exactly_marked_durable() {
    let store = RegisteredWorkStore::start_with_setup(
        "duplicate-adjudication-observation-recovery",
        insert_attempts,
    );
    let appended = store
        .storage()
        .compare_and_record_duplicate_adjudication(
            &authority(),
            &write(command(
                "command.duplicate.observation-recovery",
                None,
                DuplicateEffortKindV1::ExactDuplicate,
            )),
        )
        .unwrap();
    let store = store.restart("duplicate-adjudication-observation-recovery-restarted");

    let pending = store
        .storage()
        .pending_owner_observations(None, NonZeroU16::new(8).unwrap())
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].marker.kind,
        WorkOwnerObservationKindV1::Duplicate
    );
    assert_eq!(
        pending[0].receipt,
        WorkOwnerObservationReceiptV1::Duplicate(appended.receipt().clone())
    );
    assert!(pending[0].validate());
    assert_eq!(
        store
            .storage()
            .mark_owner_observation_durable(&pending[0].marker)
            .unwrap(),
        WorkOwnerObservationMarkOutcomeV1::Marked
    );
    assert_eq!(
        store
            .storage()
            .mark_owner_observation_durable(&pending[0].marker)
            .unwrap(),
        WorkOwnerObservationMarkOutcomeV1::Replayed
    );
    assert!(
        store
            .storage()
            .pending_owner_observations(None, NonZeroU16::new(8).unwrap())
            .unwrap()
            .is_empty()
    );
}

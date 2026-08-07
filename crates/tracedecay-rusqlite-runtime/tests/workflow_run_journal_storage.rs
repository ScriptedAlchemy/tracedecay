//! Durable workflow run journal and artifact payload store over the
//! registered Work writer.
//!
//! Falsifiable claims: journaled runs and pause transitions survive a full
//! store restart; command replay is idempotent while divergent reuse and
//! stale sequences are typed conflicts; artifact payloads are digest-verified
//! on every hydration so a corrupted row can never re-enter execution.

use std::collections::BTreeSet;

use tracedecay_application::{
    WorkflowAdmissionSnapshot, WorkflowArtifactPayload, WorkflowArtifactPersistOutcome,
    WorkflowArtifactStoreError, WorkflowArtifactStorePort, WorkflowRunAppendOutcome,
    WorkflowRunAppendRequest, WorkflowRunService, WorkflowRunStorageError, WorkflowRunStoragePort,
    work_executable_catalog_digest, workflow_artifact_payload_digest,
};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RunId, UtcMicros, WorkArtifactId, WorkArtifactRefV1, WorkCommandId,
    WorkflowDefinition, WorkflowDefinitionId, WorkflowOperationRef, WorkflowOutputName,
    WorkflowRunCommand, WorkflowRunEvent, WorkflowRunEventContext, WorkflowRunStatus, WorkflowStep,
    WorkflowStepId,
};
use tracedecay_rusqlite_runtime::workflow::WorkflowSqliteAuthority;

mod registered_workflow_store;

use registered_workflow_store::RegisteredWorkflowStore;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

/// A distinct, valid `sha256:`-tagged digest per input byte.
fn digest(byte: char) -> ManifestDigest {
    let hex_byte = format!("{:02x}", u32::from(byte) & 0xff);
    ManifestDigest::new(format!("sha256:{}", hex_byte.repeat(32))).unwrap()
}

fn context(command: &str, input: char, occurred_at: i64) -> WorkflowRunEventContext {
    WorkflowRunEventContext {
        command_id: id::<WorkCommandId>(command),
        input_digest: digest(input),
        occurred_at: UtcMicros(occurred_at),
    }
}

fn definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        id::<WorkflowDefinitionId>("workflow.definition.journal"),
        1,
        id::<ProjectId>("project.workflow.journal"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: None,
        }],
        digest('a'),
        digest('b'),
        work_executable_catalog_digest().unwrap(),
    )
    .unwrap()
}

fn admission() -> WorkflowAdmissionSnapshot {
    WorkflowAdmissionSnapshot {
        policy_digest: digest('a'),
        configuration_digest: digest('b'),
        catalog_digest: work_executable_catalog_digest().unwrap(),
        topology_digest: digest('c'),
        provider_registry_digest: digest('d'),
    }
}

fn attach(store: &RegisteredWorkflowStore) -> WorkflowSqliteAuthority {
    WorkflowSqliteAuthority::from_registered(store.storage().clone())
        .expect("attach workflow authority")
}

fn content_artifact(name: &str, content: &[u8]) -> WorkflowArtifactPayload {
    let reference = WorkArtifactRefV1::new(
        id::<WorkArtifactId>(name),
        workflow_artifact_payload_digest(content).unwrap(),
        content.len() as u64,
    )
    .unwrap();
    WorkflowArtifactPayload::new(reference, content.to_vec()).unwrap()
}

#[test]
fn paused_run_survives_restart_and_resumes_from_durable_state() {
    let store = RegisteredWorkflowStore::start("run-journal-pause-crash-resume");
    let authority = attach(&store);
    let run_id = id::<RunId>("run.workflow.journal.pause");
    let service = WorkflowRunService::new(authority.clone());
    let admitted = service
        .admit(
            run_id.clone(),
            definition(),
            admission(),
            context("command.journal.admit", '1', 1),
        )
        .unwrap();
    assert_eq!(admitted.status(), WorkflowRunStatus::Running);
    let paused = service
        .apply(
            &run_id,
            admitted.sequence(),
            WorkflowRunCommand::Pause,
            context("command.journal.pause", '2', 2),
        )
        .unwrap();
    assert_eq!(paused.status(), WorkflowRunStatus::Paused);

    // The pause is a durable typed transition, not process suspension: a full
    // store restart rebinds the channel and the run is still exactly paused.
    let store = store.restart("run-journal-pause-crash-resume-restarted");
    let reopened = attach(&store);
    let recovered = WorkflowRunStoragePort::projection(&reopened, &run_id).unwrap();
    assert_eq!(recovered.status(), WorkflowRunStatus::Paused);
    assert_eq!(recovered.sequence(), paused.sequence());

    let resumed = WorkflowRunService::new(reopened)
        .apply(
            &run_id,
            recovered.sequence(),
            WorkflowRunCommand::Resume,
            context("command.journal.resume", '3', 3),
        )
        .unwrap();
    assert_eq!(resumed.status(), WorkflowRunStatus::Running);
}

#[test]
fn command_replay_is_idempotent_and_divergent_reuse_is_a_typed_conflict() {
    let store = RegisteredWorkflowStore::start("run-journal-idempotency");
    let authority = attach(&store);
    let run_id = id::<RunId>("run.workflow.journal.idempotency");
    let admit_event = WorkflowRunEvent::admitted(
        run_id.clone(),
        definition(),
        digest('c'),
        digest('d'),
        context("command.journal.admit", '1', 1),
    )
    .unwrap();
    let appended = authority
        .append(&WorkflowRunAppendRequest {
            expected_sequence: None,
            event: admit_event.clone(),
        })
        .unwrap();
    assert!(matches!(appended, WorkflowRunAppendOutcome::Appended(_)));

    // Byte-identical replay of the same command is answered from the journal.
    let replayed = authority
        .append(&WorkflowRunAppendRequest {
            expected_sequence: None,
            event: admit_event.clone(),
        })
        .unwrap();
    assert!(matches!(replayed, WorkflowRunAppendOutcome::Replayed(_)));
    assert_eq!(store.count("workflow_run_journal"), 1);

    // The same command identity with different input is a conflict, not a
    // second admission.
    let divergent = WorkflowRunEvent::admitted(
        run_id.clone(),
        definition(),
        digest('e'),
        digest('d'),
        context("command.journal.admit", '1', 1),
    )
    .unwrap();
    assert_eq!(
        authority
            .append(&WorkflowRunAppendRequest {
                expected_sequence: None,
                event: divergent,
            })
            .unwrap_err(),
        WorkflowRunStorageError::IdempotencyConflict
    );

    // A stale expected sequence is a version conflict before any write.
    let projection = WorkflowRunStoragePort::projection(&authority, &run_id).unwrap();
    let stale = projection.next_event(
        WorkflowRunCommand::Pause,
        context("command.journal.pause", '2', 2),
    );
    let pause_event = stale.unwrap();
    assert_eq!(
        authority
            .append(&WorkflowRunAppendRequest {
                expected_sequence: Some(projection.sequence() + 1),
                event: pause_event,
            })
            .unwrap_err(),
        WorkflowRunStorageError::VersionConflict
    );
    assert_eq!(store.count("workflow_run_journal"), 1);

    assert_eq!(
        WorkflowRunStoragePort::projection(
            &authority,
            &id::<RunId>("run.workflow.journal.absent")
        )
        .unwrap_err(),
        WorkflowRunStorageError::NotFound
    );
}

#[test]
fn artifact_payloads_survive_restart_and_hydration_verifies_content() {
    let store = RegisteredWorkflowStore::start("artifact-payload-durability");
    let authority = attach(&store);
    let payload = content_artifact("artifact.workflow.journal.context", b"durable context bytes");

    assert_eq!(
        authority.persist(&payload).unwrap(),
        WorkflowArtifactPersistOutcome::Persisted
    );
    assert_eq!(
        authority.persist(&payload).unwrap(),
        WorkflowArtifactPersistOutcome::Replayed
    );
    assert_eq!(store.count("workflow_artifact_payloads"), 1);

    let store = store.restart("artifact-payload-durability-restarted");
    let reopened = attach(&store);
    assert_eq!(reopened.load(payload.artifact()).unwrap(), payload);

    let absent = content_artifact("artifact.workflow.journal.absent", b"never persisted");
    assert_eq!(
        reopened.load(absent.artifact()).unwrap_err(),
        WorkflowArtifactStoreError::Missing
    );
}

#[test]
fn corrupted_artifact_rows_are_refused_on_hydration() {
    let store = RegisteredWorkflowStore::start("artifact-payload-corruption");
    let authority = attach(&store);
    let payload = content_artifact("artifact.workflow.journal.context", b"durable context bytes");
    assert_eq!(
        authority.persist(&payload).unwrap(),
        WorkflowArtifactPersistOutcome::Persisted
    );

    // A foreign writer flips the stored bytes under the same digest row (the
    // same length keeps the schema CHECK satisfied, so only content
    // verification can catch it).
    store.inspect(|connection| {
        connection
            .execute(
                "UPDATE workflow_artifact_payloads SET payload = ?1",
                [b"DURABLE CONTEXT BYTES".as_slice()],
            )
            .unwrap();
    });
    assert_eq!(
        authority.load(payload.artifact()).unwrap_err(),
        WorkflowArtifactStoreError::DigestMismatch
    );
}

//! Workflow/handoff runtime: a direct daemon restart journey.
//!
//! Definitions, activations, single-use handoff tokens, and fan-out execution
//! fencing all share the registered Work SQLite channel
//! (`RegisteredGlobalDb::workflow_storage`). This drops the whole
//! `HostAdmissionTestRuntimeV1` — the daemon's admitted composition root, not
//! just the exact-SQL handle — and reopens it at the same profile/project
//! paths, so a real physical restart (not a logical replay) is what proves
//! durability here.

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_application::{
    TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome, TaskHandoffGrantV1, TaskHandoffScopeV1,
    WorkflowChildRecordV1, WorkflowDefinitionAuthorityPort, WorkflowExecutionAdmissionV1,
    WorkflowExecutionAuthorityPort, WorkflowExecutionFenceV1, WorkflowExecutionIdentityV1,
    WorkflowFanOutCheckpointV1,
};
use tracedecay_domain::{
    ActorId, AttemptId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId,
    UtcMicros, WorkAttemptIdentityV1, WorkFenceEpochV1, WorkLeaseFenceV1, WorkLeaseId,
    WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowOperationRef, WorkflowOutputName,
    WorkflowStepId, WorkflowStepV1, WorktreeId, canonical_sha256,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

/// A distinct, valid `sha256:`-tagged digest per input byte.
///
/// Callers pick arbitrary ASCII letters as mnemonics, but a `ManifestDigest`
/// only accepts lowercase hex (`0-9a-f`); encoding the byte's own value as
/// two hex digits keeps every mnemonic both valid and mutually distinct.
fn digest(byte: char) -> ManifestDigest {
    let hex_byte = format!("{:02x}", u32::from(byte) & 0xff);
    ManifestDigest::new(format!("sha256:{}", hex_byte.repeat(32))).unwrap()
}

fn definition() -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id("workflow.definition.daemon-restart"),
        1,
        id::<ProjectId>("project.workflow.daemon-restart"),
        vec![WorkflowStepV1 {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>("operation.prepare.v1"),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: None,
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap()
}

fn handoff_scope() -> TaskHandoffScopeV1 {
    TaskHandoffScopeV1::new(
        id::<ProjectId>("project.workflow.daemon-restart"),
        id::<RepositoryId>("repository.workflow.daemon-restart"),
        id::<WorktreeId>("worktree.workflow.daemon-restart"),
        id::<WorkflowDefinitionId>("workflow.definition.daemon-restart"),
        1,
        id::<WorkflowStepId>("prepare"),
        id::<TaskId>("task.workflow.daemon-restart.prepare"),
        id::<ThreadId>("thread.workflow.daemon-restart.prepare"),
        id::<RunId>("run.workflow.daemon-restart"),
        id::<ActorId>("actor.workflow.source"),
        id::<ActorId>("actor.workflow.target"),
    )
    .unwrap()
}

fn token_digest(secret: &str) -> ManifestDigest {
    canonical_sha256(&("tracedecay.application.task-handoff.v1", secret)).unwrap()
}

fn execution_identity() -> WorkflowExecutionIdentityV1 {
    WorkflowExecutionIdentityV1 {
        definition_id: id("workflow.definition.daemon-restart"),
        definition_version: 1,
        run_id: id::<RunId>("run.workflow.daemon-restart"),
        step_id: id::<WorkflowStepId>("prepare"),
    }
}

fn fence(epoch: u64, attempt: &str) -> WorkflowExecutionFenceV1 {
    WorkflowExecutionFenceV1 {
        attempt_id: id::<AttemptId>(attempt),
        lease: WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.workflow.daemon-restart"),
            WorkFenceEpochV1::new(epoch).unwrap(),
        )
        .unwrap(),
    }
}

/// Opens the admitted daemon composition root at `profile_root`/`project_root`.
///
/// Calling this twice at the same paths — dropping the first runtime before
/// opening the second — is the direct restart journey: the second call re-enters
/// the daemon database scope and remounts the registered store from disk with
/// no in-process state carried over.
async fn open_daemon_runtime(
    profile_root: &std::path::Path,
    project_root: &std::path::Path,
) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::project(
        profile_root,
        project_root,
        id::<ProjectId>("project.workflow.daemon-restart"),
    )
    .await
    .expect("admit daemon test runtime for workflow/handoff restart journey")
}

#[tokio::test]
async fn workflow_definition_handoff_and_execution_survive_a_daemon_restart() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();

    let definition = definition();
    let scope = handoff_scope();
    let grant = TaskHandoffGrantV1::new(
        scope.clone(),
        token_digest(&"s".repeat(48)),
        UtcMicros(10),
        UtcMicros(1_000_000),
    )
    .unwrap();
    let identity = execution_identity();
    let plan = digest('p');
    let first_fence = fence(1, "attempt.workflow.daemon-restart.1");

    // --- First admission: definitions, handoff, and fan-out execution. ---
    {
        let runtime = open_daemon_runtime(&profile_root, &project_root).await;
        let authority = runtime
            .project_workflow_storage_for_test()
            .expect("registered workflow authority must be mounted for a project runtime");

        WorkflowDefinitionAuthorityPort::insert(&authority, &definition).unwrap();
        WorkflowDefinitionAuthorityPort::compare_and_swap_activation(
            &authority,
            definition.definition_id(),
            None,
            1,
        )
        .unwrap();
        assert_eq!(
            WorkflowDefinitionAuthorityPort::active_version(&authority, definition.definition_id())
                .unwrap(),
            Some(1),
            "definition activation must be visible within the admitting runtime"
        );

        TaskHandoffAuthorityPort::issue(&authority, &grant).unwrap();

        assert_eq!(
            WorkflowExecutionAuthorityPort::begin(&authority, &identity, &first_fence, &plan)
                .unwrap(),
            WorkflowExecutionAdmissionV1::Execute
        );
        let checkpoint = WorkflowFanOutCheckpointV1 {
            plan_digest: plan.clone(),
            children: vec![WorkflowChildRecordV1 {
                task_id: id::<TaskId>("task.workflow.daemon-restart.child"),
                attempt_identity: WorkAttemptIdentityV1::new(
                    id::<TaskId>("task.workflow.daemon-restart.child"),
                    id::<RunId>("run.workflow.daemon-restart"),
                    id::<AttemptId>("attempt.workflow.daemon-restart.child"),
                )
                .unwrap(),
            }],
        };
        WorkflowExecutionAuthorityPort::checkpoint(
            &authority,
            &identity,
            &first_fence,
            &checkpoint,
        )
        .unwrap();
        // Consume the handoff token before the restart, so the restart proves
        // the single-use "consumed" fact durably survives, not just the grant.
        assert_eq!(
            TaskHandoffAuthorityPort::consume(
                &authority,
                grant.token_digest(),
                &scope,
                UtcMicros(11)
            )
            .unwrap(),
            TaskHandoffConsumeOutcome::Consumed
        );

        // The runtime — and with it the daemon database scope and every open
        // connection — is dropped at the end of this block, before the next
        // admission reopens from disk.
    }

    // --- Restart: reopen the exact same profile/project paths. ---
    let restarted = open_daemon_runtime(&profile_root, &project_root).await;
    let authority = restarted
        .project_workflow_storage_for_test()
        .expect("registered workflow authority must remount after a daemon restart");

    assert_eq!(
        WorkflowDefinitionAuthorityPort::load(&authority, definition.definition_id(), 1)
            .unwrap()
            .as_ref(),
        Some(&definition),
        "the registered definition must survive the restart byte-for-byte"
    );
    assert_eq!(
        WorkflowDefinitionAuthorityPort::active_version(&authority, definition.definition_id())
            .unwrap(),
        Some(1),
        "activation must survive the restart"
    );

    // A single-use token stays single-use across a restart: replay, not a
    // silent re-grant of Consumed's authority.
    assert_eq!(
        TaskHandoffAuthorityPort::consume(&authority, grant.token_digest(), &scope, UtcMicros(12))
            .unwrap(),
        TaskHandoffConsumeOutcome::Replay,
        "a consumed handoff token must never be redeemable again after a restart"
    );

    // A durable child intent remains recoverable after restart. Terminal truth
    // is published only by the canonical Work-backed daemon operation journey.
    let expected_checkpoint = WorkflowFanOutCheckpointV1 {
        plan_digest: plan.clone(),
        children: vec![WorkflowChildRecordV1 {
            task_id: id::<TaskId>("task.workflow.daemon-restart.child"),
            attempt_identity: WorkAttemptIdentityV1::new(
                id::<TaskId>("task.workflow.daemon-restart.child"),
                id::<RunId>("run.workflow.daemon-restart"),
                id::<AttemptId>("attempt.workflow.daemon-restart.child"),
            )
            .unwrap(),
        }],
    };
    assert_eq!(
        WorkflowExecutionAuthorityPort::begin(
            &authority,
            &identity,
            &fence(2, "attempt.workflow.daemon-restart.2"),
            &plan,
        )
        .unwrap(),
        WorkflowExecutionAdmissionV1::Recover {
            checkpoint: expected_checkpoint
        },
        "a checkpointed child intent must remain recoverable after restart"
    );
}

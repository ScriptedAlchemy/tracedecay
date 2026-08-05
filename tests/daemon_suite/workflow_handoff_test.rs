//! Workflow definition and handoff durability across a direct daemon restart.
//!
//! Definitions, activations, and single-use handoff tokens share the registered Work SQLite channel
//! (`RegisteredGlobalDb::workflow_storage`). This drops the whole
//! `HostAdmissionTestRuntimeV1` — the daemon's admitted composition root, not
//! just the exact-SQL handle — and reopens it at the same profile/project
//! paths, so a real physical restart (not a logical replay) is what proves
//! durability here.

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_application::{
    TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome, TaskHandoffGrant, TaskHandoffScope,
    WorkflowDefinitionAuthorityPort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId, UtcMicros,
    WorkflowDefinition, WorkflowDefinitionId, WorkflowOperationRef, WorkflowOutputName,
    WorkflowStep, WorkflowStepId, WorktreeId, canonical_sha256,
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

fn definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        id("workflow.definition.daemon-restart"),
        1,
        id::<ProjectId>("project.workflow.daemon-restart"),
        vec![WorkflowStep {
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

fn handoff_scope() -> TaskHandoffScope {
    TaskHandoffScope::new(
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
async fn workflow_definition_and_handoff_survive_a_daemon_restart() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join("profile");
    let project_root = tmp.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();

    let definition = definition();
    let scope = handoff_scope();
    let grant = TaskHandoffGrant::new(
        scope.clone(),
        token_digest(&"s".repeat(48)),
        UtcMicros(10),
        UtcMicros(1_000_000),
    )
    .unwrap();
    // --- First admission: definitions and handoff. ---
    {
        let runtime = open_daemon_runtime(&profile_root, &project_root).await;
        let authority = runtime
            .project_workflow_storage_for_test()
            .expect("registered workflow authority must be mounted for a project runtime");

        WorkflowDefinitionAuthorityPort::insert(&authority, &definition).unwrap();

        TaskHandoffAuthorityPort::issue(&authority, &grant).unwrap();

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
    // A single-use token stays single-use across a restart: replay, not a
    // silent re-grant of Consumed's authority.
    assert_eq!(
        TaskHandoffAuthorityPort::consume(&authority, grant.token_digest(), &scope, UtcMicros(12))
            .unwrap(),
        TaskHandoffConsumeOutcome::Replay,
        "a consumed handoff token must never be redeemable again after a restart"
    );
}

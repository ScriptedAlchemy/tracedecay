use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    WorkflowAdmissionSnapshot, WorkflowRunAppendOutcome, WorkflowRunAppendRequest,
    WorkflowRunService, WorkflowRunServiceError, WorkflowRunStorageError, WorkflowRunStoragePort,
    work_executable_catalog_digest,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    AttemptId, ManifestDigest, ProjectId, ProviderId, RunId, TaskId, UtcMicros, WorkArtifactId,
    WorkArtifactRefV1, WorkAttemptIdentityV1, WorkCommandId, WorkProviderBackendV1,
    WorkProviderRouteId, WorkProviderRouteV1, WorkflowDefinition, WorkflowDefinitionId,
    WorkflowOperationRef, WorkflowOutputArtifact, WorkflowOutputName, WorkflowOutputReference,
    WorkflowPlacementReceipt, WorkflowRunCommand, WorkflowRunEvent, WorkflowRunEventContext,
    WorkflowRunProjection, WorkflowRunStatus, WorkflowStep, WorkflowStepEffectOutcome,
    WorkflowStepEffectReceipt, WorkflowStepId, WorkflowStepOutput,
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

fn context(command: &str, input: char, occurred_at: i64) -> WorkflowRunEventContext {
    WorkflowRunEventContext {
        command_id: id::<WorkCommandId>(command),
        input_digest: digest(input),
        occurred_at: UtcMicros(occurred_at),
    }
}

fn artifact(name: &str, digest_byte: char, byte_length: u64) -> WorkArtifactRefV1 {
    WorkArtifactRefV1::new(id::<WorkArtifactId>(name), digest(digest_byte), byte_length).unwrap()
}

fn placement(run_id: &RunId, step_id: &str) -> WorkflowPlacementReceipt {
    WorkflowPlacementReceipt::new(
        run_id.clone(),
        id::<WorkflowStepId>(step_id),
        WorkProviderRouteV1::new(
            id::<ProviderId>("provider.workflow.test"),
            id::<WorkProviderRouteId>("route.workflow.test.v1"),
        )
        .unwrap(),
        WorkProviderBackendV1::CodexAppServer,
        "model.workflow.test".to_owned(),
        digest('b'),
        digest('c'),
        digest('8'),
        safe_work_topology_policy_v1().placement,
    )
    .unwrap()
}

fn definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        id::<WorkflowDefinitionId>("workflow.definition.dag"),
        1,
        id::<ProjectId>("project.workflow.dag"),
        vec![
            WorkflowStep {
                step_id: id::<WorkflowStepId>("prepare"),
                operation: id::<WorkflowOperationRef>("operation.work.start_attempt"),
                predecessors: BTreeSet::new(),
                inputs: Vec::new(),
                outputs: vec![id::<WorkflowOutputName>("context")],
                fan_out: None,
            },
            WorkflowStep {
                step_id: id::<WorkflowStepId>("review"),
                operation: id::<WorkflowOperationRef>("operation.work.start_attempt"),
                predecessors: BTreeSet::from([id::<WorkflowStepId>("prepare")]),
                inputs: vec![WorkflowOutputReference {
                    producer_step_id: id::<WorkflowStepId>("prepare"),
                    output_name: id::<WorkflowOutputName>("context"),
                }],
                outputs: vec![id::<WorkflowOutputName>("report")],
                fan_out: None,
            },
        ],
        digest('a'),
        digest('b'),
        work_executable_catalog_digest().unwrap(),
    )
    .unwrap()
}

#[derive(Clone, Default)]
struct MemoryRunStorage {
    events: Arc<Mutex<BTreeMap<RunId, Vec<WorkflowRunEvent>>>>,
}

impl WorkflowRunStoragePort for MemoryRunStorage {
    fn projection(&self, run_id: &RunId) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
        let events = self.events.lock().unwrap();
        let history = events
            .get(run_id)
            .ok_or(WorkflowRunStorageError::NotFound)?;
        WorkflowRunProjection::rebuild(history).map_err(|_| WorkflowRunStorageError::InvalidHistory)
    }

    fn append(
        &self,
        request: &WorkflowRunAppendRequest,
    ) -> Result<WorkflowRunAppendOutcome, WorkflowRunStorageError> {
        let mut events = self.events.lock().unwrap();
        let history = events.entry(request.event.run_id().clone()).or_default();
        if let Some(existing) = history
            .iter()
            .find(|event| event.command_id() == request.event.command_id())
        {
            if existing == &request.event {
                return WorkflowRunProjection::rebuild(history)
                    .map(WorkflowRunAppendOutcome::Replayed)
                    .map_err(|_| WorkflowRunStorageError::InvalidHistory);
            }
            return Err(WorkflowRunStorageError::IdempotencyConflict);
        }
        let current = history.last().map(WorkflowRunEvent::sequence);
        if current != request.expected_sequence {
            return Err(WorkflowRunStorageError::VersionConflict);
        }
        history.push(request.event.clone());
        WorkflowRunProjection::rebuild(history)
            .map(WorkflowRunAppendOutcome::Appended)
            .map_err(|_| WorkflowRunStorageError::InvalidHistory)
    }

    fn projections(&self) -> Result<Vec<WorkflowRunProjection>, WorkflowRunStorageError> {
        let events = self.events.lock().unwrap();
        events
            .values()
            .map(|history| {
                WorkflowRunProjection::rebuild(history)
                    .map_err(|_| WorkflowRunStorageError::InvalidHistory)
            })
            .collect()
    }
}

#[test]
fn admission_rejects_stale_policy_configuration_and_catalog() {
    for (snapshot, expected) in [
        (
            WorkflowAdmissionSnapshot {
                policy_digest: digest('9'),
                configuration_digest: digest('b'),
                catalog_digest: work_executable_catalog_digest().unwrap(),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            WorkflowRunServiceError::PolicyDigestMismatch,
        ),
        (
            WorkflowAdmissionSnapshot {
                policy_digest: digest('a'),
                configuration_digest: digest('9'),
                catalog_digest: work_executable_catalog_digest().unwrap(),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            WorkflowRunServiceError::ConfigurationDigestMismatch,
        ),
        (
            WorkflowAdmissionSnapshot {
                policy_digest: digest('a'),
                configuration_digest: digest('b'),
                catalog_digest: digest('9'),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            WorkflowRunServiceError::CatalogDigestMismatch,
        ),
    ] {
        let storage = MemoryRunStorage::default();
        assert_eq!(
            WorkflowRunService::new(storage.clone())
                .admit(
                    id::<RunId>("run.workflow.dag.stale"),
                    definition(),
                    snapshot,
                    context("command.workflow.dag.stale", '8', 1),
                )
                .unwrap_err(),
            expected
        );
        assert!(storage.events.lock().unwrap().is_empty());
    }
}

#[test]
fn failed_step_journals_successful_artifact_evidence() {
    let storage = MemoryRunStorage::default();
    let run_id = id::<RunId>("run.workflow.dag.partial-failure");
    let service = WorkflowRunService::new(storage.clone());
    let admitted = service
        .admit(
            run_id.clone(),
            definition(),
            WorkflowAdmissionSnapshot {
                policy_digest: digest('a'),
                configuration_digest: digest('b'),
                catalog_digest: work_executable_catalog_digest().unwrap(),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            context("command.workflow.partial.admit", '1', 1),
        )
        .unwrap();
    let started = service
        .apply(
            &run_id,
            admitted.sequence(),
            WorkflowRunCommand::StartStep {
                step_id: id::<WorkflowStepId>("prepare"),
                placement: placement(&run_id, "prepare"),
            },
            context("command.workflow.partial.start", '2', 2),
        )
        .unwrap();
    let outputs = vec![
        WorkflowStepOutput::new(
            id::<WorkflowOutputName>("context"),
            vec![WorkflowOutputArtifact::new(
                WorkAttemptIdentityV1::new(
                    id::<TaskId>("task.workflow.partial"),
                    run_id.clone(),
                    id::<AttemptId>("attempt.workflow.partial"),
                )
                .unwrap(),
                artifact("artifact.workflow.partial", 'd', 41),
            )],
        )
        .unwrap(),
    ];
    let receipt = WorkflowStepEffectReceipt::new(
        run_id.clone(),
        id::<WorkflowStepId>("prepare"),
        started
            .step(&id::<WorkflowStepId>("prepare"))
            .unwrap()
            .placement_receipt()
            .unwrap()
            .placement_digest()
            .clone(),
        WorkflowStepEffectOutcome::Failed,
        digest('9'),
        &outputs,
    )
    .unwrap();
    let failed = service
        .apply(
            &run_id,
            started.sequence(),
            WorkflowRunCommand::FailStep {
                step_id: id::<WorkflowStepId>("prepare"),
                outputs: outputs.clone(),
                effect_receipt: receipt,
            },
            context("command.workflow.partial.fail", '3', 3),
        )
        .unwrap();
    assert_eq!(
        failed
            .step(&id::<WorkflowStepId>("prepare"))
            .unwrap()
            .outputs()
            .values()
            .cloned()
            .collect::<Vec<_>>(),
        outputs
    );
    assert_eq!(failed.status(), WorkflowRunStatus::Failed);
}

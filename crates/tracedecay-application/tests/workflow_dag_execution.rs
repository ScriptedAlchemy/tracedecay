use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    WorkflowAdmissionSnapshotV1, WorkflowRunAppendOutcomeV1, WorkflowRunAppendRequestV1,
    WorkflowRunService, WorkflowRunServiceError, WorkflowRunStorageError, WorkflowRunStoragePort,
    WorkflowStepEventContextsV1, WorkflowStepExecutionError, WorkflowStepExecutionOutcomeV1,
    WorkflowStepExecutionPort, WorkflowStepExecutionRequestV1, WorkflowStepExecutionResultV1,
    WorkflowStepExecutionService, work_executable_catalog_digest,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    ManifestDigest, ProjectId, ProviderId, RunId, UtcMicros, WorkArtifactId, WorkArtifactRefV1,
    WorkCommandId, WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1,
    WorkflowDefinitionId, WorkflowDefinitionV1, WorkflowOperationRef, WorkflowOutputName,
    WorkflowOutputReferenceV1, WorkflowPlacementReceiptV1, WorkflowRunEventContextV1,
    WorkflowRunEventV1, WorkflowRunProjectionV1, WorkflowRunStatusV1, WorkflowStepEffectOutcomeV1,
    WorkflowStepEffectReceiptV1, WorkflowStepId, WorkflowStepOutputV1, WorkflowStepV1,
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

fn context(command: &str, input: char, occurred_at: i64) -> WorkflowRunEventContextV1 {
    WorkflowRunEventContextV1 {
        command_id: id::<WorkCommandId>(command),
        input_digest: digest(input),
        occurred_at: UtcMicros(occurred_at),
    }
}

fn artifact(name: &str, digest_byte: char, byte_length: u64) -> WorkArtifactRefV1 {
    WorkArtifactRefV1::new(id::<WorkArtifactId>(name), digest(digest_byte), byte_length).unwrap()
}

fn placement(run_id: &RunId, step_id: &str) -> WorkflowPlacementReceiptV1 {
    WorkflowPlacementReceiptV1::new(
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

fn definition() -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id::<WorkflowDefinitionId>("workflow.definition.dag"),
        1,
        id::<ProjectId>("project.workflow.dag"),
        vec![
            WorkflowStepV1 {
                step_id: id::<WorkflowStepId>("prepare"),
                operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
                predecessors: BTreeSet::new(),
                inputs: Vec::new(),
                outputs: vec![id::<WorkflowOutputName>("context")],
                fan_out: None,
            },
            WorkflowStepV1 {
                step_id: id::<WorkflowStepId>("review"),
                operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
                predecessors: BTreeSet::from([id::<WorkflowStepId>("prepare")]),
                inputs: vec![WorkflowOutputReferenceV1 {
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
    events: Arc<Mutex<BTreeMap<RunId, Vec<WorkflowRunEventV1>>>>,
}

impl WorkflowRunStoragePort for MemoryRunStorage {
    fn load(&self, run_id: &RunId) -> Result<Vec<WorkflowRunEventV1>, WorkflowRunStorageError> {
        self.events
            .lock()
            .unwrap()
            .get(run_id)
            .cloned()
            .ok_or(WorkflowRunStorageError::NotFound)
    }

    fn projection(
        &self,
        run_id: &RunId,
    ) -> Result<WorkflowRunProjectionV1, WorkflowRunStorageError> {
        WorkflowRunProjectionV1::rebuild(&self.load(run_id)?)
            .map_err(|_| WorkflowRunStorageError::InvalidHistory)
    }

    fn append(
        &self,
        request: &WorkflowRunAppendRequestV1,
    ) -> Result<WorkflowRunAppendOutcomeV1, WorkflowRunStorageError> {
        let mut events = self.events.lock().unwrap();
        let history = events.entry(request.event.run_id().clone()).or_default();
        if let Some(existing) = history
            .iter()
            .find(|event| event.command_id() == request.event.command_id())
        {
            if existing == &request.event {
                return WorkflowRunProjectionV1::rebuild(history)
                    .map(WorkflowRunAppendOutcomeV1::Replayed)
                    .map_err(|_| WorkflowRunStorageError::InvalidHistory);
            }
            return Err(WorkflowRunStorageError::IdempotencyConflict);
        }
        let current = history.last().map(WorkflowRunEventV1::sequence);
        if current != request.expected_sequence {
            return Err(WorkflowRunStorageError::VersionConflict);
        }
        history.push(request.event.clone());
        WorkflowRunProjectionV1::rebuild(history)
            .map(WorkflowRunAppendOutcomeV1::Appended)
            .map_err(|_| WorkflowRunStorageError::InvalidHistory)
    }
}

#[derive(Clone)]
struct RecordingExecutor {
    prepared: WorkArtifactRefV1,
    report: WorkArtifactRefV1,
    requests: Arc<Mutex<Vec<WorkflowStepExecutionRequestV1>>>,
}

impl WorkflowStepExecutionPort for RecordingExecutor {
    fn execute(
        &self,
        request: &WorkflowStepExecutionRequestV1,
    ) -> Result<WorkflowStepExecutionResultV1, WorkflowStepExecutionError> {
        self.requests.lock().unwrap().push(request.clone());
        let output = if request.step_id.as_str() == "prepare" {
            WorkflowStepOutputV1 {
                output_name: id::<WorkflowOutputName>("context"),
                artifact: self.prepared.clone(),
            }
        } else {
            WorkflowStepOutputV1 {
                output_name: id::<WorkflowOutputName>("report"),
                artifact: self.report.clone(),
            }
        };
        let outputs = vec![output];
        Ok(WorkflowStepExecutionResultV1 {
            effect_receipt: WorkflowStepEffectReceiptV1::new(
                request.run_id.clone(),
                request.step_id.clone(),
                request.placement.placement_digest().clone(),
                WorkflowStepEffectOutcomeV1::Completed,
                digest('9'),
                &outputs,
            )
            .unwrap(),
            outputs,
        })
    }
}

#[test]
fn dag_passes_exact_artifact_refs_between_steps() {
    let storage = MemoryRunStorage::default();
    let run_id = id::<RunId>("run.workflow.dag");
    let definition = definition();
    let admitted = WorkflowRunService::new(storage.clone())
        .admit(
            run_id.clone(),
            definition,
            WorkflowAdmissionSnapshotV1 {
                policy_digest: digest('a'),
                configuration_digest: digest('b'),
                catalog_digest: work_executable_catalog_digest().unwrap(),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            context("command.workflow.dag.admit", '1', 1),
        )
        .unwrap();
    assert_eq!(
        admitted.ready_steps(),
        vec![id::<WorkflowStepId>("prepare")]
    );

    let prepared = artifact("artifact.workflow.dag.context", 'd', 41);
    let report = artifact("artifact.workflow.dag.report", 'e', 23);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = RecordingExecutor {
        prepared: prepared.clone(),
        report,
        requests: Arc::clone(&requests),
    };
    let execution = WorkflowStepExecutionService::new(storage, executor);
    let prepared_state = execution
        .execute_ready_step(
            &run_id,
            1,
            &id::<WorkflowStepId>("prepare"),
            placement(&run_id, "prepare"),
            WorkflowStepEventContextsV1 {
                started: context("command.workflow.dag.prepare.start", '2', 2),
                completed: context("command.workflow.dag.prepare.complete", '3', 3),
                failed: context("command.workflow.dag.prepare.fail", '4', 3),
            },
        )
        .unwrap();
    assert!(matches!(
        prepared_state,
        WorkflowStepExecutionOutcomeV1::Succeeded(_)
    ));
    let final_state = execution
        .execute_ready_step(
            &run_id,
            3,
            &id::<WorkflowStepId>("review"),
            placement(&run_id, "review"),
            WorkflowStepEventContextsV1 {
                started: context("command.workflow.dag.review.start", '5', 4),
                completed: context("command.workflow.dag.review.complete", '6', 5),
                failed: context("command.workflow.dag.review.fail", '7', 5),
            },
        )
        .unwrap();
    let WorkflowStepExecutionOutcomeV1::Succeeded(final_state) = final_state else {
        panic!("review step failed");
    };
    assert_eq!(final_state.status(), WorkflowRunStatusV1::Completed);
    let requests = requests.lock().unwrap();
    assert!(requests[0].inputs.is_empty());
    assert_eq!(requests[1].inputs, vec![prepared]);
}

#[test]
fn admission_rejects_stale_policy_configuration_and_catalog() {
    for (snapshot, expected) in [
        (
            WorkflowAdmissionSnapshotV1 {
                policy_digest: digest('9'),
                configuration_digest: digest('b'),
                catalog_digest: work_executable_catalog_digest().unwrap(),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            WorkflowRunServiceError::PolicyDigestMismatch,
        ),
        (
            WorkflowAdmissionSnapshotV1 {
                policy_digest: digest('a'),
                configuration_digest: digest('9'),
                catalog_digest: work_executable_catalog_digest().unwrap(),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            WorkflowRunServiceError::ConfigurationDigestMismatch,
        ),
        (
            WorkflowAdmissionSnapshotV1 {
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

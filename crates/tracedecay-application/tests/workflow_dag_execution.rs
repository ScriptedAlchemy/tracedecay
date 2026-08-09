use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    WorkflowAdmissionSnapshot, WorkflowArtifactPayload, WorkflowArtifactPersistOutcome,
    WorkflowArtifactStoreError, WorkflowArtifactStorePort, WorkflowHydratedInput,
    WorkflowRunAppendOutcome, WorkflowRunAppendRequest, WorkflowRunService,
    WorkflowRunServiceError, WorkflowRunStorageError, WorkflowRunStoragePort,
    WorkflowStepEventContexts, WorkflowStepExecutionError, WorkflowStepExecutionOutcome,
    WorkflowStepExecutionPort, WorkflowStepExecutionRequest, WorkflowStepExecutionResult,
    WorkflowStepExecutionService, WorkflowStepExecutionServiceError,
    work_executable_catalog_digest, workflow_artifact_payload_digest,
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

/// An artifact whose reference digest is true for `content`, plus its payload.
fn content_artifact(name: &str, content: &[u8]) -> WorkflowArtifactPayload {
    let reference = WorkArtifactRefV1::new(
        id::<WorkArtifactId>(name),
        workflow_artifact_payload_digest(content).unwrap(),
        content.len() as u64,
    )
    .unwrap();
    WorkflowArtifactPayload::new(reference, content.to_vec()).unwrap()
}

#[derive(Clone, Default)]
struct MemoryArtifactStore {
    payloads: Arc<Mutex<BTreeMap<String, WorkflowArtifactPayload>>>,
}

impl WorkflowArtifactStorePort for MemoryArtifactStore {
    fn persist(
        &self,
        payload: &WorkflowArtifactPayload,
    ) -> Result<WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError> {
        let mut payloads = self.payloads.lock().unwrap();
        let key = payload.artifact().digest().as_str().to_owned();
        if let Some(existing) = payloads.get(&key) {
            if existing == payload {
                return Ok(WorkflowArtifactPersistOutcome::Replayed);
            }
            return Err(WorkflowArtifactStoreError::PayloadConflict);
        }
        payloads.insert(key, payload.clone());
        Ok(WorkflowArtifactPersistOutcome::Persisted)
    }

    fn load(
        &self,
        artifact: &WorkArtifactRefV1,
    ) -> Result<WorkflowArtifactPayload, WorkflowArtifactStoreError> {
        let payloads = self.payloads.lock().unwrap();
        let stored = payloads
            .get(artifact.digest().as_str())
            .ok_or(WorkflowArtifactStoreError::Missing)?;
        WorkflowArtifactPayload::new(artifact.clone(), stored.bytes().to_vec())
    }
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
                operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
                predecessors: BTreeSet::new(),
                inputs: Vec::new(),
                outputs: vec![id::<WorkflowOutputName>("context")],
                fan_out: None,
            },
            WorkflowStep {
                step_id: id::<WorkflowStepId>("review"),
                operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
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

type RecordedStepRequests =
    Arc<Mutex<Vec<(WorkflowStepExecutionRequest, Vec<WorkflowHydratedInput>)>>>;

#[derive(Clone)]
struct RecordingExecutor {
    prepared: WorkflowArtifactPayload,
    report: WorkflowArtifactPayload,
    requests: RecordedStepRequests,
}

struct MalformedExecutor;

impl WorkflowStepExecutionPort for MalformedExecutor {
    fn execute(
        &self,
        request: &WorkflowStepExecutionRequest,
        _hydrated: &[WorkflowHydratedInput],
    ) -> Result<WorkflowStepExecutionResult, WorkflowStepExecutionError> {
        Ok(WorkflowStepExecutionResult {
            outputs: Vec::new(),
            effect_receipt: WorkflowStepEffectReceipt::new(
                request.run_id.clone(),
                request.step_id.clone(),
                request.placement.placement_digest().clone(),
                WorkflowStepEffectOutcome::Completed,
                digest('9'),
                &[],
            )
            .unwrap(),
            artifact_payloads: Vec::new(),
            synthesis: None,
        })
    }
}

/// Declares an output artifact but never supplies its payload bytes, so the
/// runtime must refuse to journal the completion.
struct PayloadWithholdingExecutor {
    prepared: WorkflowArtifactPayload,
}

impl WorkflowStepExecutionPort for PayloadWithholdingExecutor {
    fn execute(
        &self,
        request: &WorkflowStepExecutionRequest,
        _hydrated: &[WorkflowHydratedInput],
    ) -> Result<WorkflowStepExecutionResult, WorkflowStepExecutionError> {
        let outputs = vec![step_output(request, "context", &self.prepared)];
        Ok(WorkflowStepExecutionResult {
            effect_receipt: WorkflowStepEffectReceipt::new(
                request.run_id.clone(),
                request.step_id.clone(),
                request.placement.placement_digest().clone(),
                WorkflowStepEffectOutcome::Completed,
                digest('9'),
                &outputs,
            )
            .unwrap(),
            outputs,
            artifact_payloads: Vec::new(),
            synthesis: None,
        })
    }
}

fn step_output(
    request: &WorkflowStepExecutionRequest,
    output_name: &str,
    payload: &WorkflowArtifactPayload,
) -> WorkflowStepOutput {
    WorkflowStepOutput::new(
        id::<WorkflowOutputName>(output_name),
        vec![WorkflowOutputArtifact::new(
            WorkAttemptIdentityV1::new(
                id::<TaskId>(&format!("task.workflow.{}", request.step_id.as_str())),
                request.run_id.clone(),
                id::<AttemptId>(&format!("attempt.workflow.{}", request.step_id.as_str())),
            )
            .unwrap(),
            payload.artifact().clone(),
        )],
    )
    .unwrap()
}

impl WorkflowStepExecutionPort for RecordingExecutor {
    fn execute(
        &self,
        request: &WorkflowStepExecutionRequest,
        hydrated: &[WorkflowHydratedInput],
    ) -> Result<WorkflowStepExecutionResult, WorkflowStepExecutionError> {
        self.requests
            .lock()
            .unwrap()
            .push((request.clone(), hydrated.to_vec()));
        let (output_name, payload) = if request.step_id.as_str() == "prepare" {
            ("context", self.prepared.clone())
        } else {
            ("report", self.report.clone())
        };
        let outputs = vec![step_output(request, output_name, &payload)];
        Ok(WorkflowStepExecutionResult {
            effect_receipt: WorkflowStepEffectReceipt::new(
                request.run_id.clone(),
                request.step_id.clone(),
                request.placement.placement_digest().clone(),
                WorkflowStepEffectOutcome::Completed,
                digest('9'),
                &outputs,
            )
            .unwrap(),
            outputs,
            artifact_payloads: vec![payload],
            synthesis: None,
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
            WorkflowAdmissionSnapshot {
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

    let prepared = content_artifact("artifact.workflow.dag.context", b"prepared context bytes");
    let report = content_artifact("artifact.workflow.dag.report", b"review report bytes");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let executor = RecordingExecutor {
        prepared: prepared.clone(),
        report,
        requests: Arc::clone(&requests),
    };
    let artifacts = MemoryArtifactStore::default();
    let execution = WorkflowStepExecutionService::new(storage, executor, artifacts.clone());
    let prepared_state = execution
        .execute_ready_step(
            &run_id,
            1,
            &id::<WorkflowStepId>("prepare"),
            placement(&run_id, "prepare"),
            WorkflowStepEventContexts {
                started: context("command.workflow.dag.prepare.start", '2', 2),
                completed: context("command.workflow.dag.prepare.complete", '3', 3),
                failed: context("command.workflow.dag.prepare.fail", '4', 3),
            },
        )
        .unwrap();
    assert!(matches!(
        prepared_state,
        WorkflowStepExecutionOutcome::Succeeded(_)
    ));
    let final_state = execution
        .execute_ready_step(
            &run_id,
            3,
            &id::<WorkflowStepId>("review"),
            placement(&run_id, "review"),
            WorkflowStepEventContexts {
                started: context("command.workflow.dag.review.start", '5', 4),
                completed: context("command.workflow.dag.review.complete", '6', 5),
                failed: context("command.workflow.dag.review.fail", '7', 5),
            },
        )
        .unwrap();
    let WorkflowStepExecutionOutcome::Succeeded(final_state) = final_state else {
        panic!("review step failed");
    };
    assert_eq!(final_state.status(), WorkflowRunStatus::Completed);
    let requests = requests.lock().unwrap();
    assert!(requests[0].0.inputs.is_empty());
    assert!(requests[0].1.is_empty());
    assert_eq!(requests[1].0.inputs.len(), 1);
    assert_eq!(
        requests[1].0.inputs[0].artifacts()[0].artifact(),
        prepared.artifact()
    );
    // The review step saw the exact bytes the prepare step persisted, not a
    // reference it had to trust.
    assert_eq!(requests[1].1.len(), 1);
    assert_eq!(requests[1].1[0].payloads, vec![prepared.clone()]);
    // Both declared payloads are durably retained for later hydration.
    assert!(artifacts.load(prepared.artifact()).is_ok());
}

#[test]
fn review_step_refuses_before_start_when_input_payload_is_missing() {
    let storage = MemoryRunStorage::default();
    let run_id = id::<RunId>("run.workflow.dag.unhydratable");
    let admitted = WorkflowRunService::new(storage.clone())
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
            context("command.workflow.unhydratable.admit", '1', 1),
        )
        .unwrap();
    let prepared = content_artifact("artifact.workflow.dag.context", b"prepared context bytes");
    let report = content_artifact("artifact.workflow.dag.report", b"review report bytes");
    let requests = Arc::new(Mutex::new(Vec::new()));
    // The prepare step persists through THIS store...
    let prepare_store = MemoryArtifactStore::default();
    let execution = WorkflowStepExecutionService::new(
        storage.clone(),
        RecordingExecutor {
            prepared: prepared.clone(),
            report: report.clone(),
            requests: Arc::clone(&requests),
        },
        prepare_store,
    );
    execution
        .execute_ready_step(
            &run_id,
            admitted.sequence(),
            &id::<WorkflowStepId>("prepare"),
            placement(&run_id, "prepare"),
            WorkflowStepEventContexts {
                started: context("command.workflow.unhydratable.start", '2', 2),
                completed: context("command.workflow.unhydratable.complete", '3', 3),
                failed: context("command.workflow.unhydratable.fail", '4', 3),
            },
        )
        .unwrap();
    // ...while the review step hydrates from an EMPTY store: a typed refusal
    // before any run state changes, not a started-then-failed step.
    let review = WorkflowStepExecutionService::new(
        storage.clone(),
        RecordingExecutor {
            prepared,
            report,
            requests: Arc::clone(&requests),
        },
        MemoryArtifactStore::default(),
    );
    let sequence = WorkflowRunStoragePort::projection(&storage, &run_id)
        .unwrap()
        .sequence();
    let error = review
        .execute_ready_step(
            &run_id,
            sequence,
            &id::<WorkflowStepId>("review"),
            placement(&run_id, "review"),
            WorkflowStepEventContexts {
                started: context("command.workflow.unhydratable.review.start", '5', 4),
                completed: context("command.workflow.unhydratable.review.complete", '6', 5),
                failed: context("command.workflow.unhydratable.review.fail", '7', 5),
            },
        )
        .unwrap_err();
    assert_eq!(
        error,
        WorkflowStepExecutionServiceError::InputArtifacts(WorkflowArtifactStoreError::Missing)
    );
    let after = WorkflowRunStoragePort::projection(&storage, &run_id).unwrap();
    assert_eq!(
        after.sequence(),
        sequence,
        "refusal must not journal events"
    );
    assert_eq!(after.status(), WorkflowRunStatus::Running);
}

#[test]
fn withheld_output_payloads_fail_the_step_instead_of_completing_it() {
    let storage = MemoryRunStorage::default();
    let run_id = id::<RunId>("run.workflow.dag.withheld");
    let admitted = WorkflowRunService::new(storage.clone())
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
            context("command.workflow.withheld.admit", '1', 1),
        )
        .unwrap();
    let artifacts = MemoryArtifactStore::default();
    let prepared = content_artifact("artifact.workflow.dag.context", b"prepared context bytes");
    let outcome = WorkflowStepExecutionService::new(
        storage.clone(),
        PayloadWithholdingExecutor { prepared },
        artifacts.clone(),
    )
    .execute_ready_step(
        &run_id,
        admitted.sequence(),
        &id::<WorkflowStepId>("prepare"),
        placement(&run_id, "prepare"),
        WorkflowStepEventContexts {
            started: context("command.workflow.withheld.start", '2', 2),
            completed: context("command.workflow.withheld.complete", '3', 3),
            failed: context("command.workflow.withheld.fail", '4', 3),
        },
    )
    .unwrap();
    let WorkflowStepExecutionOutcome::Failed(projection) = outcome else {
        panic!("a completion whose artifacts were never supplied must fail the step");
    };
    assert_eq!(projection.status(), WorkflowRunStatus::Failed);
    assert!(artifacts.payloads.lock().unwrap().is_empty());
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

fn fan_out_definition() -> WorkflowDefinition {
    WorkflowDefinition::new(
        id::<WorkflowDefinitionId>("workflow.definition.fanout"),
        1,
        id::<ProjectId>("project.workflow.fanout"),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("explore"),
            operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("candidates")],
            fan_out: Some(tracedecay_domain::WorkflowFanOut { max_width: 3 }),
        }],
        digest('a'),
        digest('b'),
        work_executable_catalog_digest().unwrap(),
    )
    .unwrap()
}

fn fan_out_attempt(run_id: &RunId, ordinal: usize) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>("task.workflow.fanout"),
        run_id.clone(),
        id::<AttemptId>(&format!("attempt.workflow.fanout.{ordinal}")),
    )
    .unwrap()
}

/// Runs a three-way fan-out where the third attempt claims to synthesize the
/// first two, citing whichever source digests the test chooses.
struct SynthesizingExecutor {
    sources: Vec<WorkflowArtifactPayload>,
    synthesis: WorkflowArtifactPayload,
    cited: BTreeSet<ManifestDigest>,
    claim: bool,
}

impl WorkflowStepExecutionPort for SynthesizingExecutor {
    fn execute(
        &self,
        request: &WorkflowStepExecutionRequest,
        _hydrated: &[WorkflowHydratedInput],
    ) -> Result<WorkflowStepExecutionResult, WorkflowStepExecutionError> {
        let mut artifacts = self
            .sources
            .iter()
            .enumerate()
            .map(|(ordinal, payload)| {
                WorkflowOutputArtifact::new(
                    fan_out_attempt(&request.run_id, ordinal),
                    payload.artifact().clone(),
                )
            })
            .collect::<Vec<_>>();
        let synthesis_attempt = fan_out_attempt(&request.run_id, self.sources.len());
        artifacts.push(WorkflowOutputArtifact::new(
            synthesis_attempt.clone(),
            self.synthesis.artifact().clone(),
        ));
        let outputs = vec![
            WorkflowStepOutput::new(id::<WorkflowOutputName>("candidates"), artifacts).unwrap(),
        ];
        let mut artifact_payloads = self.sources.clone();
        artifact_payloads.push(self.synthesis.clone());
        Ok(WorkflowStepExecutionResult {
            effect_receipt: WorkflowStepEffectReceipt::new(
                request.run_id.clone(),
                request.step_id.clone(),
                request.placement.placement_digest().clone(),
                WorkflowStepEffectOutcome::Completed,
                digest('9'),
                &outputs,
            )
            .unwrap(),
            outputs,
            artifact_payloads,
            synthesis: self
                .claim
                .then(|| tracedecay_application::WorkflowSynthesisDraft {
                    output_name: id::<WorkflowOutputName>("candidates"),
                    synthesis_attempt,
                    cited_source_digests: self.cited.clone(),
                }),
        })
    }
}

fn run_fan_out(
    run_marker: &str,
    executor: SynthesizingExecutor,
) -> (MemoryRunStorage, RunId, WorkflowStepExecutionOutcome) {
    let storage = MemoryRunStorage::default();
    let run_id = id::<RunId>(&format!("run.workflow.fanout.{run_marker}"));
    let admitted = WorkflowRunService::new(storage.clone())
        .admit(
            run_id.clone(),
            fan_out_definition(),
            WorkflowAdmissionSnapshot {
                policy_digest: digest('a'),
                configuration_digest: digest('b'),
                catalog_digest: work_executable_catalog_digest().unwrap(),
                topology_digest: digest('c'),
                provider_registry_digest: digest('8'),
            },
            context("command.workflow.fanout.admit", '1', 1),
        )
        .unwrap();
    let outcome = WorkflowStepExecutionService::new(
        storage.clone(),
        executor,
        MemoryArtifactStore::default(),
    )
    .execute_ready_step(
        &run_id,
        admitted.sequence(),
        &id::<WorkflowStepId>("explore"),
        placement(&run_id, "explore"),
        WorkflowStepEventContexts {
            started: context("command.workflow.fanout.start", '2', 2),
            completed: context("command.workflow.fanout.complete", '3', 3),
            failed: context("command.workflow.fanout.fail", '4', 3),
        },
    )
    .unwrap();
    (storage, run_id, outcome)
}

fn fan_out_payloads() -> (Vec<WorkflowArtifactPayload>, WorkflowArtifactPayload) {
    (
        vec![
            content_artifact("artifact.workflow.fanout.a", b"candidate a"),
            content_artifact("artifact.workflow.fanout.b", b"candidate b"),
        ],
        content_artifact("artifact.workflow.fanout.synthesis", b"synthesis of a+b"),
    )
}

#[test]
fn synthesis_citing_every_source_completes_with_evidence_preserved() {
    let (sources, synthesis) = fan_out_payloads();
    let cited = sources
        .iter()
        .map(|payload| payload.artifact().digest().clone())
        .collect::<BTreeSet<_>>();
    let (_, _, outcome) = run_fan_out(
        "cited",
        SynthesizingExecutor {
            sources: sources.clone(),
            synthesis: synthesis.clone(),
            cited,
            claim: true,
        },
    );
    let WorkflowStepExecutionOutcome::Succeeded(projection) = outcome else {
        panic!("fully cited synthesis must complete the fan-out step");
    };
    assert_eq!(projection.status(), WorkflowRunStatus::Completed);
    // Every source artifact remains journaled alongside the synthesis: the
    // settlement admitted another artifact, it did not rewrite evidence.
    let journaled = projection
        .step(&id::<WorkflowStepId>("explore"))
        .unwrap()
        .outputs()
        .values()
        .flat_map(WorkflowStepOutput::artifacts)
        .map(|artifact| artifact.artifact().digest().clone())
        .collect::<BTreeSet<_>>();
    for payload in sources.iter().chain(std::iter::once(&synthesis)) {
        assert!(journaled.contains(payload.artifact().digest()));
    }
}

#[test]
fn synthesis_with_incomplete_citations_fails_the_step_typed() {
    let (sources, synthesis) = fan_out_payloads();
    // Cite only the first source: minority evidence would be erased silently
    // if this completed.
    let cited = BTreeSet::from([sources[0].artifact().digest().clone()]);
    let (storage, run_id, outcome) = run_fan_out(
        "uncited",
        SynthesizingExecutor {
            sources,
            synthesis,
            cited,
            claim: true,
        },
    );
    assert!(matches!(outcome, WorkflowStepExecutionOutcome::Failed(_)));
    let projection = WorkflowRunStoragePort::projection(&storage, &run_id).unwrap();
    assert_eq!(projection.status(), WorkflowRunStatus::Failed);
}

#[test]
fn declined_synthesis_returns_the_unsynthesized_evidence_set() {
    let (sources, synthesis) = fan_out_payloads();
    let (_, _, outcome) = run_fan_out(
        "declined",
        SynthesizingExecutor {
            sources: sources.clone(),
            synthesis,
            cited: BTreeSet::new(),
            claim: false,
        },
    );
    let WorkflowStepExecutionOutcome::Succeeded(projection) = outcome else {
        panic!("declined synthesis must still return the evidence set");
    };
    let journaled = projection
        .step(&id::<WorkflowStepId>("explore"))
        .unwrap()
        .outputs()
        .values()
        .flat_map(WorkflowStepOutput::artifacts)
        .map(|artifact| artifact.artifact().digest().clone())
        .collect::<BTreeSet<_>>();
    for payload in &sources {
        assert!(journaled.contains(payload.artifact().digest()));
    }
}

#[test]
fn malformed_provider_outputs_restart_as_failed_not_running() {
    let storage = MemoryRunStorage::default();
    let run_id = id::<RunId>("run.workflow.dag.malformed");
    let admitted = WorkflowRunService::new(storage.clone())
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
            context("command.workflow.malformed.admit", '1', 1),
        )
        .unwrap();
    let outcome = WorkflowStepExecutionService::new(
        storage.clone(),
        MalformedExecutor,
        MemoryArtifactStore::default(),
    )
    .execute_ready_step(
        &run_id,
        admitted.sequence(),
        &id::<WorkflowStepId>("prepare"),
        placement(&run_id, "prepare"),
        WorkflowStepEventContexts {
            started: context("command.workflow.malformed.start", '2', 2),
            completed: context("command.workflow.malformed.complete", '3', 3),
            failed: context("command.workflow.malformed.fail", '4', 3),
        },
    )
    .unwrap();
    assert!(matches!(outcome, WorkflowStepExecutionOutcome::Failed(_)));

    let restarted = WorkflowRunStoragePort::projection(&storage, &run_id).unwrap();
    assert_eq!(restarted.status(), WorkflowRunStatus::Failed);
}

//! Single-flight activation and serving publication for ignored dependencies.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use tracedecay_code_index::production::{CodeIndexExecutionControlV1, CodeIndexProductionErrorV1};
use tracedecay_domain::canonical_sha256;

use super::{CodeIndexSchedulerRegistryV1, PendingWakeV1};
use crate::daemon::code_index_scheduler::graph_activation::CodeGraphActivationAuthorityV1;
use crate::daemon::code_index_scheduler::{
    CodeGraphReplayBindingV1, CodeIndexCadenceTriggerV1, CodeIndexIgnoredDependencyIndexOutcomeV1,
    CodeIndexIgnoredDependencyRefusalV1, CodeIndexIgnoredDependencyRequestV1,
    CodeIndexSchedulerErrorV1, DaemonCodeIndexControlV1, LatestCompleteCodeIndexV1, PendingHintsV1,
    ReconcilePassGuard,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AdmissionFlightKeyV1 {
    scope_digest: String,
    expected_generation: String,
    imports_digest: String,
}

impl AdmissionFlightKeyV1 {
    fn for_request(
        request: &CodeIndexIgnoredDependencyRequestV1,
    ) -> Result<Self, CodeIndexSchedulerErrorV1> {
        let imports_digest = canonical_sha256(&request.verified_imports)
            .map_err(|error| CodeIndexSchedulerErrorV1::Identity(error.to_string()))?;
        Ok(Self {
            scope_digest: request.scope.scope_digest.as_str().to_owned(),
            expected_generation: request.expected_generation.as_str().to_owned(),
            imports_digest: imports_digest.as_str().to_owned(),
        })
    }
}

enum AdmissionFlightCompletionV1 {
    Admitted(CodeIndexIgnoredDependencyIndexOutcomeV1),
    Failed(CodeIndexSchedulerErrorV1),
}

impl Clone for AdmissionFlightCompletionV1 {
    fn clone(&self) -> Self {
        match self {
            Self::Admitted(outcome) => Self::Admitted(outcome.clone()),
            Self::Failed(error) => Self::Failed(clone_scheduler_error(error)),
        }
    }
}

impl AdmissionFlightCompletionV1 {
    fn from_result(
        result: &Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1>,
    ) -> Self {
        match result {
            Ok(outcome) => Self::Admitted(outcome.clone()),
            Err(error) => Self::Failed(clone_scheduler_error(error)),
        }
    }

    fn into_result(
        self,
    ) -> Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1> {
        match self {
            Self::Admitted(outcome) => Ok(outcome),
            Self::Failed(error) => Err(error),
        }
    }
}

fn clone_scheduler_error(error: &CodeIndexSchedulerErrorV1) -> CodeIndexSchedulerErrorV1 {
    match error {
        CodeIndexSchedulerErrorV1::Git(error) => CodeIndexSchedulerErrorV1::Git(error.clone()),
        CodeIndexSchedulerErrorV1::Io(error) => {
            CodeIndexSchedulerErrorV1::Io(std::io::Error::new(error.kind(), error.to_string()))
        }
        CodeIndexSchedulerErrorV1::Identity(error) => {
            CodeIndexSchedulerErrorV1::Identity(error.clone())
        }
        CodeIndexSchedulerErrorV1::Production(error) => {
            CodeIndexSchedulerErrorV1::Production(clone_production_error(error))
        }
        CodeIndexSchedulerErrorV1::ProductionOpen(error) => {
            CodeIndexSchedulerErrorV1::ProductionOpen(error.clone())
        }
        CodeIndexSchedulerErrorV1::Privacy(error) => {
            CodeIndexSchedulerErrorV1::Privacy(error.clone())
        }
        CodeIndexSchedulerErrorV1::GraphProjection(error) => {
            CodeIndexSchedulerErrorV1::GraphProjection(error.clone())
        }
        CodeIndexSchedulerErrorV1::GraphActivation(error) => {
            CodeIndexSchedulerErrorV1::GraphActivation(error.clone())
        }
        CodeIndexSchedulerErrorV1::SemanticSchedule(error) => {
            CodeIndexSchedulerErrorV1::SemanticSchedule(error.clone())
        }
        CodeIndexSchedulerErrorV1::PublicationConflict(error) => {
            CodeIndexSchedulerErrorV1::PublicationConflict(error.clone())
        }
        CodeIndexSchedulerErrorV1::IgnoredDependency(error) => {
            CodeIndexSchedulerErrorV1::IgnoredDependency(error.clone())
        }
    }
}

fn clone_production_error(error: &CodeIndexProductionErrorV1) -> CodeIndexProductionErrorV1 {
    match error {
        CodeIndexProductionErrorV1::Interrupted(error) => {
            CodeIndexProductionErrorV1::Interrupted(*error)
        }
        CodeIndexProductionErrorV1::Input(error) => {
            CodeIndexProductionErrorV1::Input(error.clone())
        }
        CodeIndexProductionErrorV1::Intake(error) => CodeIndexProductionErrorV1::Intake(*error),
        CodeIndexProductionErrorV1::Generation(error) => {
            CodeIndexProductionErrorV1::Generation(error.clone())
        }
        CodeIndexProductionErrorV1::Extraction(error) => {
            CodeIndexProductionErrorV1::Extraction(error.clone())
        }
        CodeIndexProductionErrorV1::RetainedParse(error) => {
            CodeIndexProductionErrorV1::RetainedParse(error.clone())
        }
        CodeIndexProductionErrorV1::Chunk(error) => {
            CodeIndexProductionErrorV1::Chunk(error.clone())
        }
        CodeIndexProductionErrorV1::Increment(error) => {
            CodeIndexProductionErrorV1::Increment(error.clone())
        }
        CodeIndexProductionErrorV1::Lineage(error) => {
            CodeIndexProductionErrorV1::Lineage(error.clone())
        }
        CodeIndexProductionErrorV1::Capability(error) => {
            CodeIndexProductionErrorV1::Capability(error.clone())
        }
        CodeIndexProductionErrorV1::Projection(error) => {
            CodeIndexProductionErrorV1::Projection(error.clone())
        }
        CodeIndexProductionErrorV1::Publication(error) => {
            CodeIndexProductionErrorV1::Publication(error.clone())
        }
        CodeIndexProductionErrorV1::Contract(error) => {
            CodeIndexProductionErrorV1::Contract(error.clone())
        }
    }
}

pub(super) struct AdmissionFlightV1 {
    completion: Mutex<Option<AdmissionFlightCompletionV1>>,
    completed: tokio::sync::Notify,
}

impl AdmissionFlightV1 {
    fn new() -> Self {
        Self {
            completion: Mutex::new(None),
            completed: tokio::sync::Notify::new(),
        }
    }

    fn completion(&self) -> Option<AdmissionFlightCompletionV1> {
        self.completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn finish(
        &self,
        result: &Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1>,
    ) {
        *self
            .completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(AdmissionFlightCompletionV1::from_result(result));
        self.completed.notify_waiters();
    }
}

struct AdmissionFlightOwnerV1 {
    key: AdmissionFlightKeyV1,
    flight: Arc<AdmissionFlightV1>,
    flights: Arc<Mutex<BTreeMap<AdmissionFlightKeyV1, Arc<AdmissionFlightV1>>>>,
    bridge: Arc<AdmissionControlBridgeV1>,
    hints: Arc<Mutex<PendingHintsV1>>,
    wake: Arc<tokio::sync::Notify>,
    epoch: Arc<AtomicU64>,
    pending_wake: Arc<PendingWakeV1>,
    finished: bool,
}

impl AdmissionFlightOwnerV1 {
    fn finish(
        mut self,
        result: Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1>,
    ) -> Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1> {
        self.flight.finish(&result);
        self.remove_flight();
        self.finished = true;
        result
    }

    fn remove_flight(&self) {
        let mut flights = self
            .flights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if flights
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.flight))
        {
            flights.remove(&self.key);
        }
    }

    fn request_authoritative_reconcile(&self) {
        self.hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .overflow();
        DaemonCodeIndexControlV1::advance(&self.epoch);
        CodeIndexSchedulerRegistryV1::note_wake(
            &self.pending_wake,
            &self.wake,
            CodeIndexCadenceTriggerV1::QueryAdmission,
        );
    }
}

impl Drop for AdmissionFlightOwnerV1 {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.bridge.cancel();
        let cancellation = Err(CodeIndexIgnoredDependencyRefusalV1::Cancelled.into());
        self.flight.finish(&cancellation);
        self.remove_flight();
        self.request_authoritative_reconcile();
    }
}

struct AdmissionControlBridgeV1 {
    cancelled: AtomicBool,
    deadline_exceeded: AtomicBool,
}

impl AdmissionControlBridgeV1 {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            deadline_exceeded: AtomicBool::new(false),
        }
    }

    fn refresh(
        &self,
        control: &(dyn CodeIndexExecutionControlV1 + Send + Sync),
        shutting_down: &AtomicBool,
    ) {
        if shutting_down.load(Ordering::Acquire) || control.is_cancelled() {
            self.cancelled.store(true, Ordering::Release);
        }
        if control.is_deadline_exceeded() {
            self.deadline_exceeded.store(true, Ordering::Release);
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl CodeIndexExecutionControlV1 for AdmissionControlBridgeV1 {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn is_deadline_exceeded(&self) -> bool {
        self.deadline_exceeded.load(Ordering::Acquire)
    }
}

impl CodeIndexSchedulerRegistryV1 {
    pub(in crate::daemon) async fn index_verified_ignored_dependency(
        &self,
        project_root: &Path,
        request: CodeIndexIgnoredDependencyRequestV1,
        control: Arc<dyn CodeIndexExecutionControlV1 + Send + Sync + '_>,
    ) -> Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1> {
        let project_root = project_root.canonicalize()?;
        let flight_key = AdmissionFlightKeyV1::for_request(&request)?;
        let (
            repository_id,
            worktree_id,
            serving_generation,
            flights,
            hints,
            wake,
            epoch,
            pending_wake,
        ) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&project_root).ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity(
                    "ignored-dependency admission requires a mounted worktree".to_owned(),
                )
            })?;
            if request.scope.validate().is_err()
                || &request.scope.repository_id != &worktree.repository_id
                || &request.scope.worktree_id != &worktree.worktree_id
            {
                return Err(CodeIndexIgnoredDependencyRefusalV1::ScopeMismatch.into());
            }
            (
                worktree.repository_id.clone(),
                worktree.worktree_id.clone(),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.ignored_dependency_admissions),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake),
            )
        };
        let (flight, owns_flight) = {
            let mut active = flights
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = active.get(&flight_key) {
                (Arc::clone(existing), false)
            } else {
                validate_serving_request(
                    &request,
                    &repository_id,
                    &worktree_id,
                    &serving_generation,
                )?;
                let flight = Arc::new(AdmissionFlightV1::new());
                active.insert(flight_key.clone(), Arc::clone(&flight));
                (flight, true)
            }
        };

        if !owns_flight {
            return await_flight(flight, control.as_ref()).await;
        }
        let bridge = Arc::new(AdmissionControlBridgeV1::new());
        let owner = AdmissionFlightOwnerV1 {
            key: flight_key,
            flight,
            flights,
            bridge: Arc::clone(&bridge),
            hints,
            wake,
            epoch,
            pending_wake,
            finished: false,
        };
        let result = self
            .run_ignored_dependency_admission(&project_root, request, control.as_ref(), &bridge)
            .await;
        owner.finish(result)
    }

    async fn run_ignored_dependency_admission(
        &self,
        project_root: &Path,
        request: CodeIndexIgnoredDependencyRequestV1,
        control: &(dyn CodeIndexExecutionControlV1 + Send + Sync),
        bridge: &Arc<AdmissionControlBridgeV1>,
    ) -> Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1> {
        let (
            repository_id,
            worktree_id,
            scheduler,
            serving_generation,
            serving_generation_epoch,
            graph_activation,
            publication_gate,
            shutting_down,
            wake,
            pending_wake,
            reconcile_in_progress,
            background_reconcile_admission,
        ) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(project_root).ok_or_else(|| {
                CodeIndexSchedulerErrorV1::Identity(
                    "ignored-dependency admission lost its mounted worktree".to_owned(),
                )
            })?;
            (
                worktree.repository_id.clone(),
                worktree.worktree_id.clone(),
                Arc::clone(&worktree.scheduler),
                Arc::clone(&worktree.serving_generation),
                Arc::clone(&worktree.serving_generation_epoch),
                worktree.graph_activation.clone(),
                Arc::clone(&worktree.semantic_evaluation_publication_gate),
                Arc::clone(&worktree.shutting_down),
                Arc::clone(&worktree.wake),
                Arc::clone(&worktree.pending_wake),
                Arc::clone(&worktree.reconcile_in_progress),
                Arc::clone(&self.background_reconcile_admission),
            )
        };
        refuse_if_interrupted(control, &shutting_down)?;
        let _publication =
            acquire_publication_gate(publication_gate.as_ref(), control, &shutting_down).await?;
        let _daemon_admission =
            acquire_daemon_admission(background_reconcile_admission, control, &shutting_down)
                .await?;
        refuse_if_interrupted(control, &shutting_down)?;
        let _reconcile_pass = ReconcilePassGuard::enter(&reconcile_in_progress);
        let serving =
            validate_serving_request(&request, &repository_id, &worktree_id, &serving_generation)?;
        let expected_generation = request.expected_generation.clone();

        bridge.refresh(control, &shutting_down);
        let build_scheduler = Arc::clone(&scheduler);
        let build_bridge = Arc::clone(bridge);
        let build_serving = serving.clone();
        let mut build_task = tokio::task::spawn_blocking(move || {
            let mut scheduler = build_scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let build = scheduler.index_verified_ignored_dependency(
                &build_serving,
                request,
                build_bridge.as_ref(),
            )?;
            let replay_binding =
                scheduler.code_graph_replay_binding(&build.outcome.generation_id)?;
            Ok::<_, CodeIndexSchedulerErrorV1>((build, replay_binding))
        });
        let (build, replay_binding) = loop {
            bridge.refresh(control, &shutting_down);
            tokio::select! {
                result = &mut build_task => {
                    break result.map_err(|error| CodeIndexSchedulerErrorV1::Identity(
                        format!("ignored-dependency indexing task failed: {error}"),
                    ))??;
                }
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        };
        // `build_and_publish` has now atomically advanced the durable active
        // pointer. This is the irreversible commit boundary: cancellation was
        // honored during capture/build/publication, but from here this owner
        // must finish graph activation and the serving CAS instead of reporting
        // a refusal for a generation that a restart will restore.
        let request_reactivation = || {
            scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .request_background_reconcile();
            Self::note_wake(
                &pending_wake,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
        };
        if build
            .latest
            .generation()
            .manifest()
            .parent_generation
            .as_ref()
            != Some(&expected_generation)
        {
            request_reactivation();
            return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
        }
        let project_id = build.latest.generation().manifest().project_id.clone();

        let activation = activate_committed_generation(
            &graph_activation,
            &project_id,
            &repository_id,
            &worktree_id,
            build.latest.clone(),
            replay_binding,
        )
        .await;
        if let Err(error) = activation {
            request_reactivation();
            return Err(error);
        }
        let swap_scheduler = Arc::clone(&scheduler);
        let swap_serving_generation = Arc::clone(&serving_generation);
        let swap_serving_generation_epoch = Arc::clone(&serving_generation_epoch);
        let incumbent = serving.clone();
        let candidate = build.latest.clone();
        let swap_task = tokio::task::spawn_blocking(move || {
            // Durable publication already committed this candidate. Do not
            // consult request cancellation while acquiring the scheduler or
            // serving locks: this CAS must converge the live head with the
            // restart authority before the operation returns.
            let scheduler = swap_scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !scheduler.active_publication_matches(&candidate)? {
                return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
            }
            let Some(current) =
                exact_activated_serving_generation(&swap_serving_generation, &incumbent)
            else {
                return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
            };
            if current.generation().manifest().generation_id != expected_generation {
                return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
            }
            let mut serving = swap_serving_generation
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *serving = Some(candidate.clone());
            swap_serving_generation_epoch.fetch_add(1, Ordering::AcqRel);
            drop(serving);
            let _ = scheduler.schedule_semantic_generation(candidate.generation());
            Ok::<_, CodeIndexSchedulerErrorV1>(())
        })
        .await;
        let swap = match swap_task {
            Ok(swap) => swap,
            Err(error) => {
                request_reactivation();
                return Err(CodeIndexSchedulerErrorV1::Identity(format!(
                    "ignored-dependency serving-swap task failed: {error}"
                )));
            }
        };
        match swap {
            Ok(()) => {}
            Err(CodeIndexSchedulerErrorV1::IgnoredDependency(
                CodeIndexIgnoredDependencyRefusalV1::StaleGeneration,
            )) => {
                request_reactivation();
                return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
            }
            Err(error) => {
                request_reactivation();
                return Err(error);
            }
        }
        if let Ok(authority) = build.latest.test_attribution_authority() {
            self.test_attribution_authorities
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    project_root.to_path_buf(),
                    (build.outcome.generation_id.clone(), authority),
                );
        }
        Self::publish_generation(
            &self.generation_publications,
            project_root.to_path_buf(),
            &build.publication,
        );
        Ok(build.outcome)
    }
}

fn validate_serving_request(
    request: &CodeIndexIgnoredDependencyRequestV1,
    repository_id: &tracedecay_domain::RepositoryId,
    worktree_id: &tracedecay_domain::WorktreeId,
    serving_generation: &RwLock<Option<LatestCompleteCodeIndexV1>>,
) -> Result<LatestCompleteCodeIndexV1, CodeIndexSchedulerErrorV1> {
    if request.scope.validate().is_err()
        || &request.scope.repository_id != repository_id
        || &request.scope.worktree_id != worktree_id
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::ScopeMismatch.into());
    }
    let serving = serving_generation
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration)?;
    if &request.scope.project_id != &serving.generation().manifest().project_id
        || &request.scope.reference != &serving.generation().snapshot().reference
    {
        return Err(CodeIndexIgnoredDependencyRefusalV1::ScopeMismatch.into());
    }
    if &request.expected_generation != &serving.generation().manifest().generation_id {
        return Err(CodeIndexIgnoredDependencyRefusalV1::StaleGeneration.into());
    }
    Ok(serving)
}

pub(super) fn exact_activated_serving_generation(
    serving_generation: &RwLock<Option<LatestCompleteCodeIndexV1>>,
    candidate: &LatestCompleteCodeIndexV1,
) -> Option<LatestCompleteCodeIndexV1> {
    let serving = serving_generation
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()?;
    let serving_generation = serving.generation();
    let candidate_generation = candidate.generation();
    (serving_generation.manifest() == candidate_generation.manifest()
        && serving_generation.snapshot() == candidate_generation.snapshot()
        && serving_generation.ignored_source_admissions()
            == candidate_generation.ignored_source_admissions()
        && serving_generation.ignored_source_admissions_digest()
            == candidate_generation.ignored_source_admissions_digest())
    .then_some(serving)
}

async fn await_flight(
    flight: Arc<AdmissionFlightV1>,
    control: &(dyn CodeIndexExecutionControlV1 + Send + Sync),
) -> Result<CodeIndexIgnoredDependencyIndexOutcomeV1, CodeIndexSchedulerErrorV1> {
    let active_route = AtomicBool::new(false);
    loop {
        if let Some(completion) = flight.completion() {
            return completion.into_result();
        }
        refuse_if_interrupted(control, &active_route)?;
        tokio::select! {
            () = flight.completed.notified() => {}
            () = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}

async fn activate_committed_generation(
    graph_activation: &CodeGraphActivationAuthorityV1,
    project_id: &tracedecay_domain::ProjectId,
    repository_id: &tracedecay_domain::RepositoryId,
    worktree_id: &tracedecay_domain::WorktreeId,
    latest: LatestCompleteCodeIndexV1,
    replay_binding: CodeGraphReplayBindingV1,
) -> Result<(), CodeIndexSchedulerErrorV1> {
    graph_activation
        .activate(
            project_id,
            repository_id,
            worktree_id,
            latest,
            replay_binding,
            Arc::new(AtomicBool::new(false)),
        )
        .await
}

async fn acquire_publication_gate<'a>(
    gate: &'a tokio::sync::Mutex<()>,
    control: &(dyn CodeIndexExecutionControlV1 + Send + Sync),
    shutting_down: &AtomicBool,
) -> Result<tokio::sync::MutexGuard<'a, ()>, CodeIndexSchedulerErrorV1> {
    let mut lock = std::pin::pin!(gate.lock());
    loop {
        tokio::select! {
            guard = &mut lock => return Ok(guard),
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                refuse_if_interrupted(control, shutting_down)?;
            }
        }
    }
}

async fn acquire_daemon_admission(
    admission: Arc<tokio::sync::Semaphore>,
    control: &(dyn CodeIndexExecutionControlV1 + Send + Sync),
    shutting_down: &AtomicBool,
) -> Result<tokio::sync::OwnedSemaphorePermit, CodeIndexSchedulerErrorV1> {
    let mut acquire = std::pin::pin!(admission.acquire_owned());
    loop {
        tokio::select! {
            permit = &mut acquire => {
                return permit
                    .map_err(|_| CodeIndexIgnoredDependencyRefusalV1::Cancelled.into());
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                refuse_if_interrupted(control, shutting_down)?;
            }
        }
    }
}

fn refuse_if_interrupted(
    control: &(dyn CodeIndexExecutionControlV1 + Send + Sync),
    shutting_down: &AtomicBool,
) -> Result<(), CodeIndexSchedulerErrorV1> {
    if shutting_down.load(Ordering::Acquire) || control.is_cancelled() {
        Err(CodeIndexIgnoredDependencyRefusalV1::Cancelled.into())
    } else if control.is_deadline_exceeded() {
        Err(CodeIndexIgnoredDependencyRefusalV1::DeadlineExceeded.into())
    } else {
        Ok(())
    }
}

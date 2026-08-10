//! One typed runtime registry entry per canonical project.
//! Publication and shutdown operate on each project's components as a unit.

#[cfg(test)]
use std::any::Any;
use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex as StdMutex, MutexGuard};

use tokio::sync::{Mutex as AsyncMutex, watch};
use tracedecay_usecases::feedback::FeedbackCycleRuntime;
use tracedecay_usecases::primitives::PrimitiveProjectRuntime;

use super::invocation::{
    DaemonLspInvocationOwner, RegisteredCallableCodeRuntime, RegisteredConfigurationRuntime,
    RegisteredFeedbackRuntime, RegisteredRetainedRuntime, RegisteredWorkRuntime,
    SwitchableFeedbackCycleRuntimeV1, UnavailableFeedbackCycleRuntimeV1,
};

mod observability;
mod request_snapshot;
mod shutdown;

pub(crate) use observability::RegisteredObservabilityProducerV1;
use shutdown::ShutdownState;

#[cfg(test)]
mod observability_tests;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestFirst(u8);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestSecond(u8);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestOmitted(u8);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestLifecycleEvent {
    Construct { slot: u8, mark: u8 },
    Stage(u8),
    Publish,
    Drop { slot: u8, mark: u8 },
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestLifecycleSpy {
    events: std::sync::Mutex<Vec<TestLifecycleEvent>>,
    live: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl TestLifecycleSpy {
    fn record(&self, event: TestLifecycleEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }

    fn events(&self) -> Vec<TestLifecycleEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
#[derive(Debug)]
struct RecordingComponent<const SLOT: u8> {
    mark: u8,
    spy: Arc<TestLifecycleSpy>,
}

#[cfg(test)]
impl<const SLOT: u8> RecordingComponent<SLOT> {
    fn new(mark: u8, spy: Arc<TestLifecycleSpy>) -> Self {
        spy.live.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        spy.record(TestLifecycleEvent::Construct { slot: SLOT, mark });
        Self { mark, spy }
    }
}

#[cfg(test)]
impl<const SLOT: u8> Drop for RecordingComponent<SLOT> {
    fn drop(&mut self) {
        self.spy.record(TestLifecycleEvent::Drop {
            slot: SLOT,
            mark: self.mark,
        });
        self.spy
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Everything one canonical project's daemon runtime owns.
///
/// A slot is `None` until that component is registered.
#[derive(Default)]
pub(crate) struct ProjectRuntime {
    callable_code: Option<RegisteredCallableCodeRuntime>,
    feedback: Option<RegisteredFeedbackRuntime>,
    feedback_cycle: Option<Arc<FeedbackCycleRuntime>>,
    feedback_cycle_input: Option<Arc<SwitchableFeedbackCycleRuntimeV1>>,
    primitive: Option<PrimitiveProjectRuntime>,
    configuration: Option<RegisteredConfigurationRuntime>,
    work: Option<RegisteredWorkRuntime>,
    retained: Option<RegisteredRetainedRuntime>,
    lsp_owner: Option<DaemonLspInvocationOwner>,
    #[cfg(test)]
    test_marker: Option<Arc<dyn Any + Send + Sync>>,
    semantic: Option<crate::semantic_code::DaemonSemanticRuntimeHandleV1>,
    semantic_activation_reconciler: Option<
        Arc<crate::daemon::semantic_activation_reconciler::DaemonSemanticActivationReconcilerV1>,
    >,
    observability: Option<RegisteredObservabilityProducerV1>,
    reservations: Vec<TypeId>,
    #[cfg(test)]
    test_first: Option<TestFirst>,
    #[cfg(test)]
    test_second: Option<TestSecond>,
    #[cfg(test)]
    test_omitted: Option<TestOmitted>,
    #[cfg(test)]
    recording_first: Option<RecordingComponent<1>>,
    #[cfg(test)]
    recording_second: Option<RecordingComponent<2>>,
}

impl ProjectRuntime {
    fn has_components(&self) -> bool {
        self.callable_code.is_some()
            || self.feedback.is_some()
            || self.feedback_cycle.is_some()
            || self.feedback_cycle_input.is_some()
            || self.primitive.is_some()
            || self.configuration.is_some()
            || self.work.is_some()
            || self.retained.is_some()
            || self.lsp_owner.is_some()
            || self.semantic.is_some()
            || self.semantic_activation_reconciler.is_some()
            || self.observability.is_some()
            || {
                #[cfg(test)]
                {
                    self.test_marker.is_some()
                        || self.test_first.is_some()
                        || self.test_second.is_some()
                        || self.test_omitted.is_some()
                        || self.recording_first.is_some()
                        || self.recording_second.is_some()
                }
                #[cfg(not(test))]
                {
                    false
                }
            }
    }
}

/// One typed component of a project runtime.
///
/// Implementing this is what makes a kind of runtime reachable through the
/// registry; there is no per-component registrar, accessor, or expiry branch.
pub(crate) trait ProjectRuntimeComponent: Sized + Send + 'static {
    fn slot(runtime: &mut ProjectRuntime) -> &mut Option<Self>;

    fn peek(runtime: &ProjectRuntime) -> Option<&Self>;
}

macro_rules! project_runtime_components {
    ($($component:ty => $field:ident),+ $(,)?) => {
        $(
            impl ProjectRuntimeComponent for $component {
                fn slot(runtime: &mut ProjectRuntime) -> &mut Option<Self> {
                    &mut runtime.$field
                }

                fn peek(runtime: &ProjectRuntime) -> Option<&Self> {
                    runtime.$field.as_ref()
                }
            }
        )+
    };
}

project_runtime_components!(
    RegisteredCallableCodeRuntime => callable_code,
    RegisteredFeedbackRuntime => feedback,
    Arc<FeedbackCycleRuntime> => feedback_cycle,
    Arc<SwitchableFeedbackCycleRuntimeV1> => feedback_cycle_input,
    PrimitiveProjectRuntime => primitive,
    RegisteredConfigurationRuntime => configuration,
    RegisteredWorkRuntime => work,
    RegisteredRetainedRuntime => retained,
    DaemonLspInvocationOwner => lsp_owner,
    crate::semantic_code::DaemonSemanticRuntimeHandleV1 => semantic,
    Arc<crate::daemon::semantic_activation_reconciler::DaemonSemanticActivationReconcilerV1> => semantic_activation_reconciler,
    RegisteredObservabilityProducerV1 => observability,
);

#[cfg(test)]
project_runtime_components!(
    Arc<dyn Any + Send + Sync> => test_marker,
    TestFirst => test_first,
    TestSecond => test_second,
    TestOmitted => test_omitted,
    RecordingComponent<1> => recording_first,
    RecordingComponent<2> => recording_second,
);

#[derive(Clone, Copy)]
struct ReservedProjectRuntimeSlot {
    type_id: TypeId,
    occupied: fn(&ProjectRuntime) -> bool,
}

#[derive(Clone, Default)]
struct ProjectRuntimeReservation {
    slots: Vec<ReservedProjectRuntimeSlot>,
}

impl ProjectRuntimeReservation {
    fn reserve<C>(&mut self)
    where
        C: ProjectRuntimeComponent,
    {
        let type_id = TypeId::of::<C>();
        if self.slots.iter().any(|slot| slot.type_id == type_id) {
            return;
        }
        self.slots.push(ReservedProjectRuntimeSlot {
            type_id,
            occupied: |runtime| C::peek(runtime).is_some(),
        });
    }

    fn contains<C>(&self) -> bool
    where
        C: ProjectRuntimeComponent,
    {
        let type_id = TypeId::of::<C>();
        self.slots.iter().any(|slot| slot.type_id == type_id)
    }

    fn conflicts_with(&self, runtime: &ProjectRuntime) -> bool {
        self.slots
            .iter()
            .any(|slot| (slot.occupied)(runtime) || runtime.reservations.contains(&slot.type_id))
    }

    fn conflicts_with_components(&self, runtime: &ProjectRuntime) -> bool {
        self.slots.iter().any(|slot| (slot.occupied)(runtime))
    }

    fn is_complete(&self, runtime: &ProjectRuntime) -> bool {
        self.slots.iter().all(|slot| (slot.occupied)(runtime))
    }

    fn has_same_slots(&self, other: &Self) -> bool {
        self.slots.len() == other.slots.len()
            && self.slots.iter().all(|slot| {
                other
                    .slots
                    .iter()
                    .any(|other| other.type_id == slot.type_id)
            })
    }

    fn type_ids(&self) -> impl Iterator<Item = TypeId> + '_ {
        self.slots.iter().map(|slot| slot.type_id)
    }
}

/// Components prepared for one all-or-nothing project-runtime publication.
///
/// Typed slots are reserved under the registry lock, construction runs after
/// releasing it, and every staged component becomes reachable under one final
/// lock acquisition. A conflict leaves every incumbent and staged component
/// untouched.
pub(crate) struct ProjectRuntimePublication {
    staged: ProjectRuntime,
    reservation: ProjectRuntimeReservation,
    #[cfg(test)]
    lifecycle_spy: Option<Arc<TestLifecycleSpy>>,
}

impl ProjectRuntimePublication {
    fn new(reservation: ProjectRuntimeReservation) -> Self {
        Self {
            staged: ProjectRuntime::default(),
            reservation,
            #[cfg(test)]
            lifecycle_spy: None,
        }
    }

    #[cfg(test)]
    fn record_lifecycle_with(&mut self, spy: Arc<TestLifecycleSpy>) {
        self.lifecycle_spy = Some(spy);
    }

    /// Stages one component. A bundle cannot name the same typed slot twice.
    pub(crate) fn stage<C>(&mut self, component: C) -> Result<(), ProjectRuntimeAlreadyRegistered>
    where
        C: ProjectRuntimeComponent,
    {
        if !self.reservation.contains::<C>() {
            return Err(ProjectRuntimeAlreadyRegistered);
        }
        let slot = C::slot(&mut self.staged);
        if slot.is_some() {
            return Err(ProjectRuntimeAlreadyRegistered);
        }
        *slot = Some(component);
        Ok(())
    }

    fn conflicts_with(&self, incumbent: &ProjectRuntime) -> bool {
        self.reservation.conflicts_with_components(incumbent)
    }

    fn commit_into(
        mut self,
        incumbent: &mut ProjectRuntime,
    ) -> Result<(), ProjectRuntimeAlreadyRegistered> {
        #[cfg(test)]
        if let Some(spy) = &self.lifecycle_spy {
            spy.record(TestLifecycleEvent::Publish);
        }
        if !self.reservation.is_complete(&self.staged) || self.conflicts_with(incumbent) {
            return Err(ProjectRuntimeAlreadyRegistered);
        }
        macro_rules! move_all_components {
            ($source:ident, $target:ident) => {{
                macro_rules! move_component {
                    ($field:ident) => {
                        if $source.$field.is_some() {
                            $target.$field = $source.$field.take();
                        }
                    };
                }
                move_component!(callable_code);
                move_component!(feedback);
                move_component!(feedback_cycle);
                move_component!(feedback_cycle_input);
                move_component!(primitive);
                move_component!(configuration);
                move_component!(work);
                move_component!(retained);
                move_component!(lsp_owner);
                #[cfg(test)]
                move_component!(test_marker);
                move_component!(semantic);
                #[cfg(test)]
                {
                    move_component!(test_first);
                    move_component!(test_second);
                    move_component!(recording_first);
                    move_component!(recording_second);
                }
            }};
        }
        let mut prepared = ProjectRuntime::default();
        let staged = &mut self.staged;
        move_all_components!(staged, prepared);
        assert!(
            !staged.has_components(),
            "every staged runtime component must be published"
        );
        let prepared = &mut prepared;
        move_all_components!(prepared, incumbent);
        Ok(())
    }
}

/// A component was already published for this project.
///
/// Registration refuses rather than replaces: a second registration of a live
/// component would detach the first without shutting it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectRuntimeAlreadyRegistered;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectRuntimeRegistryError {
    AlreadyRegistered,
    Closed,
}

impl From<ProjectRuntimeAlreadyRegistered> for ProjectRuntimeRegistryError {
    fn from(_: ProjectRuntimeAlreadyRegistered) -> Self {
        Self::AlreadyRegistered
    }
}

#[derive(Clone)]
pub(crate) struct ProjectRuntimeRegistryV1 {
    runtimes: Arc<StdMutex<BTreeMap<PathBuf, ProjectRuntime>>>,
    /// Roots retired by a deletion/replacement cleanup. Registration for a
    /// retired root is refused until the root is explicitly readmitted, so a
    /// late open cannot resurrect a runtime over a deleted store.
    retired_roots: Arc<StdMutex<BTreeSet<PathBuf>>>,
    reservation_changed: watch::Sender<u64>,
    reservation_blocking_changed: Arc<(StdMutex<u64>, Condvar)>,
    /// The blocking drain is retained independently of whichever async
    /// shutdown caller first requested it. A retry can therefore join the
    /// same work after that caller is cancelled.
    shutdown_task: Arc<AsyncMutex<Option<tokio::task::JoinHandle<()>>>>,
    closed: Arc<AtomicBool>,
    shutdown_started: Arc<AtomicBool>,
    shutdown_complete: watch::Sender<ShutdownState>,
    #[cfg(test)]
    commit_starting: Arc<StdMutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    #[cfg(test)]
    drain_waiting: Arc<StdMutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl Default for ProjectRuntimeRegistryV1 {
    fn default() -> Self {
        let (reservation_changed, _) = watch::channel(0);
        let (shutdown_complete, _) = watch::channel(ShutdownState::Pending);
        Self {
            runtimes: Arc::new(StdMutex::new(BTreeMap::new())),
            retired_roots: Arc::new(StdMutex::new(BTreeSet::new())),
            reservation_changed,
            reservation_blocking_changed: Arc::new((StdMutex::new(0), Condvar::new())),
            shutdown_task: Arc::new(AsyncMutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
            shutdown_started: Arc::new(AtomicBool::new(false)),
            shutdown_complete,
            #[cfg(test)]
            commit_starting: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            drain_waiting: Arc::new(StdMutex::new(None)),
        }
    }
}

struct ProjectRuntimeReservationLease {
    registry: ProjectRuntimeRegistryV1,
    project_root: PathBuf,
    reservation: ProjectRuntimeReservation,
    active: bool,
}

impl ProjectRuntimeReservationLease {
    async fn release(mut self) {
        self.release_inner().await;
        self.active = false;
    }

    async fn commit(
        mut self,
        publication: ProjectRuntimePublication,
    ) -> Result<(), ProjectRuntimeRegistryError> {
        #[cfg(test)]
        if let Some(commit_starting) = self
            .registry
            .commit_starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            commit_starting.send(()).expect("commit-starting receiver");
        }
        let retired_roots = self.registry.lock_retired_roots();
        let mut runtimes = self.registry.lock_runtimes();
        let runtime = runtimes.entry(self.project_root.clone()).or_default();
        let result = if self.registry.closed.load(Ordering::Acquire)
            || retired_roots.contains(&self.project_root)
        {
            Err(ProjectRuntimeRegistryError::Closed)
        } else if self.reservation.has_same_slots(&publication.reservation) {
            publication.commit_into(runtime).map_err(Into::into)
        } else {
            Err(ProjectRuntimeRegistryError::AlreadyRegistered)
        };
        runtime.reservations.retain(|type_id| {
            !self
                .reservation
                .type_ids()
                .any(|reserved| reserved == *type_id)
        });
        let remove_project = runtime.reservations.is_empty() && !runtime.has_components();
        if remove_project {
            runtimes.remove(&self.project_root);
        }
        drop(runtimes);
        drop(retired_roots);
        self.active = false;
        self.registry.signal_reservation_changed();
        result
    }

    async fn release_inner(&self) {
        let mut runtimes = self.registry.lock_runtimes();
        Self::release_reservation(&mut runtimes, &self.project_root, &self.reservation);
        drop(runtimes);
        self.registry.signal_reservation_changed();
    }

    fn release_reservation(
        runtimes: &mut BTreeMap<PathBuf, ProjectRuntime>,
        project_root: &Path,
        reservation: &ProjectRuntimeReservation,
    ) {
        if let Some(runtime) = runtimes.get_mut(project_root) {
            runtime
                .reservations
                .retain(|type_id| !reservation.type_ids().any(|reserved| reserved == *type_id));
            let remove_project = runtime.reservations.is_empty() && !runtime.has_components();
            if remove_project {
                runtimes.remove(project_root);
            }
        }
    }
}

impl Drop for ProjectRuntimeReservationLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut runtimes = self.registry.lock_runtimes();
        ProjectRuntimeReservationLease::release_reservation(
            &mut runtimes,
            &self.project_root,
            &self.reservation,
        );
        drop(runtimes);
        self.registry.signal_reservation_changed();
    }
}

impl ProjectRuntimeRegistryV1 {
    fn lock_retired_roots(&self) -> MutexGuard<'_, BTreeSet<PathBuf>> {
        match self.retired_roots.lock() {
            Ok(retired) => retired,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn lock_runtimes(&self) -> MutexGuard<'_, BTreeMap<PathBuf, ProjectRuntime>> {
        self.runtimes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn signal_reservation_changed(&self) {
        let (version, changed) = &*self.reservation_blocking_changed;
        let mut version = version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *version = version.wrapping_add(1);
        changed.notify_all();
        drop(version);
        self.reservation_changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    async fn reserve(
        &self,
        project_root: PathBuf,
        reservation: ProjectRuntimeReservation,
    ) -> Result<ProjectRuntimeReservationLease, ProjectRuntimeRegistryError> {
        let retired_roots = self.lock_retired_roots();
        let mut runtimes = self.lock_runtimes();
        if self.closed.load(Ordering::Acquire) || retired_roots.contains(&project_root) {
            return Err(ProjectRuntimeRegistryError::Closed);
        }
        let runtime = runtimes.entry(project_root.clone()).or_default();
        if reservation.conflicts_with(runtime) {
            return Err(ProjectRuntimeRegistryError::AlreadyRegistered);
        }
        runtime.reservations.extend(reservation.type_ids());
        drop(runtimes);
        drop(retired_roots);
        Ok(ProjectRuntimeReservationLease {
            registry: self.clone(),
            project_root,
            reservation,
            active: true,
        })
    }

    /// Publish a component, refusing if this project already has a live one.
    pub(crate) async fn register<C>(
        &self,
        project_root: PathBuf,
        component: C,
    ) -> Result<(), ProjectRuntimeRegistryError>
    where
        C: ProjectRuntimeComponent,
    {
        loop {
            let mut reservation_changed = self.reservation_changed.subscribe();
            {
                let retired_roots = self.lock_retired_roots();
                let mut runtimes = self.lock_runtimes();
                if self.closed.load(Ordering::Acquire) || retired_roots.contains(&project_root) {
                    return Err(ProjectRuntimeRegistryError::Closed);
                }
                let runtime = runtimes.entry(project_root.clone()).or_default();
                if !runtime.reservations.contains(&TypeId::of::<C>()) {
                    let slot = C::slot(runtime);
                    if slot.is_some() {
                        return Err(ProjectRuntimeRegistryError::AlreadyRegistered);
                    }
                    *slot = Some(component);
                    return Ok(());
                }
            }
            if reservation_changed.changed().await.is_err() {
                return Err(ProjectRuntimeRegistryError::Closed);
            }
        }
    }

    /// Publish a component over whatever was there.
    ///
    /// Only for components whose caller has already established that the
    /// replacement carries the same authority as the incumbent.
    pub(crate) async fn publish<C>(
        &self,
        project_root: PathBuf,
        component: C,
    ) -> Result<(), ProjectRuntimeRegistryError>
    where
        C: ProjectRuntimeComponent,
    {
        loop {
            let mut reservation_changed = self.reservation_changed.subscribe();
            {
                let retired_roots = self.lock_retired_roots();
                let mut runtimes = self.lock_runtimes();
                if self.closed.load(Ordering::Acquire) || retired_roots.contains(&project_root) {
                    return Err(ProjectRuntimeRegistryError::Closed);
                }
                let runtime = runtimes.entry(project_root.clone()).or_default();
                if !runtime.reservations.contains(&TypeId::of::<C>()) {
                    *C::slot(runtime) = Some(component);
                    return Ok(());
                }
            }
            if reservation_changed.changed().await.is_err() {
                return Err(ProjectRuntimeRegistryError::Closed);
            }
        }
    }

    async fn publish_atomically_after_preflight<T, E, F, Fut>(
        &self,
        project_root: PathBuf,
        reservation: ProjectRuntimeReservation,
        build: F,
    ) -> Result<T, E>
    where
        E: From<ProjectRuntimeAlreadyRegistered> + From<ProjectRuntimeRegistryError>,
        F: FnOnce(ProjectRuntimePublication) -> Fut,
        Fut: Future<Output = Result<(ProjectRuntimePublication, T), E>>,
    {
        let lease = self
            .reserve(project_root, reservation.clone())
            .await
            .map_err(E::from)?;
        let publication = ProjectRuntimePublication::new(reservation);
        match build(publication).await {
            Ok((publication, output)) => {
                lease.commit(publication).await.map_err(E::from)?;
                Ok(output)
            }
            Err(error) => {
                lease.release().await;
                Err(error)
            }
        }
    }

    /// Publishes feedback admission as one registry transaction.
    ///
    /// The feedback slots are reserved before `build` starts because opening a
    /// feedback runtime persists its producer boot. Construction runs without
    /// the registry lock, while the reservation prevents a racing writer from
    /// occupying any staged slot before the atomic commit.
    pub(crate) async fn publish_feedback_atomically<T, E, F, Fut>(
        &self,
        project_root: PathBuf,
        build: F,
    ) -> Result<T, E>
    where
        E: From<ProjectRuntimeAlreadyRegistered> + From<ProjectRuntimeRegistryError>,
        F: FnOnce(ProjectRuntimePublication) -> Fut,
        Fut: Future<Output = Result<(ProjectRuntimePublication, T), E>>,
    {
        let mut reservation = ProjectRuntimeReservation::default();
        reservation.reserve::<RegisteredCallableCodeRuntime>();
        reservation.reserve::<RegisteredFeedbackRuntime>();
        reservation.reserve::<Arc<SwitchableFeedbackCycleRuntimeV1>>();
        self.publish_atomically_after_preflight(project_root, reservation, build)
            .await
    }

    /// Withdraw a component, returning it if it was there.
    #[cfg(test)]
    pub(crate) async fn withdraw<C>(&self, project_root: &Path) -> Option<C>
    where
        C: ProjectRuntimeComponent,
    {
        loop {
            let mut reservation_changed = self.reservation_changed.subscribe();
            {
                let mut runtimes = self.lock_runtimes();
                let runtime = runtimes.get_mut(project_root)?;
                if !runtime.reservations.contains(&TypeId::of::<C>()) {
                    return C::slot(runtime).take();
                }
            }
            if reservation_changed.changed().await.is_err() {
                return None;
            }
        }
    }

    pub(crate) async fn get<C>(&self, project_root: &Path) -> Option<C>
    where
        C: ProjectRuntimeComponent + Clone,
    {
        self.read::<C, _, _>(project_root, Clone::clone).await
    }

    /// Read one component through a projection, under one lock.
    pub(crate) async fn read<C, T, F>(&self, project_root: &Path, read: F) -> Option<T>
    where
        C: ProjectRuntimeComponent,
        F: FnOnce(&C) -> T,
    {
        let runtimes = self.lock_runtimes();
        runtimes.get(project_root).and_then(C::peek).map(read)
    }

    /// Project a value from the one registered component that matches.
    ///
    /// Project-id lookups have no root to disambiguate linked worktrees. An
    /// ambiguous match therefore remains unavailable instead of attaching an
    /// observation to whichever root happens to sort first.
    pub(crate) fn find_current<C, T, F>(&self, mut find: F) -> Option<T>
    where
        C: ProjectRuntimeComponent,
        F: FnMut(&C) -> Option<T>,
    {
        let runtimes = self.lock_runtimes();
        let mut matches = runtimes.values().filter_map(C::peek).filter_map(&mut find);
        let only = matches.next()?;
        matches.next().is_none().then_some(only)
    }

    /// Register a component, or accept an incumbent the caller recognizes as
    /// the same authority, under one lock.
    ///
    /// `reconcile` sees the incumbent and either accepts it — returning `Ok`,
    /// having refreshed whatever the caller renews — or refuses. `build` runs
    /// only when the slot is empty, so a component that owns processes or
    /// connections is never constructed just to be dropped on the reconcile
    /// path. Splitting the check from the insert would let a second
    /// registration land in between.
    pub(crate) async fn register_or_reconcile<C, E, R, B>(
        &self,
        project_root: PathBuf,
        reconcile: R,
        build: B,
    ) -> Result<(), E>
    where
        C: ProjectRuntimeComponent,
        E: From<ProjectRuntimeRegistryError>,
        R: FnOnce(&mut C) -> Result<(), E>,
        B: FnOnce() -> Result<C, E>,
    {
        loop {
            let mut reservation_changed = self.reservation_changed.subscribe();
            {
                let mut runtimes = self.lock_runtimes();
                if self.closed.load(Ordering::Acquire) {
                    return Err(ProjectRuntimeRegistryError::Closed.into());
                }
                let runtime = runtimes.entry(project_root.clone()).or_default();
                if !runtime.reservations.contains(&TypeId::of::<C>()) {
                    let slot = C::slot(runtime);
                    return match slot.as_mut() {
                        Some(incumbent) => reconcile(incumbent),
                        None => {
                            *slot = Some(build()?);
                            Ok(())
                        }
                    };
                }
            }
            if reservation_changed.changed().await.is_err() {
                return Err(ProjectRuntimeRegistryError::Closed.into());
            }
        }
    }

    fn component_with_canonical_fallback<C>(
        runtimes: &BTreeMap<PathBuf, ProjectRuntime>,
        project_root: &Path,
        canonical_root: Option<&Path>,
    ) -> Option<C>
    where
        C: ProjectRuntimeComponent + Clone,
    {
        runtimes
            .get(project_root)
            .and_then(C::peek)
            .or_else(|| {
                canonical_root
                    .and_then(|root| runtimes.get(root))
                    .and_then(C::peek)
            })
            .cloned()
    }

    pub(crate) async fn holds<C>(&self, project_root: &Path) -> bool
    where
        C: ProjectRuntimeComponent,
    {
        self.read::<C, _, _>(project_root, |_| ()).await.is_some()
    }

    /// The one project holding this component, when exactly one does.
    ///
    /// Answering with a component while several projects hold one would attach
    /// a request to whichever project happened to sort first.
    #[cfg(test)]
    pub(crate) async fn sole<C>(&self) -> Option<C>
    where
        C: ProjectRuntimeComponent + Clone,
    {
        let runtimes = self.lock_runtimes();
        let mut held = runtimes.values().filter_map(C::peek);
        let only = held.next()?;
        held.next().is_none().then(|| only.clone())
    }

    #[cfg(test)]
    pub(crate) async fn is_empty(&self) -> bool {
        self.lock_runtimes().is_empty()
    }

    #[cfg(test)]
    pub(super) async fn feedback_publication_state(
        &self,
        project_root: &Path,
    ) -> (bool, bool, bool) {
        let runtimes = self.lock_runtimes();
        let runtime = runtimes.get(project_root);
        (
            runtime.is_some_and(|runtime| runtime.callable_code.is_some()),
            runtime.is_some_and(|runtime| runtime.feedback.is_some()),
            runtime.is_some_and(|runtime| runtime.feedback_cycle_input.is_some()),
        )
    }

    #[cfg(test)]
    pub(super) fn arm_commit_starting(&self, commit_starting: tokio::sync::oneshot::Sender<()>) {
        *self
            .commit_starting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(commit_starting);
    }

    #[cfg(test)]
    pub(super) fn arm_shutdown_drain_waiting(
        &self,
        drain_waiting: tokio::sync::oneshot::Sender<()>,
    ) {
        *self
            .drain_waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(drain_waiting);
    }
}

#[cfg(test)]
mod tests;

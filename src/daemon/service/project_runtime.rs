//! One runtime per canonical project.
//!
//! The daemon used to keep a separate map for each kind of thing a project
//! owns: one for its callable-code authorization, one for its feedback runtime,
//! one for its cycle, one for its cycle input, one for primitives, one for
//! configuration, one for Work, one for the LSP owner, three for advisory, one
//! for semantic scheduling. Every registration, lookup, and shutdown named the
//! project once per map, so a project could exist in some of them and not
//! others, and shutdown was a sequence of mirrored branches whose order had to
//! be maintained by hand.
//!
//! [`ProjectRuntimeRegistryV1`] holds one [`ProjectRuntime`] per project root
//! instead. Each kind of component is a typed slot on it, reached through the
//! [`ProjectRuntimeComponent`] trait rather than through a map of its own, so
//! registering, reading, updating, and expiring a component are the same few
//! operations whatever the component is, and a project's components are
//! published and withdrawn under a single lock. Shutdown takes the whole map
//! once and runs one ordered sequence over it.

use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::application::feedback::Pr12FeedbackCycleRuntime;
use crate::application::primitives::Pr12PrimitiveProjectRuntime;

use super::invocation::{
    DaemonLspInvocationOwner, Pr13AdvisoryCycleInvocationPortV1, Pr13HookOrchestrationPortV1,
    RegisteredCallableCodeRuntime, RegisteredConfigurationRuntime, RegisteredFeedbackRuntime,
    RegisteredWorkRuntime, SwitchableFeedbackCycleRuntimeV1, UnavailableFeedbackCycleRuntimeV1,
};

/// Everything one canonical project's daemon runtime owns.
///
/// A slot is `None` until that component is registered.
#[derive(Default)]
pub(crate) struct ProjectRuntime {
    callable_code: Option<RegisteredCallableCodeRuntime>,
    feedback: Option<RegisteredFeedbackRuntime>,
    feedback_cycle: Option<Arc<Pr12FeedbackCycleRuntime>>,
    feedback_cycle_input: Option<Arc<SwitchableFeedbackCycleRuntimeV1>>,
    primitive: Option<Pr12PrimitiveProjectRuntime>,
    configuration: Option<RegisteredConfigurationRuntime>,
    work: Option<RegisteredWorkRuntime>,
    lsp_owner: Option<DaemonLspInvocationOwner>,
    advisory: Option<Arc<dyn Any + Send + Sync>>,
    advisory_cycle_invoker: Option<Arc<dyn Pr13AdvisoryCycleInvocationPortV1>>,
    advisory_hook_orchestrator: Option<Arc<dyn Pr13HookOrchestrationPortV1>>,
    semantic: Option<crate::semantic_code::DaemonSemanticRuntimeHandleV1>,
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
    Arc<Pr12FeedbackCycleRuntime> => feedback_cycle,
    Arc<SwitchableFeedbackCycleRuntimeV1> => feedback_cycle_input,
    Pr12PrimitiveProjectRuntime => primitive,
    RegisteredConfigurationRuntime => configuration,
    RegisteredWorkRuntime => work,
    DaemonLspInvocationOwner => lsp_owner,
    Arc<dyn Any + Send + Sync> => advisory,
    Arc<dyn Pr13AdvisoryCycleInvocationPortV1> => advisory_cycle_invoker,
    Arc<dyn Pr13HookOrchestrationPortV1> => advisory_hook_orchestrator,
    crate::semantic_code::DaemonSemanticRuntimeHandleV1 => semantic,
);

/// A component was already published for this project.
///
/// Registration refuses rather than replaces: a second registration of a live
/// component would detach the first without shutting it down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectRuntimeAlreadyRegistered;

#[derive(Clone, Default)]
pub(crate) struct ProjectRuntimeRegistryV1 {
    runtimes: Arc<Mutex<BTreeMap<PathBuf, ProjectRuntime>>>,
}

impl ProjectRuntimeRegistryV1 {
    /// Publish a component, refusing if this project already has a live one.
    pub(crate) async fn register<C>(
        &self,
        project_root: PathBuf,
        component: C,
    ) -> Result<(), ProjectRuntimeAlreadyRegistered>
    where
        C: ProjectRuntimeComponent,
    {
        let mut runtimes = self.runtimes.lock().await;
        let slot = C::slot(runtimes.entry(project_root).or_default());
        if slot.is_some() {
            return Err(ProjectRuntimeAlreadyRegistered);
        }
        *slot = Some(component);
        Ok(())
    }

    /// Publish a component over whatever was there.
    ///
    /// Only for components whose caller has already established that the
    /// replacement carries the same authority as the incumbent.
    pub(crate) async fn publish<C>(&self, project_root: PathBuf, component: C)
    where
        C: ProjectRuntimeComponent,
    {
        let mut runtimes = self.runtimes.lock().await;
        *C::slot(runtimes.entry(project_root).or_default()) = Some(component);
    }

    /// Withdraw a component, returning it if it was there.
    pub(crate) async fn withdraw<C>(&self, project_root: &Path) -> Option<C>
    where
        C: ProjectRuntimeComponent,
    {
        let mut runtimes = self.runtimes.lock().await;
        runtimes
            .get_mut(project_root)
            .and_then(|runtime| C::slot(runtime).take())
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
        let runtimes = self.runtimes.lock().await;
        runtimes.get(project_root).and_then(C::peek).map(read)
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
        R: FnOnce(&mut C) -> Result<(), E>,
        B: FnOnce() -> Result<C, E>,
    {
        let mut runtimes = self.runtimes.lock().await;
        let slot = C::slot(runtimes.entry(project_root).or_default());
        match slot.as_mut() {
            Some(incumbent) => reconcile(incumbent),
            None => {
                *slot = Some(build()?);
                Ok(())
            }
        }
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
    pub(crate) async fn sole<C>(&self) -> Option<C>
    where
        C: ProjectRuntimeComponent + Clone,
    {
        let runtimes = self.runtimes.lock().await;
        let mut held = runtimes.values().filter_map(C::peek);
        let only = held.next()?;
        held.next().is_none().then(|| only.clone())
    }

    /// Shut every project runtime down and leave the registry empty.
    ///
    /// The ordering constraints below are the ones that used to be spread
    /// across a dozen branches of the daemon's expiry path:
    ///
    /// - a production advisory input retains its LSP factory, whose feedback
    ///   adapter retains the switchable cycle router, so every router is
    ///   pointed at an unavailable runtime before any feedback runtime is
    ///   dropped — otherwise that cycle keeps the project graph and database
    ///   runtimes alive past shutdown;
    /// - a Work runtime owns live provider processes, so each is stopped and
    ///   joined rather than merely detached;
    /// - a semantic handle is published in a process-wide table, so it is
    ///   unregistered and cancelled rather than dropped.
    pub(crate) async fn shut_down_all(&self) {
        let runtimes = std::mem::take(&mut *self.runtimes.lock().await);

        for runtime in runtimes.values() {
            let (Some(router), Some(feedback)) = (&runtime.feedback_cycle_input, &runtime.feedback)
            else {
                continue;
            };
            let _ = router.replace(Arc::new(UnavailableFeedbackCycleRuntimeV1::new(
                feedback.project_id().clone(),
                feedback.source_observation_port(),
            )));
        }

        for (project_root, runtime) in runtimes {
            if let Some(work) = runtime.work {
                let work_runtime = work.into_runtime();
                let _ = tokio::task::spawn_blocking(move || work_runtime.shutdown()).await;
            }
            if let Some(semantic) = runtime.semantic {
                crate::application::semantic_runtime::unregister_project_semantic_runtime(
                    &project_root,
                );
                semantic.cancel();
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn is_empty(&self) -> bool {
        self.runtimes.lock().await.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Any component exercises the registry the same way, so these use the one
    /// component that is cheap to construct rather than a test-only slot.
    type Component = Arc<dyn Any + Send + Sync>;

    fn component(mark: u32) -> Component {
        Arc::new(mark)
    }

    fn mark(component: &Component) -> Option<u32> {
        component.downcast_ref::<u32>().copied()
    }

    fn root(name: &str) -> PathBuf {
        PathBuf::from("/projects").join(name)
    }

    #[tokio::test]
    async fn a_second_registration_is_refused_and_leaves_the_incumbent_live() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project = root("alpha");

        registry
            .register(project.clone(), component(1))
            .await
            .expect("the first registration owns the empty slot");
        let refused = registry.register(project.clone(), component(2)).await;

        assert_eq!(refused, Err(ProjectRuntimeAlreadyRegistered));
        assert_eq!(
            registry.get::<Component>(&project).await.as_ref().and_then(mark),
            Some(1),
            "a refused registration must not detach the live component"
        );
    }

    #[tokio::test]
    async fn publishing_replaces_the_incumbent() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project = root("alpha");

        registry.publish(project.clone(), component(1)).await;
        registry.publish(project.clone(), component(2)).await;

        assert_eq!(
            registry.get::<Component>(&project).await.as_ref().and_then(mark),
            Some(2)
        );
    }

    #[tokio::test]
    async fn withdrawing_hands_the_component_back_exactly_once() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project = root("alpha");
        registry.publish(project.clone(), component(1)).await;

        let withdrawn = registry.withdraw::<Component>(&project).await;

        assert_eq!(withdrawn.as_ref().and_then(mark), Some(1));
        assert!(!registry.holds::<Component>(&project).await);
        assert!(registry.withdraw::<Component>(&project).await.is_none());
    }

    #[tokio::test]
    async fn one_project_s_components_are_not_another_s() {
        let registry = ProjectRuntimeRegistryV1::default();
        registry.publish(root("alpha"), component(1)).await;

        assert!(!registry.holds::<Component>(&root("beta")).await);
        assert!(registry.get::<Component>(&root("beta")).await.is_none());
    }

    #[tokio::test]
    async fn a_sole_component_is_only_answered_while_exactly_one_project_holds_it() {
        let registry = ProjectRuntimeRegistryV1::default();
        assert!(registry.sole::<Component>().await.is_none(), "none held");

        registry.publish(root("alpha"), component(1)).await;
        assert_eq!(
            registry.sole::<Component>().await.as_ref().and_then(mark),
            Some(1)
        );

        registry.publish(root("beta"), component(2)).await;
        assert!(
            registry.sole::<Component>().await.is_none(),
            "answering while two projects hold one would attach a request to \
             whichever project sorted first"
        );
    }

    #[tokio::test]
    async fn reconciling_an_occupied_slot_never_builds_a_replacement() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project = root("alpha");
        registry.publish(project.clone(), component(1)).await;
        let builds = AtomicUsize::new(0);

        let accepted = registry
            .register_or_reconcile::<Component, (), _, _>(
                project.clone(),
                |_| Ok(()),
                || {
                    builds.fetch_add(1, Ordering::SeqCst);
                    Ok(component(2))
                },
            )
            .await;

        assert_eq!(accepted, Ok(()));
        assert_eq!(
            builds.load(Ordering::SeqCst),
            0,
            "a component that owns processes must not be constructed just to \
             be dropped on the reconcile path"
        );
        assert_eq!(
            registry.get::<Component>(&project).await.as_ref().and_then(mark),
            Some(1)
        );
    }

    #[tokio::test]
    async fn reconciling_an_empty_slot_builds_once_and_keeps_the_build_error() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project = root("alpha");

        let failed = registry
            .register_or_reconcile::<Component, &str, _, _>(
                project.clone(),
                |_| Ok(()),
                || Err("the provider runtime would not open"),
            )
            .await;
        assert_eq!(failed, Err("the provider runtime would not open"));
        assert!(
            !registry.holds::<Component>(&project).await,
            "a failed build must not leave a slot claimed"
        );

        let built = registry
            .register_or_reconcile::<Component, &str, _, _>(
                project.clone(),
                |_| Ok(()),
                || Ok(component(1)),
            )
            .await;
        assert_eq!(built, Ok(()));
        assert_eq!(
            registry.get::<Component>(&project).await.as_ref().and_then(mark),
            Some(1)
        );
    }

    #[tokio::test]
    async fn a_refusing_reconcile_keeps_the_incumbent() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project = root("alpha");
        registry.publish(project.clone(), component(1)).await;

        let refused = registry
            .register_or_reconcile::<Component, &str, _, _>(
                project.clone(),
                |_| Err("a different authority is already registered"),
                || Ok(component(2)),
            )
            .await;

        assert_eq!(refused, Err("a different authority is already registered"));
        assert_eq!(
            registry.get::<Component>(&project).await.as_ref().and_then(mark),
            Some(1)
        );
    }

    #[tokio::test]
    async fn reading_answers_only_for_a_project_that_holds_the_component() {
        let registry = ProjectRuntimeRegistryV1::default();
        let project = root("alpha");
        registry.publish(project.clone(), component(7)).await;

        assert_eq!(
            registry
                .read::<Component, _, _>(&project, |held| mark(held))
                .await,
            Some(Some(7))
        );
        assert!(
            registry
                .read::<Component, _, _>(&root("beta"), |_| ())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn shutting_down_empties_the_registry() {
        let registry = ProjectRuntimeRegistryV1::default();
        registry.publish(root("alpha"), component(1)).await;
        registry.publish(root("beta"), component(2)).await;

        registry.shut_down_all().await;

        assert!(registry.is_empty().await);
        assert!(!registry.holds::<Component>(&root("alpha")).await);
    }
}

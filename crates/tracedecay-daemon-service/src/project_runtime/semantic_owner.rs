use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;
use tracedecay_application::doctor::{
    SemanticOwnerDegradedReasonV1, SemanticOwnerPrerequisiteV1, SemanticOwnerStateV1,
};
use tracedecay_runtime_core::cancellation::CancellationToken;

/// Project-owned lifetime and typed state for the independently scheduled
/// semantic activation-owner registration task.
#[derive(Clone)]
pub struct RegisteredSemanticOwnerTaskV1 {
    inner: Arc<RegisteredSemanticOwnerTaskInnerV1>,
}

struct RegisteredSemanticOwnerTaskInnerV1 {
    signals: SemanticOwnerRegistrationSignalsV1,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Cloneable task signals that deliberately do not retain the task handle.
/// The worker captures this value, avoiding an owner → task → owner cycle.
#[derive(Clone)]
pub struct SemanticOwnerRegistrationSignalsV1 {
    inner: Arc<SemanticOwnerRegistrationSignalsInnerV1>,
}

struct SemanticOwnerRegistrationSignalsInnerV1 {
    state: watch::Sender<SemanticOwnerStateV1>,
    configuration_ready: watch::Sender<bool>,
    production_runtime_ready: watch::Sender<bool>,
    cancellation: CancellationToken,
}

impl SemanticOwnerRegistrationSignalsV1 {
    fn new() -> Self {
        let (state, _) = watch::channel(SemanticOwnerStateV1::PendingPrerequisites {
            missing: vec![
                SemanticOwnerPrerequisiteV1::ConfigurationRuntime,
                SemanticOwnerPrerequisiteV1::ProductionSemanticRuntime,
            ],
        });
        let (configuration_ready, _) = watch::channel(false);
        let (production_runtime_ready, _) = watch::channel(false);
        Self {
            inner: Arc::new(SemanticOwnerRegistrationSignalsInnerV1 {
                state,
                configuration_ready,
                production_runtime_ready,
                cancellation: CancellationToken::new(),
            }),
        }
    }

    pub fn state(&self) -> SemanticOwnerStateV1 {
        self.inner.state.borrow().clone()
    }

    pub fn subscribe_state(&self) -> watch::Receiver<SemanticOwnerStateV1> {
        self.inner.state.subscribe()
    }

    pub fn set_state(&self, state: SemanticOwnerStateV1) {
        self.inner.state.send_replace(state);
    }

    pub fn mark_configuration_runtime_ready(&self) {
        self.inner.configuration_ready.send_replace(true);
    }

    pub fn mark_production_runtime_ready(&self) {
        self.inner.production_runtime_ready.send_replace(true);
    }

    pub fn subscribe_configuration_ready(&self) -> watch::Receiver<bool> {
        self.inner.configuration_ready.subscribe()
    }

    pub fn subscribe_production_runtime_ready(&self) -> watch::Receiver<bool> {
        self.inner.production_runtime_ready.subscribe()
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.inner.cancellation.clone()
    }
}

impl RegisteredSemanticOwnerTaskV1 {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegisteredSemanticOwnerTaskInnerV1 {
                signals: SemanticOwnerRegistrationSignalsV1::new(),
                task: Mutex::new(None),
            }),
        }
    }

    pub fn signals(&self) -> SemanticOwnerRegistrationSignalsV1 {
        self.inner.signals.clone()
    }

    pub fn state(&self) -> SemanticOwnerStateV1 {
        self.inner.signals.state()
    }

    pub fn subscribe_state(&self) -> watch::Receiver<SemanticOwnerStateV1> {
        self.inner.signals.subscribe_state()
    }

    pub fn set_state(&self, state: SemanticOwnerStateV1) {
        self.inner.signals.set_state(state);
    }

    pub fn mark_configuration_runtime_ready(&self) {
        self.inner.signals.mark_configuration_runtime_ready();
    }

    pub fn mark_production_runtime_ready(&self) {
        self.inner.signals.mark_production_runtime_ready();
    }

    pub fn subscribe_configuration_ready(&self) -> watch::Receiver<bool> {
        self.inner.signals.subscribe_configuration_ready()
    }

    pub fn subscribe_production_runtime_ready(&self) -> watch::Receiver<bool> {
        self.inner.signals.subscribe_production_runtime_ready()
    }

    pub fn cancellation(&self) -> CancellationToken {
        self.inner.signals.cancellation()
    }

    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> bool {
        let mut retained = self
            .inner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if retained.is_some() {
            return false;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.set_state(SemanticOwnerStateV1::Degraded {
                reason: SemanticOwnerDegradedReasonV1::TaskRuntimeUnavailable,
                detail: "semantic owner registration has no Tokio runtime".to_owned(),
            });
            return false;
        };
        *retained = Some(runtime.spawn(task));
        true
    }

    pub fn cancel(&self) {
        self.inner.signals.cancellation().cancel();
    }

    #[hotpath::skip]
    pub async fn cancel_and_join(&self) -> bool {
        self.cancel();
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        match task {
            Some(task) => match task.await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(%error, "semantic owner registration task shutdown failed");
                    false
                }
            },
            None => true,
        }
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn has_retained_task(&self) -> bool {
        self.inner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
}

impl Default for RegisteredSemanticOwnerTaskV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RegisteredSemanticOwnerTaskInnerV1 {
    fn drop(&mut self) {
        self.signals.cancellation().cancel();
        let task = match self.task.get_mut() {
            Ok(task) => task.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(task) = task {
            task.abort();
        }
    }
}

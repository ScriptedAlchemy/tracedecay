use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_agent_hosts::automation::AutomationRunControl;

use super::super::DaemonLifecycle;

#[derive(Clone, Default)]
pub(super) struct AutomationSchedulerStop {
    requested: Arc<AtomicBool>,
}

impl AutomationSchedulerStop {
    pub(super) fn run_control(&self, lifecycle: DaemonLifecycle) -> AutomationRunControl {
        let requested = Arc::clone(&self.requested);
        AutomationRunControl::from_interrupted(Arc::new(move || {
            !lifecycle.accepting() || requested.load(Ordering::Acquire)
        }))
    }

    pub(super) fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{AutomationSchedulerStop, DaemonLifecycle};

    #[test]
    fn run_control_observes_scheduler_retirement() {
        let stop = AutomationSchedulerStop::default();
        let control = stop.run_control(DaemonLifecycle::default());

        assert!(!control.read_control().interrupted());
        stop.request();
        assert!(control.read_control().interrupted());
    }

    #[test]
    fn run_control_observes_daemon_draining_independently() {
        let lifecycle = DaemonLifecycle::default();
        let control = AutomationSchedulerStop::default().run_control(lifecycle.clone());

        assert!(!control.read_control().interrupted());
        lifecycle.begin_draining();
        assert!(control.read_control().interrupted());
    }
}

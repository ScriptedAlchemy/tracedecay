//! Scheduler stop token that does not depend on daemon lifecycle types.
//!
//! The composition root supplies an already-resolved interruption predicate
//! (daemon draining, worker handle, or a test latch).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::AutomationRunControl;

#[derive(Clone, Default)]
pub struct AutomationSchedulerStop {
    requested: Arc<AtomicBool>,
}

impl AutomationSchedulerStop {
    #[must_use]
    pub fn run_control(
        &self,
        daemon_accepting: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> AutomationRunControl {
        let requested = Arc::clone(&self.requested);
        AutomationRunControl::from_interrupted(Arc::new(move || {
            !daemon_accepting() || requested.load(Ordering::Acquire)
        }))
    }

    pub fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::AutomationSchedulerStop;

    #[test]
    fn run_control_observes_scheduler_retirement() {
        let stop = AutomationSchedulerStop::default();
        let control = stop.run_control(Arc::new(|| true));

        assert!(!control.read_control().interrupted());
        stop.request();
        assert!(control.read_control().interrupted());
    }

    #[test]
    fn run_control_observes_daemon_draining_independently() {
        let accepting = Arc::new(AtomicBool::new(true));
        let control = AutomationSchedulerStop::default().run_control({
            let accepting = Arc::clone(&accepting);
            Arc::new(move || accepting.load(Ordering::Acquire))
        });

        assert!(!control.read_control().interrupted());
        accepting.store(false, Ordering::Release);
        assert!(control.read_control().interrupted());
    }
}

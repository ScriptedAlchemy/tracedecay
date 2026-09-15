//! Reader-pool admission, wait, execution, and release counters.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use super::{ReaderAdmissionSnapshot, SqliteVmSnapshot, duration_micros};

#[derive(Default)]
pub(crate) struct ReaderAdmissionRecorder {
    acquire_events: AtomicU64,
    wait_events: AtomicU64,
    saturated_events: AtomicU64,
    interrupted_events: AtomicU64,
    release_events: AtomicU64,
    wait_micros: AtomicU64,
    execution_micros: AtomicU64,
    fullscan_steps: AtomicU64,
    sort_steps: AtomicU64,
    vm_steps: AtomicU64,
}

impl ReaderAdmissionRecorder {
    pub(crate) fn acquired(&self, waited: Duration, waited_for_capacity: bool) {
        self.acquire_events.fetch_add(1, Ordering::Relaxed);
        self.wait_micros
            .fetch_add(duration_micros(waited), Ordering::Relaxed);
        if waited_for_capacity {
            self.wait_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn saturated(&self, waited: Duration, waited_for_capacity: bool) {
        self.saturated_events.fetch_add(1, Ordering::Relaxed);
        self.wait_micros
            .fetch_add(duration_micros(waited), Ordering::Relaxed);
        if waited_for_capacity {
            self.wait_events.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn interrupted(&self) {
        self.interrupted_events.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn released(&self) {
        self.release_events.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn executed(&self, elapsed: Duration, sqlite_vm: SqliteVmSnapshot) {
        self.execution_micros
            .fetch_add(duration_micros(elapsed), Ordering::Relaxed);
        self.fullscan_steps
            .fetch_add(sqlite_vm.fullscan_steps, Ordering::Relaxed);
        self.sort_steps
            .fetch_add(sqlite_vm.sort_steps, Ordering::Relaxed);
        self.vm_steps
            .fetch_add(sqlite_vm.vm_steps, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> ReaderAdmissionSnapshot {
        ReaderAdmissionSnapshot {
            acquire_events: self.acquire_events.load(Ordering::Relaxed),
            wait_events: self.wait_events.load(Ordering::Relaxed),
            saturated_events: self.saturated_events.load(Ordering::Relaxed),
            interrupted_events: self.interrupted_events.load(Ordering::Relaxed),
            release_events: self.release_events.load(Ordering::Relaxed),
            wait_micros: self.wait_micros.load(Ordering::Relaxed),
            execution_micros: self.execution_micros.load(Ordering::Relaxed),
            sqlite_vm: SqliteVmSnapshot {
                fullscan_steps: self.fullscan_steps.load(Ordering::Relaxed),
                sort_steps: self.sort_steps.load(Ordering::Relaxed),
                vm_steps: self.vm_steps.load(Ordering::Relaxed),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_wait_and_release_counters_stay_exact() {
        let recorder = ReaderAdmissionRecorder::default();
        recorder.acquired(Duration::from_micros(7), true);
        recorder.saturated(Duration::from_micros(3), false);
        recorder.interrupted();
        recorder.released();
        recorder.executed(
            Duration::from_micros(11),
            SqliteVmSnapshot {
                fullscan_steps: 2,
                sort_steps: 1,
                vm_steps: 9,
            },
        );
        let snapshot = recorder.snapshot();
        assert_eq!(snapshot.acquire_events, 1);
        assert_eq!(snapshot.wait_events, 1);
        assert_eq!(snapshot.saturated_events, 1);
        assert_eq!(snapshot.interrupted_events, 1);
        assert_eq!(snapshot.release_events, 1);
        assert_eq!(snapshot.wait_micros, 10);
        assert_eq!(snapshot.execution_micros, 11);
        assert_eq!(snapshot.sqlite_vm.fullscan_steps, 2);
        assert_eq!(snapshot.sqlite_vm.sort_steps, 1);
        assert_eq!(snapshot.sqlite_vm.vm_steps, 9);
    }
}

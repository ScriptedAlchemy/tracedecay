use std::time::{Duration, Instant};

use crate::observation::ObservationCancellation;

pub(super) const HOST_SCAN_WINDOW: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HostScanEvidence {
    pub cancelled: bool,
    pub deadline_elapsed: bool,
    pub input_bound_reached: bool,
    pub unit_bound_reached: bool,
    pub non_durable_units: u64,
    pub unavailable_units: u64,
}

impl HostScanEvidence {
    pub(super) const fn is_deferred(self) -> bool {
        self.cancelled
            || self.deadline_elapsed
            || self.input_bound_reached
            || self.unit_bound_reached
            || self.non_durable_units > 0
            || self.unavailable_units > 0
    }
}

#[derive(Clone, Debug)]
pub(super) struct HostScanBudget {
    remaining_input_bytes: u64,
    remaining_units: usize,
    consumed_input_bytes: u64,
    deadline: Instant,
    cancellation: ObservationCancellation,
    evidence: HostScanEvidence,
}

impl HostScanBudget {
    pub(super) const fn new(
        max_input_bytes: u64,
        max_units: usize,
        deadline: Instant,
        cancellation: ObservationCancellation,
    ) -> Self {
        Self {
            remaining_input_bytes: max_input_bytes,
            remaining_units: max_units,
            consumed_input_bytes: 0,
            deadline,
            cancellation,
            evidence: HostScanEvidence {
                cancelled: false,
                deadline_elapsed: false,
                input_bound_reached: false,
                unit_bound_reached: false,
                non_durable_units: 0,
                unavailable_units: 0,
            },
        }
    }

    pub(super) fn checkpoint(&mut self) -> bool {
        if self.cancellation.is_cancelled() {
            self.evidence.cancelled = true;
            return false;
        }
        if Instant::now() >= self.deadline {
            self.evidence.deadline_elapsed = true;
            return false;
        }
        true
    }

    pub(super) fn try_charge_unit(&mut self) -> bool {
        if !self.checkpoint() {
            return false;
        }
        if self.remaining_units == 0 {
            self.evidence.unit_bound_reached = true;
            return false;
        }
        self.remaining_units -= 1;
        true
    }

    pub(super) fn try_charge_input(&mut self, bytes: u64) -> bool {
        if !self.checkpoint() {
            return false;
        }
        if bytes > self.remaining_input_bytes {
            self.evidence.input_bound_reached = true;
            return false;
        }
        self.remaining_input_bytes -= bytes;
        self.consumed_input_bytes = self.consumed_input_bytes.saturating_add(bytes);
        true
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(super) fn cancellation(&self) -> ObservationCancellation {
        self.cancellation.clone()
    }

    pub(super) const fn consumed_input_bytes(&self) -> u64 {
        self.consumed_input_bytes
    }

    pub(super) fn mark_non_durable(&mut self) {
        self.evidence.non_durable_units = self.evidence.non_durable_units.saturating_add(1);
    }

    pub(super) fn mark_unavailable(&mut self) {
        self.evidence.unavailable_units = self.evidence.unavailable_units.saturating_add(1);
    }

    pub(super) const fn evidence(&self) -> HostScanEvidence {
        self.evidence
    }
}

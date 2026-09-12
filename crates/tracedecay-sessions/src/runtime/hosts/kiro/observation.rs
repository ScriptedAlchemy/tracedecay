use crate::runtime::snapshot_observation::SnapshotAdmissionRecord;

use super::PROVIDER;

/// A normalized Kiro snapshot record retained until daemon-owned admission commits it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KiroSnapshotObservationRecord {
    pub(super) session_id: String,
    pub(super) native_record_id: String,
    pub(super) order: u64,
    pub(super) payload: Vec<u8>,
}

impl SnapshotAdmissionRecord for KiroSnapshotObservationRecord {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn native_record_id(&self) -> &str {
        &self.native_record_id
    }

    fn order(&self) -> u64 {
        self.order
    }

    fn payload(&self) -> &[u8] {
        &self.payload
    }
}

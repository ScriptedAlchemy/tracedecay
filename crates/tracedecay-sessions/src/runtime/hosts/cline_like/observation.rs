use crate::runtime::snapshot_observation::SnapshotAdmissionRecord;
#[cfg(test)]
use crate::runtime::snapshot_observation::snapshot_cursor_after;
#[cfg(test)]
use crate::runtime::source::TranscriptIngestResult;
use tracedecay_domain::ObservationSourceGenerationV1;
#[cfg(test)]
use tracedecay_domain::{ObservationScopeV1, ObservationSourceCursorV1};

/// A normalized snapshot record retained until daemon-owned admission commits it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClineLikeSnapshotObservationRecord {
    pub(super) provider: &'static str,
    pub(super) session_id: String,
    /// `Some` for the UI-message stream, which is appended independently of the
    /// API history and therefore owns its own source identity and generation.
    pub(super) source_key: Option<String>,
    pub(super) generation: ObservationSourceGenerationV1,
    pub(super) native_record_id: String,
    pub(super) order: u64,
    pub(super) payload: Vec<u8>,
}

impl SnapshotAdmissionRecord for ClineLikeSnapshotObservationRecord {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn source_key(&self) -> Option<&str> {
        self.source_key.as_deref()
    }

    fn generation(&self) -> Option<ObservationSourceGenerationV1> {
        Some(self.generation)
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

#[cfg(test)]
impl ClineLikeSnapshotObservationRecord {
    pub(super) fn cursor_after(
        &self,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
    ) -> TranscriptIngestResult<ObservationSourceCursorV1> {
        snapshot_cursor_after(
            self.provider,
            &self.session_id,
            self.source_key.as_deref(),
            self.order,
            scope,
            generation,
        )
    }
}

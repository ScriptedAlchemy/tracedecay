use std::collections::HashSet;

use crate::runtime::source::TranscriptIngestError;

/// Outcome of one composer sweep pass.
#[derive(Debug, Default, Clone)]
pub struct CursorComposerSweepOutcome {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
    /// Serialized bytes of new observation payloads processed by this pass.
    pub bytes_consumed: u64,
    /// At least one composer source or queued projection was deferred.
    pub deferred_by_byte_cap: bool,
    /// Bounded set of composer session ids observed during the sweep. The
    /// JSONL sweep skips these so the two Cursor sources do not double-ingest
    /// the same session within the bounded discovery window.
    pub owned_session_ids: HashSet<String>,
    projected_session_ids: HashSet<String>,
}

impl CursorComposerSweepOutcome {
    pub(super) fn add_projection(
        &mut self,
        session_ids: impl IntoIterator<Item = String>,
        messages: u64,
        deferred: bool,
    ) {
        self.projected_session_ids.extend(session_ids);
        self.sessions_upserted =
            u64::try_from(self.projected_session_ids.len()).unwrap_or(u64::MAX);
        self.messages_upserted = self.messages_upserted.saturating_add(messages);
        self.deferred_by_byte_cap |= deferred;
    }

    pub(super) fn finished(mut self, bytes_consumed: u64, deferred: bool) -> Self {
        self.bytes_consumed = bytes_consumed;
        self.deferred_by_byte_cap |= deferred;
        self
    }

    pub(in crate::runtime) fn projected_session_ids(&self) -> std::collections::BTreeSet<String> {
        self.projected_session_ids.iter().cloned().collect()
    }

    pub(in crate::runtime) fn jsonl_skip_session_ids(&self) -> HashSet<String> {
        self.owned_session_ids
            .union(&self.projected_session_ids)
            .cloned()
            .collect()
    }

    pub(super) fn terminated(
        self,
        error: TranscriptIngestError,
        bytes_consumed: u64,
        deferred: bool,
    ) -> CursorComposerSweepFailure {
        CursorComposerSweepFailure {
            outcome: self.finished(bytes_consumed, deferred),
            error,
        }
    }
}

/// Typed termination of a composer sweep after zero or more durable writes.
#[derive(Debug)]
pub struct CursorComposerSweepFailure {
    pub outcome: CursorComposerSweepOutcome,
    pub error: TranscriptIngestError,
}

pub type CursorComposerSweepResult = Result<CursorComposerSweepOutcome, CursorComposerSweepFailure>;

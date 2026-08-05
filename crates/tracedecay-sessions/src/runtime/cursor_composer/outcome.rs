use std::collections::HashSet;

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
}

impl CursorComposerSweepOutcome {
    pub(super) fn add_projection(&mut self, sessions: u64, messages: u64, deferred: bool) {
        self.sessions_upserted = self.sessions_upserted.saturating_add(sessions);
        self.messages_upserted = self.messages_upserted.saturating_add(messages);
        self.deferred_by_byte_cap |= deferred;
    }
}

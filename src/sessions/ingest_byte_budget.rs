/// Aggregate byte budget shared by bounded provider-ingest sweeps.
///
/// Bounded and unbounded construction is explicit. A bounded budget normally
/// treats exhaustion as deferral even for an empty charge; snapshot adapters
/// retain their historical behavior through [`Self::bounded_allowing_empty`].
#[derive(Debug)]
pub(crate) struct IngestByteBudget {
    remaining: Option<u64>,
    consumed: u64,
    deferred: bool,
    allow_empty_when_exhausted: bool,
}

impl IngestByteBudget {
    pub(crate) const fn bounded(limit: u64) -> Self {
        Self {
            remaining: Some(limit),
            consumed: 0,
            deferred: false,
            allow_empty_when_exhausted: false,
        }
    }

    pub(crate) const fn bounded_allowing_empty(limit: u64) -> Self {
        Self {
            remaining: Some(limit),
            consumed: 0,
            deferred: false,
            allow_empty_when_exhausted: true,
        }
    }

    pub(crate) const fn unbounded() -> Self {
        Self {
            remaining: None,
            consumed: 0,
            deferred: false,
            allow_empty_when_exhausted: true,
        }
    }

    pub(crate) const fn exhausted(&self) -> bool {
        matches!(self.remaining, Some(0))
    }

    pub(crate) const fn remaining(&self) -> Option<u64> {
        self.remaining
    }

    pub(crate) const fn consumed(&self) -> u64 {
        self.consumed
    }

    pub(crate) const fn deferred(&self) -> bool {
        self.deferred
    }

    pub(crate) const fn defer(&mut self) {
        self.deferred = true;
    }

    /// Records progress already admitted by a provider-specific reader.
    ///
    /// Readers receive [`Self::remaining`] as their cap, then report actual
    /// progress. Saturating subtraction preserves accounting if a reader
    /// reports beyond that cap, while unbounded budgets retain `None`.
    pub(crate) fn record_progress(&mut self, bytes: u64, deferred: bool) {
        if let Some(remaining) = &mut self.remaining {
            *remaining = remaining.saturating_sub(bytes);
        }
        self.consumed = self.consumed.saturating_add(bytes);
        self.deferred |= deferred;
    }

    pub(crate) fn try_consume(&mut self, bytes: u64) -> bool {
        let Some(remaining) = self.remaining else {
            self.consumed = self.consumed.saturating_add(bytes);
            return true;
        };
        if bytes > remaining || (remaining == 0 && !self.allow_empty_when_exhausted) {
            self.deferred = true;
            return false;
        }
        self.remaining = Some(remaining - bytes);
        self.consumed = self.consumed.saturating_add(bytes);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_budget_tracks_exact_fill_and_deferral() {
        let mut budget = IngestByteBudget::bounded(5);
        assert!(budget.try_consume(3));
        assert!(!budget.try_consume(3));
        assert!(budget.try_consume(2));
        assert!(budget.exhausted());
        assert_eq!(budget.remaining(), Some(0));
        assert_eq!(budget.consumed(), 5);
        assert!(budget.deferred());
    }

    #[test]
    fn exhausted_policy_preserves_snapshot_empty_charge_semantics() {
        let mut strict = IngestByteBudget::bounded(0);
        assert!(!strict.try_consume(0));
        assert!(strict.deferred());

        let mut snapshot = IngestByteBudget::bounded_allowing_empty(0);
        assert!(snapshot.try_consume(0));
        assert!(!snapshot.deferred());
    }

    #[test]
    fn unbounded_budget_saturates_consumption_without_deferral() {
        let mut budget = IngestByteBudget::unbounded();
        assert!(budget.try_consume(u64::MAX));
        assert!(budget.try_consume(1));
        assert_eq!(budget.remaining(), None);
        assert_eq!(budget.consumed(), u64::MAX);
        assert!(!budget.deferred());
    }

    #[test]
    fn progress_recording_preserves_reader_accounting() {
        let mut bounded = IngestByteBudget::bounded(5);
        bounded.record_progress(3, false);
        bounded.record_progress(4, true);
        bounded.record_progress(1, false);
        assert_eq!(bounded.remaining(), Some(0));
        assert_eq!(bounded.consumed(), 8);
        assert!(bounded.deferred());

        let mut unbounded = IngestByteBudget::unbounded();
        unbounded.record_progress(u64::MAX, false);
        unbounded.record_progress(1, false);
        assert_eq!(unbounded.remaining(), None);
        assert_eq!(unbounded.consumed(), u64::MAX);
        assert!(!unbounded.deferred());
    }
}

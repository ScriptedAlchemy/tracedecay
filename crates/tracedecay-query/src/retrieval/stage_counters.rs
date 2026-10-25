//! Work counters the composition-stage tests assert on: compact and fused sort
//! passes, comparator-record builds, ranking evidence normalization, and
//! diversity cap-key derivations.
//!
//! Outside `cfg(test)` every recorder is an empty inline function and no
//! counter state exists. Under test the counters are thread-local, so each
//! libtest thread observes only the compositions it ran itself.

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static COMPACT_SORTS: Cell<usize> = const { Cell::new(0) };
    static FUSED_SORTS: Cell<usize> = const { Cell::new(0) };
    static COMPARATOR_RECORDS: Cell<usize> = const { Cell::new(0) };
    static RANKING_EVIDENCE_NORMALIZATIONS: Cell<usize> = const { Cell::new(0) };
    static CAP_KEY_DERIVATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Work performed by the composition stages on the current thread since the
/// last [`reset`].
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct StageWork {
    pub compact_sorts: usize,
    pub fused_sorts: usize,
    pub comparator_records: usize,
    pub ranking_evidence_normalizations: usize,
    pub cap_key_derivations: usize,
}

#[cfg(test)]
pub(super) fn reset() {
    COMPACT_SORTS.with(|count| count.set(0));
    FUSED_SORTS.with(|count| count.set(0));
    COMPARATOR_RECORDS.with(|count| count.set(0));
    RANKING_EVIDENCE_NORMALIZATIONS.with(|count| count.set(0));
    CAP_KEY_DERIVATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn snapshot() -> StageWork {
    StageWork {
        compact_sorts: COMPACT_SORTS.with(Cell::get),
        fused_sorts: FUSED_SORTS.with(Cell::get),
        comparator_records: COMPARATOR_RECORDS.with(Cell::get),
        ranking_evidence_normalizations: RANKING_EVIDENCE_NORMALIZATIONS.with(Cell::get),
        cap_key_derivations: CAP_KEY_DERIVATIONS.with(Cell::get),
    }
}

#[cfg(test)]
fn bump(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    counter.with(|count| count.set(count.get() + 1));
}

#[inline]
pub(super) fn record_compact_sort() {
    #[cfg(test)]
    bump(&COMPACT_SORTS);
}

#[inline]
pub(super) fn record_fused_sort() {
    #[cfg(test)]
    bump(&FUSED_SORTS);
}

#[inline]
pub(super) fn record_comparator_record() {
    #[cfg(test)]
    bump(&COMPARATOR_RECORDS);
}

#[inline]
pub(super) fn record_ranking_evidence_normalization() {
    #[cfg(test)]
    bump(&RANKING_EVIDENCE_NORMALIZATIONS);
}

#[inline]
pub(super) fn record_cap_key_derivation() {
    #[cfg(test)]
    bump(&CAP_KEY_DERIVATIONS);
}

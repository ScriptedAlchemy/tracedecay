//! Bounded fair multi-source scheduling for host-admission intake.
//!
//! Queues are strictly bounded (per source and globally). Overflow is an explicit
//! backpressure outcome — never an unbounded buffer.

use std::collections::VecDeque;

/// Hard limits for the fair multi-source scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FairScheduleBounds {
    /// Maximum pending records across all sources.
    pub(crate) total_records: usize,
    /// Maximum pending records for any single source.
    pub(crate) records_per_source: usize,
    /// Maximum retained payload + source-key bytes for any single source.
    pub(crate) bytes_per_source: usize,
    /// Maximum payload bytes accepted for one enqueue.
    pub(crate) record_bytes: usize,
    /// Maximum UTF-8 bytes in one source key.
    pub(crate) source_bytes: usize,
    /// Maximum retained payload + source-key bytes.
    pub(crate) total_bytes: usize,
}

impl FairScheduleBounds {
    pub(crate) const fn with_byte_bounds(
        max_total_records: usize,
        max_records_per_source: usize,
        max_record_bytes: usize,
        max_source_bytes: usize,
        max_total_bytes: usize,
        max_bytes_per_source: usize,
    ) -> Self {
        Self {
            total_records: max_total_records,
            records_per_source: max_records_per_source,
            bytes_per_source: max_bytes_per_source,
            record_bytes: max_record_bytes,
            source_bytes: max_source_bytes,
            total_bytes: max_total_bytes,
        }
    }
}

/// Outcome of a bounded enqueue attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FairEnqueueOutcome {
    Accepted {
        total_pending: usize,
        source_pending: usize,
    },
    /// Global or per-source depth would be exceeded.
    Backpressured,
    /// Payload exceeds `max_record_bytes`.
    RecordTooLarge,
    /// Source key exceeds `max_source_bytes`.
    SourceTooLarge,
}

/// One fair-rotation pop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FairPop {
    pub(crate) seq: u64,
    pub(crate) source: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) total_pending: usize,
}

#[derive(Debug)]
struct ScheduledRecord {
    seq: u64,
    payload: Vec<u8>,
    retained_bytes: usize,
}

#[derive(Debug)]
struct SourceQueue {
    source: String,
    records: VecDeque<ScheduledRecord>,
    retained_bytes: usize,
}

/// Deterministic round-robin scheduler over bounded per-source queues.
///
/// Sources are visited in first-seen order. After a successful pop from source
/// `i`, the next search starts at `i + 1`, so no source can starve another while
/// both have pending work.
#[derive(Debug)]
pub(crate) struct FairSourceScheduler {
    bounds: FairScheduleBounds,
    /// Fair order: the front source is served next, then rotated to the back.
    queues: VecDeque<SourceQueue>,
    total_pending: usize,
    total_bytes: usize,
}

impl FairSourceScheduler {
    pub(crate) fn new(bounds: FairScheduleBounds) -> Self {
        Self {
            bounds,
            queues: VecDeque::new(),
            total_pending: 0,
            total_bytes: 0,
        }
    }

    pub(crate) fn total_pending(&self) -> usize {
        self.total_pending
    }

    #[cfg(test)]
    pub(crate) fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    #[cfg(test)]
    pub(crate) fn source_pending(&self, source: &str) -> usize {
        self.queues
            .iter()
            .find(|queue| queue.source == source)
            .map_or(0, |queue| queue.records.len())
    }

    #[cfg(test)]
    pub(crate) fn source_count(&self) -> usize {
        self.queues.len()
    }

    /// Enqueue a record for `source`, or return an explicit overflow disposition.
    #[cfg(test)]
    pub(crate) fn try_enqueue(&mut self, source: &str, payload: Vec<u8>) -> FairEnqueueOutcome {
        let retained_bytes = payload.len();
        self.try_enqueue_record(source, 0, payload, retained_bytes, false)
    }

    /// Enqueue a durable record reference while accounting for its retained payload.
    pub(crate) fn try_enqueue_reference(
        &mut self,
        source: &str,
        seq: u64,
        retained_bytes: usize,
    ) -> FairEnqueueOutcome {
        self.try_enqueue_record(source, seq, Vec::new(), retained_bytes, false)
    }

    /// Restore a lease at its source head without undoing global fair rotation.
    pub(crate) fn requeue_front_reference(
        &mut self,
        source: &str,
        seq: u64,
        retained_bytes: usize,
    ) -> FairEnqueueOutcome {
        self.try_enqueue_record(source, seq, Vec::new(), retained_bytes, true)
    }

    fn try_enqueue_record(
        &mut self,
        source: &str,
        seq: u64,
        payload: Vec<u8>,
        retained_bytes: usize,
        front: bool,
    ) -> FairEnqueueOutcome {
        if source.len() > self.bounds.source_bytes {
            return FairEnqueueOutcome::SourceTooLarge;
        }
        if retained_bytes > self.bounds.record_bytes {
            return FairEnqueueOutcome::RecordTooLarge;
        }
        if self.total_pending >= self.bounds.total_records {
            return FairEnqueueOutcome::Backpressured;
        }
        let existing = self.queues.iter().position(|queue| queue.source == source);
        let source_pending = existing.map_or(0, |index| self.queues[index].records.len());
        if source_pending >= self.bounds.records_per_source {
            return FairEnqueueOutcome::Backpressured;
        }
        let source_bytes = if existing.is_none() { source.len() } else { 0 };
        let Some(admitted_bytes) = retained_bytes.checked_add(source_bytes) else {
            return FairEnqueueOutcome::Backpressured;
        };
        let retained_by_source = existing.map_or(0, |index| self.queues[index].retained_bytes);
        let Some(next_source_bytes) = retained_by_source.checked_add(admitted_bytes) else {
            return FairEnqueueOutcome::Backpressured;
        };
        if next_source_bytes > self.bounds.bytes_per_source {
            return FairEnqueueOutcome::Backpressured;
        }
        let Some(next_total_bytes) = self.total_bytes.checked_add(admitted_bytes) else {
            return FairEnqueueOutcome::Backpressured;
        };
        if next_total_bytes > self.bounds.total_bytes {
            return FairEnqueueOutcome::Backpressured;
        }

        let record = ScheduledRecord {
            seq,
            payload,
            retained_bytes,
        };

        if let Some(index) = existing {
            self.queues[index].retained_bytes = next_source_bytes;
            if front {
                self.queues[index].records.push_front(record);
            } else {
                self.queues[index].records.push_back(record);
            }
        } else {
            self.queues.push_back(SourceQueue {
                source: source.to_string(),
                records: VecDeque::from([record]),
                retained_bytes: admitted_bytes,
            });
        }
        self.total_pending += 1;
        self.total_bytes = next_total_bytes;
        FairEnqueueOutcome::Accepted {
            total_pending: self.total_pending,
            source_pending: source_pending + 1,
        }
    }

    /// Pop the next record under fair rotation, or `None` when empty.
    pub(crate) fn pop_next(&mut self) -> Option<FairPop> {
        if self.total_pending == 0 {
            return None;
        }
        let mut queue = self.queues.pop_front()?;
        let record = queue.records.pop_front()?;
        let source = queue.source.clone();
        self.total_pending -= 1;
        self.total_bytes = self.total_bytes.saturating_sub(record.retained_bytes);
        if queue.records.is_empty() {
            self.total_bytes = self.total_bytes.saturating_sub(queue.source.len());
        } else {
            queue.retained_bytes = queue.retained_bytes.saturating_sub(record.retained_bytes);
            self.queues.push_back(queue);
        }
        Some(FairPop {
            seq: record.seq,
            source,
            payload: record.payload,
            total_pending: self.total_pending,
        })
    }

    /// Snapshot pending depths in first-seen source order (for tests/diagnostics).
    #[cfg(test)]
    pub(crate) fn pending_depths(&self) -> Vec<(String, usize)> {
        self.queues
            .iter()
            .map(|queue| (queue.source.clone(), queue.records.len()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> FairScheduleBounds {
        FairScheduleBounds::with_byte_bounds(4, 2, 16, 8, 48, 24)
    }

    #[test]
    fn fair_rotation_interleaves_sources_deterministically() {
        let mut scheduler = FairSourceScheduler::new(bounds());
        assert_eq!(
            scheduler.try_enqueue("a", b"a1".to_vec()),
            FairEnqueueOutcome::Accepted {
                total_pending: 1,
                source_pending: 1
            }
        );
        assert_eq!(
            scheduler.try_enqueue("b", b"b1".to_vec()),
            FairEnqueueOutcome::Accepted {
                total_pending: 2,
                source_pending: 1
            }
        );
        assert_eq!(
            scheduler.try_enqueue("a", b"a2".to_vec()),
            FairEnqueueOutcome::Accepted {
                total_pending: 3,
                source_pending: 2
            }
        );
        assert_eq!(
            scheduler.try_enqueue("b", b"b2".to_vec()),
            FairEnqueueOutcome::Accepted {
                total_pending: 4,
                source_pending: 2
            }
        );

        let pops: Vec<(String, Vec<u8>)> = (0..4)
            .map(|_| {
                let pop = scheduler.pop_next().expect("pending");
                (pop.source, pop.payload)
            })
            .collect();
        assert_eq!(
            pops,
            vec![
                ("a".into(), b"a1".to_vec()),
                ("b".into(), b"b1".to_vec()),
                ("a".into(), b"a2".to_vec()),
                ("b".into(), b"b2".to_vec()),
            ]
        );
        assert!(scheduler.pop_next().is_none());
        assert_eq!(scheduler.total_pending(), 0);
        assert_eq!(scheduler.total_bytes(), 0);
        assert_eq!(scheduler.source_count(), 0);
    }

    #[test]
    fn global_and_per_source_bounds_backpressure_without_growth() {
        let mut scheduler = FairSourceScheduler::new(bounds());
        assert!(matches!(
            scheduler.try_enqueue("a", b"1".to_vec()),
            FairEnqueueOutcome::Accepted { .. }
        ));
        assert!(matches!(
            scheduler.try_enqueue("a", b"2".to_vec()),
            FairEnqueueOutcome::Accepted { .. }
        ));
        assert_eq!(
            scheduler.try_enqueue("a", b"3".to_vec()),
            FairEnqueueOutcome::Backpressured
        );
        assert_eq!(scheduler.source_pending("a"), 2);
        assert_eq!(scheduler.total_pending(), 2);

        assert!(matches!(
            scheduler.try_enqueue("b", b"1".to_vec()),
            FairEnqueueOutcome::Accepted { .. }
        ));
        assert!(matches!(
            scheduler.try_enqueue("c", b"1".to_vec()),
            FairEnqueueOutcome::Accepted { .. }
        ));
        assert_eq!(
            scheduler.try_enqueue("d", b"1".to_vec()),
            FairEnqueueOutcome::Backpressured
        );
        assert_eq!(scheduler.total_pending(), 4);
        assert_eq!(
            scheduler.pending_depths(),
            vec![("a".into(), 2), ("b".into(), 1), ("c".into(), 1),]
        );
    }

    #[test]
    fn oversized_record_is_rejected_without_enqueue() {
        let mut scheduler = FairSourceScheduler::new(bounds());
        assert_eq!(
            scheduler.try_enqueue("a", vec![0u8; 17]),
            FairEnqueueOutcome::RecordTooLarge
        );
        assert_eq!(scheduler.total_pending(), 0);
        assert_eq!(scheduler.source_count(), 0);
    }

    #[test]
    fn fair_rotation_does_not_pin_on_empty_source() {
        let mut scheduler =
            FairSourceScheduler::new(FairScheduleBounds::with_byte_bounds(8, 4, 32, 8, 128, 64));
        scheduler.try_enqueue("a", b"a1".to_vec());
        scheduler.try_enqueue("b", b"b1".to_vec());
        scheduler.try_enqueue("c", b"c1".to_vec());
        let first = scheduler.pop_next().unwrap();
        assert_eq!(first.source, "a");
        // Drain b so the next fair step past a must skip the empty slot and land on c.
        let second = scheduler.pop_next().unwrap();
        assert_eq!(second.source, "b");
        let third = scheduler.pop_next().unwrap();
        assert_eq!(third.source, "c");
        scheduler.try_enqueue("a", b"a2".to_vec());
        scheduler.try_enqueue("c", b"c2".to_vec());
        // Cursor advanced past c; next search starts at a (wrap), then c.
        let fourth = scheduler.pop_next().unwrap();
        assert_eq!(fourth.source, "a");
        let fifth = scheduler.pop_next().unwrap();
        assert_eq!(fifth.source, "c");
    }

    #[test]
    fn source_and_total_byte_bounds_are_explicit() {
        let bounds = FairScheduleBounds::with_byte_bounds(8, 4, 8, 3, 8, 8);
        assert_eq!(bounds.total_bytes, 8);
        let mut scheduler = FairSourceScheduler::new(bounds);
        assert_eq!(
            scheduler.try_enqueue("long", b"x".to_vec()),
            FairEnqueueOutcome::SourceTooLarge
        );
        assert!(matches!(
            scheduler.try_enqueue("a", b"1234".to_vec()),
            FairEnqueueOutcome::Accepted { .. }
        ));
        assert_eq!(scheduler.total_bytes(), 5);
        assert_eq!(
            scheduler.try_enqueue("b", b"123".to_vec()),
            FairEnqueueOutcome::Backpressured
        );
        assert_eq!(scheduler.total_bytes(), 5);
    }

    #[test]
    fn per_source_byte_bound_preserves_capacity_for_another_source() {
        let mut scheduler =
            FairSourceScheduler::new(FairScheduleBounds::with_byte_bounds(8, 8, 16, 8, 32, 11));
        assert!(matches!(
            scheduler.try_enqueue("a", b"1234567890".to_vec()),
            FairEnqueueOutcome::Accepted { .. }
        ));
        assert_eq!(
            scheduler.try_enqueue("a", b"x".to_vec()),
            FairEnqueueOutcome::Backpressured
        );
        assert!(matches!(
            scheduler.try_enqueue("b", b"1234567890".to_vec()),
            FairEnqueueOutcome::Accepted { .. }
        ));
        assert_eq!(scheduler.total_pending(), 2);
    }

    #[test]
    fn sequential_unique_source_churn_releases_all_source_state() {
        let mut scheduler =
            FairSourceScheduler::new(FairScheduleBounds::with_byte_bounds(2, 1, 8, 16, 32, 24));
        for index in 0..10_000 {
            let source = format!("s{index}");
            assert!(matches!(
                scheduler.try_enqueue(&source, vec![index as u8]),
                FairEnqueueOutcome::Accepted { .. }
            ));
            let popped = scheduler.pop_next().expect("just enqueued");
            assert_eq!(popped.source, source);
            assert_eq!(scheduler.source_count(), 0);
            assert_eq!(scheduler.total_pending(), 0);
            assert_eq!(scheduler.total_bytes(), 0);
        }
    }
}

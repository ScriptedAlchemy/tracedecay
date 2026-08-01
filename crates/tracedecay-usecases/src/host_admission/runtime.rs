//! Runtime orchestration for the daemon-owned host-admission spool.
//!
//! This layer deliberately knows nothing about provider payload formats. The
//! daemon supplies a bounded, privacy-filtered envelope and later classifies it
//! as committed, exact duplicate, retryable, or durably quarantined terminal.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{
    FairEnqueueOutcome, FairScheduleBounds, FairSourceScheduler, HostAdmissionOutcome,
    HostAdmissionSpool, SpoolBounds, SpoolIntegrity, SpoolOpenReport, SpoolRecord, TerminalReason,
};

#[cfg(test)]
use super::HostAdmissionStatus;

pub(crate) const DEFAULT_MAX_REPLAY_RECORDS_PER_PASS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurableHostAdmission {
    pub seq: u64,
    pub outcome: HostAdmissionOutcome,
}

#[derive(Debug)]
pub struct HostAdmissionRuntime {
    spool: HostAdmissionSpool,
    scheduler: FairSourceScheduler,
    queued: BTreeSet<u64>,
    leased: BTreeSet<u64>,
    completed: BTreeSet<u64>,
    #[cfg(test)]
    max_replay_records_per_pass: usize,
}

impl HostAdmissionRuntime {
    pub fn open_for_database(
        database_path: &Path,
    ) -> Result<(Self, SpoolOpenReport), HostAdmissionOutcome> {
        let parent = database_path
            .parent()
            .ok_or_else(HostAdmissionOutcome::spool_corrupted)?;
        let name = database_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(HostAdmissionOutcome::spool_corrupted)?;
        Self::open(
            parent.join(format!(".{name}.host-admission")),
            SpoolBounds::default(),
        )
    }

    pub fn open(
        dir: impl Into<PathBuf>,
        bounds: SpoolBounds,
    ) -> Result<(Self, SpoolOpenReport), HostAdmissionOutcome> {
        Self::open_with_replay_limit(dir, bounds, DEFAULT_MAX_REPLAY_RECORDS_PER_PASS)
    }

    fn open_with_replay_limit(
        dir: impl Into<PathBuf>,
        bounds: SpoolBounds,
        max_replay_records_per_pass: usize,
    ) -> Result<(Self, SpoolOpenReport), HostAdmissionOutcome> {
        if max_replay_records_per_pass == 0 {
            return Err(HostAdmissionOutcome::spool_corrupted());
        }
        let (spool, report) =
            HostAdmissionSpool::open(dir, bounds).map_err(|error| error.to_outcome())?;
        if !matches!(report.integrity, SpoolIntegrity::Healthy) {
            return Err(HostAdmissionOutcome::spool_corrupted());
        }
        let mut runtime = Self {
            spool,
            scheduler: FairSourceScheduler::new(runtime_schedule_bounds(bounds)),
            queued: BTreeSet::new(),
            leased: BTreeSet::new(),
            completed: BTreeSet::new(),
            #[cfg(test)]
            max_replay_records_per_pass,
        };
        runtime.schedule_missing()?;
        Ok((runtime, report))
    }

    /// Durably appends before returning acceptance to the daemon caller.
    pub(crate) fn admit(
        &mut self,
        source: &str,
        payload: &[u8],
    ) -> Result<DurableHostAdmission, HostAdmissionOutcome> {
        let record = self
            .spool
            .append(source, payload)
            .map_err(|error| error.to_outcome())?;
        self.schedule_record(&record)?;
        Ok(DurableHostAdmission {
            seq: record.seq,
            outcome: HostAdmissionOutcome::accepted_for_replay(),
        })
    }

    /// Lease one fair durable record without deleting it from the spool.
    pub(crate) fn try_lease_next(&mut self) -> Result<Option<SpoolRecord>, HostAdmissionOutcome> {
        if self.scheduler.total_pending() == 0 {
            self.schedule_missing()?;
        }
        while let Some(next) = self.scheduler.pop_next() {
            self.queued.remove(&next.seq);
            if self.completed.contains(&next.seq) || !self.leased.insert(next.seq) {
                continue;
            }
            let Some(record) = self.spool.pending_record(next.seq) else {
                self.leased.remove(&next.seq);
                return Err(HostAdmissionOutcome::spool_corrupted());
            };
            return Ok(Some(record.clone()));
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(crate) fn lease_next(&mut self) -> Option<SpoolRecord> {
        self.try_lease_next().expect("lease scheduler")
    }

    /// Requeue a cancelled or retryable lease; durable bytes never leave the spool.
    pub(crate) fn defer(&mut self, seq: u64) -> Result<(), HostAdmissionOutcome> {
        if !self.leased.contains(&seq) {
            return Err(HostAdmissionOutcome::spool_ack_conflict());
        }
        let Some(record) = self.spool.pending_record(seq).cloned() else {
            return Err(HostAdmissionOutcome::spool_ack_conflict());
        };
        self.leased.remove(&seq);
        self.schedule_record_front(&record)
    }

    /// Requeue leases abandoned when a replay task was cancelled or dropped.
    /// The daemon replay mutex guarantees no live worker owns them at this boundary.
    pub(crate) fn recover_leases(&mut self) -> Result<usize, HostAdmissionOutcome> {
        self.spool
            .ensure_replay_allowed()
            .map_err(|error| error.to_outcome())?;
        let count = self.leased.len();
        if count > 0 {
            self.leased.clear();
            self.rebuild_scheduler()?;
        }
        Ok(count)
    }

    /// Mark an authoritative lease and delete only the contiguous completed prefix.
    pub(crate) fn commit(&mut self, seq: u64) -> Result<usize, HostAdmissionOutcome> {
        if seq <= self.spool.committed_through() {
            return Ok(0);
        }
        if !self.leased.remove(&seq) {
            return Err(HostAdmissionOutcome::spool_ack_conflict());
        }
        self.completed.insert(seq);
        self.flush_completed_prefix()
    }

    /// Resolve a permanent terminal lease without reporting canonical success.
    ///
    /// The spool publishes the full checksummed frame and typed reason before
    /// active capacity is reclaimed.
    pub(crate) fn quarantine(
        &mut self,
        seq: u64,
        reason: TerminalReason,
    ) -> Result<usize, HostAdmissionOutcome> {
        if !self.leased.contains(&seq) {
            return Err(HostAdmissionOutcome::spool_ack_conflict());
        }
        self.spool
            .quarantine(seq, reason)
            .map_err(|error| error.to_outcome())?;
        self.leased.remove(&seq);
        self.queued.remove(&seq);
        self.completed.remove(&seq);
        let committed = self.flush_completed_prefix()?;
        self.rebuild_scheduler()?;
        Ok(committed.saturating_add(1))
    }

    fn flush_completed_prefix(&mut self) -> Result<usize, HostAdmissionOutcome> {
        let through = self
            .spool
            .pending_records()
            .iter()
            .take_while(|record| self.completed.contains(&record.seq))
            .map(|record| record.seq)
            .last();
        let Some(through) = through else {
            return Ok(0);
        };
        let committed = match self.spool.ack_through(through) {
            Ok(committed) => committed,
            Err(error) => {
                self.completed.clear();
                self.rebuild_scheduler()?;
                return Err(error.to_outcome());
            }
        };
        self.completed.retain(|candidate| *candidate > through);
        self.queued.retain(|candidate| *candidate > through);
        self.leased.retain(|candidate| *candidate > through);
        Ok(committed)
    }

    /// Produces one bounded, deterministic round-robin replay pass.
    ///
    /// The spool remains authoritative: scheduler pops select work but never
    /// remove durable records. A failed source therefore cannot prevent another
    /// source from making canonical progress, while acknowledgement still
    /// respects the spool's global commit watermark.
    #[cfg(test)]
    pub(crate) fn fair_replay_batch(&self) -> Result<Vec<SpoolRecord>, HostAdmissionOutcome> {
        self.spool
            .ensure_replay_allowed()
            .map_err(|error| error.to_outcome())?;
        let pending = self.spool.pending_records();
        if pending.is_empty() {
            return Ok(Vec::new());
        }

        let total = pending.len();
        let bounds = self.spool.bounds();
        let mut scheduler = FairSourceScheduler::new(FairScheduleBounds::with_byte_bounds(
            total,
            bounds.max_records_per_source,
            bounds.max_record_bytes,
            bounds.max_source_bytes,
            bounds.max_spool_bytes,
            bounds.max_spool_bytes_per_source,
        ));
        for record in pending {
            match scheduler.try_enqueue_reference(&record.source, record.seq, record.payload.len())
            {
                FairEnqueueOutcome::Accepted { .. } => {}
                FairEnqueueOutcome::RecordTooLarge | FairEnqueueOutcome::SourceTooLarge => {
                    return Err(HostAdmissionOutcome::spool_corrupted());
                }
                FairEnqueueOutcome::Backpressured => {
                    return Err(HostAdmissionOutcome::spool_overflow());
                }
            }
        }

        let mut selected = Vec::with_capacity(total.min(self.max_replay_records_per_pass));
        while selected.len() < self.max_replay_records_per_pass {
            let Some(next) = scheduler.pop_next() else {
                break;
            };
            let Some(record) = pending.iter().find(|record| record.seq == next.seq) else {
                return Err(HostAdmissionOutcome::spool_corrupted());
            };
            selected.push(record.clone());
        }
        Ok(selected)
    }

    fn schedule_missing(&mut self) -> Result<(), HostAdmissionOutcome> {
        let missing = self
            .spool
            .pending_records()
            .iter()
            .filter(|record| {
                !self.queued.contains(&record.seq)
                    && !self.leased.contains(&record.seq)
                    && !self.completed.contains(&record.seq)
            })
            .cloned()
            .collect::<Vec<_>>();
        for record in missing {
            self.schedule_record(&record)?;
        }
        Ok(())
    }

    fn rebuild_scheduler(&mut self) -> Result<(), HostAdmissionOutcome> {
        self.scheduler = FairSourceScheduler::new(runtime_schedule_bounds(self.spool.bounds()));
        self.queued.clear();
        self.schedule_missing()
    }

    fn schedule_record(&mut self, record: &SpoolRecord) -> Result<(), HostAdmissionOutcome> {
        let outcome =
            self.scheduler
                .try_enqueue_reference(&record.source, record.seq, record.payload.len());
        self.finish_schedule(record.seq, outcome)
    }

    fn schedule_record_front(&mut self, record: &SpoolRecord) -> Result<(), HostAdmissionOutcome> {
        let outcome = self.scheduler.requeue_front_reference(
            &record.source,
            record.seq,
            record.payload.len(),
        );
        self.finish_schedule(record.seq, outcome)
    }

    fn finish_schedule(
        &mut self,
        seq: u64,
        outcome: FairEnqueueOutcome,
    ) -> Result<(), HostAdmissionOutcome> {
        match outcome {
            FairEnqueueOutcome::Accepted { .. } => {
                self.queued.insert(seq);
                Ok(())
            }
            FairEnqueueOutcome::RecordTooLarge | FairEnqueueOutcome::SourceTooLarge => {
                Err(HostAdmissionOutcome::spool_corrupted())
            }
            FairEnqueueOutcome::Backpressured => Err(HostAdmissionOutcome::spool_overflow()),
        }
    }

    /// Deletes a record only after canonical commit or exact duplicate.
    ///
    /// Every other disposition leaves the frame durable for a later pass.
    #[cfg(test)]
    pub(crate) fn acknowledge(
        &mut self,
        seq: u64,
        canonical_outcome: HostAdmissionOutcome,
    ) -> HostAdmissionOutcome {
        if !matches!(
            canonical_outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ) {
            return canonical_outcome;
        }
        match self.spool.ack(seq) {
            Ok(_) => canonical_outcome,
            Err(error) => error.to_outcome(),
        }
    }

    pub(super) fn pending_count(&self) -> usize {
        self.spool.pending_count()
    }

    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    pub(super) fn quarantine_count(&self) -> usize {
        self.spool.quarantine_count()
    }
}

fn runtime_schedule_bounds(bounds: SpoolBounds) -> FairScheduleBounds {
    FairScheduleBounds::with_byte_bounds(
        bounds.max_records,
        bounds.max_records_per_source,
        bounds.max_record_bytes,
        bounds.max_source_bytes,
        bounds.max_spool_bytes,
        bounds.max_spool_bytes_per_source,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn bounds() -> SpoolBounds {
        SpoolBounds::new(256, 32, 4096, 16)
    }

    fn open(temp: &TempDir) -> HostAdmissionRuntime {
        HostAdmissionRuntime::open(temp.path(), bounds()).unwrap().0
    }

    #[test]
    fn append_is_durable_before_attempt_and_commit_deletes_afterward() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let admitted = runtime.admit("claude", b"event-one").unwrap();

        assert_eq!(
            admitted.outcome,
            HostAdmissionOutcome::accepted_for_replay()
        );
        assert_eq!(runtime.pending_count(), 1);
        assert!(temp.path().join("records.bin").metadata().unwrap().len() > 0);

        let attempted = runtime.fair_replay_batch().unwrap();
        assert_eq!(attempted.len(), 1);
        assert_eq!(attempted[0].seq, admitted.seq);
        assert_eq!(
            runtime.acknowledge(
                admitted.seq,
                HostAdmissionOutcome::replay_completed(true, false),
            ),
            HostAdmissionOutcome::replay_completed(true, false),
        );
        assert_eq!(runtime.pending_count(), 0);
        assert_eq!(
            HostAdmissionRuntime::open(temp.path(), bounds())
                .unwrap()
                .0
                .pending_count(),
            0
        );
    }

    #[test]
    fn identical_envelopes_remain_distinct_durable_admissions() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);

        let first = runtime.admit("claude", b"same-envelope").unwrap();
        let second = runtime.admit("claude", b"same-envelope").unwrap();

        assert_ne!(first.seq, second.seq);
        assert_eq!(runtime.pending_count(), 2);
        assert_eq!(
            runtime
                .fair_replay_batch()
                .unwrap()
                .iter()
                .map(|record| record.seq)
                .collect::<Vec<_>>(),
            [first.seq, second.seq]
        );
    }

    #[test]
    fn unavailable_backpressured_and_cancelled_attempts_remain_durable() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let admitted = runtime.admit("codex", b"retry-me").unwrap();

        for outcome in [
            HostAdmissionOutcome::retained_unavailable("authority_unavailable"),
            HostAdmissionOutcome::retained_backpressured("daemon_backpressure"),
            HostAdmissionOutcome::retained_backpressured("admission_cancelled"),
        ] {
            assert_eq!(runtime.acknowledge(admitted.seq, outcome), outcome);
            assert_eq!(runtime.pending_count(), 1);
        }

        drop(runtime);
        let reopened = open(&temp);
        assert_eq!(reopened.pending_count(), 1);
        assert_eq!(reopened.fair_replay_batch().unwrap()[0].seq, admitted.seq);
    }

    #[test]
    fn recovery_replays_commit_before_ack_as_exact_duplicate_once() {
        let temp = TempDir::new().unwrap();
        let seq = {
            let mut runtime = open(&temp);
            runtime.admit("cursor", b"commit-crash-window").unwrap().seq
        };

        let mut restarted = open(&temp);
        let recovered = restarted.fair_replay_batch().unwrap();
        assert_eq!(
            recovered
                .iter()
                .map(|record| record.seq)
                .collect::<Vec<_>>(),
            [seq]
        );
        assert_eq!(
            restarted.acknowledge(seq, HostAdmissionOutcome::replay_completed(false, true),),
            HostAdmissionOutcome::replay_completed(false, true),
        );
        assert_eq!(restarted.pending_count(), 0);
        assert!(open(&temp).fair_replay_batch().unwrap().is_empty());
    }

    #[test]
    fn fair_replay_is_bounded_and_rotates_sources() {
        let temp = TempDir::new().unwrap();
        let mut runtime = HostAdmissionRuntime::open_with_replay_limit(temp.path(), bounds(), 3)
            .unwrap()
            .0;
        for (source, payload) in [
            ("a", b"a1".as_slice()),
            ("b", b"b1".as_slice()),
            ("a", b"a2".as_slice()),
            ("b", b"b2".as_slice()),
        ] {
            runtime.admit(source, payload).unwrap();
        }

        let batch = runtime.fair_replay_batch().unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(
            batch
                .iter()
                .map(|record| (record.source.as_str(), record.payload.as_slice()))
                .collect::<Vec<_>>(),
            [
                ("a", b"a1".as_slice()),
                ("b", b"b1".as_slice()),
                ("a", b"a2".as_slice())
            ]
        );
    }

    #[test]
    fn deferred_head_preserves_source_order_and_flushes_completed_prefix() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let first = runtime.admit("a", b"a1").unwrap();
        let second = runtime.admit("b", b"b1").unwrap();
        let third = runtime.admit("a", b"a2").unwrap();

        assert_eq!(runtime.lease_next().unwrap().seq, first.seq);
        assert_eq!(runtime.lease_next().unwrap().seq, second.seq);
        assert_eq!(runtime.commit(second.seq).unwrap(), 0);
        runtime.defer(first.seq).unwrap();

        assert_eq!(
            runtime.lease_next().unwrap().seq,
            first.seq,
            "retry must stay ahead of the later record from the same source"
        );
        assert_eq!(runtime.commit(first.seq).unwrap(), 2);
        assert_eq!(runtime.lease_next().unwrap().seq, third.seq);
        assert_eq!(runtime.commit(third.seq).unwrap(), 1);
        assert_eq!(runtime.pending_count(), 0);
    }

    #[test]
    fn deferred_source_rotates_behind_other_sources_without_reordering_itself() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let first = runtime.admit("a", b"a1").unwrap();
        let second = runtime.admit("a", b"a2").unwrap();
        let other = runtime.admit("b", b"b1").unwrap();

        assert_eq!(runtime.lease_next().unwrap().seq, first.seq);
        runtime.defer(first.seq).unwrap();
        assert_eq!(runtime.lease_next().unwrap().seq, other.seq);
        assert_eq!(runtime.lease_next().unwrap().seq, first.seq);
        runtime.defer(first.seq).unwrap();
        assert_eq!(runtime.lease_next().unwrap().seq, first.seq);
        assert_eq!(runtime.commit(first.seq).unwrap(), 1);
        assert_eq!(runtime.lease_next().unwrap().seq, second.seq);
    }

    #[test]
    fn cancelled_replay_recovers_every_lease_without_losing_fair_order() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let first = runtime.admit("a", b"a1").unwrap();
        let second = runtime.admit("b", b"b1").unwrap();
        let third = runtime.admit("a", b"a2").unwrap();

        assert_eq!(runtime.lease_next().unwrap().seq, first.seq);
        assert_eq!(runtime.lease_next().unwrap().seq, second.seq);
        assert_eq!(runtime.recover_leases().unwrap(), 2);
        assert_eq!(runtime.recover_leases().unwrap(), 0);

        assert_eq!(runtime.lease_next().unwrap().seq, first.seq);
        assert_eq!(runtime.lease_next().unwrap().seq, second.seq);
        assert_eq!(runtime.lease_next().unwrap().seq, third.seq);
        assert_eq!(runtime.pending_count(), 3);
    }

    #[test]
    fn restart_recovers_a_dropped_lease_from_durable_bytes() {
        let temp = TempDir::new().unwrap();
        let admitted = {
            let mut runtime = open(&temp);
            let admitted = runtime.admit("claude", b"cancelled").unwrap();
            assert_eq!(runtime.lease_next().unwrap().seq, admitted.seq);
            admitted
        };

        let mut restarted = open(&temp);
        assert_eq!(restarted.lease_next().unwrap().seq, admitted.seq);
        assert_eq!(restarted.pending_count(), 1);
    }

    #[test]
    fn overflow_and_corruption_surface_stable_dispositions() {
        let temp = TempDir::new().unwrap();
        let bounded = SpoolBounds::new(4, 8, 128, 1);
        let mut runtime = HostAdmissionRuntime::open(temp.path(), bounded).unwrap().0;
        assert_eq!(
            runtime.admit("a", b"12345").unwrap_err(),
            HostAdmissionOutcome::spool_record_too_large(),
        );
        runtime.admit("a", b"1234").unwrap();
        assert_eq!(
            runtime.admit("b", b"x").unwrap_err(),
            HostAdmissionOutcome::spool_overflow(),
        );

        let first = runtime.spool.pending_records()[0].clone();
        drop(runtime);
        let records_path = temp.path().join("records.bin");
        let mut bytes = fs::read(&records_path).unwrap();
        bytes[first.file_offset as usize + first.framed_len - 1] ^= 1;
        fs::write(records_path, bytes).unwrap();
        assert_eq!(
            HostAdmissionRuntime::open(temp.path(), bounded).unwrap_err(),
            HostAdmissionOutcome::spool_corrupted(),
        );
    }

    #[test]
    fn unsupported_spool_version_stays_distinct_at_runtime_boundary() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("meta.json"),
            br#"{"version":2,"committed_through":0,"next_seq":1,"integrity":"healthy"}"#,
        )
        .unwrap();

        assert_eq!(
            HostAdmissionRuntime::open(temp.path(), bounds()).unwrap_err(),
            HostAdmissionOutcome::spool_unsupported_version()
        );
    }

    #[test]
    fn lease_scheduler_failure_is_typed_instead_of_clean_exhaustion() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        runtime.admit("a", b"durable").unwrap();
        runtime.scheduler = FairSourceScheduler::new(FairScheduleBounds::with_byte_bounds(
            0, 0, 256, 32, 4096, 4096,
        ));
        runtime.queued.clear();

        assert_eq!(
            runtime.try_lease_next().unwrap_err(),
            HostAdmissionOutcome::spool_overflow()
        );
        assert_eq!(runtime.pending_count(), 1);
    }

    #[test]
    fn stale_scheduler_reference_is_typed_instead_of_clean_exhaustion() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        assert!(matches!(
            runtime.scheduler.try_enqueue_reference("a", 999, 1),
            FairEnqueueOutcome::Accepted { .. }
        ));
        runtime.queued.insert(999);

        assert_eq!(
            runtime.try_lease_next().unwrap_err(),
            HostAdmissionOutcome::spool_corrupted()
        );
    }

    #[test]
    fn out_of_order_success_is_retained_as_ack_conflict() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let first = runtime.admit("a", b"blocked").unwrap();
        let second = runtime.admit("b", b"committed").unwrap();

        assert_eq!(
            runtime.acknowledge(
                second.seq,
                HostAdmissionOutcome::replay_completed(true, false),
            ),
            HostAdmissionOutcome::spool_ack_conflict(),
        );
        assert_eq!(runtime.pending_count(), 2);
        assert_eq!(
            runtime
                .acknowledge(
                    first.seq,
                    HostAdmissionOutcome::replay_completed(true, false),
                )
                .status,
            HostAdmissionStatus::Committed,
        );
        assert_eq!(
            runtime
                .acknowledge(
                    second.seq,
                    HostAdmissionOutcome::replay_completed(false, true),
                )
                .status,
            HostAdmissionStatus::ExactDuplicate,
        );
        assert_eq!(runtime.pending_count(), 0);
    }

    #[test]
    fn terminal_quarantine_flushes_completed_sibling_without_success_status() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let terminal = runtime.admit("a", b"terminal").unwrap();
        let sibling = runtime.admit("b", b"committed").unwrap();

        assert_eq!(runtime.lease_next().unwrap().seq, terminal.seq);
        assert_eq!(runtime.lease_next().unwrap().seq, sibling.seq);
        assert_eq!(runtime.commit(sibling.seq).unwrap(), 0);
        assert_eq!(
            runtime
                .quarantine(terminal.seq, TerminalReason::MalformedPayload)
                .unwrap(),
            2
        );
        assert_eq!(runtime.pending_count(), 0);
        assert_eq!(runtime.quarantine_count(), 1);

        let reopened = open(&temp);
        assert_eq!(reopened.pending_count(), 0);
        assert_eq!(reopened.quarantine_count(), 1);
    }
}

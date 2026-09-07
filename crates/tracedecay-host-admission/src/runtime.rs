//! Runtime orchestration for the daemon-owned host-admission spool.
//!
//! This layer deliberately knows nothing about provider payload formats. The
//! daemon supplies a bounded, privacy-filtered envelope and later classifies it
//! as committed, exact duplicate, retryable, or durably quarantined terminal.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tracedecay_domain::errors::TraceDecayError;

use tracedecay_sessions::admission::HostAdmissionOutcome;
#[cfg(test)]
use tracedecay_sessions::admission::HostAdmissionStatus;

#[cfg(test)]
use super::take_cloned_payload_bytes;
use super::{
    FairEnqueueOutcome, FairScheduleBounds, FairSourceScheduler, HostAdmissionSpool, SpoolBounds,
    SpoolError, SpoolIntegrity, SpoolOpenReport, SpoolRecord, TerminalReason,
};

#[derive(Clone, Debug, PartialEq, Eq)]
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
}

impl HostAdmissionRuntime {
    pub fn open_for_database(
        database_path: &Path,
    ) -> Result<(Self, SpoolOpenReport), TraceDecayError> {
        let parent = database_path
            .parent()
            .ok_or_else(|| SpoolError::MetadataCorrupted.to_open_error())?;
        let name = database_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| SpoolError::MetadataCorrupted.to_open_error())?;
        Self::open(
            parent.join(format!(".{name}.host-admission")),
            SpoolBounds::default(),
        )
    }

    pub fn open(
        dir: impl Into<PathBuf>,
        bounds: SpoolBounds,
    ) -> Result<(Self, SpoolOpenReport), TraceDecayError> {
        let (spool, report) = hotpath::measure_block!("usecases.admission.open", {
            HostAdmissionSpool::open(dir, bounds)
        })
        .map_err(|error| error.to_open_error())?;
        if !matches!(report.integrity, SpoolIntegrity::Healthy) {
            return Err(SpoolError::MetadataCorrupted.to_open_error());
        }
        let mut runtime = Self {
            spool,
            scheduler: FairSourceScheduler::new(runtime_schedule_bounds(bounds)),
            queued: BTreeSet::new(),
            leased: BTreeSet::new(),
            completed: BTreeSet::new(),
        };
        runtime.schedule_missing().map_err(open_outcome_error)?;
        Ok((runtime, report))
    }

    /// Durably appends before returning acceptance to the daemon caller.
    pub(crate) fn admit(
        &mut self,
        source: &str,
        payload: &[u8],
    ) -> Result<DurableHostAdmission, HostAdmissionOutcome> {
        let record = hotpath::measure_block!("usecases.admission.admit", {
            self.spool.append(source, payload)
        })
        .map_err(|error| error.to_outcome())?;
        hotpath::measure_block!("usecases.admission.schedule", {
            self.schedule_record(&record)
        })?;
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
        let Some(record) = self.spool.pending_record(seq) else {
            return Err(HostAdmissionOutcome::spool_ack_conflict());
        };
        self.leased.remove(&seq);
        let outcome = self.scheduler.requeue_front_reference(
            &record.source,
            record.seq,
            record.payload.len(),
        );
        finish_schedule(&mut self.queued, seq, outcome)
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

    /// Enqueue every retained record the scheduler does not yet track.
    ///
    /// The spool already owns the pending payloads and the scheduler stores only
    /// `(source, seq, len)`, so this reads the spool's records in place rather
    /// than copying the pending byte volume a second time.
    fn schedule_missing(&mut self) -> Result<(), HostAdmissionOutcome> {
        for record in self.spool.pending_records() {
            if self.queued.contains(&record.seq)
                || self.leased.contains(&record.seq)
                || self.completed.contains(&record.seq)
            {
                continue;
            }
            let outcome = self.scheduler.try_enqueue_reference(
                &record.source,
                record.seq,
                record.payload.len(),
            );
            finish_schedule(&mut self.queued, record.seq, outcome)?;
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
        finish_schedule(&mut self.queued, record.seq, outcome)
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

fn finish_schedule(
    queued: &mut BTreeSet<u64>,
    seq: u64,
    outcome: FairEnqueueOutcome,
) -> Result<(), HostAdmissionOutcome> {
    match outcome {
        FairEnqueueOutcome::Accepted { .. } => {
            queued.insert(seq);
            Ok(())
        }
        FairEnqueueOutcome::RecordTooLarge | FairEnqueueOutcome::SourceTooLarge => {
            Err(HostAdmissionOutcome::spool_corrupted())
        }
        FairEnqueueOutcome::Backpressured => Err(HostAdmissionOutcome::spool_overflow()),
    }
}

fn open_outcome_error(outcome: HostAdmissionOutcome) -> TraceDecayError {
    TraceDecayError::hook_runtime_with_status(
        outcome.reason_code.unwrap_or("spool_unavailable"),
        outcome.retryable,
        "host-admission spool open failed",
        outcome.status.as_wire(),
    )
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

    fn assert_reset_required(error: &TraceDecayError) {
        let (authority, reason) = error
            .reset_required_context()
            .expect("future spool shape must require typed reset");
        assert_eq!(authority, "host-admission spool");
        assert!(reason.contains("version 2"));
        assert!(error.hook_runtime_context().is_none());
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

        let leased = runtime.lease_next().unwrap();
        assert_eq!(leased.seq, admitted.seq);
        assert_eq!(
            leased.payload, b"event-one",
            "the lease carries the real spool payload"
        );
        assert!(runtime.lease_next().is_none());
        assert_eq!(runtime.commit(admitted.seq).unwrap(), 1);
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
        assert_eq!(runtime.lease_next().unwrap().seq, first.seq);
        assert_eq!(runtime.lease_next().unwrap().seq, second.seq);
        assert!(runtime.lease_next().is_none());
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
            assert_eq!(runtime.acknowledge(admitted.seq, outcome.clone()), outcome);
            assert_eq!(runtime.pending_count(), 1);
        }

        drop(runtime);
        let mut reopened = open(&temp);
        assert_eq!(reopened.pending_count(), 1);
        assert_eq!(reopened.lease_next().unwrap().seq, admitted.seq);
    }

    #[test]
    fn recovery_replays_commit_before_ack_as_exact_duplicate_once() {
        let temp = TempDir::new().unwrap();
        let seq = {
            let mut runtime = open(&temp);
            runtime.admit("cursor", b"commit-crash-window").unwrap().seq
        };

        let mut restarted = open(&temp);
        assert_eq!(restarted.lease_next().unwrap().seq, seq);
        assert!(restarted.lease_next().is_none());
        assert_eq!(
            restarted.acknowledge(seq, HostAdmissionOutcome::replay_completed(false, true),),
            HostAdmissionOutcome::replay_completed(false, true),
        );
        assert_eq!(restarted.pending_count(), 0);
        assert!(open(&temp).lease_next().is_none());
    }

    #[test]
    fn leases_rotate_sources_and_carry_the_spool_payload() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        for (source, payload) in [
            ("a", b"a1".as_slice()),
            ("b", b"b1".as_slice()),
            ("a", b"a2".as_slice()),
            ("b", b"b2".as_slice()),
        ] {
            runtime.admit(source, payload).unwrap();
        }

        let leased: Vec<SpoolRecord> = std::iter::from_fn(|| runtime.lease_next()).collect();
        assert_eq!(
            leased
                .iter()
                .map(|record| (record.source.as_str(), record.payload.as_slice()))
                .collect::<Vec<_>>(),
            [
                ("a", b"a1".as_slice()),
                ("b", b"b1".as_slice()),
                ("a", b"a2".as_slice()),
                ("b", b"b2".as_slice()),
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

    fn large_record_bounds() -> SpoolBounds {
        SpoolBounds::new(64 * 1024, 32, 1024 * 1024, 64)
    }

    const LARGE_PAYLOAD_LEN: usize = 8 * 1024;

    /// Uneven source mix so fair order differs from sequence order:
    /// `a` owns six records, `b` and `c` three each.
    const LARGE_RECORD_SOURCES: [&str; 12] =
        ["a", "a", "a", "b", "b", "c", "a", "c", "b", "a", "c", "a"];

    /// Fresh round-robin over first-seen sources, as indices into the admitted
    /// sequences: a b c a b c a b c a a a.
    const FRESH_FAIR_ORDER: [usize; 12] = [0, 3, 5, 1, 4, 7, 2, 8, 10, 6, 9, 11];

    fn admit_large_records(runtime: &mut HostAdmissionRuntime) -> Vec<u64> {
        let payload = vec![0xA5u8; LARGE_PAYLOAD_LEN];
        LARGE_RECORD_SOURCES
            .iter()
            .map(|source| runtime.admit(source, &payload).unwrap().seq)
            .collect()
    }

    fn lease_all(runtime: &mut HostAdmissionRuntime) -> Vec<u64> {
        std::iter::from_fn(|| runtime.lease_next())
            .map(|record| record.seq)
            .collect()
    }

    #[test]
    fn opening_and_rebuilding_fair_queues_copies_no_payload_bytes() {
        let temp = TempDir::new().unwrap();
        let seqs = {
            let mut runtime = HostAdmissionRuntime::open(temp.path(), large_record_bounds())
                .unwrap()
                .0;
            admit_large_records(&mut runtime)
        };
        let retained_bytes = LARGE_PAYLOAD_LEN * seqs.len();
        let fair_order = FRESH_FAIR_ORDER.map(|index| seqs[index]);

        take_cloned_payload_bytes();
        let mut runtime = HostAdmissionRuntime::open(temp.path(), large_record_bounds())
            .unwrap()
            .0;
        assert_eq!(runtime.pending_count(), seqs.len());
        assert_eq!(
            take_cloned_payload_bytes(),
            0,
            "open must schedule {retained_bytes} retained payload bytes by reference"
        );

        assert_eq!(lease_all(&mut runtime), fair_order);
        assert_eq!(
            take_cloned_payload_bytes(),
            retained_bytes,
            "each lease materializes exactly its own payload"
        );

        assert_eq!(runtime.recover_leases().unwrap(), seqs.len());
        assert_eq!(
            take_cloned_payload_bytes(),
            0,
            "abandoned-lease recovery must rebuild {} references without payload copies",
            seqs.len()
        );
        assert_eq!(lease_all(&mut runtime), fair_order);
        assert_eq!(runtime.recover_leases().unwrap(), seqs.len());

        let head = runtime.lease_next().unwrap();
        assert_eq!(head.seq, fair_order[0]);
        take_cloned_payload_bytes();
        assert_eq!(
            runtime
                .quarantine(head.seq, TerminalReason::MalformedPayload)
                .unwrap(),
            1
        );
        assert_eq!(
            take_cloned_payload_bytes(),
            0,
            "quarantine must republish the frame and rebuild {} references without payload copies",
            seqs.len() - 1
        );
        assert_eq!(runtime.quarantine_count(), 1);
        // A rebuild restarts rotation over the remaining records: a b c a b c a b c a a.
        let remaining = [1, 3, 5, 2, 4, 7, 6, 8, 10, 9, 11].map(|index| seqs[index]);
        assert_eq!(lease_all(&mut runtime), remaining);
    }

    #[test]
    fn deferring_a_large_lease_copies_no_payload_bytes() {
        let temp = TempDir::new().unwrap();
        let mut runtime = HostAdmissionRuntime::open(temp.path(), large_record_bounds())
            .unwrap()
            .0;
        let payload = vec![0x5Au8; LARGE_PAYLOAD_LEN];
        let first = runtime.admit("a", &payload).unwrap().seq;
        let second = runtime.admit("a", &payload).unwrap().seq;

        take_cloned_payload_bytes();
        let leased = runtime.lease_next().unwrap();
        assert_eq!((leased.seq, leased.payload), (first, payload.clone()));
        assert_eq!(
            take_cloned_payload_bytes(),
            LARGE_PAYLOAD_LEN,
            "a lease owns exactly one payload copy"
        );

        runtime.defer(first).unwrap();
        assert_eq!(
            take_cloned_payload_bytes(),
            0,
            "defer must requeue the lease by reference"
        );
        assert_eq!(
            runtime.lease_next().unwrap().seq,
            first,
            "the deferred head stays ahead of its source's later record"
        );
        assert_eq!(runtime.lease_next().unwrap().seq, second);
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
        let error = HostAdmissionRuntime::open(temp.path(), bounded).unwrap_err();
        assert_eq!(
            error.hook_runtime_context(),
            Some(("spool_corrupted", false, "host-admission spool open failed"))
        );
        assert!(error.reset_required_context().is_none());
    }

    #[test]
    fn future_metadata_version_requires_typed_reset_without_replay_or_mutation() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        runtime.admit("claude", b"retained").unwrap();
        drop(runtime);

        let meta_path = temp.path().join("meta.json");
        let records_path = temp.path().join("records.bin");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        meta["version"] = serde_json::Value::from(2);
        let future_meta = serde_json::to_vec(&meta).unwrap();
        fs::write(&meta_path, &future_meta).unwrap();
        let records_before = fs::read(&records_path).unwrap();

        let error = HostAdmissionRuntime::open(temp.path(), bounds()).unwrap_err();
        assert_reset_required(&error);
        assert_eq!(fs::read(&meta_path).unwrap(), future_meta);
        assert_eq!(fs::read(&records_path).unwrap(), records_before);
    }

    #[test]
    fn future_frame_version_requires_typed_reset_without_replay_or_truncation() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        runtime.admit("claude", b"retained").unwrap();
        drop(runtime);

        let meta_path = temp.path().join("meta.json");
        let records_path = temp.path().join("records.bin");
        let meta_before = fs::read(&meta_path).unwrap();
        let mut future_records = fs::read(&records_path).unwrap();
        future_records[4..6].copy_from_slice(&2u16.to_le_bytes());
        fs::write(&records_path, &future_records).unwrap();

        let error = HostAdmissionRuntime::open(temp.path(), bounds()).unwrap_err();
        assert_reset_required(&error);
        assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
        assert_eq!(fs::read(&records_path).unwrap(), future_records);
    }

    #[test]
    fn future_quarantine_tail_version_requires_reset_without_truncation() {
        let temp = TempDir::new().unwrap();
        let mut runtime = open(&temp);
        let admitted = runtime.admit("claude", b"terminal").unwrap();
        assert_eq!(runtime.lease_next().unwrap().seq, admitted.seq);
        runtime
            .quarantine(admitted.seq, TerminalReason::MalformedPayload)
            .unwrap();
        drop(runtime);

        let meta_path = temp.path().join("meta.json");
        let records_path = temp.path().join("records.bin");
        let quarantine_path = temp.path().join("quarantine.bin");
        let meta_before = fs::read(&meta_path).unwrap();
        let records_before = fs::read(&records_path).unwrap();
        let mut future_quarantine = fs::read(&quarantine_path).unwrap();
        future_quarantine[4..6].copy_from_slice(&2u16.to_le_bytes());
        fs::write(&quarantine_path, &future_quarantine).unwrap();

        let error = HostAdmissionRuntime::open(temp.path(), bounds()).unwrap_err();
        assert_reset_required(&error);
        assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
        assert_eq!(fs::read(&records_path).unwrap(), records_before);
        assert_eq!(fs::read(&quarantine_path).unwrap(), future_quarantine);
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

//! Derived run-lifecycle index over the append-only automation run ledger.
//!
//! The ledger file is the only durable run authority. This module keeps one
//! in-process, non-authoritative index per ledger that maps a run ID to the
//! committed lifecycle of that run (the canonical byte span of every observed
//! status plus the newest state). Exact lookups, terminal publications, and
//! logical-lifecycle readers consult the index so their work is proportional
//! to the selected run instead of the complete history.
//!
//! Validity is tied to the exact ledger file identity and a committed
//! frontier:
//! * `identity` is the opened handle's device/file index, so a replaced file
//!   never matches a cached index.
//! * `frontier` is a byte offset at a committed row boundary; every row in
//!   `[0, frontier)` has been folded and made durable. Production writers only
//!   append rows or truncate an *uncommitted* tail during recovery, so bytes
//!   below a committed frontier are never rewritten in place.
//! * `witness` is the digest of the final `<= 4 KiB` before the frontier, read
//!   under the exclusive ledger lock. Together with the identity and the
//!   `frontier <= len` check it rejects a same-length rewrite the same way the
//!   task-summary memo does.
//!
//! A missing or stale index is a recovery state: the whole committed history
//! is revalidated and re-folded before any answer is produced. It is never
//! treated as an authoritative "run not found".
//!
//! The index also owns read-side stabilization. Exact binding, replay
//! publication, and stale-spool retirement previously synced the ledger on
//! every open so that visible-but-unsynced bytes from a crashed writer could
//! not be trusted. Now a writer syncs at its own commit boundary and reports
//! the append through [`record_durable_run_ledger_append`]; bytes that reach
//! the index any other way are synced exactly once, when the frontier first
//! advances over them. An unchanged ledger is therefore read without a sync.

use std::collections::HashMap;
use std::ops::Range;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::exact_lookup::{
    ForwardJsonlScanner, RunLedgerRowProjection, canonical_completion_key, read_exact_span,
    require_committed_jsonl_eof, scan_jsonl_row, spans_match,
};
use super::{
    AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger, run_status_index,
    sync_run_ledger_file_and_parent, valid_run_status_transition,
};
use crate::automation::backend::{AgentTaskKind, task_key as canonical_task_key};
use crate::automation::config_error;
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Bytes hashed before the frontier as the index's content witness.
const FRONTIER_WITNESS_BYTES: u64 = 4096;
/// Maximum distinct ledgers indexed by one process. Every entry is a derived
/// cache that is equally cheap to rebuild from its ledger.
const RUN_LEDGER_INDEX_CAPACITY: usize = 64;

/// The lifecycle-relevant facts of one ledger row, independent of whether the
/// row was projected from the file or is a candidate that has not been
/// written yet.
#[derive(Clone, Copy, Debug)]
pub(super) struct LifecycleRow<'a> {
    task: AgentTaskKind,
    trigger: AutomationTrigger,
    task_key: Option<&'a str>,
    status: AutomationRunStatus,
    completion: (i64, i64),
}

impl<'a> LifecycleRow<'a> {
    pub(super) fn from_projection(row: &'a RunLedgerRowProjection) -> Result<Self> {
        let (completed_at, completed_at_micros, _) = canonical_completion_key(row)?;
        Ok(Self {
            task: row.task,
            trigger: row.trigger,
            task_key: row.task_key.as_deref(),
            status: row.status,
            completion: (completed_at, completed_at_micros),
        })
    }

    pub(super) fn from_record(record: &'a AutomationRunLedgerRecord) -> Result<Self> {
        let completion = super::canonical_completion_parts(
            record.schema_version,
            &record.completed_at,
            record.completed_at_micros,
        )?;
        Ok(Self {
            task: record.task,
            trigger: record.trigger,
            task_key: record.task_key.as_deref(),
            status: record.status,
            completion,
        })
    }

    fn effective_task_key(&self) -> &'a str {
        self.task_key
            .unwrap_or_else(|| canonical_task_key(self.task))
    }
}

/// How one more row relates to a run's committed lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LifecycleStep {
    /// The row repeats an already committed status. It is admissible only if
    /// its bytes equal the canonical span returned here.
    Replay(Range<u64>),
    /// The row legally advances the lifecycle.
    Advance,
}

/// Committed lifecycle of one logical run.
///
/// This is the single transition authority shared by the exact lookup index,
/// the lenient operator-page reader, and ordinary append deduplication. It
/// holds no file handle: byte comparison of a replayed status is the caller's
/// job because only the caller knows where the replayed bytes live.
#[derive(Clone, Debug)]
pub(super) struct RunLifecycle {
    task: AgentTaskKind,
    trigger: AutomationTrigger,
    /// Explicit task key when it differs from the canonical key of `task`.
    task_key: Option<Box<str>>,
    /// Canonical committed span per status, indexed by [`run_status_index`].
    status_spans: [Option<Range<u64>>; 5],
    newest_status: AutomationRunStatus,
    newest_completion: (i64, i64),
}

impl RunLifecycle {
    /// Opens a lifecycle from the first committed row of a run.
    pub(super) fn open(projection: &RunLedgerRowProjection) -> Result<Self> {
        let row = LifecycleRow::from_projection(projection)?;
        let canonical = canonical_task_key(row.task);
        let task_key = (row.effective_task_key() != canonical)
            .then(|| Box::<str>::from(row.effective_task_key()));
        let mut status_spans: [Option<Range<u64>>; 5] = std::array::from_fn(|_| None);
        status_spans[run_status_index(row.status)] = Some(projection.span.clone());
        Ok(Self {
            task: row.task,
            trigger: row.trigger,
            task_key,
            status_spans,
            newest_status: row.status,
            newest_completion: row.completion,
        })
    }

    fn effective_task_key(&self) -> &str {
        self.task_key
            .as_deref()
            .unwrap_or_else(|| canonical_task_key(self.task))
    }

    pub(super) fn newest_status(&self) -> AutomationRunStatus {
        self.newest_status
    }

    pub(super) fn newest_span(&self) -> Result<Range<u64>> {
        self.status_spans[run_status_index(self.newest_status)]
            .clone()
            .ok_or_else(|| config_error("automation run lifecycle lost its newest committed span"))
    }

    /// Pure transition check: immutable identity, replayed status, legal
    /// status order, and completion-time monotonicity.
    pub(super) fn step(
        &self,
        row: &LifecycleRow<'_>,
        run_id: &str,
        path: &Path,
    ) -> Result<LifecycleStep> {
        if row.task != self.task
            || row.effective_task_key() != self.effective_task_key()
            || row.trigger != self.trigger
        {
            return Err(config_error(format!(
                "automation run ledger '{}' mutates immutable identity for run '{run_id}'",
                path.display()
            )));
        }
        if let Some(canonical) = self.status_spans[run_status_index(row.status)].as_ref() {
            return Ok(LifecycleStep::Replay(canonical.clone()));
        }
        if !valid_run_status_transition(Some(self.newest_status), row.status) {
            return Err(config_error(format!(
                "automation run ledger '{}' contains an invalid lifecycle for run '{run_id}'",
                path.display()
            )));
        }
        if row.completion < self.newest_completion {
            return Err(config_error(format!(
                "automation run ledger '{}' regresses completion time for run '{run_id}'",
                path.display()
            )));
        }
        Ok(LifecycleStep::Advance)
    }

    /// Folds one committed ledger row of this run, verifying a replayed
    /// status byte-for-byte against its canonical span.
    pub(super) fn fold(
        &mut self,
        file: &std::fs::File,
        path: &Path,
        projection: &RunLedgerRowProjection,
    ) -> Result<()> {
        let row = LifecycleRow::from_projection(projection)?;
        match self.step(&row, &projection.run_id, path)? {
            LifecycleStep::Replay(canonical) => {
                if spans_match(file, path, &canonical, &projection.span)? {
                    Ok(())
                } else {
                    Err(conflicting_replay(path, &projection.run_id))
                }
            }
            LifecycleStep::Advance => {
                self.status_spans[run_status_index(row.status)] = Some(projection.span.clone());
                self.newest_status = row.status;
                self.newest_completion = row.completion;
                Ok(())
            }
        }
    }
}

pub(super) fn conflicting_replay(path: &Path, run_id: &str) -> TraceDecayError {
    config_error(format!(
        "automation run ledger '{}' repeats a conflicting lifecycle state for run '{run_id}'",
        path.display()
    ))
}

/// Folds one committed row into the lifecycle map of the runs it selects.
pub(super) fn fold_selected_row(
    runs: &mut HashMap<String, RunLifecycle>,
    file: &std::fs::File,
    path: &Path,
    projection: &RunLedgerRowProjection,
) -> Result<()> {
    match runs.get_mut(projection.run_id.as_str()) {
        Some(lifecycle) => lifecycle.fold(file, path, projection),
        None => {
            runs.insert(projection.run_id.clone(), RunLifecycle::open(projection)?);
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LedgerFileIdentity {
    device: u64,
    file_index: u64,
}

#[cfg(unix)]
fn ledger_file_identity(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<LedgerFileIdentity> {
    Ok(LedgerFileIdentity {
        device: metadata.dev(),
        file_index: metadata.ino(),
    })
}

#[cfg(windows)]
fn ledger_file_identity(
    file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<LedgerFileIdentity> {
    let information =
        tracedecay_private_fs::windows_file::information(file).map_err(TraceDecayError::from)?;
    Ok(LedgerFileIdentity {
        device: u64::from(information.volume_serial_number),
        file_index: information.file_index,
    })
}

/// Committed run lifecycles of one ledger file up to its indexed frontier.
pub(super) struct RunLedgerIndex {
    identity: LedgerFileIdentity,
    frontier: u64,
    witness: [u8; 32],
    runs: HashMap<String, RunLifecycle>,
}

impl RunLedgerIndex {
    fn empty(identity: LedgerFileIdentity) -> Self {
        Self {
            identity,
            frontier: 0,
            witness: Sha256::digest(b"").into(),
            runs: HashMap::new(),
        }
    }

    pub(super) fn lifecycle(&self, run_id: &str) -> Option<&RunLifecycle> {
        self.runs.get(run_id)
    }

    /// True when this index still describes the committed prefix of `file`.
    fn is_current(
        &self,
        file: &std::fs::File,
        path: &Path,
        identity: LedgerFileIdentity,
        len: u64,
    ) -> Result<bool> {
        if self.identity != identity || self.frontier > len {
            return Ok(false);
        }
        Ok(frontier_witness(file, path, self.frontier)? == self.witness)
    }

    /// Folds every committed row in `[frontier, len)` and advances the
    /// frontier. Callers make those bytes durable first.
    #[hotpath::measure(label = "hosts.automation.run_ledger_index.fold")]
    fn fold_to(&mut self, file: &std::fs::File, path: &Path, len: u64) -> Result<()> {
        let mut rows = ForwardJsonlScanner::new_from(file, path, self.frontier, len)?;
        while let Some(span) = rows.next_span()? {
            let Some(projection) = scan_jsonl_row(file, path, span)? else {
                continue;
            };
            fold_selected_row(&mut self.runs, file, path, &projection)?;
        }
        self.witness = frontier_witness(file, path, len)?;
        self.frontier = len;
        Ok(())
    }
}

fn frontier_witness(file: &std::fs::File, path: &Path, frontier: u64) -> Result<[u8; 32]> {
    let window = frontier.min(FRONTIER_WITNESS_BYTES);
    let window_len = usize::try_from(window)
        .map_err(|_| config_error("automation run ledger witness window is not representable"))?;
    let mut bytes = vec![0_u8; window_len];
    read_exact_span(file, path, frontier - window, &mut bytes)?;
    Ok(Sha256::digest(&bytes).into())
}

type RunLedgerIndexes = HashMap<PathBuf, RunLedgerIndex>;

static RUN_LEDGER_INDEXES: std::sync::OnceLock<std::sync::Mutex<RunLedgerIndexes>> =
    std::sync::OnceLock::new();

fn run_ledger_indexes() -> std::sync::MutexGuard<'static, RunLedgerIndexes> {
    let indexes = RUN_LEDGER_INDEXES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    // Entries are plain derived data; a poisoned lock leaves no invariant to
    // repair beyond rebuilding from the ledger.
    match indexes.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The index key is the canonical ledger path; the stored identity is what
/// actually validates a hit.
fn index_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn take_index(key: &Path) -> Option<RunLedgerIndex> {
    run_ledger_indexes().remove(key)
}

fn store_index(key: PathBuf, index: RunLedgerIndex) {
    let mut indexes = run_ledger_indexes();
    if indexes.len() >= RUN_LEDGER_INDEX_CAPACITY
        && !indexes.contains_key(&key)
        && let Some(evicted) = indexes.keys().next().cloned()
    {
        indexes.remove(&evicted);
    }
    indexes.insert(key, index);
}

struct OpenLedger {
    identity: LedgerFileIdentity,
    len: u64,
}

fn inspect_ledger(file: &std::fs::File, path: &Path) -> Result<OpenLedger> {
    let metadata = file.metadata().map_err(|error| TraceDecayError::File {
        message: format!(
            "failed to inspect automation run ledger for its lifecycle index: {error}"
        ),
        path: path.display().to_string(),
    })?;
    Ok(OpenLedger {
        identity: ledger_file_identity(file, &metadata)?,
        len: metadata.len(),
    })
}

/// Brings the index for `file` to its current committed frontier, rebuilding
/// it from byte zero when it is missing or stale.
#[hotpath::measure(label = "hosts.automation.run_ledger_index.refresh")]
fn refresh_index(
    taken: Option<RunLedgerIndex>,
    file: &std::fs::File,
    path: &Path,
    ledger: &OpenLedger,
) -> Result<RunLedgerIndex> {
    let mut index = match taken {
        Some(index) if index.is_current(file, path, ledger.identity, ledger.len)? => index,
        _ => RunLedgerIndex::empty(ledger.identity),
    };
    if index.frontier < ledger.len {
        require_committed_jsonl_eof(file, path, ledger.len)?;
        // Rows beyond the frontier were committed outside this index (another
        // process, an ordinary append that bypassed it, or a rebuild). Make
        // them durable before any caller trusts them.
        sync_run_ledger_file_and_parent(path, file)?;
        index.fold_to(file, path, ledger.len)?;
    }
    Ok(index)
}

/// Runs `read` against the committed lifecycle index of the open ledger.
///
/// The caller holds the exclusive ledger lock. A refresh failure discards the
/// cached index so the next access rebuilds from the ledger.
pub(super) fn with_run_ledger_index<T>(
    file: &std::fs::File,
    path: &Path,
    read: impl FnOnce(&RunLedgerIndex) -> Result<T>,
) -> Result<T> {
    let ledger = inspect_ledger(file, path)?;
    let key = index_key(path);
    let index = refresh_index(take_index(&key), file, path, &ledger)?;
    let result = read(&index);
    store_index(key, index);
    result
}

/// Advances the index over rows this process just appended and synced in
/// `[pre_append_eof, len)`. An index that does not sit exactly at
/// `pre_append_eof` is discarded and rebuilt on the next read instead of being
/// patched.
pub(super) fn record_durable_run_ledger_append(
    file: &std::fs::File,
    path: &Path,
    pre_append_eof: u64,
) {
    let key = index_key(path);
    let Some(mut index) = take_index(&key) else {
        return;
    };
    let advanced = (|| -> Result<bool> {
        let ledger = inspect_ledger(file, path)?;
        if index.frontier != pre_append_eof
            || !index.is_current(file, path, ledger.identity, ledger.len)?
        {
            return Ok(false);
        }
        index.fold_to(file, path, ledger.len)?;
        Ok(true)
    })();
    match advanced {
        Ok(true) => store_index(key, index),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            automation_run_ledger = %path.display(),
            error = %error,
            "discarding automation run ledger lifecycle index after a durable append"
        ),
    }
}

/// Drops the cached index after recovery rewrote the ledger tail so the next
/// read revalidates the complete committed history.
pub(super) fn discard_run_ledger_index(path: &Path) {
    take_index(&index_key(path));
}

#[cfg(test)]
mod tests {
    use super::super::exact_lookup::scan_receipt;
    use super::*;

    fn scan_receipt_snapshot() -> scan_receipt::ScanReceipt {
        scan_receipt::snapshot()
    }

    fn ledger_line(run_id: &str, status: &str, completed_at: i64) -> String {
        format!(
            "{{\"schema_version\":2,\"run_id\":\"{run_id}\",\"trigger\":\"scheduler\",\
             \"task\":\"memory_curator\",\"backend\":\"codex_app_server\",\"status\":\"{status}\",\
             \"accepted_count\":0,\"rejected_count\":0,\"started_at\":\"{completed_at}\",\
             \"completed_at\":\"{completed_at}\",\"completed_at_micros\":{}}}",
            completed_at.saturating_mul(1_000_000),
        )
    }

    fn write_ledger(lines: &[String]) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join(super::super::RUN_LEDGER_FILENAME);
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("ledger");
        (temp, path)
    }

    fn open(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("ledger handle")
    }

    fn projection(path: &Path, span: Range<u64>) -> RunLedgerRowProjection {
        let file = open(path);
        scan_jsonl_row(&file, path, span)
            .expect("row")
            .expect("nonblank row")
    }

    fn newest_status(path: &Path, run_id: &str) -> Option<AutomationRunStatus> {
        let file = open(path);
        with_run_ledger_index(&file, path, |index| {
            Ok(index.lifecycle(run_id).map(RunLifecycle::newest_status))
        })
        .expect("index")
    }

    #[test]
    fn step_reports_replay_advance_and_each_refusal() {
        let queued = ledger_line("target", "queued", 1);
        let running = ledger_line("target", "running", 2);
        let (_temp, path) = write_ledger(&[queued.clone(), running.clone()]);
        let queued_span = 0..queued.len() as u64;
        let running_span = queued.len() as u64 + 1..(queued.len() + 1 + running.len()) as u64;
        let mut lifecycle = RunLifecycle::open(&projection(&path, queued_span.clone())).unwrap();

        let replay = projection(&path, queued_span.clone());
        assert_eq!(
            lifecycle
                .step(
                    &LifecycleRow::from_projection(&replay).unwrap(),
                    "target",
                    &path
                )
                .unwrap(),
            LifecycleStep::Replay(queued_span)
        );
        let advance = projection(&path, running_span);
        assert_eq!(
            lifecycle
                .step(
                    &LifecycleRow::from_projection(&advance).unwrap(),
                    "target",
                    &path
                )
                .unwrap(),
            LifecycleStep::Advance
        );
        let file = open(&path);
        lifecycle.fold(&file, &path, &advance).unwrap();
        assert_eq!(lifecycle.newest_status(), AutomationRunStatus::Running);

        let mut mutated = LifecycleRow::from_projection(&advance).unwrap();
        mutated.trigger = AutomationTrigger::Dashboard;
        assert!(
            lifecycle
                .step(&mutated, "target", &path)
                .unwrap_err()
                .to_string()
                .contains("mutates immutable identity")
        );
        let mut backwards = LifecycleRow::from_projection(&advance).unwrap();
        backwards.status = AutomationRunStatus::Succeeded;
        backwards.completion = (1, 1_000_000);
        assert!(
            lifecycle
                .step(&backwards, "target", &path)
                .unwrap_err()
                .to_string()
                .contains("regresses completion time")
        );
        lifecycle
            .fold(
                &file,
                &path,
                &RunLedgerRowProjection {
                    status: AutomationRunStatus::Failed,
                    completed_at: "3".to_owned(),
                    completed_at_micros: Some(3_000_000),
                    span: 0..1,
                    ..advance.clone()
                },
            )
            .unwrap();
        let mut after_terminal = LifecycleRow::from_projection(&advance).unwrap();
        after_terminal.status = AutomationRunStatus::Skipped;
        after_terminal.completion = (4, 4_000_000);
        assert!(
            lifecycle
                .step(&after_terminal, "target", &path)
                .unwrap_err()
                .to_string()
                .contains("invalid lifecycle")
        );
    }

    #[test]
    fn fold_rejects_a_replayed_status_with_different_bytes() {
        let queued = ledger_line("target", "queued", 1);
        let running = ledger_line("target", "running", 2);
        let conflicting = ledger_line("target", "queued", 3);
        let (_temp, path) = write_ledger(&[queued.clone(), running.clone(), conflicting]);
        let file = open(&path);
        let mut lifecycle = RunLifecycle::open(&projection(&path, 0..queued.len() as u64)).unwrap();
        let running_start = queued.len() as u64 + 1;
        lifecycle
            .fold(
                &file,
                &path,
                &projection(&path, running_start..running_start + running.len() as u64),
            )
            .unwrap();
        let conflict_start = running_start + running.len() as u64 + 1;
        let conflict = projection(&path, conflict_start..conflict_start + queued.len() as u64);

        let error = lifecycle.fold(&file, &path, &conflict).unwrap_err();

        assert!(error.to_string().contains("conflicting lifecycle state"));
        assert_eq!(lifecycle.newest_status(), AutomationRunStatus::Running);
    }

    #[test]
    fn warm_index_folds_only_appended_rows_and_syncs_only_on_change() {
        let mut lines = vec![ledger_line("target", "queued", 1)];
        lines.extend(
            (0..3_000).map(|index| ledger_line(&format!("unrelated-{index}"), "running", 2)),
        );
        let (_temp, path) = write_ledger(&lines);

        let cold = scan_receipt_snapshot();
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Queued)
        );
        let cold = scan_receipt_snapshot().since(cold);
        assert!(
            cold.rows_decoded >= 3_001,
            "cold rebuild decodes every row: {cold:?}"
        );
        assert_eq!(cold.syncs, 1, "a rebuild stabilizes the ledger once");

        let warm = scan_receipt_snapshot();
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Queued)
        );
        let warm = scan_receipt_snapshot().since(warm);
        assert_eq!(warm.rows_decoded, 0, "an unchanged ledger decodes no rows");
        assert_eq!(warm.syncs, 0, "an unchanged ledger is not resynced");

        // A foreign writer appends a row: only that row is folded, after one
        // write-through sync.
        let mut appended = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append handle");
        std::io::Write::write_all(
            &mut appended,
            format!("{}\n", ledger_line("target", "running", 3)).as_bytes(),
        )
        .expect("foreign append");
        let incremental = scan_receipt_snapshot();
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Running)
        );
        let incremental = scan_receipt_snapshot().since(incremental);
        assert_eq!(incremental.rows_decoded, 1);
        assert_eq!(incremental.syncs, 1);
    }

    #[test]
    fn stale_index_is_rebuilt_from_the_complete_ledger() {
        let original = ledger_line("target", "queued", 1);
        let rewritten = ledger_line("tarGet", "queued", 1);
        assert_eq!(original.len(), rewritten.len());
        let mut lines = vec![original.clone()];
        lines
            .extend((0..200).map(|index| ledger_line(&format!("unrelated-{index}"), "running", 2)));
        let (_temp, path) = write_ledger(&lines);
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Queued)
        );

        // Truncation below the committed frontier.
        std::fs::write(&path, format!("{original}\n")).expect("truncate");
        let receipt = scan_receipt_snapshot();
        assert_eq!(
            newest_status(&path, "unrelated-0"),
            None,
            "a truncated ledger must not answer from the stale index"
        );
        assert_eq!(scan_receipt_snapshot().since(receipt).rows_decoded, 1);

        // Equal-length rewrite of the bytes under the frontier witness.
        std::fs::write(&path, format!("{rewritten}\n")).expect("rewrite");
        assert_eq!(newest_status(&path, "target"), None);
        assert_eq!(
            newest_status(&path, "tarGet"),
            Some(AutomationRunStatus::Queued)
        );

        // Replaced file identity with identical bytes at a longer frontier.
        let replacement = path.with_extension("replacement");
        std::fs::write(&replacement, format!("{rewritten}\n{original}\n")).expect("replacement");
        std::fs::rename(&replacement, &path).expect("replace ledger");
        let receipt = scan_receipt_snapshot();
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Queued)
        );
        assert_eq!(
            scan_receipt_snapshot().since(receipt).rows_decoded,
            2,
            "a replaced ledger identity rebuilds from byte zero"
        );
    }

    #[test]
    fn foreign_conflicting_replay_is_refused_without_poisoning_the_index() {
        let queued = ledger_line("target", "queued", 1);
        let running = ledger_line("target", "running", 2);
        let (_temp, path) = write_ledger(&[queued.clone(), running.clone()]);
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Running)
        );

        std::fs::write(
            &path,
            format!(
                "{queued}\n{running}\n{}\n",
                ledger_line("target", "queued", 3)
            ),
        )
        .expect("conflicting append");
        let file = open(&path);
        let error = with_run_ledger_index(&file, &path, |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("conflicting lifecycle state"));

        std::fs::write(&path, format!("{queued}\n{running}\n")).expect("restore");
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Running)
        );
    }

    #[test]
    fn incomplete_tail_is_refused_and_recovered_after_truncation() {
        let queued = ledger_line("target", "queued", 1);
        let (_temp, path) = write_ledger(std::slice::from_ref(&queued));
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Queued)
        );

        std::fs::write(&path, format!("{queued}\n{{\"run_id\":\"partial")).expect("partial tail");
        let file = open(&path);
        let error = with_run_ledger_index(&file, &path, |_| Ok(())).unwrap_err();
        assert!(error.to_string().contains("incomplete durable tail"));

        std::fs::write(&path, format!("{queued}\n")).expect("recovered tail");
        discard_run_ledger_index(&path);
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Queued)
        );
    }

    #[test]
    fn durable_append_receipt_advances_only_an_index_at_the_pre_append_frontier() {
        let queued = ledger_line("target", "queued", 1);
        let (_temp, path) = write_ledger(std::slice::from_ref(&queued));
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Queued)
        );
        let pre_append_eof = queued.len() as u64 + 1;

        let running = ledger_line("target", "running", 2);
        std::fs::write(&path, format!("{queued}\n{running}\n")).expect("append");
        record_durable_run_ledger_append(&open(&path), &path, pre_append_eof);
        let receipt = scan_receipt_snapshot();
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Running)
        );
        let receipt = scan_receipt_snapshot().since(receipt);
        assert_eq!(
            receipt.rows_decoded, 0,
            "the receipt already folded the row"
        );
        assert_eq!(receipt.syncs, 0, "the writer owned the sync");

        // A receipt for a frontier the index does not sit at discards it.
        let terminal = ledger_line("target", "succeeded", 3);
        std::fs::write(&path, format!("{queued}\n{running}\n{terminal}\n")).expect("append");
        record_durable_run_ledger_append(&open(&path), &path, 0);
        let receipt = scan_receipt_snapshot();
        assert_eq!(
            newest_status(&path, "target"),
            Some(AutomationRunStatus::Succeeded)
        );
        assert_eq!(scan_receipt_snapshot().since(receipt).rows_decoded, 3);
    }
}

//! The parent side: run one seeded crash point in a child process, reopen the
//! store, and check the recovery invariants.

use std::{
    os::unix::process::ExitStatusExt,
    path::Path,
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use tracedecay_sql::{ENGINE, Engine};
use tracedecay_store::RuntimeSubmitOutcomeV1;

use crate::{
    CHILD_ENV,
    scenario::{Mode, Plan},
    store,
};

const SIGABRT: i32 = 6;
const WORKERS: u64 = 4;

/// Runs `seeds` crash points of `mode` and fails with every failing seed.
pub fn run(mode: Mode, seeds: u64) {
    if !matches!(mode, Mode::Failpoint(_)) && ENGINE != Engine::Native {
        println!("TD_CRASH_SKIP {mode}: CrashIo needs --config=native");
        return;
    }
    let next = AtomicU64::new(0);
    let failures = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                loop {
                    let seed = next.fetch_add(1, Ordering::Relaxed);
                    if seed >= seeds {
                        break;
                    }
                    let nth = Plan::new(mode, seed).nth;
                    match point(mode, seed) {
                        Ok(()) => println!("TD_CRASH_POINT {mode} seed={seed} nth={nth} ok"),
                        Err(error) => {
                            println!("TD_CRASH_POINT {mode} seed={seed} nth={nth} FAIL {error}");
                            failures
                                .lock()
                                .unwrap()
                                .push(format!("{mode} seed={seed} nth={nth}: {error}"));
                        }
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    println!(
        "TD_CRASH_SUMMARY {mode} points={seeds} failures={}",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {seeds} crash points failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[derive(Debug, Default)]
struct ChildLog {
    submits: Vec<usize>,
    acks: Vec<(usize, u64)>,
    watermarks: Vec<u64>,
    done: bool,
}

impl ChildLog {
    fn parse(stdout: &[u8]) -> Self {
        let mut log = Self::default();
        for line in String::from_utf8_lossy(stdout).lines() {
            // libtest prints `test crash_child ... ` without a newline before
            // the child's first event.
            let Some((_, event)) = line.split_once("TD|") else {
                continue;
            };
            let fields = event.split(' ').collect::<Vec<_>>();
            match fields.as_slice() {
                ["submit", index] => log.submits.push(index.parse().unwrap()),
                ["ack", index, sequence] => log
                    .acks
                    .push((index.parse().unwrap(), sequence.parse().unwrap())),
                ["wm", sequence] => log.watermarks.push(sequence.parse().unwrap()),
                ["done"] => log.done = true,
                _ => panic!("malformed child line {line:?}"),
            }
        }
        log
    }
}

fn point(mode: Mode, seed: u64) -> Result<(), String> {
    let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = directory.path().join("store.db");
    match mode {
        Mode::DriverIo(_) => store::create_driver_store(&path),
        Mode::Failpoint(_) | Mode::WriterIo(_) => store::create_writer_store(&path),
    }
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    let output = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
        .args(["crash_child", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, format!("{mode} {seed} {}", path.display()))
        .output()
        .map_err(|error| format!("spawn child: {error}"))?;
    let log = ChildLog::parse(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.signal() != Some(SIGABRT) || !stderr.contains("TD_CRASH ") {
        let tail = stderr.lines().rev().take(8).collect::<Vec<_>>();
        return Err(format!(
            "child did not crash at the armed point (status {}, done {}): {tail:?}",
            output.status, log.done
        ));
    }
    let plan = Plan::new(mode, seed);
    match mode {
        Mode::DriverIo(_) => verify_driver(&path, &log),
        Mode::Failpoint(_) | Mode::WriterIo(_) => verify_writer(&path, mode, &plan, &log),
    }
}

fn ensure(condition: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message()) }
}

fn sequence_of(receipts: &[(u64, String)], index: usize) -> Option<u64> {
    let key = store::key(index);
    receipts
        .iter()
        .find(|(_, stored)| *stored == key)
        .map(|(sequence, _)| *sequence)
}

/// Durable ledger shape after any reopen: integrity, receipts contiguous from
/// 1, one checkpoint row whose watermark equals the last durable sequence,
/// and one diagnostics publication per receipt with the last one current.
fn check_ledger(durable: &store::WriterDurable) -> Result<u64, String> {
    ensure(durable.integrity == "ok", || {
        format!("integrity_check: {}", durable.integrity)
    })?;
    let sequences = durable.receipts.iter().map(|(sequence, _)| *sequence);
    ensure(
        sequences.clone().eq(1..=durable.receipts.len() as u64),
        || format!("receipts not contiguous: {:?}", durable.receipts),
    )?;
    let last = durable.receipts.len() as u64;
    let expected_checkpoints = if last == 0 {
        vec![]
    } else {
        vec![(last, last)]
    };
    ensure(durable.checkpoints == expected_checkpoints, || {
        format!(
            "checkpoint rows {:?} disagree with durable sequence {last}",
            durable.checkpoints
        )
    })?;
    ensure(durable.publications == last, || {
        format!(
            "{} diagnostics publications for {last} receipts",
            durable.publications
        )
    })?;
    let current = durable.receipts.last().map(|(_, key)| {
        let index = key.rsplit('.').next().unwrap().parse().unwrap();
        store::generation(index)
    });
    ensure(durable.current_generation == current, || {
        format!(
            "current generation {:?}, last receipt names {current:?}",
            durable.current_generation
        )
    })?;
    Ok(last)
}

fn verify_writer(path: &Path, mode: Mode, plan: &Plan, log: &ChildLog) -> Result<(), String> {
    let durable = store::inspect_writer(path);
    let last = check_ledger(&durable)?;
    for &(index, sequence) in &log.acks {
        ensure(
            sequence_of(&durable.receipts, index) == Some(sequence),
            || format!("acknowledged request {index} at {sequence} is not durable"),
        )?;
    }
    for &watermark in &log.watermarks {
        ensure(watermark <= last, || {
            format!("published watermark {watermark} is past durable sequence {last}")
        })?;
    }
    let in_flight = log
        .submits
        .iter()
        .copied()
        .filter(|index| !log.acks.iter().any(|(acked, _)| acked == index))
        .collect::<Vec<_>>();
    // A concurrent pair may commit in two batches, so one in-flight request
    // can be durable but unacknowledged when the other's batch crashes.
    match mode {
        Mode::Failpoint("before_commit") => ensure(
            in_flight
                .iter()
                .any(|index| sequence_of(&durable.receipts, *index).is_none()),
            || format!("before_commit crash left every in-flight {in_flight:?} durable"),
        )?,
        Mode::Failpoint("after_commit_before_watermark") => ensure(
            in_flight
                .iter()
                .any(|index| sequence_of(&durable.receipts, *index).is_some()),
            || format!("after_commit crash lost every in-flight commit {in_flight:?}"),
        )?,
        _ => {}
    }

    // Recovery: every request is retried. A durable one must settle as the
    // exact replay of its receipt (commit-unknown resolved by receipt lookup);
    // a lost one must commit fresh at the next sequence; nothing conflicts.
    let attachment = store::attach(path);
    let mut next = last;
    for index in 0..plan.requests {
        let outcome = store::submit(&attachment, index)
            .map_err(|error| format!("resubmit {index}: {error}"))?;
        let durable_sequence = sequence_of(&durable.receipts, index);
        match (&outcome, durable_sequence) {
            (RuntimeSubmitOutcomeV1::ExactReplay { receipt }, Some(sequence))
                if receipt.commit_sequence.0 == sequence => {}
            (RuntimeSubmitOutcomeV1::Committed { receipt }, None)
                if receipt.commit_sequence.0 == next + 1 =>
            {
                next += 1;
            }
            _ => {
                return Err(format!(
                    "retry of request {index} (durable at {durable_sequence:?}) settled as {outcome:?}"
                ));
            }
        }
    }
    attachment.drain()?;
    attachment.close_and_join()?;
    let recovered = check_ledger(&store::inspect_writer(path))?;
    ensure(recovered == plan.requests as u64, || {
        format!(
            "{recovered} receipts after recovery, want {}",
            plan.requests
        )
    })
}

fn verify_driver(path: &Path, log: &ChildLog) -> Result<(), String> {
    let (integrity, rows) = store::inspect_driver(path);
    ensure(integrity == "ok", || {
        format!("integrity_check: {integrity}")
    })?;
    for (position, (id, payload)) in rows.iter().enumerate() {
        ensure(*id == position as u64, || {
            format!(
                "rows not contiguous: {:?}",
                rows.iter().map(|(id, _)| id).collect::<Vec<_>>()
            )
        })?;
        ensure(*payload == store::row_payload(position), || {
            format!("row {id} payload torn")
        })?;
    }
    for &(index, _) in &log.acks {
        ensure(index < rows.len(), || {
            format!("acknowledged row {index} lost; {} rows durable", rows.len())
        })?;
    }
    Ok(())
}

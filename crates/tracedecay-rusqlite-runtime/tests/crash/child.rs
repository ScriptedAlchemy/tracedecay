//! The re-executed child. It runs one seeded workload until the armed crash
//! point aborts it, and reports progress to the parent over its stdout pipe:
//! `TD|submit <i>` before request `i` leaves, `TD|ack <i> <seq>` once its
//! receipt is in hand, and `TD|wm <seq>` whenever the writer's published
//! commit sequence moves.

use std::{
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use tracedecay_rusqlite_runtime::{failpoint, repository::RepositoryRuntimePhysicalAttachment};
use tracedecay_sql::{CheckpointMode, Connection, ConnectionControl, native::crash_io, params};
use tracedecay_store::RuntimeSubmitOutcomeV1;

use crate::{
    CHILD_ENV,
    scenario::{CHECKPOINT_EVERY, Mode, Plan},
    store,
};

pub fn run_if_requested() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let mut parts = spec.splitn(3, ' ');
    let mode = Mode::parse(parts.next().expect("child mode"));
    let seed = parts.next().expect("child seed").parse().expect("seed");
    let path = Path::new(parts.next().expect("child store path"));
    let plan = Plan::new(mode, seed);
    match mode {
        Mode::Failpoint(name) => {
            failpoint::arm(name, plan.nth);
            writer_workload(path, &plan);
        }
        Mode::WriterIo(io) => {
            crash_io::arm(io.plan(plan.nth));
            writer_workload(path, &plan);
        }
        Mode::DriverIo(io) => {
            crash_io::arm(io.plan(plan.nth));
            driver_workload(path, &plan);
        }
    }
    emit("done");
}

fn emit(line: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "TD|{line}").unwrap();
    out.flush().unwrap();
}

fn receipt_sequence(outcome: RuntimeSubmitOutcomeV1, replay: bool) -> u64 {
    match (outcome, replay) {
        (RuntimeSubmitOutcomeV1::Committed { receipt }, false)
        | (RuntimeSubmitOutcomeV1::ExactReplay { receipt }, true) => receipt.commit_sequence.0,
        (outcome, _) => panic!("unexpected outcome (replay: {replay}): {outcome:?}"),
    }
}

fn writer_workload(path: &Path, plan: &Plan) {
    let attachment = Arc::new(store::attach(path));
    let stop = Arc::new(AtomicBool::new(false));
    let poller = {
        let attachment = Arc::clone(&attachment);
        let stop = Arc::clone(&stop);
        thread::spawn(move || watch_watermark(&attachment, &stop))
    };
    let mut acked = Vec::new();
    let mut index = 0;
    while index < plan.requests {
        let step = if plan.concurrent[index] && index + 1 < plan.requests {
            vec![index, index + 1]
        } else {
            vec![index]
        };
        for request in &step {
            emit(&format!("submit {request}"));
        }
        let outcomes = thread::scope(|scope| {
            step.iter()
                .map(|&request| {
                    let attachment = &attachment;
                    scope.spawn(move || (request, store::submit(attachment, request)))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        for (request, outcome) in outcomes {
            let sequence = receipt_sequence(outcome.unwrap(), false);
            emit(&format!("ack {request} {sequence}"));
            acked.push((request, sequence));
        }
        if let Some(pick) = plan.replay[index] {
            let (request, sequence) = acked[pick as usize % acked.len()];
            let replayed = receipt_sequence(store::submit(&attachment, request).unwrap(), true);
            assert_eq!(
                replayed, sequence,
                "replay of request {request} moved its receipt"
            );
        }
        if step
            .iter()
            .any(|request| request % CHECKPOINT_EVERY == CHECKPOINT_EVERY - 1)
        {
            store::checkpoint(&attachment);
        }
        index += step.len();
    }
    stop.store(true, Ordering::Release);
    poller.join().unwrap();
    attachment.drain().unwrap();
    attachment.close_and_join().unwrap();
}

fn watch_watermark(attachment: &RepositoryRuntimePhysicalAttachment, stop: &AtomicBool) {
    let mut last = 0;
    while !stop.load(Ordering::Acquire) {
        let sequence = attachment
            .snapshot()
            .writer
            .map_or(0, |writer| writer.commit_sequence.0);
        if sequence != last {
            last = sequence;
            emit(&format!("wm {sequence}"));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn driver_workload(path: &Path, plan: &Plan) {
    // Native policy denies caller `PRAGMA synchronous`, so this runs at the
    // driver's default durability; power-loss modes hold it to the
    // acknowledged-commit bar anyway.
    let mut connection = Connection::open(path).unwrap();
    for index in 0..plan.requests {
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO crash_rows (id, payload) VALUES (?1, ?2)",
                params![index as i64, store::row_payload(index)],
            )
            .unwrap();
        transaction.commit().unwrap();
        emit(&format!("ack {index} {index}"));
        if index % CHECKPOINT_EVERY == CHECKPOINT_EVERY - 1 {
            let mode = if plan.truncate[index] {
                CheckpointMode::Truncate
            } else {
                CheckpointMode::Passive
            };
            connection.checkpoint(mode).unwrap();
        }
    }
}

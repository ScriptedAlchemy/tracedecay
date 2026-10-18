//! How the worker loop learns there is work, and where that work is parked.
//!
//! One wait point ([`wait_for_work`]) covers every ingress channel so the
//! worker never spins, and [`select_auxiliary_work`] is the round-robin that
//! keeps maintenance from starving product writes.

use std::{collections::HashMap, collections::VecDeque, task::Poll, time::Instant};

use asupersync::{Cx, channel::mpsc};
use tracedecay_domain::process_heap::collect_idle_thread_heap_v1;
use tracedecay_store::{StorageRuntimeErrorV1, StoreOperationIdV1};

use crate::{
    admission::{FairQueue, QueueItem},
    exact_sql::WriterCommand as ExactSqlWriterCommand,
    telemetry::WriterTelemetry,
};

use super::super::request::{
    AcceptedRequest, CheckpointCommand, IncrementalVacuumCommand, SharedReply,
};
pub(super) enum WorkerWake {
    Write(Option<AcceptedRequest>),
    ExactSql(Box<Option<ExactSqlWriterCommand>>),
    IncrementalVacuum(Box<Option<IncrementalVacuumCommand>>),
    Checkpoint(Box<Option<CheckpointCommand>>),
    Shutdown,
    CheckpointRetry,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn wait_for_work(
    cx: &Cx,
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    exact_sql_receiver: &mut mpsc::Receiver<ExactSqlWriterCommand>,
    incremental_vacuum_receiver: &mut mpsc::Receiver<IncrementalVacuumCommand>,
    checkpoint_receiver: &mut mpsc::Receiver<CheckpointCommand>,
    shutdown_receiver: &mut mpsc::UnboundedReceiver<()>,
    input_closed: bool,
    exact_sql_closed: bool,
    incremental_vacuum_closed: bool,
    checkpoint_closed: bool,
    deadline: Option<Instant>,
) -> WorkerWake {
    let arm = |deadline: Instant| {
        Box::pin(crate::storage_pool::sleep(
            deadline.saturating_duration_since(Instant::now()),
        ))
    };
    let mut retry = deadline.map(arm);
    std::future::poll_fn(|task_cx| {
        // Writer shutdown, rather than request cancellation, owns accepted work.
        let wake = cx.masked(|| {
            if shutdown_receiver.poll_recv(cx, task_cx).is_ready() {
                return Poll::Ready(WorkerWake::Shutdown);
            }
            if !checkpoint_closed
                && let Poll::Ready(command) = checkpoint_receiver.poll_recv(cx, task_cx)
            {
                return Poll::Ready(WorkerWake::Checkpoint(Box::new(command.ok())));
            }
            if !exact_sql_closed
                && let Poll::Ready(command) = exact_sql_receiver.poll_recv(cx, task_cx)
            {
                return Poll::Ready(WorkerWake::ExactSql(Box::new(command.ok())));
            }
            if !incremental_vacuum_closed
                && let Poll::Ready(command) = incremental_vacuum_receiver.poll_recv(cx, task_cx)
            {
                return Poll::Ready(WorkerWake::IncrementalVacuum(Box::new(command.ok())));
            }
            if !input_closed && let Poll::Ready(item) = receiver.poll_recv(cx, task_cx) {
                return Poll::Ready(WorkerWake::Write(item.ok()));
            }
            Poll::Pending
        });
        if wake.is_ready() {
            return wake;
        }
        // `Instant` decides the deadline. The pool timer keeps its own clock
        // and only wakes the task, so one that fires early is armed again for
        // the rest of the wait.
        if let (Some(deadline), Some(retry)) = (deadline, retry.as_mut()) {
            loop {
                if Instant::now() >= deadline {
                    return Poll::Ready(WorkerWake::CheckpointRetry);
                }
                if retry.as_mut().poll(task_cx).is_pending() {
                    break;
                }
                *retry = arm(deadline);
            }
        }
        collect_idle_thread_heap_v1();
        Poll::Pending
    })
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_wake(
    wake: WorkerWake,
    queue: &mut FairQueue<AcceptedRequest>,
    inflight: &mut HashMap<StoreOperationIdV1, SharedReply>,
    exact_sql_queue: &mut VecDeque<ExactSqlWriterCommand>,
    incremental_vacuum_queue: &mut VecDeque<IncrementalVacuumCommand>,
    checkpoint_queue: &mut VecDeque<CheckpointCommand>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
    exact_sql_closed: &mut bool,
    incremental_vacuum_closed: &mut bool,
    checkpoint_closed: &mut bool,
) {
    match wake {
        WorkerWake::Write(Some(item)) => {
            let _ = enqueue(queue, inflight, item, telemetry);
        }
        WorkerWake::Write(None) => *input_closed = true,
        WorkerWake::ExactSql(command) => match *command {
            Some(command) => exact_sql_queue.push_back(command),
            None => *exact_sql_closed = true,
        },
        WorkerWake::IncrementalVacuum(command) => match *command {
            Some(command) => incremental_vacuum_queue.push_back(command),
            None => *incremental_vacuum_closed = true,
        },
        WorkerWake::Checkpoint(command) => match *command {
            Some(command) => checkpoint_queue.push_back(command),
            None => *checkpoint_closed = true,
        },
        WorkerWake::Shutdown => {}
        WorkerWake::CheckpointRetry => {}
    }
}

/// Move every command already sitting in `receiver` into `queue`.
///
/// Each auxiliary channel (exact SQL, incremental vacuum,
/// checkpoint) drains identically, park the command, stop on empty, and latch
/// `input_closed` once the sender is gone, so they share this one loop. The
/// product-write channel does not: it settles duplicates through
/// [`drain_ingress`] instead of parking them.
pub(super) fn drain_command_ingress<T>(
    receiver: &mut mpsc::Receiver<T>,
    queue: &mut VecDeque<T>,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(command) => queue.push_back(command),
            Err(mpsc::RecvError::Empty) => break,
            Err(mpsc::RecvError::Disconnected | mpsc::RecvError::Cancelled) => {
                *input_closed = true;
                break;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AuxiliaryWork {
    ExactSql,
    IncrementalVacuum,
}

pub(super) fn select_auxiliary_work(
    exact_sql_waiting: bool,
    incremental_vacuum_waiting: bool,
    product_queue_empty: bool,
    prefer_auxiliary: bool,
    next: AuxiliaryWork,
) -> Option<AuxiliaryWork> {
    if !product_queue_empty && !prefer_auxiliary {
        return None;
    }
    let waiting = |work| match work {
        AuxiliaryWork::ExactSql => exact_sql_waiting,
        AuxiliaryWork::IncrementalVacuum => incremental_vacuum_waiting,
    };
    let order = match next {
        AuxiliaryWork::ExactSql => [AuxiliaryWork::ExactSql, AuxiliaryWork::IncrementalVacuum],
        AuxiliaryWork::IncrementalVacuum => {
            [AuxiliaryWork::IncrementalVacuum, AuxiliaryWork::ExactSql]
        }
    };
    order.into_iter().find(|work| waiting(*work))
}

pub(super) fn drain_ingress(
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    queue: &mut FairQueue<AcceptedRequest>,
    inflight: &mut HashMap<StoreOperationIdV1, SharedReply>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(item) => {
                let _ = enqueue(queue, inflight, item, telemetry);
            }
            Err(mpsc::RecvError::Empty) => break,
            Err(mpsc::RecvError::Disconnected | mpsc::RecvError::Cancelled) => {
                *input_closed = true;
                break;
            }
        }
    }
}

pub(super) fn enqueue(
    queue: &mut FairQueue<AcceptedRequest>,
    inflight: &mut HashMap<StoreOperationIdV1, SharedReply>,
    item: AcceptedRequest,
    telemetry: &WriterTelemetry,
) -> bool {
    let operation_id = item.operation_id().clone();
    let admission_bytes = item.admission_bytes();
    if let Some(leader) = queue.get_mut(&operation_id) {
        if leader.matches_follower(&item) {
            leader.attach_follower(item);
            telemetry.released(1, admission_bytes);
            return true;
        }
        return reject_duplicate(item, operation_id, telemetry);
    }
    if let Some(hub) = inflight.get(&operation_id)
        && hub.overlaps(&item)
    {
        if !hub.matches(&item) {
            return reject_duplicate(item, operation_id, telemetry);
        }
        hub.attach_request(item);
        telemetry.released(1, admission_bytes);
        return true;
    }
    if let Err(item) = queue.push(item) {
        return reject_duplicate(item, operation_id, telemetry);
    }
    true
}

fn reject_duplicate(
    item: AcceptedRequest,
    operation_id: StoreOperationIdV1,
    telemetry: &WriterTelemetry,
) -> bool {
    let result = Err(StorageRuntimeErrorV1::DuplicateOperationInFlight {
        operation_id: operation_id.as_str().to_owned(),
    });
    telemetry.released(1, item.admission_bytes());
    telemetry.completed(&result);
    item.settle(result);
    false
}

#[cfg(test)]
mod tests {
    use asupersync::{Budget, channel::mpsc, runtime::RuntimeBuilder};

    use super::{
        AcceptedRequest, CheckpointCommand, ExactSqlWriterCommand, IncrementalVacuumCommand,
        WorkerWake, wait_for_work,
    };

    #[test]
    fn shutdown_wakes_the_batch_wait_without_waiting_for_its_deadline() {
        let (_write_tx, mut write_rx) = mpsc::channel::<AcceptedRequest>(1);
        let (_exact_sql_tx, mut exact_sql_rx) = mpsc::channel::<ExactSqlWriterCommand>(1);
        let (_vacuum_tx, mut vacuum_rx) = mpsc::channel::<IncrementalVacuumCommand>(1);
        let (_checkpoint_tx, mut checkpoint_rx) = mpsc::channel::<CheckpointCommand>(1);
        let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
        shutdown_tx.send(()).expect("shutdown receiver is open");
        let runtime = RuntimeBuilder::current_thread()
            .build()
            .expect("test runtime");

        let cx = runtime
            .handle()
            .try_request_cx_with_budget(Budget::INFINITE)
            .unwrap();
        let wake = runtime.block_on(wait_for_work(
            &cx,
            &mut write_rx,
            &mut exact_sql_rx,
            &mut vacuum_rx,
            &mut checkpoint_rx,
            &mut shutdown_rx,
            false,
            false,
            false,
            false,
            None,
        ));

        assert!(matches!(wake, WorkerWake::Shutdown));
    }
    #[test]
    fn cancelled_context_does_not_abandon_the_writer_shutdown_wait() {
        let runtime = RuntimeBuilder::current_thread().build().expect("runtime");
        let cx = runtime
            .handle()
            .try_request_cx_with_budget(Budget::INFINITE)
            .unwrap();
        cx.set_cancel_requested(true);
        let (_write_tx, mut write_rx) = mpsc::channel::<AcceptedRequest>(1);
        let (_exact_sql_tx, mut exact_sql_rx) = mpsc::channel::<ExactSqlWriterCommand>(1);
        let (_vacuum_tx, mut vacuum_rx) = mpsc::channel::<IncrementalVacuumCommand>(1);
        let (_checkpoint_tx, mut checkpoint_rx) = mpsc::channel::<CheckpointCommand>(1);
        let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
        let shutdown = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1));
            shutdown_tx.send(()).expect("shutdown receiver is open");
        });
        let wake = runtime.block_on(wait_for_work(
            &cx,
            &mut write_rx,
            &mut exact_sql_rx,
            &mut vacuum_rx,
            &mut checkpoint_rx,
            &mut shutdown_rx,
            false,
            false,
            false,
            false,
            None,
        ));
        assert!(matches!(wake, WorkerWake::Shutdown));
        shutdown.join().unwrap();
    }

    #[test]
    fn checkpoint_deadline_wakes_empty_ingress() {
        let runtime = RuntimeBuilder::current_thread().build().expect("runtime");
        let cx = runtime
            .handle()
            .try_request_cx_with_budget(Budget::INFINITE)
            .unwrap();
        let (_write_tx, mut write_rx) = mpsc::channel::<AcceptedRequest>(1);
        let (_exact_sql_tx, mut exact_sql_rx) = mpsc::channel::<ExactSqlWriterCommand>(1);
        let (_vacuum_tx, mut vacuum_rx) = mpsc::channel::<IncrementalVacuumCommand>(1);
        let (_checkpoint_tx, mut checkpoint_rx) = mpsc::channel::<CheckpointCommand>(1);
        let (_shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
        let wake = runtime.block_on(wait_for_work(
            &cx,
            &mut write_rx,
            &mut exact_sql_rx,
            &mut vacuum_rx,
            &mut checkpoint_rx,
            &mut shutdown_rx,
            false,
            false,
            false,
            false,
            Some(std::time::Instant::now()),
        ));
        assert!(matches!(wake, WorkerWake::CheckpointRetry));
    }
}

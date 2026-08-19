//! How the worker loop learns there is work, and where that work is parked.
//!
//! One await point ([`wait_for_work`]) covers every ingress channel so the
//! worker never spins, and [`select_auxiliary_work`] is the round-robin that
//! keeps maintenance from starving product writes.

use std::{collections::VecDeque, future::poll_fn, pin::Pin, task::Poll};

use tokio::sync::mpsc;

use crate::{
    admission::{FairQueue, QueueItem},
    exact_sql::WriterCommand as ExactSqlWriterCommand,
    telemetry::WriterTelemetry,
};

use super::super::{
    backup::OnlineBackupCommand,
    request::{AcceptedRequest, CheckpointCommand, IncrementalVacuumCommand},
    settlement::infrastructure,
};
use super::HARD_CHECKPOINT_RETRY_INTERVAL;

pub(super) enum WorkerWake {
    Write(Option<AcceptedRequest>),
    ExactSql(Box<Option<ExactSqlWriterCommand>>),
    IncrementalVacuum(Box<Option<IncrementalVacuumCommand>>),
    OnlineBackup(Box<Option<OnlineBackupCommand>>),
    Checkpoint(Box<Option<CheckpointCommand>>),
    Shutdown,
    CheckpointRetry,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn wait_for_work(
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    exact_sql_receiver: &mut mpsc::Receiver<ExactSqlWriterCommand>,
    incremental_vacuum_receiver: &mut mpsc::Receiver<IncrementalVacuumCommand>,
    online_backup_receiver: &mut mpsc::Receiver<OnlineBackupCommand>,
    checkpoint_receiver: &mut mpsc::Receiver<CheckpointCommand>,
    shutdown_receiver: &mut mpsc::UnboundedReceiver<()>,
    input_closed: bool,
    exact_sql_closed: bool,
    incremental_vacuum_closed: bool,
    online_backup_closed: bool,
    checkpoint_closed: bool,
    retry_checkpoint: bool,
) -> WorkerWake {
    let receive = poll_fn(|context| {
        if Pin::new(&mut *shutdown_receiver)
            .poll_recv(context)
            .is_ready()
        {
            return Poll::Ready(WorkerWake::Shutdown);
        }
        if !checkpoint_closed
            && let Poll::Ready(command) = Pin::new(&mut *checkpoint_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::Checkpoint(Box::new(command)));
        }
        if !exact_sql_closed
            && let Poll::Ready(command) = Pin::new(&mut *exact_sql_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::ExactSql(Box::new(command)));
        }
        if !incremental_vacuum_closed
            && let Poll::Ready(command) =
                Pin::new(&mut *incremental_vacuum_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::IncrementalVacuum(Box::new(command)));
        }
        if !online_backup_closed
            && let Poll::Ready(command) = Pin::new(&mut *online_backup_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::OnlineBackup(Box::new(command)));
        }
        if !input_closed && let Poll::Ready(item) = Pin::new(&mut *receiver).poll_recv(context) {
            return Poll::Ready(WorkerWake::Write(item));
        }
        Poll::Pending
    });
    if retry_checkpoint {
        match tokio::time::timeout(HARD_CHECKPOINT_RETRY_INTERVAL, receive).await {
            Ok(wake) => wake,
            Err(_) => WorkerWake::CheckpointRetry,
        }
    } else {
        receive.await
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_wake(
    wake: WorkerWake,
    queue: &mut FairQueue<AcceptedRequest>,
    exact_sql_queue: &mut VecDeque<ExactSqlWriterCommand>,
    incremental_vacuum_queue: &mut VecDeque<IncrementalVacuumCommand>,
    online_backup_queue: &mut VecDeque<OnlineBackupCommand>,
    checkpoint_queue: &mut VecDeque<CheckpointCommand>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
    exact_sql_closed: &mut bool,
    incremental_vacuum_closed: &mut bool,
    online_backup_closed: &mut bool,
    checkpoint_closed: &mut bool,
) {
    match wake {
        WorkerWake::Write(Some(item)) => enqueue(queue, item, telemetry),
        WorkerWake::Write(None) => *input_closed = true,
        WorkerWake::ExactSql(command) => match *command {
            Some(command) => exact_sql_queue.push_back(command),
            None => *exact_sql_closed = true,
        },
        WorkerWake::IncrementalVacuum(command) => match *command {
            Some(command) => incremental_vacuum_queue.push_back(command),
            None => *incremental_vacuum_closed = true,
        },
        WorkerWake::OnlineBackup(command) => match *command {
            Some(command) => online_backup_queue.push_back(command),
            None => *online_backup_closed = true,
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
/// Each auxiliary channel (exact SQL, incremental vacuum, online backup,
/// checkpoint) drains identically — park the command, stop on empty, and latch
/// `input_closed` once the sender is gone — so they share this one loop. The
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
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
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
    OnlineBackup,
}

pub(super) fn select_auxiliary_work(
    exact_sql_waiting: bool,
    incremental_vacuum_waiting: bool,
    online_backup_waiting: bool,
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
        AuxiliaryWork::OnlineBackup => online_backup_waiting,
    };
    let order = match next {
        AuxiliaryWork::ExactSql => [
            AuxiliaryWork::ExactSql,
            AuxiliaryWork::IncrementalVacuum,
            AuxiliaryWork::OnlineBackup,
        ],
        AuxiliaryWork::IncrementalVacuum => [
            AuxiliaryWork::IncrementalVacuum,
            AuxiliaryWork::OnlineBackup,
            AuxiliaryWork::ExactSql,
        ],
        AuxiliaryWork::OnlineBackup => [
            AuxiliaryWork::OnlineBackup,
            AuxiliaryWork::ExactSql,
            AuxiliaryWork::IncrementalVacuum,
        ],
    };
    order.into_iter().find(|work| waiting(*work))
}

pub(super) fn drain_ingress(
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    queue: &mut FairQueue<AcceptedRequest>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(item) => enqueue(queue, item, telemetry),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *input_closed = true;
                break;
            }
        }
    }
}

fn enqueue(
    queue: &mut FairQueue<AcceptedRequest>,
    item: AcceptedRequest,
    telemetry: &WriterTelemetry,
) {
    if let Err(item) = queue.push(item) {
        let result = Err(infrastructure(
            "duplicate operation id reached persistent writer",
        ));
        telemetry.released(1, item.admission_bytes());
        telemetry.completed(&result);
        item.settle(result);
    }
}

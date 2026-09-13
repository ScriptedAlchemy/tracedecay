//! How the worker loop learns there is work, and where that work is parked.
//!
//! One await point ([`wait_for_work`]) covers every ingress channel so the
//! worker never spins, and [`select_auxiliary_work`] is the round-robin that
//! keeps maintenance from starving product writes.

use std::{
    collections::{HashMap, VecDeque},
    future::poll_fn,
    pin::Pin,
    task::Poll,
};

use tokio::sync::mpsc;
use tracedecay_store::{StorageRuntimeErrorV1, StoreOperationIdV1};

use crate::{
    admission::{FairQueue, QueueItem},
    exact_sql::WriterCommand as ExactSqlWriterCommand,
    telemetry::WriterTelemetry,
};

use super::super::{
    backup::OnlineBackupCommand,
    request::{AcceptedRequest, CheckpointCommand, IncrementalVacuumCommand, SharedReply},
};
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
    checkpoint_retry_after: Option<std::time::Duration>,
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
    if let Some(retry_after) = checkpoint_retry_after {
        match tokio::time::timeout(retry_after, receive).await {
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
    inflight: &mut HashMap<StoreOperationIdV1, SharedReply>,
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
    inflight: &mut HashMap<StoreOperationIdV1, SharedReply>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(item) => {
                let _ = enqueue(queue, inflight, item, telemetry);
            }
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
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
    if let Some(hub) = inflight.get(&operation_id) {
        if !hub.matches(&item) {
            return reject_duplicate(item, operation_id, telemetry);
        }
        if hub.can_attach(&item) {
            hub.attach_request(item);
            telemetry.released(1, admission_bytes);
            return true;
        }
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
    use tokio::{runtime::Builder, sync::mpsc};

    use super::{
        AcceptedRequest, CheckpointCommand, ExactSqlWriterCommand, IncrementalVacuumCommand,
        OnlineBackupCommand, WorkerWake, wait_for_work,
    };

    #[test]
    fn shutdown_wakes_the_batch_wait_without_waiting_for_its_deadline() {
        let (_write_tx, mut write_rx) = mpsc::channel::<AcceptedRequest>(1);
        let (_exact_sql_tx, mut exact_sql_rx) = mpsc::channel::<ExactSqlWriterCommand>(1);
        let (_vacuum_tx, mut vacuum_rx) = mpsc::channel::<IncrementalVacuumCommand>(1);
        let (_backup_tx, mut backup_rx) = mpsc::channel::<OnlineBackupCommand>(1);
        let (_checkpoint_tx, mut checkpoint_rx) = mpsc::channel::<CheckpointCommand>(1);
        let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
        shutdown_tx.send(()).expect("shutdown receiver is open");
        let runtime = Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");

        let wake = runtime.block_on(wait_for_work(
            &mut write_rx,
            &mut exact_sql_rx,
            &mut vacuum_rx,
            &mut backup_rx,
            &mut checkpoint_rx,
            &mut shutdown_rx,
            false,
            false,
            false,
            false,
            false,
            None,
        ));

        assert!(matches!(wake, WorkerWake::Shutdown));
    }
}

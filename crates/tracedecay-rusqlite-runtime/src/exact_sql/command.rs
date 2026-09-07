//! What the writer thread receives, and how it runs one write transaction.
//!
//! The handle side of the transport only sends [`WriterCommand`]s; everything
//! that touches the writer's connection happens here, on the writer thread, so
//! a caller never holds the connection across a channel.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use rusqlite::{Connection, ErrorCode, Transaction, TransactionBehavior};

use super::guard::{AuthorizedDatabaseOperation, with_exact_sql_guard};
use super::{
    EXACT_SQL_TRANSACTION_IDLE_LIMIT, EXACT_SQL_TRANSACTION_LIMIT, ExactSqlAttachment,
    ExactSqlCommitReceipt, ExactSqlError, ExactSqlRollbackReceipt, ExactSqlRows, ExactSqlStatement,
    ExactSqlWriteAuthority, ExactSqlWriteIntent, ExecutionPolicy, MAX_EXACT_SQL_ATTACHMENTS,
    SqlRequest, SqlResult, TransactionPolicy, attach_database, detach_database, execute_batch,
    execute_query_unchecked, execute_request, publish_last_insert_rowid, sqlite_error,
    verify_write_authority,
};
use rusqlite::limits::Limit;

pub(crate) enum WriterCommand {
    Dispatch {
        request: SqlRequest,
        reply: async_channel::Sender<Result<SqlResult, ExactSqlError>>,
        last_insert_rowid: Arc<AtomicI64>,
        authority: Option<Arc<dyn ExactSqlWriteAuthority>>,
    },
    BeginTransaction {
        behavior: TransactionBehavior,
        policy: TransactionPolicy,
        receiver: Receiver<TransactionCommand>,
        reply: async_channel::Sender<Result<(), ExactSqlError>>,
        last_insert_rowid: Arc<AtomicI64>,
        expired: Arc<AtomicBool>,
        authority: Option<Arc<dyn ExactSqlWriteAuthority>>,
    },
    CheckpointWalTruncate {
        reply: async_channel::Sender<Result<ExactSqlRows, ExactSqlError>>,
        authority: Option<Arc<dyn ExactSqlWriteAuthority>>,
    },
    Vacuum {
        reply: async_channel::Sender<Result<(), ExactSqlError>>,
        authority: Option<Arc<dyn ExactSqlWriteAuthority>>,
    },
}

const BEGIN_BUSY_ATTEMPT_BUDGET: u8 = 64;

/// Takes SQLite's write lock on the worker thread, retrying while it is busy.
///
/// This is measured separately from the caller-side begin it serves. The two
/// run on different threads — the caller waits on a channel while this waits on
/// the lock — so reporting both under one label sums a queue wait and a lock
/// wait into a single population whose mean and p95 describe neither. Keep the
/// names distinct: the split is what says whether a slow begin was blocked by
/// SQLite or merely by the worker being busy with something else.
pub(super) fn begin_transaction_with_busy_retry<'connection>(
    connection: &'connection Connection,
    behavior: TransactionBehavior,
    shutdown_requested: &AtomicBool,
) -> rusqlite::Result<Transaction<'connection>> {
    if !matches!(behavior, TransactionBehavior::Immediate) {
        return Transaction::new_unchecked(connection, behavior);
    }
    hotpath::measure_block!("rusqlite.exact_sql.write_lock", {
        retry_busy_begin(
            || Transaction::new_unchecked(connection, behavior),
            shutdown_requested,
        )
    })
}

pub(super) fn retry_busy_begin<T>(
    mut begin: impl FnMut() -> rusqlite::Result<T>,
    shutdown_requested: &AtomicBool,
) -> rusqlite::Result<T> {
    let deadline = Instant::now() + EXACT_SQL_TRANSACTION_IDLE_LIMIT;
    let mut attempts_remaining = BEGIN_BUSY_ATTEMPT_BUDGET;
    let mut original_busy_error = None;
    loop {
        if shutdown_requested.load(Ordering::Acquire)
            && let Some(original) = original_busy_error
        {
            return Err(original);
        }
        match begin() {
            Ok(value) => {
                if shutdown_requested.load(Ordering::Acquire)
                    && let Some(original) = original_busy_error
                {
                    return Err(original);
                }
                return Ok(value);
            }
            Err(error) if sqlite_busy_or_locked(&error) => {
                attempts_remaining = attempts_remaining.saturating_sub(1);
                let exhausted = attempts_remaining == 0
                    || shutdown_requested.load(Ordering::Acquire)
                    || Instant::now() >= deadline;
                match original_busy_error.take() {
                    Some(original) if exhausted => return Err(original),
                    Some(original) => original_busy_error = Some(original),
                    None if exhausted => return Err(error),
                    None => original_busy_error = Some(error),
                }
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
        }
    }
}

fn sqlite_busy_or_locked(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(error, _)
            if matches!(error.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// Clock behind every transaction lease deadline: idle, absolute, renewal.
///
/// Production reads the monotonic wall clock. Tests may freeze and advance a
/// writer-thread-local fake instead, so lease renewal is provable without
/// sleeping through real lease periods.
#[cfg(not(test))]
fn lease_now() -> Instant {
    Instant::now()
}

#[cfg(test)]
use lease_clock::lease_now;

pub(crate) enum TransactionCommand {
    Attach {
        attachment: ExactSqlAttachment,
        reply: async_channel::Sender<Result<(), ExactSqlError>>,
    },
    Dispatch {
        request: SqlRequest,
        execution_policy: ExecutionPolicy,
        reply: async_channel::Sender<Result<SqlResult, ExactSqlError>>,
    },
    Commit {
        reply: async_channel::Sender<Result<ExactSqlCommitReceipt, ExactSqlError>>,
    },
    Rollback {
        reply: async_channel::Sender<Result<ExactSqlRollbackReceipt, ExactSqlError>>,
    },
}

#[hotpath::measure(label = "rusqlite_runtime.exact_sql.writer_command")]
pub(crate) fn run_writer_command(
    connection: &mut Connection,
    command: WriterCommand,
    shutdown_requested: &Arc<AtomicBool>,
) {
    match command {
        WriterCommand::Dispatch {
            request,
            reply,
            last_insert_rowid,
            authority,
        } => {
            if let Err(error) = verify_write_authority(authority.as_deref(), request.intent()) {
                let _ = reply.try_send(Err(error));
                return;
            }
            // One-shot execution only. The sibling spans
            // `rusqlite.exact_sql.transaction` and `rusqlite.exact_sql.vacuum`
            // split the pooled `rusqlite.writer.exact_sql` population, so a
            // slow exact-SQL lane is attributable to a specific command shape.
            let (mut result, inserted) = hotpath::measure_block!("rusqlite.exact_sql.execute", {
                execute_request(
                    connection,
                    request,
                    false,
                    Some(Arc::clone(shutdown_requested)),
                    None,
                    true,
                    None,
                )
            });
            publish_last_insert_rowid(
                &mut result,
                inserted,
                connection.last_insert_rowid(),
                &last_insert_rowid,
            );
            let _ = reply.try_send(result);
        }
        WriterCommand::BeginTransaction {
            behavior,
            policy,
            receiver,
            reply,
            last_insert_rowid,
            expired,
            authority,
        } => {
            if policy == TransactionPolicy::AuthorizedLongLease && authority.is_none() {
                let _ = reply.try_send(Err(ExactSqlError::AuthorityDenied(
                    "long-lease transaction requires attached write authority".to_owned(),
                )));
                return;
            }
            if let Err(error) =
                verify_write_authority(authority.as_deref(), ExactSqlWriteIntent::BeginTransaction)
            {
                let _ = reply.try_send(Err(error));
                return;
            }
            let completion = {
                let before = connection.total_changes();
                match begin_transaction_with_busy_retry(connection, behavior, shutdown_requested) {
                    // The whole writer-thread hold of one interactive
                    // transaction, caller think-time included. Every queued
                    // write and command behind it waits inside this span, so
                    // it — not SQLite execution — is what explains begin
                    // latency elsewhere while an interactive lease is open.
                    Ok(transaction) if reply.try_send(Ok(())).is_ok() => {
                        Some(hotpath::measure_block!(
                            "rusqlite.exact_sql.transaction",
                            run_transaction(
                                transaction,
                                receiver,
                                before,
                                shutdown_requested,
                                &last_insert_rowid,
                                &expired,
                                authority,
                                policy,
                            )
                        ))
                    }
                    Ok(_) => {
                        crate::hotpath_observe::record_exact_sql_transaction_outcome(
                            crate::hotpath_observe::ExactSqlTransactionOutcome::Abandoned,
                        );
                        None
                    }
                    Err(error) => {
                        crate::hotpath_observe::record_exact_sql_transaction_outcome(
                            crate::hotpath_observe::ExactSqlTransactionOutcome::BeginFailed,
                        );
                        let _ =
                            reply.try_send(Err(sqlite_error("begin exact SQL transaction", error)));
                        None
                    }
                }
            };
            if let Some(completion) = completion {
                crate::hotpath_observe::record_exact_sql_transaction_outcome(
                    completion.outcome(expired.load(Ordering::Acquire)),
                );
                if completion.finish(connection).is_err() {
                    shutdown_requested.store(true, Ordering::Release);
                }
            }
        }
        WriterCommand::CheckpointWalTruncate { reply, authority } => {
            if let Err(error) =
                verify_write_authority(authority.as_deref(), ExactSqlWriteIntent::Query)
            {
                let _ = reply.try_send(Err(error));
                return;
            }
            let statement = match ExactSqlStatement::new(
                "PRAGMA wal_checkpoint(TRUNCATE)".to_owned(),
                Vec::new(),
            ) {
                Ok(statement) => statement,
                Err(error) => {
                    let _ = reply.try_send(Err(error));
                    return;
                }
            };
            let result = hotpath::measure_block!("rusqlite.wal_checkpoint", {
                with_exact_sql_guard(
                    connection,
                    false,
                    false,
                    Some(Arc::clone(shutdown_requested)),
                    None,
                    true,
                    None,
                    crate::connection::authorize_writer,
                    false,
                    None,
                    None,
                    || execute_query_unchecked(connection, statement),
                )
            });
            let _ = reply.try_send(result);
        }
        WriterCommand::Vacuum { reply, authority } => {
            let Some(authority) = authority else {
                let _ = reply.try_send(Err(ExactSqlError::AuthorityDenied(
                    "exclusive-maintenance vacuum requires attached write authority".to_owned(),
                )));
                return;
            };
            if let Err(error) =
                verify_write_authority(Some(authority.as_ref()), ExactSqlWriteIntent::Vacuum)
            {
                let _ = reply.try_send(Err(error));
                return;
            }
            let previous_attachment_limit =
                match connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 1) {
                    Ok(previous) => previous,
                    Err(error) => {
                        let _ = reply.try_send(Err(sqlite_error(
                            "open exclusive-maintenance vacuum attachment slot",
                            error,
                        )));
                        return;
                    }
                };
            let mut result = hotpath::measure_block!("rusqlite.exact_sql.vacuum", {
                with_exact_sql_guard(
                    connection,
                    false,
                    true,
                    Some(Arc::clone(shutdown_requested)),
                    None,
                    true,
                    Some((Arc::clone(&authority), ExactSqlWriteIntent::Vacuum)),
                    crate::connection::authorize_writer,
                    true,
                    Some(AuthorizedDatabaseOperation::Vacuum),
                    None,
                    || {
                        execute_batch(connection, "PRAGMA auto_vacuum = INCREMENTAL; VACUUM")
                            .map(|_| ())
                    },
                )
            });
            if let Err(error) =
                connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, previous_attachment_limit)
            {
                shutdown_requested.store(true, Ordering::Release);
                if result.is_ok() {
                    result = Err(sqlite_error(
                        "restore exclusive-maintenance vacuum attachment limit",
                        error,
                    ));
                }
            }
            let _ = reply.try_send(result);
        }
    }
}

pub(crate) fn reject_writer_command(command: WriterCommand) {
    match command {
        WriterCommand::Dispatch { reply, .. } => {
            let _ = reply.try_send(Err(ExactSqlError::WriterUnavailable));
        }
        WriterCommand::BeginTransaction { reply, .. } => {
            let _ = reply.try_send(Err(ExactSqlError::WriterUnavailable));
        }
        WriterCommand::CheckpointWalTruncate { reply, .. } => {
            let _ = reply.try_send(Err(ExactSqlError::WriterUnavailable));
        }
        WriterCommand::Vacuum { reply, .. } => {
            let _ = reply.try_send(Err(ExactSqlError::WriterUnavailable));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_transaction(
    transaction: Transaction<'_>,
    receiver: Receiver<TransactionCommand>,
    before: u64,
    shutdown_requested: &Arc<AtomicBool>,
    last_insert_rowid: &AtomicI64,
    expired: &AtomicBool,
    authority: Option<Arc<dyn ExactSqlWriteAuthority>>,
    policy: TransactionPolicy,
) -> TransactionCompletion {
    let mut attachments = Vec::new();
    let mut previous_attachment_limit = None;
    let mut idle_deadline = lease_now() + EXACT_SQL_TRANSACTION_IDLE_LIMIT;
    let mut transaction_deadline = lease_now() + EXACT_SQL_TRANSACTION_LIMIT;
    loop {
        if shutdown_requested.load(Ordering::Acquire) {
            let _ = transaction.rollback();
            return TransactionCompletion::abandoned(attachments, previous_attachment_limit);
        }
        let now = lease_now();
        if now >= idle_deadline || now >= transaction_deadline {
            expired.store(true, Ordering::Release);
            let _ = transaction.rollback();
            return TransactionCompletion::abandoned(attachments, previous_attachment_limit);
        }
        let wait = idle_deadline
            .saturating_duration_since(now)
            .min(transaction_deadline.saturating_duration_since(now))
            .min(Duration::from_millis(25));
        let command = match receiver.recv_timeout(wait) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return TransactionCompletion::abandoned(attachments, previous_attachment_limit);
            }
        };
        match command {
            TransactionCommand::Attach { attachment, reply } => {
                if lease_now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(ExactSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if attachments.iter().any(|attached: &ExactSqlAttachment| {
                    attached
                        .database_name()
                        .eq_ignore_ascii_case(attachment.database_name())
                }) {
                    let _ = reply.try_send(Err(ExactSqlError::InvalidAttachment));
                    continue;
                }
                if let Err(error) =
                    verify_write_authority(authority.as_deref(), ExactSqlWriteIntent::Execute)
                {
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if previous_attachment_limit.is_none() {
                    match transaction
                        .set_limit(Limit::SQLITE_LIMIT_ATTACHED, MAX_EXACT_SQL_ATTACHMENTS)
                    {
                        Ok(previous) => previous_attachment_limit = Some(previous),
                        Err(error) => {
                            let _ = transaction.rollback();
                            let _ = reply.try_send(Err(sqlite_error(
                                "open exact SQL attachment limit",
                                error,
                            )));
                            return TransactionCompletion::abandoned(attachments, None);
                        }
                    }
                }
                let result = attach_database(
                    &transaction,
                    &attachment,
                    true,
                    Some(Arc::clone(shutdown_requested)),
                    Some(transaction_deadline),
                );
                match result {
                    Ok(()) => {
                        attachments.push(attachment);
                        if let Err(error) = verify_write_authority(
                            authority.as_deref(),
                            ExactSqlWriteIntent::Execute,
                        ) {
                            let _ = transaction.rollback();
                            let _ = reply.try_send(Err(error));
                            return TransactionCompletion::abandoned(
                                attachments,
                                previous_attachment_limit,
                            );
                        }
                        let _ = reply.try_send(Ok(()));
                        idle_deadline = lease_now() + EXACT_SQL_TRANSACTION_IDLE_LIMIT;
                    }
                    Err(error) => {
                        let _ = transaction.rollback();
                        let _ = reply.try_send(Err(error));
                        return TransactionCompletion::abandoned(
                            attachments,
                            previous_attachment_limit,
                        );
                    }
                }
            }
            TransactionCommand::Dispatch {
                request,
                execution_policy,
                reply,
            } => {
                if lease_now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(ExactSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) = verify_write_authority(authority.as_deref(), request.intent()) {
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if execution_policy == ExecutionPolicy::AuthorityRevalidated
                    && policy != TransactionPolicy::AuthorizedLongLease
                {
                    let _ = reply.try_send(Err(ExactSqlError::AuthorityDenied(
                        "authority-revalidated batches require an authority-bound long-lease transaction"
                            .to_owned(),
                    )));
                    continue;
                }
                let intent = request.intent();
                let repeated_authority =
                    if execution_policy == ExecutionPolicy::AuthorityRevalidated {
                        let Some(authority) = authority.as_ref() else {
                            let _ = transaction.rollback();
                            let _ = reply.try_send(Err(ExactSqlError::AuthorityDenied(
                                "authority-revalidated batch requires attached write authority"
                                    .to_owned(),
                            )));
                            return TransactionCompletion::abandoned(
                                attachments,
                                previous_attachment_limit,
                            );
                        };
                        Some((Arc::clone(authority), intent))
                    } else {
                        None
                    };
                let execution_deadline =
                    (execution_policy == ExecutionPolicy::Bounded).then_some(transaction_deadline);
                let (mut result, inserted) = execute_request(
                    &transaction,
                    request,
                    true,
                    Some(Arc::clone(shutdown_requested)),
                    execution_deadline,
                    execution_policy == ExecutionPolicy::Bounded,
                    repeated_authority,
                );
                if shutdown_requested.load(Ordering::Acquire) {
                    let _ = transaction.rollback();
                    let _ = reply.try_send(result);
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) = verify_write_authority(authority.as_deref(), intent) {
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if matches!(&result, Err(ExactSqlError::AuthorityDenied(_))) {
                    let _ = transaction.rollback();
                    let _ = reply.try_send(result);
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if execution_policy == ExecutionPolicy::Bounded
                    && lease_now() >= transaction_deadline
                {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(ExactSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                publish_last_insert_rowid(
                    &mut result,
                    inserted,
                    transaction.last_insert_rowid(),
                    last_insert_rowid,
                );
                let succeeded = result.is_ok();
                let _ = reply.try_send(result);
                if succeeded {
                    let renewed_at = lease_now();
                    idle_deadline = renewed_at + EXACT_SQL_TRANSACTION_IDLE_LIMIT;
                    // A long-lease transaction earns its next lease by
                    // committing progress: full-index replacement writes far
                    // more rows than one fixed lease can carry, but it never
                    // stalls. Idleness, shutdown, and authority revocation
                    // still cancel it, and `Ordinary` never renews.
                    if policy == TransactionPolicy::AuthorizedLongLease {
                        transaction_deadline = renewed_at + EXACT_SQL_TRANSACTION_LIMIT;
                    }
                }
            }
            TransactionCommand::Commit { reply } => {
                if lease_now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(ExactSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) =
                    verify_write_authority(authority.as_deref(), ExactSqlWriteIntent::Commit)
                {
                    let _ = transaction.rollback();
                    let _ = reply.try_send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                let changed_rows = transaction.total_changes().saturating_sub(before);
                let result = hotpath::measure_block!("rusqlite.commit", {
                    transaction
                        .commit()
                        .map(|()| ExactSqlCommitReceipt { changed_rows })
                        .map_err(|error| sqlite_error("commit immediate transaction", error))
                });
                return TransactionCompletion {
                    attachments,
                    previous_attachment_limit,
                    terminal: Some(TransactionTerminal::Commit { reply, result }),
                };
            }
            TransactionCommand::Rollback { reply } => {
                let discarded_changed_rows = transaction.total_changes().saturating_sub(before);
                let result = transaction
                    .rollback()
                    .map(|()| ExactSqlRollbackReceipt {
                        discarded_changed_rows,
                    })
                    .map_err(|error| sqlite_error("rollback immediate transaction", error));
                return TransactionCompletion {
                    attachments,
                    previous_attachment_limit,
                    terminal: Some(TransactionTerminal::Rollback { reply, result }),
                };
            }
        }
    }
}

struct TransactionCompletion {
    attachments: Vec<ExactSqlAttachment>,
    previous_attachment_limit: Option<i32>,
    terminal: Option<TransactionTerminal>,
}

enum TransactionTerminal {
    Commit {
        reply: async_channel::Sender<Result<ExactSqlCommitReceipt, ExactSqlError>>,
        result: Result<ExactSqlCommitReceipt, ExactSqlError>,
    },
    Rollback {
        reply: async_channel::Sender<Result<ExactSqlRollbackReceipt, ExactSqlError>>,
        result: Result<ExactSqlRollbackReceipt, ExactSqlError>,
    },
}

impl TransactionCompletion {
    fn abandoned(
        attachments: Vec<ExactSqlAttachment>,
        previous_attachment_limit: Option<i32>,
    ) -> Self {
        Self {
            attachments,
            previous_attachment_limit,
            terminal: None,
        }
    }

    /// Classifies how this transaction released the writer thread, for the
    /// `rusqlite.exact_sql.transaction.*` outcome counters. A missing
    /// terminal with the lease flag raised is an expiry; without it, the
    /// caller disconnected or shutdown/authority loss rolled the work back.
    fn outcome(&self, expired: bool) -> crate::hotpath_observe::ExactSqlTransactionOutcome {
        match &self.terminal {
            Some(TransactionTerminal::Commit { result: Ok(_), .. }) => {
                crate::hotpath_observe::ExactSqlTransactionOutcome::Committed
            }
            Some(TransactionTerminal::Commit { result: Err(_), .. }) => {
                crate::hotpath_observe::ExactSqlTransactionOutcome::CommitFailed
            }
            Some(TransactionTerminal::Rollback { .. }) => {
                crate::hotpath_observe::ExactSqlTransactionOutcome::RolledBack
            }
            None if expired => crate::hotpath_observe::ExactSqlTransactionOutcome::Expired,
            None => crate::hotpath_observe::ExactSqlTransactionOutcome::Abandoned,
        }
    }

    fn finish(self, connection: &Connection) -> Result<(), ExactSqlError> {
        let mut cleanup_error = None;
        for attachment in self.attachments.into_iter().rev() {
            if let Err(error) = detach_database(connection, attachment.database_name(), None)
                && cleanup_error.is_none()
            {
                cleanup_error = Some(error);
            }
        }
        if let Some(previous) = self.previous_attachment_limit
            && let Err(error) = connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, previous)
            && cleanup_error.is_none()
        {
            cleanup_error = Some(sqlite_error("restore exact SQL attachment limit", error));
        }
        match self.terminal {
            Some(TransactionTerminal::Commit { reply, result }) => {
                let response = match (result, cleanup_error.as_ref()) {
                    (Ok(_), Some(error)) => Err(error.clone()),
                    (result, _) => result,
                };
                let _ = reply.try_send(response);
            }
            Some(TransactionTerminal::Rollback { reply, result }) => {
                let response = match (result, cleanup_error.as_ref()) {
                    (Ok(_), Some(error)) => Err(error.clone()),
                    (result, _) => result,
                };
                let _ = reply.try_send(response);
            }
            None => {}
        }
        cleanup_error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
pub(crate) mod lease_clock {
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    thread_local! {
        static FAKE_LEASE_NOW: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    /// Real monotonic time until [`advance`] freezes this thread's clock.
    pub(crate) fn lease_now() -> Instant {
        FAKE_LEASE_NOW.with(Cell::get).unwrap_or_else(Instant::now)
    }

    /// Freezes this thread's lease clock `by` past its current reading.
    ///
    /// Only code already running on the writer thread — in practice an
    /// [`super::ExactSqlWriteAuthority`] verification — can move the clock
    /// the transaction loop reads.
    pub(crate) fn advance(by: Duration) {
        let advanced = lease_now() + by;
        FAKE_LEASE_NOW.with(|fake| fake.set(Some(advanced)));
    }
}

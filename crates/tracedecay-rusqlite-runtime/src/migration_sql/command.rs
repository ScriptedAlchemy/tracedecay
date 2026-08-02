//! What the writer thread receives, and how it runs one write transaction.
//!
//! The handle side of the transport only sends [`WriterCommand`]s; everything
//! that touches the writer's connection happens here, on the writer thread, so
//! a caller never holds the connection across a channel.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::guard::{AuthorizedDatabaseOperation, with_migration_guard};
use super::{
    MAX_MIGRATION_ATTACHMENTS, MIGRATION_SQL_TRANSACTION_IDLE_LIMIT,
    MIGRATION_SQL_TRANSACTION_LIMIT, MigrationSqlAttachment, MigrationSqlCommitReceipt,
    MigrationSqlError, MigrationSqlRequest, MigrationSqlResult, MigrationSqlRollbackReceipt,
    MigrationSqlRows, MigrationSqlStatement, MigrationSqlStepPolicy, MigrationSqlTransactionPolicy,
    MigrationSqlWriteAuthority, MigrationSqlWriteIntent, attach_database, detach_database,
    execute_batch, execute_query_unchecked, execute_request, publish_last_insert_rowid,
    sqlite_error, verify_write_authority,
};
use rusqlite::limits::Limit;

pub(crate) enum WriterCommand {
    Dispatch {
        request: MigrationSqlRequest,
        reply: SyncSender<Result<MigrationSqlResult, MigrationSqlError>>,
        last_insert_rowid: Arc<AtomicI64>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
    BeginTransaction {
        behavior: TransactionBehavior,
        policy: MigrationSqlTransactionPolicy,
        receiver: Receiver<TransactionCommand>,
        reply: SyncSender<Result<(), MigrationSqlError>>,
        last_insert_rowid: Arc<AtomicI64>,
        expired: Arc<AtomicBool>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
    CheckpointWalTruncate {
        reply: SyncSender<Result<MigrationSqlRows, MigrationSqlError>>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
    Vacuum {
        reply: SyncSender<Result<(), MigrationSqlError>>,
        authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    },
}

pub(crate) enum TransactionCommand {
    Attach {
        attachment: MigrationSqlAttachment,
        reply: SyncSender<Result<(), MigrationSqlError>>,
    },
    Dispatch {
        request: MigrationSqlRequest,
        step_policy: MigrationSqlStepPolicy,
        reply: SyncSender<Result<MigrationSqlResult, MigrationSqlError>>,
    },
    Commit {
        reply: SyncSender<Result<MigrationSqlCommitReceipt, MigrationSqlError>>,
    },
    Rollback {
        reply: SyncSender<Result<MigrationSqlRollbackReceipt, MigrationSqlError>>,
    },
}

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
                let _ = reply.send(Err(error));
                return;
            }
            let (mut result, inserted) = execute_request(
                connection,
                request,
                false,
                Some(Arc::clone(shutdown_requested)),
                None,
                true,
                None,
            );
            publish_last_insert_rowid(
                &mut result,
                inserted,
                connection.last_insert_rowid(),
                &last_insert_rowid,
            );
            let _ = reply.send(result);
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
            if policy == MigrationSqlTransactionPolicy::AuthorizedLongLease && authority.is_none() {
                let _ = reply.send(Err(MigrationSqlError::AuthorityDenied(
                    "long-lease transaction requires attached write authority".to_owned(),
                )));
                return;
            }
            if let Err(error) = verify_write_authority(
                authority.as_deref(),
                MigrationSqlWriteIntent::BeginTransaction,
            ) {
                let _ = reply.send(Err(error));
                return;
            }
            let completion = {
                let before = connection.total_changes();
                match connection.transaction_with_behavior(behavior) {
                    Ok(transaction) if reply.send(Ok(())).is_ok() => Some(run_transaction(
                        transaction,
                        receiver,
                        before,
                        shutdown_requested,
                        &last_insert_rowid,
                        &expired,
                        authority,
                        policy,
                    )),
                    Ok(_) => None,
                    Err(error) => {
                        let _ = reply.send(Err(sqlite_error("begin migration transaction", error)));
                        None
                    }
                }
            };
            if completion.is_some_and(|completion| completion.finish(connection).is_err()) {
                shutdown_requested.store(true, Ordering::Release);
            }
        }
        WriterCommand::CheckpointWalTruncate { reply, authority } => {
            if let Err(error) =
                verify_write_authority(authority.as_deref(), MigrationSqlWriteIntent::Query)
            {
                let _ = reply.send(Err(error));
                return;
            }
            let statement = MigrationSqlStatement::new(
                "PRAGMA wal_checkpoint(TRUNCATE)".to_owned(),
                Vec::new(),
            )
            .expect("fixed WAL checkpoint statement is valid");
            let result = with_migration_guard(
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
            );
            let _ = reply.send(result);
        }
        WriterCommand::Vacuum { reply, authority } => {
            let Some(authority) = authority else {
                let _ = reply.send(Err(MigrationSqlError::AuthorityDenied(
                    "exclusive-maintenance vacuum requires attached write authority".to_owned(),
                )));
                return;
            };
            if let Err(error) =
                verify_write_authority(Some(authority.as_ref()), MigrationSqlWriteIntent::Vacuum)
            {
                let _ = reply.send(Err(error));
                return;
            }
            let previous_attachment_limit =
                match connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 1) {
                    Ok(previous) => previous,
                    Err(error) => {
                        let _ = reply.send(Err(sqlite_error(
                            "open exclusive-maintenance vacuum attachment slot",
                            error,
                        )));
                        return;
                    }
                };
            let mut result = with_migration_guard(
                connection,
                false,
                true,
                Some(Arc::clone(shutdown_requested)),
                None,
                true,
                Some((Arc::clone(&authority), MigrationSqlWriteIntent::Vacuum)),
                crate::connection::authorize_writer,
                true,
                Some(AuthorizedDatabaseOperation::Vacuum),
                None,
                || {
                    execute_batch(connection, "PRAGMA auto_vacuum = INCREMENTAL; VACUUM")
                        .map(|_| ())
                },
            );
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
            let _ = reply.send(result);
        }
    }
}

pub(crate) fn reject_writer_command(command: WriterCommand) {
    match command {
        WriterCommand::Dispatch { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
        }
        WriterCommand::BeginTransaction { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
        }
        WriterCommand::CheckpointWalTruncate { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
        }
        WriterCommand::Vacuum { reply, .. } => {
            let _ = reply.send(Err(MigrationSqlError::WriterUnavailable));
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
    authority: Option<Arc<dyn MigrationSqlWriteAuthority>>,
    policy: MigrationSqlTransactionPolicy,
) -> TransactionCompletion {
    let mut attachments = Vec::new();
    let mut previous_attachment_limit = None;
    let mut idle_deadline = Instant::now() + MIGRATION_SQL_TRANSACTION_IDLE_LIMIT;
    let mut transaction_deadline = Instant::now() + MIGRATION_SQL_TRANSACTION_LIMIT;
    loop {
        if shutdown_requested.load(Ordering::Acquire) {
            let _ = transaction.rollback();
            return TransactionCompletion::abandoned(attachments, previous_attachment_limit);
        }
        let now = Instant::now();
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
                if Instant::now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if attachments.iter().any(|attached: &MigrationSqlAttachment| {
                    attached
                        .database_name()
                        .eq_ignore_ascii_case(attachment.database_name())
                }) {
                    let _ = reply.send(Err(MigrationSqlError::InvalidAttachment));
                    continue;
                }
                if let Err(error) =
                    verify_write_authority(authority.as_deref(), MigrationSqlWriteIntent::Execute)
                {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if previous_attachment_limit.is_none() {
                    match transaction
                        .set_limit(Limit::SQLITE_LIMIT_ATTACHED, MAX_MIGRATION_ATTACHMENTS)
                    {
                        Ok(previous) => previous_attachment_limit = Some(previous),
                        Err(error) => {
                            let _ = transaction.rollback();
                            let _ = reply
                                .send(Err(sqlite_error("open migration attachment limit", error)));
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
                            MigrationSqlWriteIntent::Execute,
                        ) {
                            let _ = transaction.rollback();
                            let _ = reply.send(Err(error));
                            return TransactionCompletion::abandoned(
                                attachments,
                                previous_attachment_limit,
                            );
                        }
                        let _ = reply.send(Ok(()));
                        idle_deadline = Instant::now() + MIGRATION_SQL_TRANSACTION_IDLE_LIMIT;
                    }
                    Err(error) => {
                        let _ = transaction.rollback();
                        let _ = reply.send(Err(error));
                        return TransactionCompletion::abandoned(
                            attachments,
                            previous_attachment_limit,
                        );
                    }
                }
            }
            TransactionCommand::Dispatch {
                request,
                step_policy,
                reply,
            } => {
                if Instant::now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) = verify_write_authority(authority.as_deref(), request.intent()) {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if step_policy == MigrationSqlStepPolicy::AuthorizedLongSchema
                    && policy != MigrationSqlTransactionPolicy::AuthorizedLongLease
                {
                    let _ = reply.send(Err(MigrationSqlError::AuthorityDenied(
                        "long schema steps require an authority-bound long-lease transaction"
                            .to_owned(),
                    )));
                    continue;
                }
                let intent = request.intent();
                let repeated_authority =
                    (step_policy == MigrationSqlStepPolicy::AuthorizedLongSchema).then(|| {
                        (
                            Arc::clone(authority.as_ref().expect("schema authority")),
                            intent,
                        )
                    });
                let execution_deadline = (step_policy == MigrationSqlStepPolicy::Bounded)
                    .then_some(transaction_deadline);
                let (mut result, inserted) = execute_request(
                    &transaction,
                    request,
                    true,
                    Some(Arc::clone(shutdown_requested)),
                    execution_deadline,
                    step_policy == MigrationSqlStepPolicy::Bounded,
                    repeated_authority,
                );
                if shutdown_requested.load(Ordering::Acquire) {
                    let _ = transaction.rollback();
                    let _ = reply.send(result);
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) = verify_write_authority(authority.as_deref(), intent) {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if matches!(&result, Err(MigrationSqlError::AuthorityDenied(_))) {
                    let _ = transaction.rollback();
                    let _ = reply.send(result);
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if step_policy == MigrationSqlStepPolicy::Bounded
                    && Instant::now() >= transaction_deadline
                {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
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
                let _ = reply.send(result);
                if succeeded {
                    let renewed_at = Instant::now();
                    idle_deadline = renewed_at + MIGRATION_SQL_TRANSACTION_IDLE_LIMIT;
                    // A long-lease transaction earns its next lease by
                    // committing progress: full-index replacement writes far
                    // more rows than one fixed lease can carry, but it never
                    // stalls. Idleness, shutdown, and authority revocation
                    // still cancel it, and `Ordinary` never renews.
                    if policy == MigrationSqlTransactionPolicy::AuthorizedLongLease {
                        transaction_deadline = renewed_at + MIGRATION_SQL_TRANSACTION_LIMIT;
                    }
                }
            }
            TransactionCommand::Commit { reply } => {
                if Instant::now() >= transaction_deadline {
                    expired.store(true, Ordering::Release);
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(MigrationSqlError::TransactionExpired));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                if let Err(error) =
                    verify_write_authority(authority.as_deref(), MigrationSqlWriteIntent::Commit)
                {
                    let _ = transaction.rollback();
                    let _ = reply.send(Err(error));
                    return TransactionCompletion::abandoned(
                        attachments,
                        previous_attachment_limit,
                    );
                }
                let changed_rows = transaction.total_changes().saturating_sub(before);
                let result = transaction
                    .commit()
                    .map(|()| MigrationSqlCommitReceipt { changed_rows })
                    .map_err(|error| sqlite_error("commit immediate transaction", error));
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
                    .map(|()| MigrationSqlRollbackReceipt {
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
    attachments: Vec<MigrationSqlAttachment>,
    previous_attachment_limit: Option<i32>,
    terminal: Option<TransactionTerminal>,
}

enum TransactionTerminal {
    Commit {
        reply: SyncSender<Result<MigrationSqlCommitReceipt, MigrationSqlError>>,
        result: Result<MigrationSqlCommitReceipt, MigrationSqlError>,
    },
    Rollback {
        reply: SyncSender<Result<MigrationSqlRollbackReceipt, MigrationSqlError>>,
        result: Result<MigrationSqlRollbackReceipt, MigrationSqlError>,
    },
}

impl TransactionCompletion {
    fn abandoned(
        attachments: Vec<MigrationSqlAttachment>,
        previous_attachment_limit: Option<i32>,
    ) -> Self {
        Self {
            attachments,
            previous_attachment_limit,
            terminal: None,
        }
    }

    fn finish(self, connection: &Connection) -> Result<(), MigrationSqlError> {
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
            cleanup_error = Some(sqlite_error("restore migration attachment limit", error));
        }
        match self.terminal {
            Some(TransactionTerminal::Commit { reply, result }) => {
                let response = match (result, cleanup_error.as_ref()) {
                    (Ok(_), Some(error)) => Err(error.clone()),
                    (result, _) => result,
                };
                let _ = reply.send(response);
            }
            Some(TransactionTerminal::Rollback { reply, result }) => {
                let response = match (result, cleanup_error.as_ref()) {
                    (Ok(_), Some(error)) => Err(error.clone()),
                    (result, _) => result,
                };
                let _ = reply.send(response);
            }
            None => {}
        }
        cleanup_error.map_or(Ok(()), Err)
    }
}

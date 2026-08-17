//! Retained recovery for settled Work blocked-interval observations.
//!
//! Run control owns both the interval receipt and its delivery marker. The
//! recovery owner therefore scans through the application authority, durably
//! claims the canonical owner fact in the shared observability outbox, and
//! only then asks run control to compare-and-swap the exact receipt to
//! delivered. A failed or cancelled claim leaves the receipt pending; the
//! storage cursor is cyclic, so a later cycle or daemon restart sees it again.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use tracedecay_application::{
    ApplicationContractError, ApplicationProblem, CapabilityGrantSnapshot, RequestContext,
};
use tracedecay_domain::{ActorId, WorkBlockedIntervalReceiptV1};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, work_blocked_interval_observation_envelope,
};

use super::work_blocked_interval_recovery_context;

const RECOVERY_PAGE_LIMIT: u32 = 32;
const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);

enum RecoveryFailureV1 {
    Database(crate::errors::TraceDecayError),
    Application(ApplicationProblem),
}

impl fmt::Display for RecoveryFailureV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "registered Work database: {error}"),
            Self::Application(problem) => {
                write!(
                    formatter,
                    "Work run-control application authority: {problem:?}"
                )
            }
        }
    }
}

/// Project-owned lifetime for settled blocked-interval recovery.
///
/// Clones share one worker. The final owner drop cancels and aborts that
/// worker, preventing a superseded project runtime from starting another
/// recovery cycle.
#[derive(Clone)]
pub(in crate::daemon::service::invocation) struct WorkBlockedIntervalObservationRecoveryOwnerV1 {
    inner: Arc<WorkBlockedIntervalObservationRecoveryInnerV1>,
}

struct WorkBlockedIntervalObservationRecoveryInnerV1 {
    cancellation: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl WorkBlockedIntervalObservationRecoveryOwnerV1 {
    pub(in crate::daemon::service::invocation) fn mount(
        database: crate::global_db::RegisteredGlobalDbLeaseV1,
        actor: ActorId,
        grant: CapabilityGrantSnapshot,
        producer: Arc<BoundedObservabilityProducerV1>,
    ) -> Result<Self, ApplicationContractError> {
        if producer.identity().authorized_scope_ref != grant.scope.project_id.as_str() {
            return Err(ApplicationContractError::Domain(
                "Work blocked-interval recovery producer scope mismatch".to_owned(),
            ));
        }
        let context = work_blocked_interval_recovery_context(&actor, &grant)?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            ApplicationContractError::Domain(
                "Work blocked-interval recovery runtime unavailable".to_owned(),
            )
        })?;
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let task = runtime.spawn(run_recovery(
            database,
            context,
            producer,
            worker_cancellation,
        ));
        Ok(Self {
            inner: Arc::new(WorkBlockedIntervalObservationRecoveryInnerV1 {
                cancellation,
                task: Mutex::new(Some(task)),
            }),
        })
    }

    pub(in crate::daemon::service) async fn shutdown(&self) {
        self.inner.cancellation.cancel();
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "Work blocked-interval recovery shutdown failed");
        }
    }
}

impl Drop for WorkBlockedIntervalObservationRecoveryInnerV1 {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let task = match self.task.get_mut() {
            Ok(task) => task.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(task) = task {
            task.abort();
        }
    }
}

async fn run_recovery(
    database: crate::global_db::RegisteredGlobalDbLeaseV1,
    context: RequestContext,
    producer: Arc<BoundedObservabilityProducerV1>,
    cancellation: CancellationToken,
) {
    loop {
        let read_database = database.clone();
        let read_context = context.clone();
        let mut read = tokio::task::spawn_blocking(move || {
            read_pending_receipts(&read_database, &read_context)
        });
        let receipts = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                read.abort();
                return;
            }
            result = &mut read => match result {
                Ok(Ok(receipts)) => receipts,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Work blocked-interval recovery scan failed");
                    Vec::new()
                }
                Err(error) => {
                    tracing::warn!(?error, "Work blocked-interval recovery scan worker failed");
                    Vec::new()
                }
            },
        };

        for receipt in receipts {
            if !receipt.is_settled() {
                tracing::warn!(
                    "Work blocked-interval recovery refused an unsettled source receipt"
                );
                continue;
            }
            let envelope = match work_blocked_interval_observation_envelope(
                producer.as_ref(),
                context.scope().project_id.as_str(),
                &receipt,
            ) {
                Ok(envelope) => envelope,
                Err(error) => {
                    tracing::warn!(
                        error,
                        "Work blocked-interval recovery refused an invalid source receipt"
                    );
                    continue;
                }
            };
            let emission = producer.emit_owner_fact(envelope);
            tokio::pin!(emission);
            let emission = tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                outcome = &mut emission => outcome,
            };
            if let Err(error) = emission {
                tracing::warn!(
                    ?error,
                    "Work blocked-interval durable observability claim failed"
                );
                continue;
            }

            let mark_database = database.clone();
            let mark_context = context.clone();
            let mut mark = tokio::task::spawn_blocking(move || {
                mark_receipt_durable(&mark_database, &mark_context, &receipt)
            });
            let marked = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    mark.abort();
                    return;
                }
                result = &mut mark => result,
            };
            match marked {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(
                    %error,
                    "Work blocked-interval exact source marker remains pending"
                ),
                Err(error) => {
                    tracing::warn!(?error, "Work blocked-interval source marker worker failed")
                }
            }
        }

        tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            () = tokio::time::sleep(RECOVERY_INTERVAL) => {}
        }
    }
}

fn read_pending_receipts(
    database: &crate::global_db::RegisteredGlobalDb,
    context: &RequestContext,
) -> Result<Vec<WorkBlockedIntervalReceiptV1>, RecoveryFailureV1> {
    let work = database
        .work_application_services()
        .map_err(RecoveryFailureV1::Database)?;
    work.run_control()
        .next_settled_blocked_intervals_for_observation(context, RECOVERY_PAGE_LIMIT)
        .map_err(RecoveryFailureV1::Application)
}

fn mark_receipt_durable(
    database: &crate::global_db::RegisteredGlobalDb,
    context: &RequestContext,
    receipt: &WorkBlockedIntervalReceiptV1,
) -> Result<(), RecoveryFailureV1> {
    let work = database
        .work_application_services()
        .map_err(RecoveryFailureV1::Database)?;
    work.run_control()
        .mark_settled_blocked_interval_durable(context, receipt)
        .map_err(RecoveryFailureV1::Application)
}

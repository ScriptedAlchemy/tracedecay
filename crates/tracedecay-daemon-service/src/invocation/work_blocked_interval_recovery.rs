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

use super::recovery_schedule::run_recovery_loop;
use super::work_blocked_interval_recovery_context;

const RECOVERY_PAGE_LIMIT: u32 = 32;
/// A restart scan provides prompt recovery; this interval is only a backstop
/// for writes committed by older processes that could not emit this process's
/// in-memory signal.
const SAFETY_INTERVAL: Duration = Duration::from_secs(60);

enum RecoveryFailureV1 {
    Database(tracedecay_domain::errors::TraceDecayError),
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
pub(crate) struct WorkBlockedIntervalObservationRecoveryOwnerV1 {
    inner: Arc<WorkBlockedIntervalObservationRecoveryInnerV1>,
}

struct WorkBlockedIntervalObservationRecoveryInnerV1 {
    cancellation: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl WorkBlockedIntervalObservationRecoveryOwnerV1 {
    pub(crate) fn mount(
        database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        actor: ActorId,
        grant: CapabilityGrantSnapshot,
        producer: Arc<BoundedObservabilityProducerV1>,
        signal: tokio::sync::watch::Receiver<u64>,
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
            signal,
            worker_cancellation,
        ));
        Ok(Self {
            inner: Arc::new(WorkBlockedIntervalObservationRecoveryInnerV1 {
                cancellation,
                task: Mutex::new(Some(task)),
            }),
        })
    }

    /// Stop this owner from starting another recovery cycle, synchronously.
    ///
    /// The half of [`Self::shutdown`] that must run at shutdown *prepare*
    /// time rather than when the drain finally reaches this project: the
    /// owner is joined deep inside the project-runtime drain, so an
    /// un-cancelled loop keeps scanning while earlier phases run (settled
    /// blocked-interval scans were still logging seconds after the daemon
    /// began shutting down). Idempotent, so the join below stays correct
    /// whether or not it already ran.
    pub(super) fn cancel(&self) {
        self.inner.cancellation.cancel();
    }

    #[hotpath::skip]
    pub(super) async fn shutdown(&self) {
        self.cancel();
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
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    context: RequestContext,
    producer: Arc<BoundedObservabilityProducerV1>,
    signal: tokio::sync::watch::Receiver<u64>,
    cancellation: CancellationToken,
) {
    let scan_cancellation = cancellation.clone();
    run_recovery_loop(
        signal,
        cancellation,
        SAFETY_INTERVAL,
        "Work blocked-interval recovery",
        move |_| {
            let database = database.clone();
            let context = context.clone();
            let producer = Arc::clone(&producer);
            let cancellation = scan_cancellation.clone();
            async move { recover_once(database, context, producer, cancellation).await }
        },
    )
    .await;
}

async fn recover_once(
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    context: RequestContext,
    producer: Arc<BoundedObservabilityProducerV1>,
    cancellation: CancellationToken,
) -> Result<(), String> {
    let read_database = database.clone();
    let read_context = context.clone();
    let mut read =
        tokio::task::spawn_blocking(move || read_pending_receipts(&read_database, &read_context));
    let receipts = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            read.abort();
            read.await
                .map_err(|error| format!("cancelled scan worker did not join: {error}"))?
                .map_err(|error| format!("cancelled scan completed with failure: {error}"))?;
            return Ok(());
        }
        result = &mut read => result
            .map_err(|error| format!("scan worker failed: {error}"))?
            .map_err(|error| format!("scan failed: {error}"))?,
    };

    for receipt in receipts {
        if !receipt.is_settled() {
            return Err("source receipt is not settled".to_owned());
        }
        let envelope = work_blocked_interval_observation_envelope(
            producer.as_ref(),
            context.scope().project_id.as_str(),
            &receipt,
        )
        .map_err(|error| format!("source receipt is invalid: {error}"))?;
        let emission = producer.emit_owner_fact(envelope);
        tokio::pin!(emission);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(()),
            outcome = &mut emission => {
                outcome
                    .map_err(|error| format!("durable observability claim failed: {error:?}"))?;
            }
        }

        let mark_database = database.clone();
        let mark_context = context.clone();
        let mut mark = tokio::task::spawn_blocking(move || {
            mark_receipt_durable(&mark_database, &mark_context, &receipt)
        });
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                mark.abort();
                mark.await
                    .map_err(|error| format!("cancelled marker worker did not join: {error}"))?
                    .map_err(|error| {
                        format!("cancelled marker completed with failure: {error}")
                    })?;
                return Ok(());
            }
            result = &mut mark => result
                .map_err(|error| format!("source marker worker failed: {error}"))?
                .map_err(|error| format!("exact source marker remains pending: {error}"))?,
        }
    }
    Ok(())
}

fn read_pending_receipts(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    context: &RequestContext,
) -> Result<Vec<WorkBlockedIntervalReceiptV1>, RecoveryFailureV1> {
    let work = tracedecay_usecases::work::RegisteredWorkApplicationServicesV1::attach(database)
        .map_err(RecoveryFailureV1::Database)?;
    work.run_control()
        .next_settled_blocked_intervals_for_observation(context, RECOVERY_PAGE_LIMIT)
        .map_err(RecoveryFailureV1::Application)
}

fn mark_receipt_durable(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    context: &RequestContext,
    receipt: &WorkBlockedIntervalReceiptV1,
) -> Result<(), RecoveryFailureV1> {
    let work = tracedecay_usecases::work::RegisteredWorkApplicationServicesV1::attach(database)
        .map_err(RecoveryFailureV1::Database)?;
    work.run_control()
        .mark_settled_blocked_interval_durable(context, receipt)
        .map_err(RecoveryFailureV1::Application)
}

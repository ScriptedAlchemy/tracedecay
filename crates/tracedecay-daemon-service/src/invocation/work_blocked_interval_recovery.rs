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

use tracedecay_application::observability::{
    BoundedObservabilityProducerV1, work_blocked_interval_observation_envelope,
};
use tracedecay_contracts::{
    ApplicationContractError, ApplicationProblem, CapabilityGrantSnapshot, RequestContext,
};
use tracedecay_domain::{ActorId, WorkBlockedIntervalReceiptV1};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::native_runtime::{NativeTask, NativeTaskOutcome};

use super::recovery_schedule::{RecoveryScheduleV1, RecoveryWarningTrackerV1, run_recovery_loop};
use super::work_blocked_interval_recovery_context;

const RECOVERY_PAGE_LIMIT: u32 = 32;
/// A restart scan provides prompt recovery; this interval is only a backstop
/// for writes committed by older processes that could not emit this process's
/// in-memory signal.
const SAFETY_INTERVAL: Duration = Duration::from_mins(1);

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
    task: Mutex<Option<NativeTask<()>>>,
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
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let task = NativeTask::spawn(run_recovery(
            database,
            context,
            producer,
            signal,
            worker_cancellation,
        ))
        .map_err(|error| {
            ApplicationContractError::Domain(format!(
                "Work blocked-interval recovery could not start: {error}"
            ))
        })?;
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

    pub(super) async fn shutdown(&self) {
        self.cancel();
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let outcome = task.wait().await;
            if outcome != NativeTaskOutcome::Completed(()) {
                tracing::warn!(?outcome, "Work blocked-interval recovery shutdown failed");
            }
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
    let receipt_warnings = Arc::new(Mutex::new(RecoveryWarningTrackerV1::default()));
    run_recovery_loop(
        signal,
        cancellation,
        RecoveryScheduleV1::with_safety_interval(SAFETY_INTERVAL),
        "Work blocked-interval recovery",
        move |_| {
            let database = database.clone();
            let context = context.clone();
            let producer = Arc::clone(&producer);
            let cancellation = scan_cancellation.clone();
            let receipt_warnings = Arc::clone(&receipt_warnings);
            async move {
                recover_once(database, context, producer, cancellation, receipt_warnings).await
            }
        },
    )
    .await;
}

async fn recover_once(
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    context: RequestContext,
    producer: Arc<BoundedObservabilityProducerV1>,
    cancellation: CancellationToken,
    receipt_warnings: Arc<Mutex<RecoveryWarningTrackerV1>>,
) -> Result<(), String> {
    let read_database = database.clone();
    let read_context = context.clone();
    let read = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        read_pending_receipts(&read_database, &read_context)
    });
    tokio::pin!(read);
    let receipts = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            read.await
                .map_err(|error| format!("cancelled scan worker did not join: {error}"))?
                .map_err(|error| format!("cancelled scan completed with failure: {error}"))?;
            return Ok(());
        }
        result = &mut read => result
            .map_err(|error| format!("scan worker failed: {error}"))?
            .map_err(|error| format!("scan failed: {error}"))?,
    };

    let mut skipped_receipt = false;
    for receipt in receipts {
        if !receipt.is_settled() {
            skipped_receipt = true;
            receipt_warnings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .warn(
                    "Work blocked-interval recovery",
                    "source receipt is not settled",
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
                skipped_receipt = true;
                receipt_warnings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .warn(
                        "Work blocked-interval recovery",
                        &format!("source receipt is invalid: {error}"),
                    );
                continue;
            }
        };
        let emission = producer.emit_owner_fact(envelope);
        tokio::pin!(emission);
        let emission_result = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(()),
            outcome = &mut emission => outcome,
        };
        if let Err(error) = emission_result {
            skipped_receipt = true;
            receipt_warnings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .warn(
                    "Work blocked-interval recovery",
                    &format!("durable observability claim failed: {error:?}"),
                );
            continue;
        }

        let mark_database = database.clone();
        let mark_context = context.clone();
        let mark = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
            mark_receipt_durable(&mark_database, &mark_context, &receipt)
        });
        tokio::pin!(mark);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
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
    if !skipped_receipt {
        receipt_warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recover("Work blocked-interval recovery");
    }
    Ok(())
}

fn read_pending_receipts(
    database: &tracedecay_global_db::RegisteredGlobalDb,
    context: &RequestContext,
) -> Result<Vec<WorkBlockedIntervalReceiptV1>, RecoveryFailureV1> {
    let work = tracedecay_application::work::RegisteredWorkApplicationServicesV1::attach(database)
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
    let work = tracedecay_application::work::RegisteredWorkApplicationServicesV1::attach(database)
        .map_err(RecoveryFailureV1::Database)?;
    work.run_control()
        .mark_settled_blocked_interval_durable(context, receipt)
        .map_err(RecoveryFailureV1::Application)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tracedecay_application::observability::{
        BoundedObservabilityProducerV1, ObservabilityProducerIdentityV1,
    };
    use tracedecay_contracts::{
        CapabilityGrantId, CapabilityGrantSnapshot, DisclosureClass, ResolvedScope,
    };
    use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, UtcMicros};
    use tracedecay_runtime_core::native_runtime;
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::WorkBlockedIntervalObservationRecoveryOwnerV1;
    use crate::invocation::types::WorkDurableWriteSignalV1;
    use crate::invocation::work::workflow_census::WorkflowFanOutCensusObservationRecoveryOwnerV1;

    const HANG_GUARD: Duration = Duration::from_secs(30);

    fn work_grant(scope: &ResolvedScope, actor: &ActorId) -> CapabilityGrantSnapshot {
        CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work.native-recovery").expect("grant id"),
            1,
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest"),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            scope.clone(),
            tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
                .iter()
                .map(|(_, capability, _)| CapabilityId::new(*capability).expect("capability"))
                .collect(),
            tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
                .iter()
                .map(|(_, _, use_case)| UseCaseId::new(*use_case).expect("use case"))
                .collect(),
            DisclosureClass::Sensitive,
        )
        .expect("Work grant")
    }

    /// Both Work observation recovery loops run on the native runtime: with no
    /// Tokio runtime anywhere they mount, and shutdown joins them.
    #[test]
    fn work_observation_recovery_owners_run_without_a_tokio_runtime() {
        let site = "daemon-service.work_blocked_interval_recovery.start";
        let profile = tempfile::tempdir().expect("profile root");
        let project = tempfile::tempdir().expect("project root");
        let tokio_misses = native_runtime::tokio_runtime_missing_count(site);
        let _probe = crate::invocation::NATIVE_WRITER_WAITS_PROBE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let writer_waits = tracedecay_rusqlite_runtime::exact_sql::native_thread_writer_waits();
        native_runtime::block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_err());
            let project_id = ProjectId::new("project.work.native-recovery").expect("project id");
            let host = tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
                profile.path(),
                project.path(),
                project_id.clone(),
            )
            .await
            .expect("registered project runtime");
            let database = host
                .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
                .expect("registered project database");
            let producer = Arc::new(
                BoundedObservabilityProducerV1::start(
                    database.clone(),
                    ObservabilityProducerIdentityV1 {
                        authorized_scope_ref: project_id.as_str().to_owned(),
                        process_boot_id: "boot:native-recovery".to_owned(),
                        producer_revision: "native-recovery-producer.v1".to_owned(),
                        configuration_revision: "native-recovery-config.v1".to_owned(),
                        policy_revision: "native-recovery-policy.v1".to_owned(),
                    },
                    8,
                )
                .expect("observability producer"),
            );
            let actor = ActorId::new("actor.work.native-recovery").expect("actor id");
            let scope = ResolvedScope::new(
                project_id.clone(),
                tracedecay_domain::RepositoryId::new("repository.work.native-recovery")
                    .expect("repository id"),
                tracedecay_domain::WorktreeId::new("worktree.work.native-recovery")
                    .expect("worktree id"),
                None,
            )
            .expect("resolved scope");
            let signal = WorkDurableWriteSignalV1::default();
            let blocked = WorkBlockedIntervalObservationRecoveryOwnerV1::mount(
                database.clone(),
                actor.clone(),
                work_grant(&scope, &actor),
                Arc::clone(&producer),
                signal.subscribe(),
            )
            .unwrap_or_else(|error| panic!("blocked-interval recovery must mount: {error}"));
            let census = WorkflowFanOutCensusObservationRecoveryOwnerV1::mount(
                database,
                project_id,
                producer,
                signal.subscribe(),
            )
            .unwrap_or_else(|error| panic!("census recovery must mount: {error}"));
            native_runtime::timeout(HANG_GUARD, async {
                blocked.shutdown().await;
                census.shutdown().await;
            })
            .await
            .expect("shutdown must join both recovery loops");
        })
        .expect("native runtime");
        assert_eq!(
            native_runtime::tokio_runtime_missing_count(site),
            tokio_misses,
            "mounting must not look for a Tokio runtime"
        );
        assert_eq!(
            tracedecay_rusqlite_runtime::exact_sql::native_thread_writer_waits(),
            writer_waits,
            "recovery scans must not park a native thread on a synchronous writer wait"
        );
    }
}

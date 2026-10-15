//! Daemon Hook V2 replay consumer.
//!
//! Owns the per-project sweep, delivery-receipt drain, and pending-work
//! admission. Host-spool replay itself lives in `tracedecay-hooks` and is
//! invoked with an already-admitted spool plus the project's wire identity.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use tracedecay_contracts::ApplicationContractError;
use tracedecay_contracts::retrieval::{
    StatusHookReplayFailureV1, StatusHookReplaySpoolV1, StatusHookReplayV1,
};
use tracedecay_domain::NativeHostIdentityV1;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_global_db::DeliverySourceReceiptReadV1;
use tracedecay_hooks::{
    HookDeliveryReceiptSpoolV1, HookDeliverySourceReceiptV1, HookReplayAdmissionOutcomeV1,
    HookReplayPassReportV1, HookScopeBindingV1, HookSpoolConfigV1, HookSpoolV1,
    admit_replayed_envelope_with_authoritative_session, drain_host_spool_once, hook_v2_spool_root,
    published_hook_scope_binding,
};

use tracedecay_mcp::handlers::hook_runtime::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_envelope,
    admit_hook_v2_replayed_envelope_with_lifecycle, hook_v2_pending_work_envelopes,
};

mod spool_opener;
mod spool_watch;

#[cfg(not(unix))]
pub(in crate::daemon) use spool_opener::PortableSpoolOpenerOwners;
pub(in crate::daemon) use spool_opener::spawn_spooled_hook_opener;

/// The longest a retained record or receipt waits for its next delivery
/// attempt. Every append and receipt publication wakes the drain, and startup
/// opens every project holding either, so this sweep only retries work a pass
/// retained (for example a settlement the authority refused), a spool a pass
/// could not drain, or a wake the spool watch could not deliver.
const REPLAY_INTERVAL: Duration = Duration::from_secs(30);

/// Whether a spool root is absent. Only a missing root is an empty spool;
/// any other unreadable state is the drain's failure to report.
fn spool_root_absent(root: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Windows collapses ENOTDIR into ERROR_PATH_NOT_FOUND, so a path
            // through a regular file also reports NotFound; absence is only
            // honest once no existing ancestor is a non-directory.
            let mut ancestor = root.parent();
            while let Some(path) = ancestor {
                match std::fs::symlink_metadata(path) {
                    Ok(meta) if !meta.is_dir() => {
                        return Err(format!(
                            "hook spool root could not be inspected: {} is not a directory",
                            path.display()
                        ));
                    }
                    Ok(_) => return Ok(true),
                    Err(_) => ancestor = path.parent(),
                }
            }
            Ok(true)
        }
        Err(error) => Err(format!("hook spool root could not be inspected: {error}")),
    }
}

/// The cause a pending-work redrive left its producer work owed, or `None`
/// when the redrive admitted it.
fn unresolved_pending_work(outcome: &HookV2AdmissionOutcomeV1) -> Option<&'static str> {
    match outcome {
        HookV2AdmissionOutcomeV1::Admitted { .. }
        | HookV2AdmissionOutcomeV1::ExactDuplicate { .. } => None,
        HookV2AdmissionOutcomeV1::Conflict => Some("conflicted with an earlier admission"),
        HookV2AdmissionOutcomeV1::CatchupRequired => Some("requires binding catch-up"),
        HookV2AdmissionOutcomeV1::Backpressured => Some("was backpressured"),
        HookV2AdmissionOutcomeV1::Unavailable => Some("was unavailable"),
    }
}

fn replay_admission_outcome(outcome: HookV2AdmissionOutcomeV1) -> HookReplayAdmissionOutcomeV1 {
    match outcome {
        HookV2AdmissionOutcomeV1::Admitted { .. } => HookReplayAdmissionOutcomeV1::Admitted,
        HookV2AdmissionOutcomeV1::ExactDuplicate { .. } => {
            HookReplayAdmissionOutcomeV1::ExactDuplicate
        }
        HookV2AdmissionOutcomeV1::Conflict => HookReplayAdmissionOutcomeV1::Conflict,
        HookV2AdmissionOutcomeV1::CatchupRequired => HookReplayAdmissionOutcomeV1::CatchupRequired,
        HookV2AdmissionOutcomeV1::Backpressured => HookReplayAdmissionOutcomeV1::Backpressured,
        HookV2AdmissionOutcomeV1::Unavailable => HookReplayAdmissionOutcomeV1::Unavailable,
    }
}

/// Settles and releases one host's delivery receipts. The drain shares the
/// spool with hook callbacks, which write it while the daemon is down; a
/// drain woken by a publication waits out the publishing writer. A receipt
/// that is not settled stays durable for the next sweep, and the failure
/// names its cause. `Ok(true)` means a full batch settled and receipts
/// remain behind it.
#[tracing::instrument(name = "daemon.hook_replay.receipt_drain", level = "trace", skip_all)]
async fn drain_hook_delivery_receipts(
    data_root: &Path,
    host: NativeHostIdentityV1,
    authority: &tracedecay_application::observability::DeliverySettlementAuthorityV1,
) -> Result<bool, String> {
    let root = tracedecay_hooks::hook_delivery_receipt_spool_root(data_root, host);
    if spool_root_absent(&root)? {
        return Ok(false);
    }
    let batch = usize::from(tracedecay_hooks::MAX_REPLAY_BATCH_RECORDS);
    let open = || {
        HookDeliveryReceiptSpoolV1::open(&root, tracedecay_hooks::HOOK_SYNCHRONOUS_BUDGET)
            .map_err(|error| error.to_string())
    };
    let receipts = open()?.pending(batch).map_err(|error| error.to_string())?;
    let full_batch = receipts.len() >= batch;

    let mut settled = Vec::new();
    let mut unsettled = None;
    for receipt in receipts {
        let receipt_hex = encode_lowercase_hex(&receipt.receipt_id);
        let source_receipt_ref = format!("hook:delivery:{receipt_hex}");
        let settlement = async {
            // A retry after spool acknowledgement has fresh wall-clock evidence
            // but the same logical key. Replay the durable settlement, keeping
            // exact database identity and its first timestamps authoritative.
            if let Some(DeliverySourceReceiptReadV1::Settled(stored)) =
                authority.attempt_for_receipt(&source_receipt_ref).await?
            {
                let stored = HookDeliverySourceReceiptV1::new(stored)
                    .map_err(|error| ApplicationContractError::Domain(error.to_string()))?;
                if !stored.same_identity(&receipt) {
                    return Err(ApplicationContractError::Domain(
                        "hook delivery source receipt identity conflict".to_owned(),
                    ));
                }
                return authority.settle(&stored.settlement).await.map(drop);
            }
            authority
                .begin_receipted(&receipt.settlement.attempt, &source_receipt_ref)
                .await?;
            authority.settle(&receipt.settlement).await.map(drop)
        }
        .await;
        match settlement {
            // A successful durable settlement is enough to release the source
            // receipt.  Early recipients legitimately return `observability: None`
            // while their fan-out census is partial; only the final recipient
            // emits the complete owner fact.  Retaining those partial files would
            // replay immutable attempts forever after the database already owns
            // them.
            Ok(()) => settled.push(receipt.receipt_id),
            Err(error) => {
                unsettled.get_or_insert_with(|| format!("delivery settlement failed: {error}"));
            }
        }
    }
    if !settled.is_empty() {
        open()?
            .acknowledge_many(&settled)
            .map_err(|error| error.to_string())?;
    }
    if let Some(cause) = unsettled {
        return Err(cause);
    }
    if !full_batch {
        return Ok(false);
    }
    HookDeliveryReceiptSpoolV1::has_receipts(&root).map_err(|error| error.to_string())
}

/// What one sweep over every host left behind.
#[derive(Default)]
struct HookReplaySweepV1 {
    /// A pass settled a full batch of records or receipts, so the next sweep
    /// runs at once.
    more_pending: bool,
    failures: Vec<StatusHookReplayFailureV1>,
}

impl HookReplaySweepV1 {
    fn fail(&mut self, host: NativeHostIdentityV1, spool: StatusHookReplaySpoolV1, cause: String) {
        tracing::warn!(host = host.hook_key(), ?spool, %cause, "hook spool drain failed");
        self.failures
            .push(StatusHookReplayFailureV1 { host, spool, cause });
    }
}

#[tracing::instrument(name = "daemon.hook_replay.sweep", level = "trace", skip_all)]
async fn drain_all_hosts(
    graph: &tracedecay_project::project::TraceDecay,
    data_root: &Path,
    delivery_settlements: &tracedecay_application::observability::DeliverySettlementAuthorityV1,
    project_sessions: &tracedecay_global_db::RegisteredGlobalDb,
    background_cpu: &Arc<tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1>,
) -> HookReplaySweepV1 {
    let mut sweep = HookReplaySweepV1::default();
    let project_id =
        tracedecay_agent_hosts::hooks::hook_project_id_for_layout(graph.hook_store_layout());
    for host in tracedecay_agent_hosts::hooks::NATIVE_HOOK_HOSTS {
        let now = hook_replay_now();
        match drain_hook_delivery_receipts(data_root, *host, delivery_settlements).await {
            Ok(more) => sweep.more_pending |= more,
            Err(cause) => sweep.fail(*host, StatusHookReplaySpoolV1::DeliveryReceipts, cause),
        }
        Box::pin(redrive_pending_work(
            graph, data_root, *host, project_id, now, &mut sweep,
        ))
        .await;
        let report = match Box::pin(drain_admitted_host_spool(
            *host,
            project_id,
            now,
            graph,
            project_sessions,
            background_cpu,
        ))
        .await
        {
            Ok(report) => report,
            Err(cause) => {
                sweep.fail(*host, StatusHookReplaySpoolV1::Records, cause);
                None
            }
        };
        if let Some(report) = report
            && (report.committed > 0 || report.duplicates > 0 || report.tombstoned > 0)
        {
            tracing::debug!(
                event = "hook_v2_replay_pass",
                host = host.hook_key(),
                committed = report.committed,
                duplicates = report.duplicates,
                tombstoned = report.tombstoned,
                retained = report.retained,
                "hook V2 replay pass completed"
            );
            // A pass settles a bounded batch; records behind it replay now,
            // not a full interval later.
            match HookSpoolV1::has_records(&hook_v2_spool_root(data_root, *host)) {
                Ok(more) => sweep.more_pending |= more,
                Err(error) => {
                    sweep.fail(*host, StatusHookReplaySpoolV1::Records, error.to_string());
                }
            }
        }
    }
    sweep
}

/// Redrives the producer work `host`'s admission ledger still owes. Work a
/// redrive cannot admit stays owed, and the sweep names it.
async fn redrive_pending_work(
    graph: &tracedecay_project::project::TraceDecay,
    data_root: &Path,
    host: NativeHostIdentityV1,
    project_id: Option<[u8; 16]>,
    now: UtcMicros,
    sweep: &mut HookReplaySweepV1,
) {
    let pending_work = match hook_v2_pending_work_envelopes(data_root, host, now) {
        Ok(pending_work) => pending_work,
        Err(error) => {
            sweep.fail(
                host,
                StatusHookReplaySpoolV1::PendingWork,
                error.to_string(),
            );
            Vec::new()
        }
    };
    let mut owed = 0_usize;
    let mut owed_cause = None;
    for envelope in pending_work {
        if project_id.is_some_and(|project_id| envelope.project_id != project_id) {
            continue;
        }
        // Owed work the daemon cannot admit now stays in the ledger and
        // is redriven by the next sweep.
        let outcome = admit_replayed_envelope_with_authoritative_session(
            envelope,
            None,
            |project_id, worktree_id, protected_session_id| async move {
                tracedecay_daemon_service::context_scout_lifecycle::lookup_registered_context_scout_native_session(
                    project_id,
                    worktree_id,
                    protected_session_id,
                )
                .await
            },
            |envelope, native_session_id| async move {
                admit_hook_v2_envelope(graph, &envelope, native_session_id, hook_replay_now())
                    .await
            },
        )
        .await;
        if let Some(cause) = unresolved_pending_work(&outcome) {
            owed += 1;
            owed_cause.get_or_insert(cause);
        }
    }
    if let Some(cause) = owed_cause {
        sweep.fail(
            host,
            StatusHookReplaySpoolV1::PendingWork,
            format!("{owed} pending producer work redrive(s) stayed owed; the first {cause}"),
        );
    }
}

/// Replays one host's spooled records, or `None` when it holds none.
async fn drain_admitted_host_spool(
    host: NativeHostIdentityV1,
    project_id: Option<[u8; 16]>,
    now: UtcMicros,
    graph: &tracedecay_project::project::TraceDecay,
    project_sessions: &tracedecay_global_db::RegisteredGlobalDb,
    background_cpu: &Arc<tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1>,
) -> Result<Option<HookReplayPassReportV1>, String> {
    let data_root = &graph.hook_store_layout().data_root;
    let root = hook_v2_spool_root(data_root, host);
    // An absent/empty records file needs neither recovery nor replay, so do
    // not acquire the cross-process writer lease merely to prove it again.
    // A hook that appends after this observation publishes its record and
    // wakes the next sweep; non-empty spools still take the lease before
    // interpreting acknowledgement or recovery state.
    if spool_root_absent(&root)?
        || !HookSpoolV1::has_records(&root).map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    let project_id =
        project_id.ok_or_else(|| "the project layout has no hook project identity".to_owned())?;
    // Sibling worktrees share this data root with their own bindings: resolve
    // each record against the binding its worktree published rather than this
    // graph's alone, caching the published lookup once per worktree per pass.
    let mut resolved_bindings =
        std::collections::HashMap::<[u8; 16], Option<HookScopeBindingV1>>::new();
    let mut resolve_binding = |worktree_id: [u8; 16]| {
        resolved_bindings
            .entry(worktree_id)
            .or_insert_with(|| published_hook_scope_binding(data_root, worktree_id, host, now))
            .clone()
    };
    let pass = Box::pin(drain_host_spool_once(
            &root,
            HookSpoolConfigV1::stock(host),
            project_id,
            &mut resolve_binding,
            now,
            |envelope, native_lifecycle| async move {
                let lifecycle_for_admission = native_lifecycle.clone();
                replay_admission_outcome(
                    admit_replayed_envelope_with_authoritative_session(
                        envelope,
                        native_lifecycle,
                        |project_id, worktree_id, protected_session_id| async move {
                            tracedecay_daemon_service::context_scout_lifecycle::lookup_registered_context_scout_native_session(
                                project_id,
                                worktree_id,
                                protected_session_id,
                            )
                            .await
                        },
                        |envelope, native_session_id| async move {
                            admit_hook_v2_replayed_envelope_with_lifecycle(
                                graph,
                                &envelope,
                                native_session_id,
                                lifecycle_for_admission,
                                project_sessions,
                                background_cpu,
                                hook_replay_now(),
                            )
                            .await
                        },
                    )
                    .await,
                )
            },
        ))
        .await;
    pass.map(Some).map_err(|error| error.to_string())
}

fn hook_replay_now() -> UtcMicros {
    UtcMicros(
        tracedecay_runtime_core::tracedecay::saturating_utc_now()
            .0
            .max(1),
    )
}

struct RegisteredReplayConsumer {
    graph: Weak<tracedecay_project::project::TraceDecay>,
    delivery_settlements:
        Weak<tracedecay_application::observability::DeliverySettlementAuthorityV1>,
    task: Option<tokio::task::JoinHandle<()>>,
    /// This registration's spool wake; its identity is the registration's.
    wake: Arc<tokio::sync::Notify>,
    /// The failures of the last finished sweep; `None` before the first.
    last_sweep: Option<Vec<StatusHookReplayFailureV1>>,
}

fn registered_replay_roots() -> &'static StdMutex<BTreeMap<PathBuf, RegisteredReplayConsumer>> {
    static ROOTS: OnceLock<StdMutex<BTreeMap<PathBuf, RegisteredReplayConsumer>>> = OnceLock::new();
    ROOTS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

/// The replay drain of the project whose hook data root is `data_root`, as
/// its consumer's last sweep left it.
pub(crate) fn hook_replay_status(data_root: &Path) -> StatusHookReplayV1 {
    let roots = registered_replay_roots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match roots.get(data_root).map(|consumer| &consumer.last_sweep) {
        None => StatusHookReplayV1::Unowned,
        Some(None) => StatusHookReplayV1::Starting,
        Some(Some(failures)) if failures.is_empty() => StatusHookReplayV1::Drained,
        Some(Some(failures)) => StatusHookReplayV1::Failed {
            failures: failures.clone(),
        },
    }
}

fn publish_sweep(
    data_root: &Path,
    graph: &Weak<tracedecay_project::project::TraceDecay>,
    failures: Vec<StatusHookReplayFailureV1>,
) {
    let mut roots = registered_replay_roots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(registered) = roots.get_mut(data_root)
        && Weak::ptr_eq(&registered.graph, graph)
    {
        registered.last_sweep = Some(failures);
    }
}

#[cfg(test)]
pub(crate) fn hook_v2_replay_consumer_registered(data_root: &Path) -> bool {
    registered_replay_roots().lock().is_ok_and(|roots| {
        roots.get(data_root).is_some_and(|consumer| {
            consumer.graph.upgrade().is_some() && consumer.delivery_settlements.upgrade().is_some()
        })
    })
}

/// Start `graph`'s replay consumer for its hook data root. The newest owner
/// of a data root takes it over: a reopen can register while the retiring
/// owner's server still holds the older graph, and that graph's release must
/// not leave the reopened project without a consumer. Returns `false` when
/// `graph`'s own consumer is already running.
pub(crate) fn register_hook_v2_replay_consumer(
    graph: Arc<tracedecay_project::project::TraceDecay>,
    delivery_settlements: Arc<tracedecay_application::observability::DeliverySettlementAuthorityV1>,
    project_sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    background_cpu: Arc<tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1>,
) -> bool {
    let data_root = graph.hook_store_layout().data_root.clone();
    let project_root = graph.project_root().to_path_buf();
    let graph = Arc::downgrade(&graph);
    let delivery_settlements = Arc::downgrade(&delivery_settlements);
    let wake = Arc::new(tokio::sync::Notify::new());
    let displaced = match registered_replay_roots().lock() {
        Ok(mut roots) => {
            if roots.get(&data_root).is_some_and(|consumer| {
                Weak::ptr_eq(&consumer.graph, &graph)
                    && consumer.graph.upgrade().is_some()
                    && consumer.delivery_settlements.upgrade().is_some()
            }) {
                return false;
            }
            roots.insert(
                data_root.clone(),
                RegisteredReplayConsumer {
                    graph: graph.clone(),
                    delivery_settlements: delivery_settlements.clone(),
                    task: None,
                    wake: Arc::clone(&wake),
                    last_sweep: None,
                },
            )
        }
        Err(_) => return false,
    };
    if let Some(task) = displaced.and_then(|consumer| consumer.task) {
        task.abort();
    }
    let task_data_root = data_root.clone();
    let task_graph = graph.clone();
    let task_delivery_settlements = delivery_settlements.clone();
    let task_project_sessions = project_sessions;
    let task_background_cpu = background_cpu;
    spool_watch::attach_consumer(&data_root, &project_root, Arc::clone(&wake));
    let task_wake = Arc::clone(&wake);
    let consumer = async move {
        loop {
            let (Some(graph_owner), Some(delivery_settlements)) =
                (task_graph.upgrade(), task_delivery_settlements.upgrade())
            else {
                break;
            };
            let sweep = Box::pin(drain_all_hosts(
                &graph_owner,
                &task_data_root,
                delivery_settlements.as_ref(),
                &task_project_sessions,
                &task_background_cpu,
            ))
            .await;
            drop(graph_owner);
            drop(delivery_settlements);
            // A sweep that leaves a backlog for the next one is not a
            // finished drain; only its failures are worth publishing early.
            if !sweep.more_pending || !sweep.failures.is_empty() {
                publish_sweep(&task_data_root, &task_graph, sweep.failures);
            }
            if sweep.more_pending {
                tokio::task::yield_now().await;
                continue;
            }
            // Retained records and failed spools wait at most this interval
            // for their next attempt; a hook append wakes the drain sooner. Keep
            // the pacing WAIT separate from sweep WORK.
            let timer = tracing::Instrument::instrument(
                async {
                    tokio::select! {
                        slept = tracedecay_runtime_core::native_runtime::sleep_until(
                            std::time::Instant::now() + REPLAY_INTERVAL,
                        ) => slept,
                        () = task_wake.notified() => Ok(()),
                    }
                },
                tracing::trace_span!("daemon.hook_replay.interval_wait"),
            )
            .await;
            if let Err(error) = timer {
                // Without a timer every wait would return at once and spin.
                tracing::error!(event = "hook_replay_timer_unavailable", %error);
                break;
            }
        }
        release_replay_registration(&task_data_root, &task_wake);
    };
    // The drain has not been traced Tokio-free, so it stays on Tokio.
    let Some(runtime) =
        tracedecay_runtime_core::native_runtime::tokio_handle("daemon.hook_v2_replay_consumer")
    else {
        release_replay_registration(&data_root, &wake);
        return false;
    };
    let task = runtime.spawn(consumer);
    match registered_replay_roots().lock() {
        Ok(mut roots) => match roots.get_mut(&data_root) {
            Some(registered) if Arc::ptr_eq(&registered.wake, &wake) => {
                registered.task = Some(task);
            }
            _ => task.abort(),
        },
        Err(_) => task.abort(),
    }
    true
}

/// Detaches the consumer woken by `wake` and drops its registration, unless a
/// newer consumer already took `data_root` over.
fn release_replay_registration(data_root: &Path, wake: &Arc<tokio::sync::Notify>) {
    spool_watch::detach_consumer(data_root, wake);
    if let Ok(mut roots) = registered_replay_roots().lock()
        && roots
            .get(data_root)
            .is_some_and(|registered| Arc::ptr_eq(&registered.wake, wake))
    {
        roots.remove(data_root);
    }
}

/// Stop and join whichever replay consumer owns `data_root`. For daemon
/// shutdown and project deletion, which end every owner of the data root.
pub(crate) async fn shutdown_hook_v2_replay_consumer(data_root: &Path) {
    stop_replay_consumer(data_root, None).await;
}

/// Stop and join `owner`'s replay consumer before releasing its graph. A
/// newer owner's consumer for the same data root keeps running.
pub(crate) async fn retire_hook_v2_replay_consumer(
    data_root: &Path,
    owner: &Weak<tracedecay_project::project::TraceDecay>,
) {
    stop_replay_consumer(data_root, Some(owner)).await;
}

async fn stop_replay_consumer(
    data_root: &Path,
    owner: Option<&Weak<tracedecay_project::project::TraceDecay>>,
) {
    let consumer = {
        let mut roots = registered_replay_roots()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let owned = roots.get(data_root).is_some_and(|registered| {
            owner.is_none_or(|owner| Weak::ptr_eq(&registered.graph, owner))
        });
        owned.then(|| roots.remove(data_root)).flatten()
    };
    let Some(consumer) = consumer else {
        return;
    };
    spool_watch::detach_consumer(data_root, &consumer.wake);
    if let Some(task) = consumer.task {
        task.abort();
        let _ = task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_admitted_redrive_settles_pending_work() {
        let duplicate = HookV2AdmissionOutcomeV1::ExactDuplicate {
            context_scout_address: None,
            ready_guidance: serde_json::Value::Null,
        };
        assert_eq!(unresolved_pending_work(&duplicate), None);
        for (outcome, cause) in [
            (HookV2AdmissionOutcomeV1::Unavailable, "was unavailable"),
            (HookV2AdmissionOutcomeV1::Backpressured, "was backpressured"),
            (
                HookV2AdmissionOutcomeV1::CatchupRequired,
                "requires binding catch-up",
            ),
            (
                HookV2AdmissionOutcomeV1::Conflict,
                "conflicted with an earlier admission",
            ),
        ] {
            assert_eq!(unresolved_pending_work(&outcome), Some(cause));
        }
    }

    #[test]
    fn only_a_missing_spool_root_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(spool_root_absent(&dir.path().join("missing")), Ok(true));
        assert_eq!(spool_root_absent(dir.path()), Ok(false));
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(spool_root_absent(&file), Ok(false));
        assert!(spool_root_absent(&file.join("child")).is_err());
    }

    struct ReplayOwnerFixture {
        _temp: tempfile::TempDir,
        _settlement_runtime: tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
        project_sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        settlements: Arc<tracedecay_application::observability::DeliverySettlementAuthorityV1>,
        background_cpu: Arc<tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1>,
    }

    type ProjectGraph = Arc<tracedecay_project::project::TraceDecay>;

    /// Two owners of one project, so both resolve the same hook data root.
    async fn replay_owner_fixture() -> (ReplayOwnerFixture, ProjectGraph, ProjectGraph) {
        let temp = tempfile::tempdir().expect("replay owner fixture");
        let project = temp.path().join("project");
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(project.join("src")).expect("project source directory");
        std::fs::write(project.join("src/lib.rs"), "pub fn fixture() {}\n").expect("source");
        let options = tracedecay_project::project::TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("global.db")),
        };
        let older = Arc::new(
            tracedecay_project::project::TraceDecay::init_with_options_for_test(
                &project,
                options.clone(),
            )
            .await
            .expect("older project owner"),
        );
        let newer = Arc::new(
            tracedecay_project::project::TraceDecay::open_with_options_for_test(&project, options)
                .await
                .expect("newer project owner"),
        );
        let project_id = tracedecay_domain::ProjectId::new("project.replay.owner").expect("id");
        let settlement_runtime =
            tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
                &temp.path().join("settlement-profile"),
                &project,
                project_id.clone(),
            )
            .await
            .expect("settlement runtime");
        let project_sessions = settlement_runtime
            .project_database_arc()
            .expect("project sessions database");
        let identity = tracedecay_application::observability::ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: "boot:replay-owner".to_owned(),
            producer_revision: "replay-owner-producer.v1".to_owned(),
            configuration_revision: "replay-owner-config.v1".to_owned(),
            policy_revision: "replay-owner-policy.v1".to_owned(),
        };
        let producer = Arc::new(
            tracedecay_application::observability::BoundedObservabilityProducerV1::start(
                project_sessions.clone(),
                identity.clone(),
                8,
            )
            .expect("producer"),
        );
        let settlements = Arc::new(
            tracedecay_application::observability::DeliverySettlementAuthorityV1::new(
                project_sessions.clone(),
                producer,
                identity,
            )
            .expect("settlement authority"),
        );
        let fixture = ReplayOwnerFixture {
            _temp: temp,
            _settlement_runtime: settlement_runtime,
            project_sessions,
            settlements,
            background_cpu: Arc::new(
                tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1::new(
                    std::num::NonZeroUsize::MIN,
                ),
            ),
        };
        (fixture, older, newer)
    }

    impl ReplayOwnerFixture {
        fn register(&self, graph: &ProjectGraph) -> bool {
            register_hook_v2_replay_consumer(
                Arc::clone(graph),
                Arc::clone(&self.settlements),
                self.project_sessions.clone(),
                Arc::clone(&self.background_cpu),
            )
        }
    }

    async fn assert_newer_owner_survives(data_root: &Path, newer: &ProjectGraph) {
        let newer_task_running = registered_replay_roots()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(data_root)
            .is_some_and(|registered| {
                Weak::ptr_eq(&registered.graph, &Arc::downgrade(newer))
                    && registered
                        .task
                        .as_ref()
                        .is_some_and(|task| !task.is_finished())
            });
        assert!(
            newer_task_running,
            "the newer owner must own the data root's running replay consumer"
        );
        assert!(hook_v2_replay_consumer_registered(data_root));
        assert!(
            spool_watch::consumer_attached(data_root),
            "the newer owner's consumer must keep the spool wake"
        );
        shutdown_hook_v2_replay_consumer(data_root).await;
        assert!(!hook_v2_replay_consumer_registered(data_root));
    }

    /// The older owner's graph is gone by the time the project reopens, so
    /// the newer owner registers; the older owner's retirement then arrives.
    #[tokio::test]
    async fn a_late_retirement_leaves_the_newer_owners_replay_consumer_running() {
        let (fixture, older, newer) = replay_owner_fixture().await;
        let data_root = older.hook_store_layout().data_root.clone();
        assert_eq!(data_root, newer.hook_store_layout().data_root);
        assert!(fixture.register(&older), "the older owner starts replay");
        let older_owner = Arc::downgrade(&older);
        drop(older);
        assert!(fixture.register(&newer), "the reopened owner starts replay");

        retire_hook_v2_replay_consumer(&data_root, &older_owner).await;

        assert_newer_owner_survives(&data_root, &newer).await;
    }

    /// The project reopens while its retiring owner's server still holds the
    /// older graph. The reopened owner must take over the data root, or the
    /// older graph's release leaves the project with no consumer at all.
    #[tokio::test]
    async fn a_reopen_takes_over_replay_from_an_owner_still_retiring() {
        let (fixture, older, newer) = replay_owner_fixture().await;
        let data_root = older.hook_store_layout().data_root.clone();
        assert!(fixture.register(&older), "the older owner starts replay");
        let newer_registered = fixture.register(&newer);

        retire_hook_v2_replay_consumer(&data_root, &Arc::downgrade(&older)).await;
        drop(older);

        assert!(
            newer_registered,
            "the reopened owner's registration must be accepted"
        );
        assert_newer_owner_survives(&data_root, &newer).await;
    }
}

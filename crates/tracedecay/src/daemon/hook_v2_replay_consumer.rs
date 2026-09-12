//! Daemon Hook V2 replay consumer.
//!
//! Owns the per-project sweep, delivery-receipt drain, and pending-work
//! admission. Host-spool replay itself lives in `tracedecay-hooks` and is
//! invoked with an already-admitted spool plus the project's wire identity.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;

use tracedecay_domain::UtcMicros;
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_hooks::{
    HookHostV1, HookReplayAdmissionOutcomeV1, HookReplayPassReportV1, HookSpoolConfigV1,
    HookSpoolV1, admit_replayed_envelope_with_authoritative_session, drain_host_spool_once,
    hook_v2_spool_root, published_hook_scope_binding,
};

use crate::mcp::tools::handlers::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_envelope, hook_v2_pending_work_envelopes,
};

/// How often a project's spools are drained after the project-open pass.
const REPLAY_INTERVAL: Duration = Duration::from_secs(30);

fn replay_admission_outcome(outcome: HookV2AdmissionOutcomeV1) -> HookReplayAdmissionOutcomeV1 {
    match outcome {
        HookV2AdmissionOutcomeV1::Admitted { .. } => HookReplayAdmissionOutcomeV1::Admitted,
        HookV2AdmissionOutcomeV1::ExactDuplicate => HookReplayAdmissionOutcomeV1::ExactDuplicate,
        HookV2AdmissionOutcomeV1::Conflict => HookReplayAdmissionOutcomeV1::Conflict,
        HookV2AdmissionOutcomeV1::CatchupRequired => HookReplayAdmissionOutcomeV1::CatchupRequired,
        HookV2AdmissionOutcomeV1::Backpressured => HookReplayAdmissionOutcomeV1::Backpressured,
        HookV2AdmissionOutcomeV1::Unavailable => HookReplayAdmissionOutcomeV1::Unavailable,
    }
}

#[hotpath::measure(label = "daemon.hook_replay.receipt_drain", future = true)]
async fn drain_hook_delivery_receipts(
    data_root: &Path,
    host: HookHostV1,
    authority: &tracedecay_application::observability::DeliverySettlementAuthorityV1,
) {
    let root = tracedecay_hooks::hook_delivery_receipt_spool_root(data_root, host);
    if !root.is_dir() {
        return;
    }
    let Ok(spool) = tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(&root) else {
        return;
    };
    let Ok(receipts) = spool.pending(usize::from(tracedecay_hooks::MAX_REPLAY_BATCH_RECORDS))
    else {
        return;
    };
    drop(spool);

    let mut settled = Vec::new();
    for receipt in receipts {
        let receipt_hex = encode_lowercase_hex(&receipt.receipt_id);
        let source_receipt_ref = format!("hook:delivery:{receipt_hex}");
        if authority
            .begin_receipted(&receipt.settlement.attempt, &source_receipt_ref)
            .await
            .is_err()
        {
            continue;
        }
        let Ok(_emission) = authority.settle(&receipt.settlement).await else {
            continue;
        };
        // A successful durable settlement is enough to release the source
        // receipt.  Early recipients legitimately return `observability: None`
        // while their fan-out census is partial; only the final recipient
        // emits the complete owner fact.  Retaining those partial files would
        // replay immutable attempts forever after the database already owns
        // them.
        settled.push(receipt.receipt_id);
    }
    if settled.is_empty() {
        return;
    }
    let Ok(spool) = tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(root) else {
        return;
    };
    for receipt_id in settled {
        let _ = spool.acknowledge(receipt_id);
    }
}

/// The sweep gauge is RAII so the consumer task's `abort()` at project close
/// cannot leave a phantom in-flight sweep behind.
struct HookReplaySweepObservation;

impl HookReplaySweepObservation {
    fn begin() -> Self {
        hotpath::gauge!("daemon.hook_replay.sweeps_active").inc(1.0);
        Self
    }
}

impl Drop for HookReplaySweepObservation {
    fn drop(&mut self) {
        hotpath::gauge!("daemon.hook_replay.sweeps_active").inc(-1.0);
    }
}

#[hotpath::measure(label = "daemon.hook_replay.sweep", future = true)]
async fn drain_all_hosts(
    graph: &crate::tracedecay::TraceDecay,
    data_root: &Path,
    delivery_settlements: &tracedecay_application::observability::DeliverySettlementAuthorityV1,
) {
    let _sweep = HookReplaySweepObservation::begin();
    let project_id =
        tracedecay_agent_hosts::hooks::hook_project_id_for_layout(graph.hook_store_layout());
    for host in tracedecay_agent_hosts::hooks::NATIVE_HOOK_HOSTS {
        let now = hook_replay_now();
        drain_hook_delivery_receipts(data_root, *host, delivery_settlements).await;
        for envelope in hook_v2_pending_work_envelopes(data_root, *host, now) {
            if project_id.is_some_and(|project_id| envelope.project_id != project_id) {
                continue;
            }
            let _ = admit_replayed_envelope_with_authoritative_session(
                envelope,
                |project_id, worktree_id, protected_session_id| async move {
                    crate::daemon::context_scout_lifecycle::lookup_registered_context_scout_native_session(
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
        }
        let Some(project_id) = project_id else {
            continue;
        };
        let report = drain_admitted_host_spool(data_root, *host, project_id, now, graph).await;
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
        }
    }
}

async fn drain_admitted_host_spool(
    data_root: &Path,
    host: HookHostV1,
    project_id: [u8; 16],
    now: UtcMicros,
    graph: &crate::tracedecay::TraceDecay,
) -> Option<HookReplayPassReportV1> {
    let root = hook_v2_spool_root(data_root, host);
    if !root.is_dir() {
        return None;
    }
    // An absent/empty records file needs neither recovery nor replay, so do
    // not acquire the cross-process writer lease merely to prove it again.
    // A hook that appends after this observation publishes a durable record
    // and is picked up by the next bounded sweep; non-empty spools still take
    // the lease before interpreting acknowledgement or recovery state.
    if !HookSpoolV1::has_durable_records(&root).ok()? {
        return None;
    }
    let (spool, _report) = HookSpoolV1::open(root, HookSpoolConfigV1::stock(host), now).ok()?;
    let binding = published_hook_scope_binding(data_root, host, now);
    Some(
        drain_host_spool_once(
            spool,
            project_id,
            binding.as_ref(),
            now,
            |envelope| async move {
                replay_admission_outcome(
                    admit_replayed_envelope_with_authoritative_session(
                        envelope,
                        |project_id, worktree_id, protected_session_id| async move {
                            crate::daemon::context_scout_lifecycle::lookup_registered_context_scout_native_session(
                                project_id,
                                worktree_id,
                                protected_session_id,
                            )
                            .await
                        },
                        |envelope, native_session_id| async move {
                            admit_hook_v2_envelope(
                                graph,
                                &envelope,
                                native_session_id,
                                hook_replay_now(),
                            )
                            .await
                        },
                    )
                    .await,
                )
            },
        )
        .await,
    )
}

fn hook_replay_now() -> UtcMicros {
    UtcMicros(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |duration| {
                duration.as_micros().min(i64::MAX as u128) as i64
            })
            .max(1),
    )
}

struct RegisteredReplayConsumer {
    graph: Weak<crate::tracedecay::TraceDecay>,
    delivery_settlements:
        Weak<tracedecay_application::observability::DeliverySettlementAuthorityV1>,
    task: Option<tokio::task::JoinHandle<()>>,
}

fn registered_replay_roots() -> &'static StdMutex<BTreeMap<PathBuf, RegisteredReplayConsumer>> {
    static ROOTS: OnceLock<StdMutex<BTreeMap<PathBuf, RegisteredReplayConsumer>>> = OnceLock::new();
    ROOTS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

#[cfg(test)]
pub(crate) fn hook_v2_replay_consumer_registered(data_root: &Path) -> bool {
    registered_replay_roots().lock().is_ok_and(|roots| {
        roots.get(data_root).is_some_and(|consumer| {
            consumer.graph.upgrade().is_some() && consumer.delivery_settlements.upgrade().is_some()
        })
    })
}

/// Start the per-project replay consumer exactly once per hook data root.
/// Returns `false` when one is already running for this root.
pub(crate) fn register_hook_v2_replay_consumer(
    graph: Arc<crate::tracedecay::TraceDecay>,
    delivery_settlements: Arc<tracedecay_application::observability::DeliverySettlementAuthorityV1>,
) -> bool {
    let data_root = graph.hook_store_layout().data_root.clone();
    let graph = Arc::downgrade(&graph);
    let delivery_settlements = Arc::downgrade(&delivery_settlements);
    match registered_replay_roots().lock() {
        Ok(mut roots) => {
            if roots.get(&data_root).is_some_and(|consumer| {
                consumer.graph.upgrade().is_some()
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
                },
            );
        }
        Err(_) => return false,
    }
    let task_data_root = data_root.clone();
    let task_graph = graph.clone();
    let task_delivery_settlements = delivery_settlements.clone();
    let task = tokio::spawn(async move {
        loop {
            let (Some(graph_owner), Some(delivery_settlements)) =
                (task_graph.upgrade(), task_delivery_settlements.upgrade())
            else {
                break;
            };
            drain_all_hosts(&graph_owner, &task_data_root, delivery_settlements.as_ref()).await;
            drop(graph_owner);
            drop(delivery_settlements);
            // Retained records wait exactly this interval for their next
            // delivery attempt; keep the pacing WAIT separate from sweep WORK.
            hotpath::future!(
                tokio::time::sleep(REPLAY_INTERVAL),
                label = "daemon.hook_replay.interval_wait"
            )
            .await;
        }
        if let Ok(mut roots) = registered_replay_roots().lock()
            && roots
                .get(&task_data_root)
                .is_some_and(|registered| Weak::ptr_eq(&registered.graph, &task_graph))
        {
            roots.remove(&task_data_root);
        }
    });
    match registered_replay_roots().lock() {
        Ok(mut roots) => match roots.get_mut(&data_root) {
            Some(registered) if Weak::ptr_eq(&registered.graph, &graph) => {
                registered.task = Some(task);
            }
            _ => task.abort(),
        },
        Err(_) => task.abort(),
    }
    true
}

/// Stop and join the exact project replay consumer before releasing its graph.
pub(crate) async fn shutdown_hook_v2_replay_consumer(data_root: &Path) {
    let task = registered_replay_roots()
        .lock()
        .ok()
        .and_then(|mut roots| roots.remove(data_root))
        .and_then(|consumer| consumer.task);
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
}

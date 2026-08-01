//! Daemon-side Hook V2 replay consumer.
//!
//! A hook that cannot reach the daemon inside its synchronous budget appends
//! the exact validated envelope to the host transport spool. Nothing else in
//! the product drained that spool, so this module closes the loop: on project
//! open, and periodically thereafter, it leases spooled batches,
//! **reauthorizes every envelope against the currently published binding**,
//! feeds the survivors through the same durable admission path the live hook
//! uses (so idempotency makes replay safe), and acknowledges each record as
//! either committed or a typed terminal tombstone.
//!
//! Bounds: one pass per host per interval, at most the spool's own fair
//! per-session batch limits, and the spool's writer lease is held only for the
//! duration of a pass so a live hook can still append.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracedecay_domain::{SessionId, UtcMicros};
use tracedecay_hooks::{
    HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1, HookConfigurationSubscriberV1,
    HookEventEnvelopeV2, HookHostV1, HookScopeBindingV1, HookSpoolAckDispositionV1, HookSpoolAckV1,
    HookSpoolConfigV1, HookSpoolRecordV1, HookSpoolV1, hook_configuration_path,
    validate_replay_batch,
};

use crate::mcp::tools::handlers::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_envelope, hook_v2_pending_work_envelopes,
};

/// How often a project's spools are drained after the project-open pass.
const REPLAY_INTERVAL: Duration = Duration::from_secs(30);
/// Fair sessions leased per host per pass. The spool caps this at four.
const REPLAY_SESSIONS_PER_PASS: usize = 4;

/// Why a spooled record was terminally dropped instead of admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HookReplayTombstoneReasonV1 {
    /// The published binding no longer authorizes this envelope.
    BindingStale,
    /// The same event identity was already admitted with different bytes.
    IdentityConflict,
    /// The record outlived the spool's maximum transport age.
    Expired,
}

impl HookReplayTombstoneReasonV1 {
    pub(crate) const fn as_key(self) -> &'static str {
        match self {
            Self::BindingStale => "binding_stale",
            Self::IdentityConflict => "admission_identity_conflict",
            Self::Expired => "transport_age_exceeded",
        }
    }
}

/// What one pass did. Every counter is per host per pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HookReplayPassReportV1 {
    pub(crate) committed: u32,
    pub(crate) duplicates: u32,
    pub(crate) tombstoned: u32,
    pub(crate) retained: u32,
    pub(crate) binding_unavailable: bool,
}

pub(crate) fn hook_v2_spool_root(data_root: &Path, host: HookHostV1) -> PathBuf {
    data_root.join("hook-v2-spool").join(host.hook_key())
}

fn current_binding(
    data_root: &Path,
    host: HookHostV1,
    now: UtcMicros,
) -> Option<HookScopeBindingV1> {
    let subscriber = HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(
        hook_configuration_path(data_root, host),
    ));
    match subscriber.load_current(host, now) {
        HookConfigurationReadOutcomeV1::Bound(snapshot) => Some(snapshot.binding),
        _ => None,
    }
}

/// Deterministic transport receipt so a re-acknowledgement after a crash is
/// recognised as the same evidence rather than a conflicting one.
fn replay_receipt_id(
    record: &HookSpoolRecordV1,
    disposition: HookSpoolAckDispositionV1,
) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(b"hook-v2-replay-receipt");
    hasher.update(record.sequence.to_le_bytes());
    hasher.update(record.envelope.event_id);
    hasher.update(match disposition {
        HookSpoolAckDispositionV1::Committed => &b"committed"[..],
        HookSpoolAckDispositionV1::TerminalTombstone => &b"tombstone"[..],
    });
    let digest = hasher.finalize();
    let mut receipt = [0u8; 16];
    receipt.copy_from_slice(&digest[..16]);
    receipt
}

fn acknowledge(
    spool: &mut HookSpoolV1,
    record: &HookSpoolRecordV1,
    disposition: HookSpoolAckDispositionV1,
    now: UtcMicros,
) -> bool {
    spool
        .acknowledge(
            HookSpoolAckV1 {
                sequence: record.sequence,
                receipt_id: replay_receipt_id(record, disposition),
                disposition,
            },
            now,
        )
        .is_ok()
}

/// Drain one host spool once. `admit` reauthorizes and admits a single
/// envelope; production passes the daemon admission path, tests pass a fake.
pub(crate) async fn drain_host_spool_once<A, F>(
    data_root: &Path,
    host: HookHostV1,
    now: UtcMicros,
    admit: A,
) -> Option<HookReplayPassReportV1>
where
    A: Fn(HookEventEnvelopeV2) -> F,
    F: Future<Output = HookV2AdmissionOutcomeV1>,
{
    let root = hook_v2_spool_root(data_root, host);
    if !root.is_dir() {
        return None;
    }
    let (mut spool, _report) = HookSpoolV1::open(root, HookSpoolConfigV1::stock(host), now).ok()?;
    let mut pass = HookReplayPassReportV1::default();

    // Age-expired records are terminal regardless of binding state: the spool
    // keeps them durable precisely until the daemon says otherwise.
    for record in spool.expired_records(now) {
        if acknowledge(
            &mut spool,
            &record,
            HookSpoolAckDispositionV1::TerminalTombstone,
            now,
        ) {
            log_tombstone(host, &record, HookReplayTombstoneReasonV1::Expired);
            pass.tombstoned = pass.tombstoned.saturating_add(1);
        }
    }

    let Some(binding) = current_binding(data_root, host, now) else {
        // Without a current binding nothing can be reauthorized. Records stay
        // durable and pending; a later pass retries.
        pass.binding_unavailable = true;
        return Some(pass);
    };

    let batches = spool
        .claim_replay_batches(now, REPLAY_SESSIONS_PER_PASS)
        .ok()?;
    for batch in batches {
        let record_count = u16::try_from(batch.records.len()).unwrap_or(u16::MAX);
        if validate_replay_batch(record_count, batch.byte_count).is_err() {
            let _ = spool.release_replay_claim(batch.claim_id);
            pass.retained = pass.retained.saturating_add(record_count.into());
            continue;
        }
        for record in &batch.records {
            // Reauthorization on every replay, not once at spool time.
            if record.envelope.validate(&binding).is_err() {
                if acknowledge(
                    &mut spool,
                    record,
                    HookSpoolAckDispositionV1::TerminalTombstone,
                    now,
                ) {
                    log_tombstone(host, record, HookReplayTombstoneReasonV1::BindingStale);
                    pass.tombstoned = pass.tombstoned.saturating_add(1);
                }
                continue;
            }
            match admit(record.envelope.clone()).await {
                HookV2AdmissionOutcomeV1::Admitted { .. } => {
                    if acknowledge(
                        &mut spool,
                        record,
                        HookSpoolAckDispositionV1::Committed,
                        now,
                    ) {
                        pass.committed = pass.committed.saturating_add(1);
                    }
                }
                HookV2AdmissionOutcomeV1::ExactDuplicate => {
                    if acknowledge(
                        &mut spool,
                        record,
                        HookSpoolAckDispositionV1::Committed,
                        now,
                    ) {
                        pass.duplicates = pass.duplicates.saturating_add(1);
                    }
                }
                HookV2AdmissionOutcomeV1::Conflict => {
                    if acknowledge(
                        &mut spool,
                        record,
                        HookSpoolAckDispositionV1::TerminalTombstone,
                        now,
                    ) {
                        log_tombstone(host, record, HookReplayTombstoneReasonV1::IdentityConflict);
                        pass.tombstoned = pass.tombstoned.saturating_add(1);
                    }
                }
                HookV2AdmissionOutcomeV1::CatchupRequired => {
                    if acknowledge(
                        &mut spool,
                        record,
                        HookSpoolAckDispositionV1::TerminalTombstone,
                        now,
                    ) {
                        log_tombstone(host, record, HookReplayTombstoneReasonV1::BindingStale);
                        pass.tombstoned = pass.tombstoned.saturating_add(1);
                    }
                }
                // Transient: keep the record pending for a later pass.
                HookV2AdmissionOutcomeV1::Backpressured | HookV2AdmissionOutcomeV1::Unavailable => {
                    pass.retained = pass.retained.saturating_add(1);
                    break;
                }
            }
        }
        let _ = spool.release_replay_claim(batch.claim_id);
    }
    Some(pass)
}

fn log_tombstone(
    host: HookHostV1,
    record: &HookSpoolRecordV1,
    reason: HookReplayTombstoneReasonV1,
) {
    tracing::debug!(
        event = "hook_v2_replay_tombstone",
        host = host.hook_key(),
        sequence = record.sequence,
        reason = reason.as_key(),
        "hook V2 replay record dropped terminally"
    );
}

async fn admit_replayed_envelope_with_authoritative_session<R, RF, A, AF>(
    envelope: HookEventEnvelopeV2,
    resolve_session: R,
    admit: A,
) -> HookV2AdmissionOutcomeV1
where
    R: FnOnce([u8; 16], [u8; 16], [u8; 32]) -> RF,
    RF: Future<Output = Option<SessionId>>,
    A: FnOnce(HookEventEnvelopeV2, Option<SessionId>) -> AF,
    AF: Future<Output = HookV2AdmissionOutcomeV1>,
{
    let native_session_id = resolve_session(
        envelope.project_id,
        envelope.worktree_id,
        envelope.protected_session_id,
    )
    .await;
    admit(envelope, native_session_id).await
}

async fn drain_all_hosts(graph: &crate::tracedecay::TraceDecay, data_root: &Path) {
    for host in crate::hooks::HOOK_V2_BOUND_HOSTS {
        let now = hook_replay_now();
        for envelope in hook_v2_pending_work_envelopes(data_root, *host, now) {
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
                    admit_hook_v2_envelope(
                        graph,
                        &envelope,
                        native_session_id,
                        hook_replay_now(),
                    )
                    .await
                },
            )
            .await;
        }
        let report = drain_host_spool_once(data_root, *host, now, |envelope| async move {
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
            .await
        })
        .await;
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
    task: Option<tokio::task::JoinHandle<()>>,
}

fn registered_replay_roots() -> &'static StdMutex<BTreeMap<PathBuf, RegisteredReplayConsumer>> {
    static ROOTS: OnceLock<StdMutex<BTreeMap<PathBuf, RegisteredReplayConsumer>>> = OnceLock::new();
    ROOTS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

#[cfg(all(test, unix))]
pub(crate) fn hook_v2_replay_consumer_registered(data_root: &Path) -> bool {
    registered_replay_roots().lock().is_ok_and(|roots| {
        roots
            .get(data_root)
            .and_then(|consumer| consumer.graph.upgrade())
            .is_some()
    })
}

/// Start the per-project replay consumer exactly once per hook data root.
/// Returns `false` when one is already running for this root.
pub(crate) fn register_hook_v2_replay_consumer(graph: Arc<crate::tracedecay::TraceDecay>) -> bool {
    let data_root = graph.hook_store_layout().data_root.clone();
    let graph = Arc::downgrade(&graph);
    match registered_replay_roots().lock() {
        Ok(mut roots) => {
            if roots
                .get(&data_root)
                .and_then(|consumer| consumer.graph.upgrade())
                .is_some()
            {
                return false;
            }
            roots.insert(
                data_root.clone(),
                RegisteredReplayConsumer {
                    graph: graph.clone(),
                    task: None,
                },
            );
        }
        Err(_) => return false,
    }
    let task_data_root = data_root.clone();
    let task_graph = graph.clone();
    let task = tokio::spawn(async move {
        loop {
            let Some(graph_owner) = task_graph.upgrade() else {
                break;
            };
            drain_all_hosts(&graph_owner, &task_data_root).await;
            drop(graph_owner);
            tokio::time::sleep(REPLAY_INTERVAL).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracedecay_hooks::{
        HOOK_CONFIGURATION_SCHEMA_VERSION, HOOK_EVENT_SCHEMA_VERSION, HookBoundaryV1,
        HookCapabilityV1, HookConfigurationFileWriterV1, HookConfigurationPublisherV1,
        HookConfigurationSnapshotV1, HookEventFamily, HookEventV2, HookOrderingV1,
        stock_event_support,
    };

    const HOST: HookHostV1 = HookHostV1::ClaudeCode;

    #[test]
    fn cursor_native_identities_use_distinct_canonical_spool_roots() {
        let data_root = Path::new("/tmp/tracedecay-hook-v2");

        assert_eq!(
            hook_v2_spool_root(data_root, HookHostV1::CursorDesktop),
            data_root.join("hook-v2-spool").join("cursor-desktop")
        );
        assert_eq!(
            hook_v2_spool_root(data_root, HookHostV1::CursorCloud),
            data_root.join("hook-v2-spool").join("cursor-cloud")
        );
    }

    fn binding(epoch: u64) -> HookScopeBindingV1 {
        HookScopeBindingV1 {
            host: HOST,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: epoch,
            binding_token: [4; 32],
            capabilities: [
                HookEventFamily::SessionBoundary,
                HookEventFamily::PromptBoundary,
                HookEventFamily::ToolLifecycle,
                HookEventFamily::SavedEdit,
                HookEventFamily::TestLifecycle,
            ]
            .into_iter()
            .map(|family| HookCapabilityV1 {
                family,
                support: stock_event_support(HOST, family),
            })
            .collect(),
        }
    }

    fn envelope(event_id: u8, binding: &HookScopeBindingV1) -> HookEventEnvelopeV2 {
        HookEventEnvelopeV2 {
            schema_version: HOOK_EVENT_SCHEMA_VERSION,
            event_id: [event_id; 16],
            producer: HOST,
            protected_session_id: [event_id.wrapping_add(1).max(1); 32],
            project_id: binding.project_id,
            repository_id: binding.repository_id,
            worktree_id: binding.worktree_id,
            worktree_epoch: binding.worktree_epoch,
            binding_token: binding.binding_token,
            ordering: HookOrderingV1::Unknown,
            observed_at: UtcMicros(11),
            event: HookEventV2::SessionBoundary {
                boundary: HookBoundaryV1::TurnComplete,
            },
        }
    }

    fn publish_binding(data_root: &Path, binding: &HookScopeBindingV1, now: UtcMicros) {
        HookConfigurationPublisherV1::new(HookConfigurationFileWriterV1::new(
            hook_configuration_path(data_root, HOST),
        ))
        .publish(HookConfigurationSnapshotV1 {
            schema_version: HOOK_CONFIGURATION_SCHEMA_VERSION,
            revision: binding.worktree_epoch,
            published_at: now,
            expires_at: UtcMicros(now.0 + 86_400_000_000),
            binding: binding.clone(),
        })
        .unwrap();
    }

    fn spool_envelopes(
        data_root: &Path,
        binding: &HookScopeBindingV1,
        envelopes: &[HookEventEnvelopeV2],
        now: UtcMicros,
    ) {
        let root = hook_v2_spool_root(data_root, HOST);
        std::fs::create_dir_all(&root).unwrap();
        let (mut spool, _) = HookSpoolV1::open(root, HookSpoolConfigV1::stock(HOST), now).unwrap();
        for envelope in envelopes {
            spool.append(envelope.clone(), binding, now).unwrap();
        }
    }

    fn pending_records(data_root: &Path, now: UtcMicros) -> u32 {
        let (spool, report) = HookSpoolV1::open(
            hook_v2_spool_root(data_root, HOST),
            HookSpoolConfigV1::stock(HOST),
            now,
        )
        .unwrap();
        drop(spool);
        report.pending_records
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(1);
            let path = std::env::temp_dir().join(format!(
                "tracedecay-hook-replay-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn admitted() -> HookV2AdmissionOutcomeV1 {
        HookV2AdmissionOutcomeV1::Admitted {
            orchestration: crate::daemon::Pr13HookOrchestrationAdmissionV1::Unavailable,
            ready_guidance: serde_json::Value::Null,
            feedback_notice: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn replay_reauthorizes_and_drains_every_admitted_record() {
        let root = TestRoot::new("drain");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, now);
        spool_envelopes(
            root.path(),
            &binding,
            &[envelope(9, &binding), envelope(10, &binding)],
            now,
        );
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);

        let report = drain_host_spool_once(root.path(), HOST, now, move |envelope| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder.lock().unwrap().push(envelope.event_id);
                admitted()
            }
        })
        .await
        .unwrap();

        assert_eq!(report.committed, 2);
        assert_eq!(report.tombstoned, 0);
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!(pending_records(root.path(), now), 0);
    }

    #[tokio::test]
    async fn a_stale_binding_tombstones_without_ever_reaching_admission() {
        let root = TestRoot::new("stale");
        let now = UtcMicros(1_000);
        let spooled_binding = binding(7);
        spool_envelopes(
            root.path(),
            &spooled_binding,
            &[envelope(9, &spooled_binding)],
            now,
        );
        // The daemon has since republished a binding with a newer epoch.
        publish_binding(root.path(), &binding(8), now);
        let admissions = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&admissions);

        let report = drain_host_spool_once(root.path(), HOST, now, move |_| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                admitted()
            }
        })
        .await
        .unwrap();

        assert_eq!(report.tombstoned, 1);
        assert_eq!(report.committed, 0);
        assert_eq!(admissions.load(Ordering::Relaxed), 0);
        assert_eq!(pending_records(root.path(), now), 0);
    }

    #[tokio::test]
    async fn an_exact_duplicate_acknowledges_and_a_conflict_tombstones() {
        let root = TestRoot::new("idempotent");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, now);
        spool_envelopes(
            root.path(),
            &binding,
            &[envelope(9, &binding), envelope(10, &binding)],
            now,
        );

        let report = drain_host_spool_once(root.path(), HOST, now, |envelope| async move {
            if envelope.event_id == [9; 16] {
                HookV2AdmissionOutcomeV1::ExactDuplicate
            } else {
                HookV2AdmissionOutcomeV1::Conflict
            }
        })
        .await
        .unwrap();

        assert_eq!(report.duplicates, 1);
        assert_eq!(report.tombstoned, 1);
        assert_eq!(pending_records(root.path(), now), 0);
    }

    #[tokio::test]
    async fn an_unavailable_daemon_retains_the_record_for_a_later_pass() {
        let root = TestRoot::new("retain");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, now);
        spool_envelopes(root.path(), &binding, &[envelope(9, &binding)], now);

        let report = drain_host_spool_once(root.path(), HOST, now, |_| async move {
            HookV2AdmissionOutcomeV1::Unavailable
        })
        .await
        .unwrap();

        assert_eq!(report.retained, 1);
        assert_eq!(report.committed, 0);
        assert_eq!(pending_records(root.path(), now), 1);

        // A later pass with a healthy daemon drains it.
        let report = drain_host_spool_once(root.path(), HOST, now, |_| async move { admitted() })
            .await
            .unwrap();
        assert_eq!(report.committed, 1);
        assert_eq!(pending_records(root.path(), now), 0);
    }

    #[tokio::test]
    async fn a_missing_binding_leaves_every_record_pending() {
        let root = TestRoot::new("unbound");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        spool_envelopes(root.path(), &binding, &[envelope(9, &binding)], now);

        let report = drain_host_spool_once(root.path(), HOST, now, |_| async move { admitted() })
            .await
            .unwrap();

        assert!(report.binding_unavailable);
        assert_eq!(pending_records(root.path(), now), 1);
    }

    #[tokio::test]
    async fn live_failure_spools_then_replay_preserves_lifecycle_for_suggestion() {
        let root = TestRoot::new("lifecycle-suggestion");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, now);
        let mut edit = envelope(9, &binding);
        edit.protected_session_id =
            crate::hooks::hook_v2_protected_session_id_for_native("session.native.replay");
        edit.event = HookEventV2::SavedEdit {
            file_id: [8; 16],
            changed_range_count: 1,
        };
        // The synchronous admission failed, so the host retained only the
        // validated, payload-free envelope for daemon replay.
        spool_envelopes(root.path(), &binding, &[edit], now);
        let suggestions = Arc::new(StdMutex::new(Vec::new()));
        let captured = Arc::clone(&suggestions);

        let report = drain_host_spool_once(root.path(), HOST, now, move |envelope| {
            let captured = Arc::clone(&captured);
            async move {
                admit_replayed_envelope_with_authoritative_session(
                    envelope,
                    |project_id, worktree_id, protected_session_id| async move {
                        assert_eq!(project_id, [1; 16]);
                        assert_eq!(worktree_id, [3; 16]);
                        assert_eq!(
                            protected_session_id,
                            crate::hooks::hook_v2_protected_session_id_for_native(
                                "session.native.replay"
                            )
                        );
                        Some(SessionId::new("session.native.replay".to_owned()).unwrap())
                    },
                    |_, native_session_id| async move {
                        if native_session_id.as_ref().map(SessionId::as_str)
                            == Some("session.native.replay")
                        {
                            captured
                                .lock()
                                .unwrap()
                                .push("replayed lifecycle suggestion");
                        }
                        HookV2AdmissionOutcomeV1::Admitted {
                            orchestration:
                                crate::daemon::Pr13HookOrchestrationAdmissionV1::Enqueued,
                            ready_guidance: serde_json::json!({
                                "suggestion": "replayed lifecycle suggestion"
                            }),
                            feedback_notice: serde_json::Value::Null,
                        }
                    },
                )
                .await
            }
        })
        .await
        .unwrap();

        assert_eq!(report.committed, 1);
        assert_eq!(
            suggestions.lock().unwrap().as_slice(),
            ["replayed lifecycle suggestion"]
        );
        assert_eq!(pending_records(root.path(), now), 0);
    }

    #[tokio::test]
    async fn kimi_and_opencode_replay_preserve_native_session_and_provider_order() {
        let seen = Arc::new(StdMutex::new(Vec::new()));
        for (host, session, sequence) in [
            (HookHostV1::KimiCode, "session.kimi.replay", 41),
            (HookHostV1::OpenCode, "session.opencode.replay", 42),
        ] {
            let mut host_binding = binding(7);
            host_binding.host = host;
            host_binding.capabilities = [
                HookEventFamily::SessionBoundary,
                HookEventFamily::PromptBoundary,
                HookEventFamily::ToolLifecycle,
                HookEventFamily::SavedEdit,
                HookEventFamily::TestLifecycle,
            ]
            .into_iter()
            .map(|family| HookCapabilityV1 {
                family,
                support: stock_event_support(host, family),
            })
            .collect();
            let mut replayed = envelope(sequence as u8, &host_binding);
            replayed.producer = host;
            replayed.protected_session_id =
                crate::hooks::hook_v2_protected_session_id_for_native(session);
            replayed.ordering = HookOrderingV1::ProviderSequence(sequence);
            replayed.event = HookEventV2::SavedEdit {
                file_id: [sequence as u8; 16],
                changed_range_count: 1,
            };
            let captured = Arc::clone(&seen);

            let outcome = admit_replayed_envelope_with_authoritative_session(
                replayed,
                move |project_id, worktree_id, protected_session_id| async move {
                    assert_eq!(project_id, [1; 16]);
                    assert_eq!(worktree_id, [3; 16]);
                    assert_eq!(
                        protected_session_id,
                        crate::hooks::hook_v2_protected_session_id_for_native(session)
                    );
                    Some(SessionId::new(session.to_owned()).unwrap())
                },
                move |envelope, native_session_id| async move {
                    captured.lock().unwrap().push((
                        envelope.producer,
                        envelope.ordering,
                        native_session_id.unwrap(),
                    ));
                    admitted()
                },
            )
            .await;
            assert!(matches!(outcome, HookV2AdmissionOutcomeV1::Admitted { .. }));
        }

        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, HookHostV1::KimiCode);
        assert_eq!(seen[0].1, HookOrderingV1::ProviderSequence(41));
        assert_eq!(seen[0].2.as_str(), "session.kimi.replay");
        assert_eq!(seen[1].0, HookHostV1::OpenCode);
        assert_eq!(seen[1].1, HookOrderingV1::ProviderSequence(42));
        assert_eq!(seen[1].2.as_str(), "session.opencode.replay");
    }

    #[tokio::test]
    async fn an_expired_record_is_tombstoned_rather_than_replayed() {
        let root = TestRoot::new("expired");
        let queued_at = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, queued_at);
        spool_envelopes(root.path(), &binding, &[envelope(9, &binding)], queued_at);
        let later = UtcMicros(queued_at.0 + tracedecay_hooks::MAX_SPOOL_AGE_MICROS + 1);
        publish_binding(root.path(), &binding, later);

        let report = drain_host_spool_once(root.path(), HOST, later, |_| async move { admitted() })
            .await
            .unwrap();

        assert_eq!(report.tombstoned, 1);
        assert_eq!(report.committed, 0);
        assert_eq!(pending_records(root.path(), later), 0);
    }

    #[test]
    fn replay_receipts_are_deterministic_and_disposition_specific() {
        let binding = binding(7);
        let record = HookSpoolRecordV1 {
            sequence: 3,
            protected_session_id: [5; 32],
            queued_at: UtcMicros(9),
            envelope: envelope(9, &binding),
            encoded_len: 17,
            checksum: [6; 32],
            framed_len: 91,
        };

        assert_eq!(
            replay_receipt_id(&record, HookSpoolAckDispositionV1::Committed),
            replay_receipt_id(&record, HookSpoolAckDispositionV1::Committed)
        );
        assert_ne!(
            replay_receipt_id(&record, HookSpoolAckDispositionV1::Committed),
            replay_receipt_id(&record, HookSpoolAckDispositionV1::TerminalTombstone)
        );
    }
}

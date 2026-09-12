//! Host-spool Hook V2 replay drain.
//!
//! A hook that cannot reach the daemon inside its synchronous budget appends
//! the exact validated envelope to the host transport spool. The caller admits
//! one host spool and the project identity; this module leases fair batches,
//! reauthorizes every envelope against the published binding for that project,
//! feeds survivors through the caller's admission callback, and acknowledges
//! each record as committed or a typed terminal tombstone.
//!
//! Bounds: one pass per admitted spool, at most the spool's own fair
//! per-session batch limits, and the writer lease is held only for the
//! duration of a pass so a live hook can still append.

use std::future::Future;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracedecay_domain::UtcMicros;

use crate::{
    HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1, HookConfigurationSubscriberV1,
    HookEventEnvelopeV2, HookHostV1, HookScopeBindingV1, HookSpoolAckDispositionV1, HookSpoolAckV1,
    HookSpoolRecordV1, HookSpoolV1, hook_configuration_path, validate_replay_batch,
};

/// Fair sessions leased per host per pass. The spool caps this at four.
const REPLAY_SESSIONS_PER_PASS: usize = 4;

/// Why a spooled record was terminally dropped instead of admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookReplayTombstoneReasonV1 {
    /// The published binding no longer authorizes this envelope.
    BindingStale,
    /// The same event identity was already admitted with different bytes.
    IdentityConflict,
    /// The record outlived the spool's maximum transport age.
    Expired,
}

impl HookReplayTombstoneReasonV1 {
    #[hotpath::skip]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::BindingStale => "binding_stale",
            Self::IdentityConflict => "admission_identity_conflict",
            Self::Expired => "transport_age_exceeded",
        }
    }
}

/// What one pass did. Every counter is per host per pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookReplayPassReportV1 {
    pub committed: u32,
    pub duplicates: u32,
    pub tombstoned: u32,
    pub retained: u32,
    pub binding_unavailable: bool,
}

/// Typed admission result the drain understands. Callers map their own
/// admission outcomes onto this closed set; nothing here talks to TraceDecay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookReplayAdmissionOutcomeV1 {
    Admitted,
    ExactDuplicate,
    Conflict,
    CatchupRequired,
    Backpressured,
    Unavailable,
}

pub fn hook_v2_spool_root(data_root: &Path, host: HookHostV1) -> PathBuf {
    data_root.join("hook-v2-spool").join(host.hook_key())
}

pub fn published_hook_scope_binding(
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

enum ReplayCompletion {
    Committed(HookSpoolRecordV1),
    ExactDuplicate(HookSpoolRecordV1),
    Tombstone(HookSpoolRecordV1, HookReplayTombstoneReasonV1),
    Retained(u32),
}

/// Drain one admitted host spool once. `admit` reauthorizes and admits a
/// single envelope; production passes the daemon admission path, tests pass a
/// fake. The spool handle is dropped across admission so a live hook can append.
#[hotpath::measure(label = "hooks.replay.host_drain", future = true)]
pub async fn drain_host_spool_once<A, F>(
    mut spool: HookSpoolV1,
    project_id: [u8; 16],
    binding: Option<&HookScopeBindingV1>,
    now: UtcMicros,
    admit: A,
) -> HookReplayPassReportV1
where
    A: Fn(HookEventEnvelopeV2, Option<crate::NativeContextScoutLifecycleV1>) -> F,
    F: Future<Output = HookReplayAdmissionOutcomeV1>,
{
    let host = spool.config().host;
    let mut pass = HookReplayPassReportV1::default();

    // Age-expired records are terminal regardless of binding state: the spool
    // keeps them durable precisely until the drain says otherwise.
    if let Ok(expired) = spool.expired_records(now) {
        for record in expired {
            if acknowledge(
                &mut spool,
                &record,
                HookSpoolAckDispositionV1::TerminalTombstone,
                now,
            ) {
                hotpath::gauge!("hooks.replay.expired").inc(1.0);
                log_tombstone(host, &record, HookReplayTombstoneReasonV1::Expired);
                pass.tombstoned = pass.tombstoned.saturating_add(1);
            }
        }
    }

    let Some(binding) = binding.filter(|binding| binding.project_id == project_id) else {
        // Without a current binding for this project nothing can be
        // reauthorized. Records stay durable and pending; a later pass retries.
        hotpath::gauge!("hooks.replay.binding_unavailable").inc(1.0);
        pass.binding_unavailable = true;
        return pass;
    };

    let Ok(batches) = spool.claim_replay_batches(now, REPLAY_SESSIONS_PER_PASS) else {
        return pass;
    };
    let mut replay_batches = Vec::with_capacity(batches.len());
    for batch in batches {
        let Ok(record_count) = u16::try_from(batch.records.len()) else {
            let _ = spool.release_replay_claim(batch.claim_id);
            return pass;
        };
        if validate_replay_batch(record_count, batch.byte_count).is_err() {
            let _ = spool.release_replay_claim(batch.claim_id);
            hotpath::gauge!("hooks.replay.retained").inc(f64::from(record_count));
            pass.retained = pass.retained.saturating_add(record_count.into());
            continue;
        }
        hotpath::gauge!("hooks.replay.batch_events").set(f64::from(record_count));
        replay_batches.push((batch.records, record_count));
    }

    // The spool lease is process-wide and appenders need it to durably accept
    // live hook events. Admission may await arbitrary daemon work, so retain
    // only bounded record copies across that await and reacquire the writer
    // solely for the final acknowledgement phase.
    let root = spool.root().to_path_buf();
    let config = spool.config();
    drop(spool);
    let mut completions = Vec::new();
    for (records, record_count) in replay_batches {
        for (index, record) in records.into_iter().enumerate() {
            if record.envelope.project_id != project_id
                || record.envelope.validate(binding).is_err()
            {
                completions.push(ReplayCompletion::Tombstone(
                    record,
                    HookReplayTombstoneReasonV1::BindingStale,
                ));
                continue;
            }
            let outcome = hotpath::future!(
                admit(record.envelope.clone(), record.native_lifecycle.clone()),
                label = "hooks.replay.delivery"
            )
            .await;
            match outcome {
                HookReplayAdmissionOutcomeV1::Admitted => {
                    completions.push(ReplayCompletion::Committed(record));
                }
                HookReplayAdmissionOutcomeV1::ExactDuplicate => {
                    completions.push(ReplayCompletion::ExactDuplicate(record));
                }
                HookReplayAdmissionOutcomeV1::Conflict => {
                    completions.push(ReplayCompletion::Tombstone(
                        record,
                        HookReplayTombstoneReasonV1::IdentityConflict,
                    ));
                }
                HookReplayAdmissionOutcomeV1::CatchupRequired => {
                    completions.push(ReplayCompletion::Tombstone(
                        record,
                        HookReplayTombstoneReasonV1::BindingStale,
                    ));
                }
                HookReplayAdmissionOutcomeV1::Backpressured
                | HookReplayAdmissionOutcomeV1::Unavailable => {
                    completions.push(ReplayCompletion::Retained(u32::from(
                        record_count.saturating_sub(index as u16),
                    )));
                    break;
                }
            }
        }
    }

    let retained_without_ack = completions.iter().fold(0_u32, |retained, completion| {
        retained.saturating_add(match completion {
            ReplayCompletion::Retained(count) => *count,
            ReplayCompletion::Committed(_)
            | ReplayCompletion::ExactDuplicate(_)
            | ReplayCompletion::Tombstone(_, _) => 1,
        })
    });
    let Ok((mut spool, _)) = HookSpoolV1::open(root, config, now) else {
        hotpath::gauge!("hooks.replay.retained").inc(f64::from(retained_without_ack));
        pass.retained = pass.retained.saturating_add(retained_without_ack);
        return pass;
    };
    for completion in completions {
        match completion {
            ReplayCompletion::Committed(record) => {
                if acknowledge(
                    &mut spool,
                    &record,
                    HookSpoolAckDispositionV1::Committed,
                    now,
                ) {
                    hotpath::gauge!("hooks.replay.delivered").inc(1.0);
                    pass.committed = pass.committed.saturating_add(1);
                }
            }
            ReplayCompletion::ExactDuplicate(record) => {
                if acknowledge(
                    &mut spool,
                    &record,
                    HookSpoolAckDispositionV1::Committed,
                    now,
                ) {
                    hotpath::gauge!("hooks.replay.duplicate").inc(1.0);
                    pass.duplicates = pass.duplicates.saturating_add(1);
                }
            }
            ReplayCompletion::Tombstone(record, reason) => {
                if acknowledge(
                    &mut spool,
                    &record,
                    HookSpoolAckDispositionV1::TerminalTombstone,
                    now,
                ) {
                    match reason {
                        HookReplayTombstoneReasonV1::Expired => {
                            hotpath::gauge!("hooks.replay.expired").inc(1.0);
                        }
                        HookReplayTombstoneReasonV1::BindingStale
                        | HookReplayTombstoneReasonV1::IdentityConflict => {
                            hotpath::gauge!("hooks.replay.refused").inc(1.0);
                        }
                    }
                    log_tombstone(host, &record, reason);
                    pass.tombstoned = pass.tombstoned.saturating_add(1);
                }
            }
            ReplayCompletion::Retained(count) => {
                hotpath::gauge!("hooks.replay.retained").inc(f64::from(count));
                pass.retained = pass.retained.saturating_add(count);
            }
        }
    }
    pass
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

pub async fn admit_replayed_envelope_with_authoritative_session<R, RF, A, AF, O>(
    envelope: HookEventEnvelopeV2,
    native_lifecycle: Option<crate::NativeContextScoutLifecycleV1>,
    resolve_session: R,
    admit: A,
) -> O
where
    R: FnOnce([u8; 16], [u8; 16], [u8; 32]) -> RF,
    RF: Future<Output = Option<tracedecay_domain::SessionId>>,
    A: FnOnce(HookEventEnvelopeV2, Option<tracedecay_domain::SessionId>) -> AF,
    AF: Future<Output = O>,
{
    let native_session_id = match native_lifecycle
        .as_ref()
        .filter(|lifecycle| lifecycle.matches_envelope(&envelope))
    {
        Some(lifecycle) => Some(lifecycle.session_id.clone()),
        None => {
            resolve_session(
                envelope.project_id,
                envelope.worktree_id,
                envelope.protected_session_id,
            )
            .await
        }
    };
    admit(envelope, native_session_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;
    use tracedecay_domain::SessionId;

    use crate::{
        HOOK_CONFIGURATION_SCHEMA_VERSION, HOOK_EVENT_SCHEMA_VERSION, HookBoundaryV1,
        HookCapabilityV1, HookConfigurationFileWriterV1, HookConfigurationPublisherV1,
        HookConfigurationSnapshotV1, HookEventFamily, HookEventV2, HookOrderingV1,
        HookSpoolConfigV1, stock_event_support,
    };

    const HOST: HookHostV1 = HookHostV1::ClaudeCode;
    const PROJECT_ID: [u8; 16] = [1; 16];

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
            project_id: PROJECT_ID,
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
        // Do not pre-create the spool root: `HookSpoolV1::open` creates it
        // owner-private itself, while a fixture-made directory would carry
        // umask-default permissions and trip the fail-closed validation.
        let root = hook_v2_spool_root(data_root, HOST);
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

    fn open_admitted_spool(data_root: &Path, now: UtcMicros) -> HookSpoolV1 {
        HookSpoolV1::open(
            hook_v2_spool_root(data_root, HOST),
            HookSpoolConfigV1::stock(HOST),
            now,
        )
        .unwrap()
        .0
    }

    async fn drain<A, F>(data_root: &Path, now: UtcMicros, admit: A) -> HookReplayPassReportV1
    where
        A: Fn(HookEventEnvelopeV2, Option<crate::NativeContextScoutLifecycleV1>) -> F,
        F: Future<Output = HookReplayAdmissionOutcomeV1>,
    {
        drain_host_spool_once(
            open_admitted_spool(data_root, now),
            PROJECT_ID,
            published_hook_scope_binding(data_root, HOST, now).as_ref(),
            now,
            admit,
        )
        .await
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

    fn admitted() -> HookReplayAdmissionOutcomeV1 {
        HookReplayAdmissionOutcomeV1::Admitted
    }

    fn protected_session_id(session: &str) -> [u8; 32] {
        Sha256::digest(session.as_bytes()).into()
    }

    #[tokio::test]
    async fn live_failure_spools_then_replay_preserves_lifecycle_for_suggestion() {
        let root = TestRoot::new("lifecycle-suggestion");
        let now = UtcMicros(1_000);
        let mut binding = binding(7);
        binding.host = HookHostV1::OpenCode;
        binding.capabilities = binding
            .capabilities
            .iter()
            .map(|capability| HookCapabilityV1 {
                family: capability.family,
                support: stock_event_support(HookHostV1::OpenCode, capability.family),
            })
            .collect();
        let mut tool_after = envelope(9, &binding);
        tool_after.producer = HookHostV1::OpenCode;
        tool_after.protected_session_id = protected_session_id("session.native.replay");
        tool_after.event = HookEventV2::ToolLifecycle {
            tool_id: [8; 16],
            phase: crate::HookLifecyclePhaseV1::Completed,
            effect_receipt_id: None,
        };
        let lifecycle = crate::NativeContextScoutLifecycleV1::new(
            "session.native.replay",
            "call.native.replay",
            tool_after.event_id,
        )
        .unwrap();
        publish_binding(root.path(), &binding, now);
        let spool_root = hook_v2_spool_root(root.path(), HookHostV1::OpenCode);
        let (mut spool, _) = HookSpoolV1::open(
            &spool_root,
            HookSpoolConfigV1::stock(HookHostV1::OpenCode),
            now,
        )
        .unwrap();
        spool
            .append_with_native_lifecycle(tool_after, Some(lifecycle), &binding, now)
            .unwrap();
        drop(spool);
        let suggestions = Arc::new(StdMutex::new(Vec::new()));
        let captured = Arc::clone(&suggestions);

        let report = drain_host_spool_once(
            HookSpoolV1::open(
                &spool_root,
                HookSpoolConfigV1::stock(HookHostV1::OpenCode),
                now,
            )
            .unwrap()
            .0,
            PROJECT_ID,
            Some(&binding),
            now,
            move |envelope, native_lifecycle| {
                let captured = Arc::clone(&captured);
                async move {
                    assert_eq!(
                        native_lifecycle
                            .as_ref()
                            .map(|lifecycle| lifecycle.call_id.as_str()),
                        Some("call.native.replay")
                    );
                    admit_replayed_envelope_with_authoritative_session(
                        envelope,
                        native_lifecycle,
                        |_, _, _| async move { panic!("retained lifecycle must be authoritative") },
                        |_, native_session_id| async move {
                            if native_session_id.as_ref().map(SessionId::as_str)
                                == Some("session.native.replay")
                            {
                                captured
                                    .lock()
                                    .unwrap()
                                    .push("replayed lifecycle suggestion");
                            }
                            admitted()
                        },
                    )
                    .await
                }
            },
        )
        .await;

        assert_eq!(report.committed, 1);
        assert_eq!(
            suggestions.lock().unwrap().as_slice(),
            ["replayed lifecycle suggestion"]
        );
        let (spool, report) = HookSpoolV1::open(
            spool_root,
            HookSpoolConfigV1::stock(HookHostV1::OpenCode),
            now,
        )
        .unwrap();
        assert_eq!(report.pending_records, 0);
        drop(spool);
    }

    #[tokio::test]
    async fn async_admission_does_not_hold_the_spool_writer_lease() {
        let data_root = TestRoot::new("lease");
        let current = UtcMicros(10);
        let binding = binding(7);
        publish_binding(data_root.path(), &binding, current);
        // `HookSpoolV1::open` creates the spool root owner-private itself;
        // pre-creating it here would leave umask-default permissions that the
        // fail-closed private-directory validation rejects.
        let spool_root = hook_v2_spool_root(data_root.path(), HOST);
        {
            let (mut spool, _) =
                HookSpoolV1::open(&spool_root, HookSpoolConfigV1::stock(HOST), current).unwrap();
            spool
                .append(envelope(9, &binding), &binding, current)
                .unwrap();
        }

        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let entered_tx = Arc::new(StdMutex::new(Some(entered_tx)));
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        let drain = drain(data_root.path(), current, move |_, _| {
            let entered_tx = Arc::clone(&entered_tx);
            let release_rx = Arc::clone(&release_rx);
            async move {
                let sender = entered_tx.lock().unwrap().take();
                let receiver = release_rx.lock().unwrap().take();
                if let Some(sender) = sender {
                    let _ = sender.send(());
                }
                if let Some(receiver) = receiver {
                    let _ = receiver.await;
                }
                admitted()
            }
        });
        tokio::pin!(drain);
        tokio::select! {
            _ = &mut drain => panic!("drain returned before admission was released"),
            result = entered_rx => result.expect("admission entered"),
        }

        let concurrent = HookSpoolV1::open(&spool_root, HookSpoolConfigV1::stock(HOST), current);
        assert!(
            concurrent.is_ok(),
            "a live hook must be able to append while replay awaits daemon admission"
        );
        drop(concurrent);

        let _ = release_tx.send(());
        let report = drain.await;
        assert_eq!(report.committed, 1);
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

        let report = drain(root.path(), now, move |_, _| {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::Relaxed);
                admitted()
            }
        })
        .await;

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

        let report = drain(root.path(), now, |envelope, _| async move {
            if envelope.event_id == [9; 16] {
                HookReplayAdmissionOutcomeV1::ExactDuplicate
            } else {
                HookReplayAdmissionOutcomeV1::Conflict
            }
        })
        .await;

        assert_eq!(report.duplicates, 1);
        assert_eq!(report.tombstoned, 1);
        assert_eq!(pending_records(root.path(), now), 0);
    }

    #[tokio::test]
    async fn an_unavailable_daemon_retains_every_record_for_a_later_pass() {
        let root = TestRoot::new("retain");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, now);
        spool_envelopes(
            root.path(),
            &binding,
            &[envelope(9, &binding), envelope(10, &binding)],
            now,
        );

        let report = drain(root.path(), now, |_, _| async move {
            HookReplayAdmissionOutcomeV1::Unavailable
        })
        .await;

        assert_eq!(report.retained, 2);
        assert_eq!(report.committed, 0);
        assert_eq!(pending_records(root.path(), now), 2);

        // A later pass with a healthy daemon drains it.
        let report = drain(root.path(), now, |_, _| async move { admitted() }).await;
        assert_eq!(report.committed, 2);
        assert_eq!(pending_records(root.path(), now), 0);
    }

    #[tokio::test]
    async fn a_missing_binding_leaves_every_record_pending() {
        let root = TestRoot::new("unbound");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        spool_envelopes(root.path(), &binding, &[envelope(9, &binding)], now);

        let report = drain(root.path(), now, |_, _| async move { admitted() }).await;

        assert!(report.binding_unavailable);
        assert_eq!(pending_records(root.path(), now), 1);
    }

    #[tokio::test]
    async fn a_foreign_project_id_leaves_every_record_pending() {
        let root = TestRoot::new("foreign-project");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, now);
        spool_envelopes(root.path(), &binding, &[envelope(9, &binding)], now);

        let report = drain_host_spool_once(
            open_admitted_spool(root.path(), now),
            [9; 16],
            published_hook_scope_binding(root.path(), HOST, now).as_ref(),
            now,
            |_, _| async move { admitted() },
        )
        .await;

        assert!(report.binding_unavailable);
        assert_eq!(report.committed, 0);
        assert_eq!(pending_records(root.path(), now), 1);
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
            replayed.protected_session_id = protected_session_id(session);
            replayed.ordering = HookOrderingV1::ProviderSequence(sequence);
            replayed.event = HookEventV2::SavedEdit {
                file_id: [sequence as u8; 16],
                changed_range_count: 1,
            };
            let captured = Arc::clone(&seen);

            let outcome = admit_replayed_envelope_with_authoritative_session(
                replayed,
                None,
                move |project_id, worktree_id, protected_session_id| async move {
                    assert_eq!(project_id, PROJECT_ID);
                    assert_eq!(worktree_id, [3; 16]);
                    assert_eq!(protected_session_id, self::protected_session_id(session));
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
            assert_eq!(outcome, HookReplayAdmissionOutcomeV1::Admitted);
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
        let later = UtcMicros(queued_at.0 + crate::MAX_SPOOL_AGE_MICROS + 1);
        publish_binding(root.path(), &binding, later);

        let report = drain(root.path(), later, |_, _| async move { admitted() }).await;

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
            native_lifecycle: None,
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

    /// Drive an actual replay rather than the receipt function alone: a pass
    /// that never acknowledges (the crash-before-ACK shape) must re-offer the
    /// identical record identities on the next pass, and an acknowledged
    /// record must never be offered a third time.
    #[tokio::test]
    async fn retained_records_replay_under_the_same_identity_without_double_delivery() {
        let root = TestRoot::new("replay-identity");
        let now = UtcMicros(1_000);
        let binding = binding(7);
        publish_binding(root.path(), &binding, now);
        spool_envelopes(
            root.path(),
            &binding,
            &[envelope(9, &binding), envelope(10, &binding)],
            now,
        );

        let offered = |seen: &Arc<StdMutex<Vec<[u8; 16]>>>| seen.lock().unwrap().clone();

        // Pass 1: the daemon is unavailable, so nothing is acknowledged.
        let first_seen: Arc<StdMutex<Vec<[u8; 16]>>> = Arc::new(StdMutex::new(Vec::new()));
        let recorder = Arc::clone(&first_seen);
        let report = drain(root.path(), now, move |envelope, _| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder.lock().unwrap().push(envelope.event_id);
                HookReplayAdmissionOutcomeV1::Unavailable
            }
        })
        .await;
        assert_eq!(
            report.retained, 2,
            "an unavailable daemon acknowledges nothing"
        );
        assert_eq!(report.committed, 0);
        assert_eq!(pending_records(root.path(), now), 2);

        // Pass 2: the replay re-offers the same identities and commits them.
        let second_seen: Arc<StdMutex<Vec<[u8; 16]>>> = Arc::new(StdMutex::new(Vec::new()));
        let recorder = Arc::clone(&second_seen);
        let report = drain(root.path(), now, move |envelope, _| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder.lock().unwrap().push(envelope.event_id);
                admitted()
            }
        })
        .await;
        assert_eq!(report.committed, 2);
        assert_eq!(pending_records(root.path(), now), 0);

        assert_eq!(
            offered(&first_seen),
            offered(&second_seen),
            "replay must re-offer the same record identities in the same order"
        );
        assert_eq!(offered(&second_seen).len(), 2);

        // Pass 3: an acknowledged record is never redelivered.
        let third_seen: Arc<StdMutex<Vec<[u8; 16]>>> = Arc::new(StdMutex::new(Vec::new()));
        let recorder = Arc::clone(&third_seen);
        let report = drain(root.path(), now, move |envelope, _| {
            let recorder = Arc::clone(&recorder);
            async move {
                recorder.lock().unwrap().push(envelope.event_id);
                admitted()
            }
        })
        .await;
        assert_eq!(
            report.committed, 0,
            "an acknowledged record must never replay a second time"
        );
        assert!(
            offered(&third_seen).is_empty(),
            "a settled spool offers nothing to admission"
        );
    }
}

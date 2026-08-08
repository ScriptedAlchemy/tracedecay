use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use tracedecay_domain::{
    DeliveryChannelIdentityV1, DeliveryDropReasonV1, DeliveryEventClassV1,
    DeliverySettlementAttemptV1, DeliverySettlementOutcomeV1, DeliverySettlementV1,
    DeliverySurfaceFamilyV1, UtcMicros, canonical_sha256,
};
use tracedecay_usecases::observability::{
    DeliverySettlementAuthorityV1, DeliverySourceReceiptReadV1, MAX_PENDING_RECEIPTED_DELIVERIES_V1,
};

use super::DashboardState;
use super::events_api::DashboardEventV1;
use super::read_model::{DashboardScopeV1, now_micros};

const MAX_PENDING_DELIVERIES: usize = 256;
const MAX_TERMINAL_RECEIPTS: usize = 512;
const DELIVERY_ACK_DEADLINE_MICROS: i64 = 30_000_000;
const DELIVERY_DEADLINE_REAPER_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalReceiptV1 {
    Delivered,
    Dropped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingDeliveryV1 {
    attempt: DeliverySettlementAttemptV1,
    connection_ref: String,
    drop_reason: Option<DeliveryDropReasonV1>,
}

#[derive(Default)]
struct PendingDeliveryStateV1 {
    pending: BTreeMap<String, PendingDeliveryV1>,
    terminal: BTreeMap<String, TerminalReceiptV1>,
    terminal_order: VecDeque<String>,
}

impl PendingDeliveryStateV1 {
    fn mark_terminal(&mut self, receipt: String, terminal: TerminalReceiptV1) {
        self.pending.remove(&receipt);
        if !self.terminal.contains_key(&receipt) {
            self.terminal_order.push_back(receipt.clone());
        }
        self.terminal.insert(receipt, terminal);
        while self.terminal_order.len() > MAX_TERMINAL_RECEIPTS {
            if let Some(oldest) = self.terminal_order.pop_front() {
                self.terminal.remove(&oldest);
            }
        }
    }
}

pub(crate) struct DashboardDeliverySettlementRegistryV1 {
    authority: Option<Arc<DeliverySettlementAuthorityV1>>,
    state: Mutex<PendingDeliveryStateV1>,
}

impl DashboardDeliverySettlementRegistryV1 {
    pub(crate) fn new(authority: Option<Arc<DeliverySettlementAuthorityV1>>) -> Self {
        Self {
            authority,
            state: Mutex::new(PendingDeliveryStateV1::default()),
        }
    }

    /// Starts the dashboard-owned bounded deadline lane. The task retains only
    /// a weak reference, so it ends with the mounted dashboard state instead
    /// of surviving restarts as a detached duplicate scheduler.
    pub(crate) fn mount_deadline_reaper(registry: &Arc<Self>) {
        if registry.authority.is_none() {
            return;
        }
        let registry = Arc::downgrade(registry);
        tokio::spawn(async move {
            loop {
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                registry.reap_expired_durable().await;
                drop(registry);
                tokio::time::sleep(DELIVERY_DEADLINE_REAPER_INTERVAL).await;
            }
        });
    }

    /// Durably admits an opaque receipt before putting it on the SSE channel.
    /// Returning false means the bounded lane rejected the event, so callers
    /// must not send a token the post-restart ACK route cannot resolve.
    pub(crate) async fn attach_receipt(
        &self,
        event: &mut DashboardEventV1,
        connection_ref: &str,
    ) -> bool {
        let Some(authority) = self.authority.as_ref() else {
            return true;
        };
        self.expire().await;
        let Some((receipt, pending)) = pending_delivery(event, connection_ref) else {
            return false;
        };
        let overflow = {
            let Ok(mut state) = self.state.lock() else {
                return false;
            };
            if state
                .pending
                .get(&receipt)
                .is_some_and(|pending| pending.drop_reason.is_none())
            {
                event.delivery_receipt = Some(receipt);
                return true;
            }
            if state.pending.contains_key(&receipt) {
                return false;
            }
            if let Some(terminal) = state.terminal.get(&receipt) {
                if *terminal == TerminalReceiptV1::Delivered {
                    event.delivery_receipt = Some(receipt);
                    return true;
                }
                return false;
            }
            if state.pending.len() >= MAX_PENDING_DELIVERIES {
                true
            } else {
                state.pending.insert(receipt.clone(), pending.clone());
                false
            }
        };
        if overflow {
            return self
                .settle_overflow(authority.as_ref(), &receipt, pending)
                .await;
        }
        match authority.begin_receipted(&pending.attempt, &receipt).await {
            Ok(tracedecay_global_db::DeliveryAttemptClaimV1::Claimed)
            | Ok(tracedecay_global_db::DeliveryAttemptClaimV1::ReplayedAttempt) => {
                event.delivery_receipt = Some(receipt);
                true
            }
            Ok(tracedecay_global_db::DeliveryAttemptClaimV1::AlreadySettled(settlement)) => {
                let terminal = match settlement.outcome {
                    DeliverySettlementOutcomeV1::Delivered
                    | DeliverySettlementOutcomeV1::Deduplicated => TerminalReceiptV1::Delivered,
                    DeliverySettlementOutcomeV1::Dropped => TerminalReceiptV1::Dropped,
                };
                if let Ok(mut state) = self.state.lock() {
                    state.mark_terminal(receipt.clone(), terminal);
                }
                if terminal == TerminalReceiptV1::Delivered {
                    event.delivery_receipt = Some(receipt);
                    true
                } else {
                    false
                }
            }
            Err(_) => {
                if let Ok(mut state) = self.state.lock()
                    && state.pending.get(&receipt) == Some(&pending)
                {
                    state.pending.remove(&receipt);
                }
                false
            }
        }
    }

    async fn acknowledge(&self, receipt: &str) -> DeliveryAckOutcomeV1 {
        let Some(authority) = self.authority.as_ref() else {
            return DeliveryAckOutcomeV1::Unavailable;
        };
        self.expire().await;
        match self.local_ack_outcome(receipt) {
            Ok(Some(outcome)) => return outcome,
            Ok(None) => {}
            Err(()) => return DeliveryAckOutcomeV1::Unavailable,
        }
        let attempt = match authority.attempt_for_receipt(receipt).await {
            Ok(Some(DeliverySourceReceiptReadV1::Pending(attempt))) => attempt,
            Ok(Some(DeliverySourceReceiptReadV1::Settled(settlement))) => {
                let terminal = match settlement.outcome {
                    DeliverySettlementOutcomeV1::Delivered
                    | DeliverySettlementOutcomeV1::Deduplicated => TerminalReceiptV1::Delivered,
                    DeliverySettlementOutcomeV1::Dropped => TerminalReceiptV1::Dropped,
                };
                if let Ok(mut state) = self.state.lock() {
                    state.mark_terminal(receipt.to_owned(), terminal);
                }
                return match terminal {
                    TerminalReceiptV1::Delivered => DeliveryAckOutcomeV1::Replayed,
                    TerminalReceiptV1::Dropped => DeliveryAckOutcomeV1::Gone,
                };
            }
            Ok(None) => return DeliveryAckOutcomeV1::Unknown,
            Err(_) => return DeliveryAckOutcomeV1::Unavailable,
        };
        let Some(observed_at) = observed_at() else {
            return DeliveryAckOutcomeV1::Unavailable;
        };
        if observed_at.0.saturating_sub(attempt.attempted_at.0) >= DELIVERY_ACK_DEADLINE_MICROS {
            let dropped = settlement(
                attempt,
                DeliverySettlementOutcomeV1::Dropped,
                Some(DeliveryDropReasonV1::Deadline),
                observed_at,
            );
            return match self.settle(receipt, dropped).await {
                Ok(_) => DeliveryAckOutcomeV1::Gone,
                Err(()) => DeliveryAckOutcomeV1::Unavailable,
            };
        }
        let delivered = settlement(
            attempt,
            DeliverySettlementOutcomeV1::Delivered,
            None,
            observed_at,
        );
        match self.settle(receipt, delivered).await {
            Ok(true) => DeliveryAckOutcomeV1::Replayed,
            Ok(false) => DeliveryAckOutcomeV1::Accepted,
            Err(()) => DeliveryAckOutcomeV1::Unavailable,
        }
    }

    /// Resolves only process-local terminal state. Durable lookup happens in
    /// the async caller after this guard has been dropped, preserving Axum's
    /// `Send` handler contract.
    fn local_ack_outcome(&self, receipt: &str) -> Result<Option<DeliveryAckOutcomeV1>, ()> {
        let state = self.state.lock().map_err(|_| ())?;
        match state.terminal.get(receipt) {
            Some(TerminalReceiptV1::Delivered) => Ok(Some(DeliveryAckOutcomeV1::Replayed)),
            Some(TerminalReceiptV1::Dropped) => Ok(Some(DeliveryAckOutcomeV1::Gone)),
            None if state
                .pending
                .get(receipt)
                .is_some_and(|pending| pending.drop_reason.is_some()) =>
            {
                Ok(Some(DeliveryAckOutcomeV1::Gone))
            }
            None => Ok(None),
        }
    }

    async fn settle_overflow(
        &self,
        authority: &DeliverySettlementAuthorityV1,
        receipt: &str,
        pending: PendingDeliveryV1,
    ) -> bool {
        if authority
            .begin_receipted(&pending.attempt, receipt)
            .await
            .is_err()
        {
            return false;
        }
        let Some(observed_at) = observed_at() else {
            return false;
        };
        let _ = authority
            .settle(&settlement(
                pending.attempt,
                DeliverySettlementOutcomeV1::Dropped,
                Some(DeliveryDropReasonV1::Backpressure),
                observed_at,
            ))
            .await;
        false
    }

    async fn settle(&self, receipt: &str, settlement: DeliverySettlementV1) -> Result<bool, ()> {
        let Some(authority) = self.authority.as_ref() else {
            return Err(());
        };
        let replayed = authority
            .settle(&settlement)
            .await
            .map_err(|_| ())?
            .receipt
            .replayed;
        let terminal = match settlement.outcome {
            DeliverySettlementOutcomeV1::Delivered | DeliverySettlementOutcomeV1::Deduplicated => {
                TerminalReceiptV1::Delivered
            }
            DeliverySettlementOutcomeV1::Dropped => TerminalReceiptV1::Dropped,
        };
        let Ok(mut state) = self.state.lock() else {
            return Err(());
        };
        state.mark_terminal(receipt.to_owned(), terminal);
        Ok(replayed)
    }

    pub(crate) async fn expire(&self) {
        let Some(observed_at) = observed_at() else {
            return;
        };
        let expired: Vec<_> = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state
                .pending
                .iter_mut()
                .filter_map(|(receipt, pending)| {
                    let reason = pending.drop_reason.or_else(|| {
                        (observed_at.0.saturating_sub(pending.attempt.attempted_at.0)
                            >= DELIVERY_ACK_DEADLINE_MICROS)
                            .then_some(DeliveryDropReasonV1::Deadline)
                    })?;
                    pending.drop_reason = Some(reason);
                    Some((receipt.clone(), pending.attempt.clone(), reason))
                })
                .collect()
        };
        for (receipt, attempt, reason) in expired {
            let _ = self
                .settle(
                    &receipt,
                    settlement(
                        attempt,
                        DeliverySettlementOutcomeV1::Dropped,
                        Some(reason),
                        observed_at,
                    ),
                )
                .await;
        }
    }

    /// Advances one bounded durable page of dashboard receipts whose browser
    /// acknowledgement deadline has passed. It is deliberately independent of
    /// SSE writes and ACK requests so process restart cannot strand a pending
    /// exact receipt forever.
    pub(crate) async fn reap_expired_durable(&self) {
        let Some(authority) = self.authority.as_ref() else {
            return;
        };
        let Some(observed_at) = observed_at() else {
            return;
        };
        let attempted_at_through =
            UtcMicros(observed_at.0.saturating_sub(DELIVERY_ACK_DEADLINE_MICROS));
        if attempted_at_through.0 <= 0 {
            return;
        }
        let due = match authority
            .pending_receipted_attempts_due(
                DeliverySurfaceFamilyV1::Dashboard,
                attempted_at_through,
                MAX_PENDING_RECEIPTED_DELIVERIES_V1,
            )
            .await
        {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "dashboard durable delivery deadline scan failed"
                );
                return;
            }
        };
        for pending in due {
            if self
                .settle(
                    &pending.receipt_ref,
                    settlement(
                        pending.attempt,
                        DeliverySettlementOutcomeV1::Dropped,
                        Some(DeliveryDropReasonV1::Deadline),
                        observed_at,
                    ),
                )
                .await
                .is_err()
            {
                tracing::warn!("dashboard durable delivery deadline settlement failed");
            }
        }
    }

    pub(crate) async fn disconnect(&self, connection_ref: &str) {
        self.drop_where(connection_ref, DeliveryDropReasonV1::Disconnected)
            .await;
    }

    pub(crate) async fn drop_receipt(&self, receipt: &str, reason: DeliveryDropReasonV1) {
        let Some(observed_at) = observed_at() else {
            return;
        };
        let attempt = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(pending) = state.pending.get_mut(receipt) else {
                return;
            };
            pending.drop_reason = Some(reason);
            pending.attempt.clone()
        };
        let _ = self
            .settle(
                receipt,
                settlement(
                    attempt,
                    DeliverySettlementOutcomeV1::Dropped,
                    Some(reason),
                    observed_at,
                ),
            )
            .await;
    }

    async fn drop_where(&self, connection_ref: &str, reason: DeliveryDropReasonV1) {
        let Some(observed_at) = observed_at() else {
            return;
        };
        let dropped: Vec<_> = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            state
                .pending
                .iter_mut()
                .filter_map(|(receipt, pending)| {
                    if pending.connection_ref != connection_ref {
                        return None;
                    }
                    pending.drop_reason = Some(reason);
                    Some((receipt.clone(), pending.attempt.clone()))
                })
                .collect()
        };
        for (receipt, attempt) in dropped {
            let _ = self
                .settle(
                    &receipt,
                    settlement(
                        attempt,
                        DeliverySettlementOutcomeV1::Dropped,
                        Some(reason),
                        observed_at,
                    ),
                )
                .await;
        }
    }
}

fn pending_delivery(
    event: &DashboardEventV1,
    connection_ref: &str,
) -> Option<(String, PendingDeliveryV1)> {
    let valid_at = UtcMicros(event.observation_time_micros);
    if valid_at.0 <= 0 {
        return None;
    }
    let attempted_at = observed_at().map(|now| now.max(valid_at))?;
    // One SSE delivery owner is the product event addressed to one concrete
    // browser connection. This keeps the denominator exact (`eligible = 1`)
    // even when several dashboards observe the same underlying activity.
    let mut owner_event = event.clone();
    owner_event.delivery_receipt = None;
    let owner = canonical_sha256(&(
        "tracedecay.dashboard-sse-recipient-event.v1",
        &owner_event,
        connection_ref,
    ))
    .ok()?;
    let attempt = DeliverySettlementAttemptV1 {
        owner_event_id: format!(
            "dashboard:sse:{}",
            owner.as_str().trim_start_matches("sha256:")
        ),
        event_class: DeliveryEventClassV1::Activity,
        channel: DeliveryChannelIdentityV1 {
            surface: DeliverySurfaceFamilyV1::Dashboard,
            channel_ref: connection_ref.to_owned(),
        },
        work_attempt: None,
        eligible: 1,
        valid_at,
        attempted_at,
    };
    attempt.validate().ok()?;
    // The public token is stable across an EventSource replay on this exact
    // connection. Attempt timing is recorded once in the pending entry and is
    // deliberately not part of the lookup key.
    let receipt = canonical_sha256(&(
        "tracedecay.dashboard-sse-receipt.v1",
        &attempt.owner_event_id,
        &attempt.channel,
    ))
    .ok()?;
    Some((
        format!("dsa1:{}", receipt.as_str().trim_start_matches("sha256:")),
        PendingDeliveryV1 {
            attempt,
            connection_ref: connection_ref.to_owned(),
            drop_reason: None,
        },
    ))
}

pub(crate) fn connection_ref(run_id: &str, scope: &DashboardScopeV1) -> Option<String> {
    let digest =
        canonical_sha256(&("tracedecay.dashboard-sse-connection.v1", run_id, scope)).ok()?;
    Some(format!(
        "dashboard:sse:{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
}

fn settlement(
    attempt: DeliverySettlementAttemptV1,
    outcome: DeliverySettlementOutcomeV1,
    drop_reason: Option<DeliveryDropReasonV1>,
    observed_at: UtcMicros,
) -> DeliverySettlementV1 {
    DeliverySettlementV1 {
        settled_at: observed_at.max(attempt.attempted_at),
        attempt,
        outcome,
        drop_reason,
    }
}

fn observed_at() -> Option<UtcMicros> {
    let value = now_micros();
    (value > 0).then_some(UtcMicros(value))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DashboardDeliveryAckRequestV1 {
    receipt: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryAckOutcomeV1 {
    Accepted,
    Replayed,
    Gone,
    Unknown,
    Unavailable,
}

pub(crate) async fn acknowledge(
    State(state): State<DashboardState>,
    Json(request): Json<DashboardDeliveryAckRequestV1>,
) -> StatusCode {
    if !valid_receipt(&request.receipt) {
        return StatusCode::BAD_REQUEST;
    }
    match state
        .delivery_settlements
        .acknowledge(request.receipt.as_str())
        .await
    {
        DeliveryAckOutcomeV1::Accepted => StatusCode::ACCEPTED,
        DeliveryAckOutcomeV1::Replayed => StatusCode::NO_CONTENT,
        DeliveryAckOutcomeV1::Gone => StatusCode::GONE,
        DeliveryAckOutcomeV1::Unknown => StatusCode::NOT_FOUND,
        DeliveryAckOutcomeV1::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    }
}

fn valid_receipt(receipt: &str) -> bool {
    receipt.len() == 69
        && receipt.starts_with("dsa1:")
        && receipt[5..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events_api::DashboardEventKindV1;
    use crate::read_model::DashboardCoverageV1;
    use tracedecay_domain::ProjectId;
    use tracedecay_usecases::observability::{
        BoundedObservabilityProducerV1, DeliverySettlementAuthorityV1,
        ObservabilityProducerIdentityV1,
    };

    fn event() -> DashboardEventV1 {
        DashboardEventV1 {
            stream: "dashboard_activity".to_owned(),
            run_id: "registered-observability-v1".to_owned(),
            event_revision: 7,
            producer_sequence: Some(7),
            retained_from_sequence: Some(1),
            dropped_events: 0,
            entity_revision: Some(7),
            scope: DashboardScopeV1 {
                project_id: Some("project.alpha".to_owned()),
                storage_mode: "profile_sharded".to_owned(),
                store_root: "/stores/profile".to_owned(),
            },
            observation_time_micros: 1,
            source_watermark: None,
            coverage: DashboardCoverageV1::unknown(),
            delivery_receipt: None,
            kind: DashboardEventKindV1::Heartbeat,
        }
    }

    #[test]
    fn receipt_identity_is_exact_per_event_and_connection() {
        let first = pending_delivery(&event(), "dashboard:sse:first").expect("first receipt");
        let replay = pending_delivery(&event(), "dashboard:sse:first").expect("replay receipt");
        let other = pending_delivery(&event(), "dashboard:sse:other").expect("other receipt");
        let mut already_attached = event();
        already_attached.delivery_receipt = Some(first.0.clone());
        let attached_replay = pending_delivery(&already_attached, "dashboard:sse:first")
            .expect("attached replay receipt");

        assert_eq!(first.0, replay.0);
        assert_eq!(first.0, attached_replay.0);
        assert_eq!(
            first.1.attempt.owner_event_id,
            replay.1.attempt.owner_event_id
        );
        assert_ne!(first.0, other.0);
        assert_ne!(
            first.1.attempt.owner_event_id,
            other.1.attempt.owner_event_id
        );
        assert_eq!(first.1.attempt.eligible, 1);
    }

    #[test]
    fn terminal_receipts_replay_exactly_and_remain_bounded() {
        let mut state = PendingDeliveryStateV1::default();
        for index in 0..(MAX_TERMINAL_RECEIPTS + 3) {
            state.mark_terminal(format!("receipt-{index}"), TerminalReceiptV1::Delivered);
        }
        assert_eq!(state.terminal.len(), MAX_TERMINAL_RECEIPTS);
        assert!(!state.terminal.contains_key("receipt-0"));
        assert_eq!(
            state
                .terminal
                .get(&format!("receipt-{}", MAX_TERMINAL_RECEIPTS + 2)),
            Some(&TerminalReceiptV1::Delivered)
        );
    }

    #[test]
    fn receipt_input_is_fixed_width_canonical_hex() {
        assert!(valid_receipt(&format!("dsa1:{}", "a".repeat(64))));
        assert!(!valid_receipt("dsa1:receipt"));
        assert!(!valid_receipt(&format!("dsa1:{}", "z".repeat(64))));
    }

    #[test]
    fn acknowledgement_future_is_send_for_the_http_router() {
        fn require_send<T: Send>(_: T) {}

        let registry = DashboardDeliverySettlementRegistryV1::new(None);
        require_send(registry.acknowledge(&format!("dsa1:{}", "a".repeat(64))));
    }

    #[tokio::test]
    async fn receipt_survives_restart_between_sse_frame_and_browser_ack() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project directory");
        let project_id = ProjectId::new("project.dashboard.delivery").expect("project id");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered project runtime");
        let database = runtime.project_database_arc().expect("project database");
        let identity = ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: "boot:dashboard-delivery".to_owned(),
            producer_revision: "dashboard-delivery-producer.v1".to_owned(),
            configuration_revision: "dashboard-delivery-config.v1".to_owned(),
            policy_revision: "dashboard-delivery-policy.v1".to_owned(),
        };
        let producer = Arc::new(
            BoundedObservabilityProducerV1::start(Arc::clone(&database), identity.clone(), 8)
                .expect("observability producer"),
        );
        let authority = Arc::new(
            DeliverySettlementAuthorityV1::new(
                Arc::clone(&database),
                Arc::clone(&producer),
                identity,
            )
            .expect("delivery authority"),
        );
        let mut frame = event();
        let first = DashboardDeliverySettlementRegistryV1::new(Some(Arc::clone(&authority)));
        assert!(
            first
                .attach_receipt(&mut frame, "dashboard:sse:connection-a")
                .await,
            "the frame must not leave before its exact receipt is durable"
        );
        let receipt = frame.delivery_receipt.take().expect("frame receipt");
        drop(first);

        let restarted = DashboardDeliverySettlementRegistryV1::new(Some(Arc::clone(&authority)));
        assert_eq!(
            restarted.acknowledge(&receipt).await,
            DeliveryAckOutcomeV1::Accepted,
            "a browser ACK after dashboard restart resolves the exact pending receipt"
        );
        drop(restarted);

        let replayed = DashboardDeliverySettlementRegistryV1::new(Some(Arc::clone(&authority)));
        assert_eq!(
            replayed.acknowledge(&receipt).await,
            DeliveryAckOutcomeV1::Replayed,
            "an ACK replay after another restart keeps the immutable terminal outcome"
        );
        drop(replayed);
        drop(authority);
        let producer = match Arc::try_unwrap(producer) {
            Ok(producer) => producer,
            Err(_) => panic!("registry releases producer"),
        };
        producer.shutdown().await.expect("flush producer");
    }

    #[tokio::test]
    async fn restarted_deadline_reaper_drops_durable_receipt_without_browser_ack() {
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project directory");
        let project_id = ProjectId::new("project.dashboard.deadline").expect("project id");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered project runtime");
        let database = runtime.project_database_arc().expect("project database");
        let identity = ObservabilityProducerIdentityV1 {
            authorized_scope_ref: project_id.as_str().to_owned(),
            process_boot_id: "boot:dashboard-deadline".to_owned(),
            producer_revision: "dashboard-deadline-producer.v1".to_owned(),
            configuration_revision: "dashboard-deadline-config.v1".to_owned(),
            policy_revision: "dashboard-deadline-policy.v1".to_owned(),
        };
        let producer = Arc::new(
            BoundedObservabilityProducerV1::start(Arc::clone(&database), identity.clone(), 8)
                .expect("observability producer"),
        );
        let authority = Arc::new(
            DeliverySettlementAuthorityV1::new(
                Arc::clone(&database),
                Arc::clone(&producer),
                identity,
            )
            .expect("delivery authority"),
        );
        let (receipt, mut pending) =
            pending_delivery(&event(), "dashboard:sse:connection-without-ack")
                .expect("durable frame receipt");
        let now = observed_at().expect("wall-clock observation");
        pending.attempt.attempted_at = UtcMicros(
            now.0
                .saturating_sub(DELIVERY_ACK_DEADLINE_MICROS.saturating_add(1)),
        );
        authority
            .begin_receipted(&pending.attempt, &receipt)
            .await
            .expect("first dashboard process durably admits the frame");

        let restarted = Arc::new(DashboardDeliverySettlementRegistryV1::new(Some(
            Arc::clone(&authority),
        )));
        DashboardDeliverySettlementRegistryV1::mount_deadline_reaper(&restarted);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    authority
                        .attempt_for_receipt(&receipt)
                        .await
                        .expect("durable receipt lookup"),
                    Some(DeliverySourceReceiptReadV1::Settled(DeliverySettlementV1 {
                        outcome: DeliverySettlementOutcomeV1::Dropped,
                        drop_reason: Some(DeliveryDropReasonV1::Deadline),
                        ..
                    }))
                ) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mounted deadline reaper settles the overdue receipt without an ACK");
        let weak_restarted = Arc::downgrade(&restarted);
        drop(restarted);
        tokio::time::timeout(Duration::from_secs(1), async {
            while weak_restarted.upgrade().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("weak deadline reaper releases its dashboard state");
        drop(authority);
        let producer = match Arc::try_unwrap(producer) {
            Ok(producer) => producer,
            Err(_) => panic!("registry releases producer"),
        };
        producer.shutdown().await.expect("flush producer");
    }
}

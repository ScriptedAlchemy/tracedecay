//! Durable delivery identity and terminalization for outbound LSP frames.

use super::*;

pub fn lsp_delivery_attempt(
    frame: &[u8],
    session_id: &tracedecay_daemon_protocol::LspSessionId,
    outbound_sequence: u64,
    observed_at: UtcMicros,
) -> Option<tracedecay_domain::DeliverySettlementAttemptV1> {
    let event = tracedecay_domain::canonical_sha256(&(
        "tracedecay.lsp-outbound-delivery.v2",
        session_id.as_str(),
        outbound_sequence,
        frame,
    ))
    .ok()?;
    let channel = tracedecay_domain::canonical_sha256(&(
        "tracedecay.lsp-delivery-channel.v1",
        session_id.as_str(),
    ))
    .ok()?;
    let event_class = serde_json::from_slice::<serde_json::Value>(frame)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .filter(|method| method == "textDocument/publishDiagnostics")
        .map_or(tracedecay_domain::DeliveryEventClassV1::Activity, |_| {
            tracedecay_domain::DeliveryEventClassV1::Diagnostic
        });
    Some(tracedecay_domain::DeliverySettlementAttemptV1 {
        owner_event_id: format!(
            "lsp:outbound:{}",
            event.as_str().trim_start_matches("sha256:")
        ),
        event_class,
        channel: tracedecay_domain::DeliveryChannelIdentityV1 {
            surface: tracedecay_domain::DeliverySurfaceFamilyV1::Lsp,
            channel_ref: format!(
                "lsp:session:{}",
                channel.as_str().trim_start_matches("sha256:")
            ),
        },
        work_attempt: None,
        eligible: 1,
        valid_at: observed_at,
        attempted_at: observed_at,
    })
}

pub fn retain_lsp_delivery_attempt(
    retained: &mut Option<tracedecay_domain::DeliverySettlementAttemptV1>,
    next_sequence: &mut u64,
    frame: &[u8],
    session_id: &tracedecay_daemon_protocol::LspSessionId,
    observed_at: UtcMicros,
) -> Option<tracedecay_domain::DeliverySettlementAttemptV1> {
    if retained.is_none() {
        let outbound_sequence = *next_sequence;
        let candidate = lsp_delivery_attempt(frame, session_id, outbound_sequence, observed_at)?;
        *next_sequence = outbound_sequence.checked_add(1)?;
        *retained = Some(candidate);
    }
    retained.clone()
}

/// What one settlement attempt for a session's in-flight outbound frame did.
///
/// The admission refusals are values rather than a swallowed log line: a
/// refused receipt means this transport's delivery evidence never became
/// durable, and only a typed outcome lets a caller, or a test, tell that
/// apart from "there was nothing in flight".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspDeliverySettlementAdmissionV1 {
    /// No unacknowledged outbound frame was retained for this session.
    NoAttemptInFlight,
    /// The session was opened without a delivery settlement recorder.
    RecorderUnavailable,
    /// The receipt is durable in the recorder's spool and queued for its
    /// write-behind lane; the recorder owns replay from there.
    Enqueued,
    /// The recorder's bounded spool refused the receipt at capacity.
    DroppedAtCapacity,
    /// The recorder refused the receipt, carrying its typed reason code.
    Refused(&'static str),
}

/// The terminal receipt for one session's in-flight outbound frame, claimed
/// out of the session so its durable admission can run without the session.
///
/// Admission publishes the receipt to the recorder's spool with file and
/// directory fsyncs. That is disk-latency work: run under the daemon-wide
/// session map lock it stalls every LSP session and every invocation (each one
/// takes that lock to expire sessions), and run on an async worker it also
/// takes the worker. Claiming under the lock and admitting after it releases
/// both.
pub(super) struct LspDeliverySettlementClaim {
    recorder: Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>,
    settlement: tracedecay_domain::DeliverySettlementV1,
}

impl LspDeliverySettlementClaim {
    /// Admits the receipt on the blocking pool. The work is queued at this
    /// call, so abandoning the returned future abandons only the wait: a
    /// claimed receipt is never lost to a dropped connection.
    pub(super) async fn admit(self) -> LspDeliverySettlementAdmissionV1 {
        match tracedecay_runtime_core::native_runtime::spawn_blocking(move || self.admit_inline())
            .await
        {
            Ok(admission) => admission,
            Err(error) => {
                tracing::warn!(%error, "LSP delivery receipt admission could not run");
                LspDeliverySettlementAdmissionV1::Refused(
                    "lsp_delivery_settlement_blocking_pool_unavailable",
                )
            }
        }
    }

    /// Admission on the calling thread, for `Drop`, which cannot await.
    fn admit_inline(self) -> LspDeliverySettlementAdmissionV1 {
        match self.recorder.try_record(self.settlement) {
            Ok(tracedecay_application::observability::DeliverySettlementRecordOutcomeV1::Enqueued) => {
                LspDeliverySettlementAdmissionV1::Enqueued
            }
            Ok(
                tracedecay_application::observability::DeliverySettlementRecordOutcomeV1::DroppedAtCapacity,
            ) => {
                tracing::warn!("LSP delivery receipt was dropped at recorder capacity");
                LspDeliverySettlementAdmissionV1::DroppedAtCapacity
            }
            Err(error) => {
                tracing::warn!(%error, "LSP delivery receipt was refused");
                LspDeliverySettlementAdmissionV1::Refused(error)
            }
        }
    }
}

impl RuntimeLspSession {
    /// Takes the exact in-flight attempt for settlement. The attempt leaves the
    /// session here, so a concurrent terminalization can never settle it twice.
    pub(super) fn claim_in_flight_delivery(
        &mut self,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) -> Result<LspDeliverySettlementClaim, LspDeliverySettlementAdmissionV1> {
        let Some(attempt) = self.in_flight_delivery_attempt.take() else {
            return Err(LspDeliverySettlementAdmissionV1::NoAttemptInFlight);
        };
        let Some(recorder) = self.delivery_settlements.as_ref() else {
            return Err(LspDeliverySettlementAdmissionV1::RecorderUnavailable);
        };
        Ok(LspDeliverySettlementClaim {
            recorder: Arc::clone(recorder),
            settlement: tracedecay_domain::DeliverySettlementV1 {
                settled_at: now_micros().max(attempt.attempted_at),
                attempt,
                outcome,
                drop_reason,
            },
        })
    }
}

impl Drop for RuntimeLspSession {
    fn drop(&mut self) {
        // Any removal without a protocol ACK discards the frame. This fallback
        // covers transport loss, TTL expiry, owner retirement, and shutdown;
        // explicit paths may settle first with a more specific reason. Drop
        // cannot await, so admission is inline here.
        if let Ok(claim) = self.claim_in_flight_delivery(
            tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
            Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
        ) {
            let _ = claim.admit_inline();
        }
        self.actor.expire();
    }
}

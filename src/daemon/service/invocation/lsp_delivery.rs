//! Durable delivery identity and terminalization for outbound LSP frames.

use super::*;

pub(super) fn lsp_delivery_attempt(
    frame: &[u8],
    session_id: &tracedecay_lsp::LspSessionId,
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

pub(super) fn retain_lsp_delivery_attempt(
    retained: &mut Option<tracedecay_domain::DeliverySettlementAttemptV1>,
    next_sequence: &mut u64,
    frame: &[u8],
    session_id: &tracedecay_lsp::LspSessionId,
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

impl RuntimeLspSession {
    pub(super) fn settle_in_flight_delivery(
        &mut self,
        outcome: tracedecay_domain::DeliverySettlementOutcomeV1,
        drop_reason: Option<tracedecay_domain::DeliveryDropReasonV1>,
    ) {
        let Some(attempt) = self.in_flight_delivery_attempt.take() else {
            return;
        };
        let Some(recorder) = self.delivery_settlements.as_ref() else {
            return;
        };
        let settlement = tracedecay_domain::DeliverySettlementV1 {
            settled_at: current_micros().max(attempt.attempted_at),
            attempt,
            outcome,
            drop_reason,
        };
        match recorder.try_record(settlement) {
            Ok(tracedecay_usecases::observability::DeliverySettlementRecordOutcomeV1::Enqueued) => {}
            Ok(
                tracedecay_usecases::observability::DeliverySettlementRecordOutcomeV1::DroppedAtCapacity,
            ) => tracing::warn!("LSP delivery receipt was dropped at recorder capacity"),
            Err(error) => tracing::warn!(%error, "LSP delivery receipt was refused"),
        }
    }
}

impl Drop for RuntimeLspSession {
    fn drop(&mut self) {
        // Any removal without a protocol ACK discards the frame. This fallback
        // covers transport loss, TTL expiry, owner retirement, and shutdown;
        // explicit paths may settle first with a more specific reason.
        self.settle_in_flight_delivery(
            tracedecay_domain::DeliverySettlementOutcomeV1::Dropped,
            Some(tracedecay_domain::DeliveryDropReasonV1::Disconnected),
        );
        self.actor.expire();
    }
}

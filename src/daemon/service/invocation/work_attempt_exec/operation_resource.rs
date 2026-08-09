//! Same-process timing evidence for settled Work provider attempts.

use std::time::Instant;

use tracedecay_domain::{
    OperationActivationOutcomeV1, OperationAvailabilityV1, OperationResourceObservedV1,
    OperationStageTimingV1, OperationStageV1, WorkAttemptStateV1, WorkAttemptV1,
};

#[derive(Clone, Copy)]
pub(super) struct AttemptAdmissionTimingV1 {
    pub(super) scheduled: Instant,
    pub(super) admitted: Instant,
}

#[cfg(test)]
impl AttemptAdmissionTimingV1 {
    pub(super) fn for_test() -> Self {
        let now = Instant::now();
        Self {
            scheduled: now,
            admitted: now,
        }
    }
}

pub(super) fn work_operation_resource_observation(
    attempt: &WorkAttemptV1,
    timing: AttemptAdmissionTimingV1,
    started: Instant,
    terminal: Instant,
) -> Option<OperationResourceObservedV1> {
    let scheduled_latency_micros = duration_micros(timing.admitted, timing.scheduled)?;
    let admitted_elapsed_micros = scheduled_latency_micros;
    let started_elapsed_micros = duration_micros(started, timing.scheduled)?;
    let terminal_elapsed_micros = duration_micros(terminal, timing.scheduled)?;
    let service_latency_micros = duration_micros(terminal, started)?;
    let activation_outcome = match attempt.state() {
        WorkAttemptStateV1::Succeeded => OperationActivationOutcomeV1::Committed,
        WorkAttemptStateV1::Failed
        | WorkAttemptStateV1::TimedOut
        | WorkAttemptStateV1::Cancelled => OperationActivationOutcomeV1::Admitted,
        _ => return None,
    };
    Some(OperationResourceObservedV1 {
        // Provider-native request identity is unavailable on these execution
        // paths. Thread and local JSON-RPC identifiers are not usage IDs.
        provider_request_id: None,
        scheduled_latency_micros,
        service_latency_micros,
        process_rss_bytes: None,
        process_pss_bytes: None,
        cpu_user_micros: None,
        cpu_system_micros: None,
        read_bytes: None,
        write_bytes: None,
        input_tokens: None,
        output_tokens: None,
        cost_amount: None,
        cost_currency: None,
        pricing_revision: None,
        stage_timings: vec![
            OperationStageTimingV1 {
                stage: OperationStageV1::Scheduled,
                elapsed_micros: 0,
            },
            OperationStageTimingV1 {
                stage: OperationStageV1::Admitted,
                elapsed_micros: admitted_elapsed_micros,
            },
            OperationStageTimingV1 {
                stage: OperationStageV1::Started,
                elapsed_micros: started_elapsed_micros,
            },
            OperationStageTimingV1 {
                stage: OperationStageV1::Terminal,
                elapsed_micros: terminal_elapsed_micros,
            },
        ],
        phase_timings: Vec::new(),
        absolute_deadline_micros: Some(attempt.execution().deadline().0),
        availability: OperationAvailabilityV1::Available,
        activation_outcome: Some(activation_outcome),
        process_count: None,
        input_bytes: None,
        output_bytes: None,
    })
}

fn duration_micros(later: Instant, earlier: Instant) -> Option<u64> {
    u64::try_from(later.checked_duration_since(earlier)?.as_micros()).ok()
}

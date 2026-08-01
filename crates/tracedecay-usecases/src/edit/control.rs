use tracedecay_application::{
    CancellationObservation, CancellationSignal, CancellationStage, Deadline, EffectTermination,
    now_micros,
};

#[derive(Clone, Debug)]
pub struct SourceEditEffectControlV1 {
    deadline: Deadline,
    cancellation: CancellationSignal,
}

impl SourceEditEffectControlV1 {
    pub fn new(deadline: Deadline, cancellation: CancellationSignal) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }

    pub(super) fn checkpoint(&self, stage: CancellationStage) -> Option<SourceEditControlStopV1> {
        let observed_at = now_micros();
        let cancellation_requested_at = self.cancellation.cancelled_at();
        let deadline_elapsed = self.deadline.is_elapsed_at(observed_at);
        let termination = match (cancellation_requested_at, deadline_elapsed) {
            (Some(requested_at), true) if requested_at > self.deadline.expires_at => {
                EffectTermination::TimedOut
            }
            (Some(_), _) => EffectTermination::Cancelled,
            (None, true) => EffectTermination::TimedOut,
            (None, false) => return None,
        };
        Some(SourceEditControlStopV1 {
            termination,
            observation: CancellationObservation { stage, observed_at },
        })
    }
}

pub(super) struct SourceEditControlStopV1 {
    pub(super) termination: EffectTermination,
    pub(super) observation: CancellationObservation,
}

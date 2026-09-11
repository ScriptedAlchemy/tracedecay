use tracedecay_contracts::{
    CancellationObservation, CancellationSignal, CancellationStage, Deadline, EffectTermination,
    now_micros,
};
use tracedecay_graph_db::GraphCancellation;

#[derive(Clone)]
struct SourceEditGraphCancellationV1 {
    deadline: Deadline,
    cancellation: CancellationSignal,
}

impl GraphCancellation for SourceEditGraphCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled() || self.deadline.is_elapsed_at(now_micros())
    }
}

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

    pub fn for_request(
        context: &tracedecay_contracts::RequestContext,
        cancellation: CancellationSignal,
    ) -> Self {
        Self::new(context.deadline().clone(), cancellation)
    }

    pub(super) fn graph_cancellation(&self) -> std::sync::Arc<dyn GraphCancellation> {
        std::sync::Arc::new(SourceEditGraphCancellationV1 {
            deadline: self.deadline.clone(),
            cancellation: self.cancellation.clone(),
        })
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_contracts::{
        CancellationContext, CancellationSignal, CancellationStage, CapabilityGrantId,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, EffectTermination, RequestContext,
        RequestId, ResolvedScope, now_micros, source_edit_operation,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };

    use super::SourceEditEffectControlV1;

    #[test]
    fn request_control_refuses_effect_after_authorization_clamped_deadline() {
        let observed_at = now_micros();
        let grant_expires_at = UtcMicros(observed_at.0.saturating_add(20_000));
        let request_deadline =
            Deadline::new(UtcMicros(observed_at.0.saturating_add(5_000_000))).expect("deadline");
        let context = request_context(observed_at, grant_expires_at, request_deadline);
        let cancellation = CancellationSignal::active("cancel.source-edit.clamped-deadline")
            .expect("cancellation");
        let control = SourceEditEffectControlV1::for_request(&context, cancellation);

        while now_micros() < grant_expires_at {
            std::thread::yield_now();
        }

        let mut effect_committed = false;
        let stop = control.checkpoint(CancellationStage::BeforeEffect);
        if stop.is_none() {
            effect_committed = true;
        }

        assert_eq!(
            stop.map(|stop| stop.termination),
            Some(EffectTermination::TimedOut)
        );
        assert!(!effect_committed, "expired grant must not permit an effect");
    }

    fn request_context(
        observed_at: UtcMicros,
        grant_expires_at: UtcMicros,
        request_deadline: Deadline,
    ) -> RequestContext {
        let operation = source_edit_operation(tracedecay_contracts::SourceEditKind::StrReplace)
            .expect("operation");
        let scope = ResolvedScope::new(
            ProjectId::new("project.source-edit-control").expect("project id"),
            RepositoryId::new("repository.source-edit-control").expect("repository id"),
            WorktreeId::new("worktree.source-edit-control").expect("worktree id"),
            None,
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.source-edit-control").expect("grant id"),
            1,
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
            ActorId::new("actor.source-edit-control").expect("issuer"),
            observed_at,
            grant_expires_at,
            scope.clone(),
            BTreeSet::from([operation.capability_id().clone()]),
            BTreeSet::from([operation.use_case_id().clone()]),
            DisclosureClass::Sensitive,
        )
        .expect("grant");
        RequestContext::new(
            ActorId::new("actor.source-edit-request").expect("requester"),
            scope,
            grant,
            RequestId::new("request.source-edit-control").expect("request id"),
            Deadline::new(UtcMicros(
                request_deadline.expires_at.0.min(grant_expires_at.0),
            ))
            .expect("clamped deadline"),
            CancellationContext::active("cancel.source-edit-context")
                .expect("context cancellation"),
        )
        .expect("request context")
    }
}

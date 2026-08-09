//! Exact, deadline-bounded Work leak evidence classification.

use tracedecay_application::clock::now_micros;
use tracedecay_application::{ApplicationProblem, Deadline, RequestContext, SafeDiagnostic};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};

pub(super) struct DaemonWorkLeakEvidenceV1 {
    pub(super) attempt: tracedecay_domain::WorkAttemptV1,
    pub(super) has_live_owner: bool,
    pub(super) effect: Option<tracedecay_application::WorkAttemptEffectHolderV1>,
    pub(super) placement: tracedecay_application::WorkPlacementReadingV1,
    pub(super) delivery: tracedecay_global_db::WorkAttemptDeliveryCensusReadV1,
    pub(super) source_fingerprint: ManifestDigest,
    pub(super) scan_started_at: UtcMicros,
}

impl tracedecay_application::WorkLeakEvidencePortV1 for DaemonWorkLeakEvidenceV1 {
    fn inspect(
        &self,
        authority: &tracedecay_domain::WorkAuthority,
        command: &tracedecay_application::AdjudicateWorkLeakCommandV1,
        scan_started_at: UtcMicros,
        scan_deadline: UtcMicros,
    ) -> Result<
        tracedecay_application::WorkLeakEvidenceInspectionV1,
        tracedecay_application::WorkLeakEvidenceErrorV1,
    > {
        if self.attempt.identity() != &command.attempt
            || self.scan_started_at != scan_started_at
            || !attempt_matches_authority(&self.attempt, authority)
        {
            return Err(tracedecay_application::WorkLeakEvidenceErrorV1::Conflict);
        }
        if self
            .effect
            .as_ref()
            .is_some_and(|holder| holder.attempt() != self.attempt.identity())
            || !placement_matches_attempt(&self.placement, self.attempt.identity())
        {
            return Err(tracedecay_application::WorkLeakEvidenceErrorV1::Conflict);
        }
        let scan_completed_at = now_micros();
        if scan_completed_at.0 > scan_deadline.0 {
            return Err(tracedecay_application::WorkLeakEvidenceErrorV1::TimedOut);
        }
        let (kind, recovery, owner_class, coverage, evidence_kind) = if self.has_live_owner
            && terminal_horizon_elapsed(
                &self.attempt,
                command.detection_horizon_micros,
                self.scan_started_at,
            ) {
            (
                tracedecay_domain::WorkExecutionLeakKindV1::LeaseAfterTerminal,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Pending,
                tracedecay_domain::LeakOwnerClassV1::Work,
                tracedecay_domain::CoverageStateV1::Known,
                "lease-after-terminal",
            )
        } else if self.effect.as_ref().is_some_and(|holder| {
            holder.is_unknown_past_deadline(self.scan_started_at, command.detection_horizon_micros)
        }) {
            (
                tracedecay_domain::WorkExecutionLeakKindV1::EffectUnknownPastDeadline,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Pending,
                tracedecay_domain::LeakOwnerClassV1::Work,
                tracedecay_domain::CoverageStateV1::Known,
                "effect-past-deadline",
            )
        } else if missing_managed_worktree_binding(&self.attempt, &self.placement) {
            (
                tracedecay_domain::WorkExecutionLeakKindV1::MissingWorktreeBinding,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Pending,
                tracedecay_domain::LeakOwnerClassV1::Work,
                tracedecay_domain::CoverageStateV1::Known,
                "managed-worktree-binding",
            )
        } else if unbounded_delivery(
            &self.delivery,
            self.attempt.identity(),
            command.detection_horizon_micros,
            self.scan_started_at,
        ) {
            (
                tracedecay_domain::WorkExecutionLeakKindV1::UnboundedDelivery,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Pending,
                tracedecay_domain::LeakOwnerClassV1::Delivery,
                tracedecay_domain::CoverageStateV1::Known,
                "delivery-horizon",
            )
        } else if !self.attempt.is_terminal()
            && !self.has_live_owner
            && self.effect.as_ref().is_some_and(|holder| {
                horizon_elapsed(
                    holder.dispatched_at(),
                    command.detection_horizon_micros,
                    self.scan_started_at,
                )
            })
        {
            (
                tracedecay_domain::WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Pending,
                tracedecay_domain::LeakOwnerClassV1::Work,
                tracedecay_domain::CoverageStateV1::Known,
                "attempt-without-live-owner",
            )
        } else if delivery_is_unbound(&self.delivery) {
            // The delivery authority defines Unbound as unknown coverage, not
            // as proof of zero deliveries. Preserve that unknown after every
            // independently proved positive leak has been considered and
            // before publishing a no-leak verdict.
            (
                tracedecay_domain::WorkExecutionLeakKindV1::Unknown,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Unknown,
                tracedecay_domain::LeakOwnerClassV1::Delivery,
                tracedecay_domain::CoverageStateV1::Unknown,
                "delivery-unbound",
            )
        } else if delivery_has_unsettled_observation(&self.delivery, self.attempt.identity()) {
            // A bounded delivery census with unsettled recipients is not a
            // no-leak result before the detection horizon. The positive
            // `UnboundedDelivery` verdict above becomes legal only once that
            // horizon has elapsed.
            (
                tracedecay_domain::WorkExecutionLeakKindV1::Unknown,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Unknown,
                tracedecay_domain::LeakOwnerClassV1::Delivery,
                tracedecay_domain::CoverageStateV1::Unknown,
                "delivery-horizon-pending",
            )
        } else if self.attempt.is_terminal() && self.has_live_owner {
            // The terminal receipt and a current owner disagree until the
            // exact terminal horizon expires. Publishing `None` here would
            // turn a still-observable lease leak into fabricated cleanliness.
            (
                tracedecay_domain::WorkExecutionLeakKindV1::Unknown,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Unknown,
                tracedecay_domain::LeakOwnerClassV1::Work,
                tracedecay_domain::CoverageStateV1::Unknown,
                "terminal-owner-horizon-pending",
            )
        } else if self.attempt.is_terminal() && unresolved_effect(&self.effect) {
            // A terminal Work state does not settle a non-observational
            // provider effect. It remains unknown until its exact source
            // deadline produces the positive leak verdict above or a
            // `NoEffect` receipt.
            (
                tracedecay_domain::WorkExecutionLeakKindV1::Unknown,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Unknown,
                tracedecay_domain::LeakOwnerClassV1::Work,
                tracedecay_domain::CoverageStateV1::Unknown,
                "effect-horizon-pending",
            )
        } else if self.attempt.is_terminal() || self.has_live_owner {
            (
                tracedecay_domain::WorkExecutionLeakKindV1::None,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::NotRequired,
                tracedecay_domain::LeakOwnerClassV1::Unknown,
                tracedecay_domain::CoverageStateV1::Known,
                "no-leak",
            )
        } else {
            // A nonterminal attempt without a live owner still needs the
            // exact dispatch receipt before its horizon can be evaluated.
            // Absence is not proof that dispatch occurred.
            (
                tracedecay_domain::WorkExecutionLeakKindV1::Unknown,
                tracedecay_domain::WorkExecutionLeakRecoveryV1::Unknown,
                tracedecay_domain::LeakOwnerClassV1::Unknown,
                tracedecay_domain::CoverageStateV1::Unknown,
                "incomplete-attempt-evidence",
            )
        };
        Ok(tracedecay_application::WorkLeakEvidenceInspectionV1 {
            evidence: tracedecay_application::VerifiedWorkLeakEvidenceV1 {
                attempt: command.attempt.clone(),
                kind,
                recovery,
                owner_class,
                coverage,
                detection_horizon_micros: command.detection_horizon_micros,
                scan_started_at: self.scan_started_at,
                scan_completed_at,
                evidence_refs: vec![opaque_leak_evidence_ref(
                    evidence_kind,
                    self.attempt.identity(),
                    self.has_live_owner,
                    &self.effect,
                    &self.placement,
                    &self.delivery,
                )?],
            },
            source_fingerprint: self.source_fingerprint.clone(),
        })
    }
}

pub(super) async fn adjudicate_leak(
    registered: &super::super::RegisteredWorkRuntime,
    services: &tracedecay_global_db::RegisteredWorkApplicationServicesV1,
    attempt_processes: &super::super::work_attempt_exec::WorkAttemptProcessRegistryV1,
    context: &RequestContext,
    command: tracedecay_application::AdjudicateWorkLeakCommandV1,
    observed_at: UtcMicros,
    deadline: &Deadline,
) -> Result<tracedecay_application::WorkLeakAdjudicationOutcomeV1, ApplicationProblem> {
    let maximum_scan_deadline = observed_at
        .0
        .saturating_add(tracedecay_application::MAX_WORK_LEAK_SCAN_MICROS_V1 as i64);
    let scan_deadline = UtcMicros(deadline.expires_at.0.min(maximum_scan_deadline));
    let storage = registered.database.work_storage().map_err(|_| {
        ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-leak.storage-unavailable".to_owned(),
            message: "The Work leak adjudication authority is unavailable.".to_owned(),
        })
    })?;
    let evidence = read_leak_evidence(
        registered,
        services,
        attempt_processes,
        context,
        &command.attempt,
        &storage,
        observed_at,
    )
    .await?;
    let service =
        tracedecay_application::WorkLeakAdjudicationServiceV1::new(storage.clone(), evidence);
    let attempt_identity = command.attempt.clone();
    match service.inspect(context, command, observed_at, scan_deadline)? {
        tracedecay_application::WorkLeakAdjudicationInspectionOutcomeV1::Replayed(receipt) => {
            Ok(tracedecay_application::WorkLeakAdjudicationOutcomeV1::Replayed(receipt))
        }
        tracedecay_application::WorkLeakAdjudicationInspectionOutcomeV1::Pending(inspection) => {
            let reread = read_leak_evidence(
                registered,
                services,
                attempt_processes,
                context,
                &attempt_identity,
                &storage,
                observed_at,
            )
            .await?;
            // `record_revalidated` checks this end-of-scan fingerprint as its
            // final operation before the receipt compare-and-swap.
            service.record_revalidated(
                context,
                &inspection,
                reread.source_fingerprint,
                now_micros(),
            )
        }
    }
}

async fn read_leak_evidence(
    registered: &super::super::RegisteredWorkRuntime,
    services: &tracedecay_global_db::RegisteredWorkApplicationServicesV1,
    attempt_processes: &super::super::work_attempt_exec::WorkAttemptProcessRegistryV1,
    context: &RequestContext,
    attempt_identity: &tracedecay_domain::WorkAttemptIdentityV1,
    storage: &tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    scan_started_at: UtcMicros,
) -> Result<DaemonWorkLeakEvidenceV1, ApplicationProblem> {
    let attempt = services.attempts().status(
        context,
        &tracedecay_application::WorkAttemptStatusRequestV1 {
            task_id: attempt_identity.task_id().clone(),
            run_id: attempt_identity.run_id().clone(),
            attempt_id: attempt_identity.attempt_id().clone(),
        },
    )?;
    let placement = services.placement().status(
        context,
        &tracedecay_application::WorkPlacementStatusRequestV1 {
            task_id: attempt.identity().task_id().clone(),
            run_id: attempt.identity().run_id().clone(),
        },
    )?;
    let delivery = registered
        .database
        .work_attempt_delivery_censuses(context.scope().project_id.as_str(), attempt.identity())
        .await
        .map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-leak.delivery-unavailable".to_owned(),
                message: "The Work delivery evidence authority is unavailable.".to_owned(),
            })
        })?;
    let effect = tracedecay_application::WorkAttemptEffectServiceV1::new(storage.clone())
        .load(context, attempt.identity())?;
    let has_live_owner =
        attempt_processes.holds_attempt(&context.scope().worktree_id, attempt.identity());
    let source_fingerprint =
        leak_evidence_source_fingerprint(&attempt, has_live_owner, &effect, &placement, &delivery)
            .map_err(tracedecay_application::work_leak_evidence_problem)?;
    Ok(DaemonWorkLeakEvidenceV1 {
        attempt,
        has_live_owner,
        effect,
        placement,
        delivery,
        source_fingerprint,
        scan_started_at,
    })
}

pub(super) fn leak_evidence_source_fingerprint(
    attempt: &tracedecay_domain::WorkAttemptV1,
    has_live_owner: bool,
    effect: &Option<tracedecay_application::WorkAttemptEffectHolderV1>,
    placement: &tracedecay_application::WorkPlacementReadingV1,
    delivery: &tracedecay_global_db::WorkAttemptDeliveryCensusReadV1,
) -> Result<ManifestDigest, tracedecay_application::WorkLeakEvidenceErrorV1> {
    let delivery_digest = delivery_evidence_digest(delivery)?;
    canonical_sha256(&(
        "tracedecay.daemon.work-leak-source-fingerprint.v1",
        attempt,
        has_live_owner,
        effect,
        placement,
        delivery_digest,
    ))
    .map_err(|_| tracedecay_application::WorkLeakEvidenceErrorV1::Unavailable)
}

fn attempt_matches_authority(
    attempt: &tracedecay_domain::WorkAttemptV1,
    authority: &tracedecay_domain::WorkAuthority,
) -> bool {
    attempt.execution().project_id() == authority.project_id()
        && attempt.execution().repository_id() == authority.repository_id()
        && attempt.execution().worktree_id() == authority.worktree_id()
}

fn placement_matches_attempt(
    placement: &tracedecay_application::WorkPlacementReadingV1,
    attempt: &tracedecay_domain::WorkAttemptIdentityV1,
) -> bool {
    match placement {
        tracedecay_application::WorkPlacementReadingV1::Absent => true,
        tracedecay_application::WorkPlacementReadingV1::Placed { placement } => {
            placement.identity().task_id() == attempt.task_id()
                && placement.identity().run_id() == attempt.run_id()
        }
    }
}

fn missing_managed_worktree_binding(
    attempt: &tracedecay_domain::WorkAttemptV1,
    placement: &tracedecay_application::WorkPlacementReadingV1,
) -> bool {
    let tracedecay_application::WorkPlacementReadingV1::Placed { placement } = placement else {
        // An explicitly absent managed placement cannot be relabeled as a
        // missing worktree binding; existing-worktree runs need no placement.
        return false;
    };
    let target = placement.target();
    if !target.kind().requires_target_root() {
        return false;
    }
    match placement.state() {
        tracedecay_domain::WorkPlacementStateV1::Admitted
        | tracedecay_domain::WorkPlacementStateV1::Quarantined => target
            .root()
            .is_none_or(|root| root != attempt.execution().worktree_root()),
        // A released target deliberately no longer has a live Work holder.
        // It is a binding leak only while its attempt remains nonterminal;
        // terminal cleanup is not retroactively an execution leak.
        tracedecay_domain::WorkPlacementStateV1::Released => !attempt.is_terminal(),
    }
}

fn unbounded_delivery(
    delivery: &tracedecay_global_db::WorkAttemptDeliveryCensusReadV1,
    attempt: &tracedecay_domain::WorkAttemptIdentityV1,
    horizon_micros: u64,
    scan_started_at: UtcMicros,
) -> bool {
    match delivery {
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::ExceededBound { .. } => true,
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Bounded(censuses) => {
            censuses.iter().any(|census| {
                census.work_attempt.as_ref() == Some(attempt)
                    && census.unknown > 0
                    && horizon_elapsed(census.valid_at, horizon_micros, scan_started_at)
            })
        }
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Unbound => false,
    }
}

fn delivery_is_unbound(delivery: &tracedecay_global_db::WorkAttemptDeliveryCensusReadV1) -> bool {
    matches!(
        delivery,
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Unbound
    )
}

fn delivery_has_unsettled_observation(
    delivery: &tracedecay_global_db::WorkAttemptDeliveryCensusReadV1,
    attempt: &tracedecay_domain::WorkAttemptIdentityV1,
) -> bool {
    match delivery {
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Bounded(censuses) => censuses
            .iter()
            .any(|census| census.work_attempt.as_ref() == Some(attempt) && census.unknown > 0),
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::ExceededBound { .. }
        | tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Unbound => false,
    }
}

fn unresolved_effect(effect: &Option<tracedecay_application::WorkAttemptEffectHolderV1>) -> bool {
    effect.as_ref().is_some_and(|holder| {
        !matches!(
            holder.effect_state(),
            tracedecay_domain::WorkEffectStateV1::Observational
        ) && holder.resolution()
            != Some(tracedecay_application::WorkAttemptEffectResolutionV1::NoEffect)
    })
}

fn horizon_elapsed(valid_at: UtcMicros, horizon_micros: u64, observed_at: UtcMicros) -> bool {
    i64::try_from(horizon_micros)
        .ok()
        .and_then(|horizon| valid_at.0.checked_add(horizon))
        .is_some_and(|deadline| observed_at.0 >= deadline)
}

fn terminal_horizon_elapsed(
    attempt: &tracedecay_domain::WorkAttemptV1,
    horizon_micros: u64,
    scan_started_at: UtcMicros,
) -> bool {
    let Some(terminal) = attempt.terminal() else {
        return false;
    };
    let observed_at = match terminal {
        tracedecay_domain::WorkTerminalEvidenceV1::Succeeded { observed_at, .. }
        | tracedecay_domain::WorkTerminalEvidenceV1::Failed { observed_at, .. }
        | tracedecay_domain::WorkTerminalEvidenceV1::TimedOut { observed_at, .. }
        | tracedecay_domain::WorkTerminalEvidenceV1::Cancelled { observed_at, .. } => *observed_at,
    };
    horizon_elapsed(observed_at, horizon_micros, scan_started_at)
}

fn opaque_leak_evidence_ref(
    kind: &str,
    attempt: &tracedecay_domain::WorkAttemptIdentityV1,
    has_live_owner: bool,
    effect: &Option<tracedecay_application::WorkAttemptEffectHolderV1>,
    placement: &tracedecay_application::WorkPlacementReadingV1,
    delivery: &tracedecay_global_db::WorkAttemptDeliveryCensusReadV1,
) -> Result<String, tracedecay_application::WorkLeakEvidenceErrorV1> {
    let delivery_digest = delivery_evidence_digest(delivery)?;
    let digest = canonical_sha256(&(
        "tracedecay.daemon.work-leak-evidence.v2",
        kind,
        attempt,
        has_live_owner,
        effect,
        placement,
        delivery_digest,
    ))
    .map_err(|_| tracedecay_application::WorkLeakEvidenceErrorV1::Unavailable)?;
    Ok(format!("work-leak:{kind}:{}", digest.as_str()))
}

fn delivery_evidence_digest(
    delivery: &tracedecay_global_db::WorkAttemptDeliveryCensusReadV1,
) -> Result<ManifestDigest, tracedecay_application::WorkLeakEvidenceErrorV1> {
    let evidence = match delivery {
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Unbound => {
            canonical_sha256(&("tracedecay.daemon.work-leak-delivery.v1", "unbound"))
        }
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Bounded(censuses) => {
            canonical_sha256(&(
                "tracedecay.daemon.work-leak-delivery.v1",
                "bounded",
                censuses,
            ))
        }
        tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::ExceededBound {
            observed_at_least,
        } => canonical_sha256(&(
            "tracedecay.daemon.work-leak-delivery.v1",
            "exceeded_bound",
            observed_at_least,
        )),
    };
    evidence.map_err(|_| tracedecay_application::WorkLeakEvidenceErrorV1::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizon_requires_the_exact_source_time_and_refuses_overflow() {
        assert!(!horizon_elapsed(UtcMicros(10), 20, UtcMicros(29)));
        assert!(horizon_elapsed(UtcMicros(10), 20, UtcMicros(30)));
        assert!(!horizon_elapsed(
            UtcMicros(i64::MAX),
            1,
            UtcMicros(i64::MAX)
        ));
        assert!(!horizon_elapsed(
            UtcMicros(10),
            u64::MAX,
            UtcMicros(i64::MAX)
        ));
        assert!(delivery_is_unbound(
            &tracedecay_global_db::WorkAttemptDeliveryCensusReadV1::Unbound
        ));
    }
}

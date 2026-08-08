use super::*;

impl BoundedObservabilityProducerV1 {
    /// Offer one owner fact without waiting for storage. The worker claims the
    /// exact normalized fact before settlement; queue pressure is represented
    /// by the producer's ordinary drop carrier.
    pub fn try_emit_owner_fact(
        &self,
        envelope: ObservabilityEnvelopeV1,
    ) -> Result<ObservabilityEmissionOutcomeV1, &'static str> {
        let _emission_guard = self
            .emission_lock
            .lock()
            .map_err(|_| "observability_producer_lock_poisoned")?;
        self.validate_admission(&envelope)?;
        self.prepare_delivery(envelope.clone(), 1, false)?;
        let owner_fact_json = normalized_owner_fact_json(&envelope)
            .map_err(|_| "observability_owner_fact_serialization")?;
        self.offer_unclaimed_owner(envelope, owner_fact_json)
    }

    /// Offer a bounded owner batch synchronously. No storage or task spawn
    /// occurs on the caller's product-response path.
    pub fn try_emit_owner_facts(
        &self,
        envelopes: Vec<ObservabilityEnvelopeV1>,
    ) -> Result<Vec<ObservabilityEmissionOutcomeV1>, &'static str> {
        if envelopes.is_empty() || envelopes.len() > MAX_PRODUCER_CAPACITY {
            return Err("observability_owner_batch_bounds");
        }
        let _emission_guard = self
            .emission_lock
            .lock()
            .map_err(|_| "observability_producer_lock_poisoned")?;
        let mut prepared = Vec::with_capacity(envelopes.len());
        for envelope in envelopes {
            self.validate_admission(&envelope)?;
            self.prepare_delivery(envelope.clone(), 1, false)?;
            let owner_fact_json = normalized_owner_fact_json(&envelope)
                .map_err(|_| "observability_owner_fact_serialization")?;
            prepared.push((envelope, owner_fact_json));
        }
        prepared
            .into_iter()
            .map(|(envelope, owner_fact_json)| {
                self.offer_unclaimed_owner(envelope, owner_fact_json)
            })
            .collect()
    }

    fn offer_unclaimed_owner(
        &self,
        envelope: ObservabilityEnvelopeV1,
        owner_fact_json: String,
    ) -> Result<ObservabilityEmissionOutcomeV1, &'static str> {
        match self.data.try_send(QueuedObservation {
            envelope,
            carried_drop: None,
            owner_fact: Some(QueuedOwnerFact {
                json: owner_fact_json,
                durable_claimed: false,
            }),
        }) {
            Ok(()) => Ok(ObservabilityEmissionOutcomeV1::Enqueued),
            Err(mpsc::error::TrySendError::Full(_)) => {
                let sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
                self.record_capacity_drop(sequence);
                Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err("observability_producer_closed"),
        }
    }

    /// Durably admit one owner-derived fact before offering its exact delivery
    /// to the bounded worker.
    pub async fn emit_owner_fact(
        &self,
        envelope: ObservabilityEnvelopeV1,
    ) -> Result<ObservabilityOwnerEmissionOutcomeV1, ApplicationContractError> {
        timeout(
            self.deadlines.persistence,
            self.emit_owner_fact_unbounded(envelope),
        )
        .await
        .map_err(|_| {
            ApplicationContractError::Domain("observability_outbox_admission_deadline".to_owned())
        })?
    }

    /// Admit a bounded owner batch under one persistence deadline. A claim
    /// committed before cancellation remains pending for worker recovery.
    pub async fn emit_owner_facts(
        &self,
        envelopes: Vec<ObservabilityEnvelopeV1>,
    ) -> Result<Vec<ObservabilityOwnerEmissionOutcomeV1>, ApplicationContractError> {
        if envelopes.is_empty() || envelopes.len() > MAX_PRODUCER_CAPACITY {
            return Err(ApplicationContractError::Domain(
                "observability_owner_batch_bounds".to_owned(),
            ));
        }
        timeout(self.deadlines.persistence, async {
            let mut outcomes = Vec::with_capacity(envelopes.len());
            for envelope in envelopes {
                outcomes.push(self.emit_owner_fact_unbounded(envelope).await?);
            }
            Ok(outcomes)
        })
        .await
        .map_err(|_| {
            ApplicationContractError::Domain("observability_outbox_admission_deadline".to_owned())
        })?
    }

    async fn emit_owner_fact_unbounded(
        &self,
        envelope: ObservabilityEnvelopeV1,
    ) -> Result<ObservabilityOwnerEmissionOutcomeV1, ApplicationContractError> {
        let _durable_guard = self.durable_emission_lock.lock().await;
        self.validate_admission(&envelope)
            .map_err(|error| ApplicationContractError::Domain(error.to_owned()))?;
        let owner_fact_json = normalized_owner_fact_json(&envelope).map_err(|error| {
            ApplicationContractError::Domain(format!(
                "observability owner fact serialization failed: {error}"
            ))
        })?;
        let owner_event_id = envelope.idempotency_key.clone();
        if let Some(existing) = self
            .db
            .observability_emission_claim(
                &self.identity.authorized_scope_ref,
                &owner_event_id,
                &owner_fact_json,
            )
            .await
            .map_err(ApplicationContractError::Domain)?
        {
            return Ok(replay_owner_claim(existing));
        }

        let permit = self.data.try_reserve().ok();
        let delayed = permit.is_none();
        let sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        let delivery = self
            .prepare_delivery(envelope, sequence, delayed)
            .map_err(|error| ApplicationContractError::Domain(error.to_owned()))?;
        let delivery_envelope_json = serde_json::to_string(&delivery).map_err(|error| {
            ApplicationContractError::Domain(format!(
                "observability delivery serialization failed: {error}"
            ))
        })?;
        let claim = self
            .db
            .claim_observability_emission(
                &self.identity.authorized_scope_ref,
                &owner_event_id,
                &owner_fact_json,
                &delivery_envelope_json,
            )
            .await
            .map_err(ApplicationContractError::Domain)?;
        match claim {
            ObservabilityEmissionClaimV1::Claimed { .. } => match permit {
                Some(permit) => {
                    permit.send(QueuedObservation {
                        envelope: delivery,
                        carried_drop: None,
                        owner_fact: Some(QueuedOwnerFact {
                            json: owner_fact_json,
                            durable_claimed: true,
                        }),
                    });
                    Ok(ObservabilityOwnerEmissionOutcomeV1::Enqueued)
                }
                None => Ok(ObservabilityOwnerEmissionOutcomeV1::DeferredDurable),
            },
            existing @ (ObservabilityEmissionClaimV1::Pending { .. }
            | ObservabilityEmissionClaimV1::Settled { .. }) => {
                drop(permit);
                Ok(replay_owner_claim(existing))
            }
        }
    }
}

pub(super) async fn claim_and_settle_durable(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    next_sequence: &AtomicU64,
    envelope: ObservabilityEnvelopeV1,
    owner_fact_json: String,
    persisted: &mut u64,
    first_error: &mut Option<ApplicationContractError>,
    persistence_deadline: Duration,
) {
    let existing = match timeout(
        persistence_deadline,
        db.observability_emission_claim(
            &envelope.scope_ref,
            &envelope.idempotency_key,
            &owner_fact_json,
        ),
    )
    .await
    {
        Ok(Ok(existing)) => existing,
        Ok(Err(error)) => {
            retain_first_error(first_error, ApplicationContractError::Domain(error));
            return;
        }
        Err(_) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain("observability_persistence_deadline".to_owned()),
            );
            return;
        }
    };
    if existing.is_some() {
        // Exact replay is already owned by its stored carrier. In particular,
        // do not stamp or allocate a fresh boot/sequence identity for it.
        return;
    }
    let sequence = next_sequence.fetch_add(1, Ordering::AcqRel);
    let envelope = match prepare_delivery_with_identity(identity, envelope, sequence, false) {
        Ok(envelope) => envelope,
        Err(error) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain(error.to_owned()),
            );
            return;
        }
    };
    let delivery_envelope_json = match serde_json::to_string(&envelope) {
        Ok(delivery) => delivery,
        Err(error) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain(format!(
                    "observability delivery serialization failed: {error}"
                )),
            );
            return;
        }
    };
    let claim = match timeout(
        persistence_deadline,
        db.claim_observability_emission(
            &envelope.scope_ref,
            &envelope.idempotency_key,
            &owner_fact_json,
            &delivery_envelope_json,
        ),
    )
    .await
    {
        Ok(Ok(claim)) => claim,
        Ok(Err(error)) => {
            retain_first_error(first_error, ApplicationContractError::Domain(error));
            return;
        }
        Err(_) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain("observability_persistence_deadline".to_owned()),
            );
            return;
        }
    };
    let delivery = match claim {
        ObservabilityEmissionClaimV1::Claimed { .. } => envelope,
        ObservabilityEmissionClaimV1::Pending {
            delivery_envelope_json,
        } => match serde_json::from_str(&delivery_envelope_json) {
            Ok(delivery) => delivery,
            Err(error) => {
                retain_first_error(
                    first_error,
                    ApplicationContractError::Domain(format!(
                        "observability pending delivery decode failed: {error}"
                    )),
                );
                return;
            }
        },
        ObservabilityEmissionClaimV1::Settled { .. } => return,
    };
    settle_durable(
        db,
        delivery,
        owner_fact_json,
        persisted,
        first_error,
        persistence_deadline,
    )
    .await;
}

fn replay_owner_claim(_claim: ObservabilityEmissionClaimV1) -> ObservabilityOwnerEmissionOutcomeV1 {
    // A pending delivery is already owned by this worker or by restart
    // recovery. Re-enqueuing it here would create a second carrier for one
    // owner fact and could race settlement with delayed-evidence recovery.
    ObservabilityOwnerEmissionOutcomeV1::Replayed
}

pub(super) async fn recover_pending(
    db: &RegisteredGlobalDb,
    identity: &ObservabilityProducerIdentityV1,
    durable_emission_lock: &AsyncMutex<()>,
    progress: &mut ProducerWorkerProgress,
    persistence_deadline: Duration,
) {
    // Admission holds the same lock from lookup through queue handoff. This
    // prevents the worker from adopting a newly claimed row before its sender
    // has either enqueued it or returned it as durable deferred work.
    let _durable_guard = durable_emission_lock.lock().await;
    let pending = match timeout(
        persistence_deadline,
        db.pending_observability_emissions(&identity.authorized_scope_ref, MAX_PRODUCER_CAPACITY),
    )
    .await
    {
        Ok(Ok(pending)) => pending,
        Ok(Err(error)) => {
            retain_first_error(
                &mut progress.first_error,
                ApplicationContractError::Domain(error),
            );
            return;
        }
        Err(_) => {
            retain_first_error(
                &mut progress.first_error,
                ApplicationContractError::Domain("observability_persistence_deadline".to_owned()),
            );
            return;
        }
    };
    for pending in pending {
        recover_pending_record(db, pending, progress, persistence_deadline).await;
    }
}

async fn recover_pending_record(
    db: &RegisteredGlobalDb,
    pending: ObservabilityEmissionOutboxRecordV1,
    progress: &mut ProducerWorkerProgress,
    persistence_deadline: Duration,
) {
    let delayed = match delayed_delivery_json(&pending.delivery_envelope_json) {
        Ok(delayed) => delayed,
        Err(error) => {
            retain_first_error(&mut progress.first_error, error);
            return;
        }
    };
    let claim = match timeout(
        persistence_deadline,
        db.delay_observability_emission(
            &pending.project_id,
            &pending.owner_event_id,
            &pending.owner_fact_json,
            &pending.delivery_envelope_json,
            &delayed,
        ),
    )
    .await
    {
        Ok(Ok(claim)) => claim,
        Ok(Err(error)) => {
            retain_first_error(
                &mut progress.first_error,
                ApplicationContractError::Domain(error),
            );
            return;
        }
        Err(_) => {
            retain_first_error(
                &mut progress.first_error,
                ApplicationContractError::Domain("observability_persistence_deadline".to_owned()),
            );
            return;
        }
    };
    let ObservabilityEmissionClaimV1::Pending {
        delivery_envelope_json,
    } = claim
    else {
        return;
    };
    let envelope = match serde_json::from_str(&delivery_envelope_json) {
        Ok(envelope) => envelope,
        Err(error) => {
            retain_first_error(
                &mut progress.first_error,
                ApplicationContractError::Domain(format!(
                    "observability pending delivery decode failed: {error}"
                )),
            );
            return;
        }
    };
    settle_durable(
        db,
        envelope,
        pending.owner_fact_json,
        &mut progress.persisted,
        &mut progress.first_error,
        persistence_deadline,
    )
    .await;
}

pub(super) async fn settle_durable(
    db: &RegisteredGlobalDb,
    envelope: ObservabilityEnvelopeV1,
    owner_fact_json: String,
    persisted: &mut u64,
    first_error: &mut Option<ApplicationContractError>,
    persistence_deadline: Duration,
) {
    let delivery_envelope_json = match serde_json::to_string(&envelope) {
        Ok(delivery) => delivery,
        Err(error) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain(format!(
                    "observability delivery serialization failed: {error}"
                )),
            );
            return;
        }
    };
    let event = analytics_event_for_delivery(&envelope, delivery_envelope_json.clone());
    match timeout(
        persistence_deadline,
        db.settle_observability_emission(
            &envelope.scope_ref,
            &envelope.idempotency_key,
            &owner_fact_json,
            &delivery_envelope_json,
            &event,
        ),
    )
    .await
    {
        Ok(Ok(_)) => *persisted = persisted.saturating_add(1),
        Ok(Err(error)) => retain_first_error(first_error, ApplicationContractError::Domain(error)),
        Err(_) => retain_first_error(
            first_error,
            ApplicationContractError::Domain("observability_persistence_deadline".to_owned()),
        ),
    }
}

/// Settles a queue carrier from the canonical outbox envelope. Recovery may
/// have changed a pending claim to its delayed representation after admission
/// handed the original carrier to the queue; the stored CAS value wins.
pub(super) async fn settle_claimed_durable(
    db: &RegisteredGlobalDb,
    envelope: ObservabilityEnvelopeV1,
    owner_fact_json: String,
    persisted: &mut u64,
    first_error: &mut Option<ApplicationContractError>,
    persistence_deadline: Duration,
) {
    let claim = match timeout(
        persistence_deadline,
        db.observability_emission_claim(
            &envelope.scope_ref,
            &envelope.idempotency_key,
            &owner_fact_json,
        ),
    )
    .await
    {
        Ok(Ok(Some(claim))) => claim,
        Ok(Ok(None)) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain(
                    "observability claimed delivery is unavailable".to_owned(),
                ),
            );
            return;
        }
        Ok(Err(error)) => {
            retain_first_error(first_error, ApplicationContractError::Domain(error));
            return;
        }
        Err(_) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain("observability_persistence_deadline".to_owned()),
            );
            return;
        }
    };
    let delivery_envelope_json = match claim {
        ObservabilityEmissionClaimV1::Pending {
            delivery_envelope_json,
        } => delivery_envelope_json,
        ObservabilityEmissionClaimV1::Settled { .. } => return,
        ObservabilityEmissionClaimV1::Claimed { .. } => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain(
                    "observability claimed delivery state is invalid".to_owned(),
                ),
            );
            return;
        }
    };
    let canonical_envelope = match serde_json::from_str(&delivery_envelope_json) {
        Ok(envelope) => envelope,
        Err(error) => {
            retain_first_error(
                first_error,
                ApplicationContractError::Domain(format!(
                    "observability pending delivery decode failed: {error}"
                )),
            );
            return;
        }
    };
    settle_durable(
        db,
        canonical_envelope,
        owner_fact_json,
        persisted,
        first_error,
        persistence_deadline,
    )
    .await;
}

fn analytics_event_for_delivery(
    envelope: &ObservabilityEnvelopeV1,
    metadata_json: String,
) -> AnalyticsEventInsert {
    AnalyticsEventInsert {
        provider: "tracedecay-observability".to_owned(),
        project_id: envelope.scope_ref.clone(),
        session_id: None,
        timestamp: envelope.event_time_micros.div_euclid(1_000_000),
        event_kind: envelope.event_kind.clone(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: Some(envelope.idempotency_key.clone()),
        outcome: envelope
            .terminal_result
            .map(|result| format!("{result:?}").to_ascii_lowercase()),
        metadata_json: Some(metadata_json),
    }
}

fn retain_first_error(
    first_error: &mut Option<ApplicationContractError>,
    error: ApplicationContractError,
) {
    if first_error.is_none() {
        *first_error = Some(error);
    }
}

fn normalized_owner_fact_json(
    envelope: &ObservabilityEnvelopeV1,
) -> Result<String, serde_json::Error> {
    let mut owner = envelope.clone();
    owner.producer_revision = "producer-owned".to_owned();
    owner.configuration_revision = "producer-owned".to_owned();
    owner.policy_revision = "producer-owned".to_owned();
    owner.watermark = "producer-owned".to_owned();
    owner.process_boot_id = "producer-owned".to_owned();
    owner.producer_sequence = 0;
    serde_json::to_string(&owner)
}

pub(super) fn mark_delivery_delayed(envelope: &mut ObservabilityEnvelopeV1) {
    envelope.delayed_count = envelope.emitted_count;
    if matches!(
        envelope.coverage,
        CoverageStateV1::Known | CoverageStateV1::Sampled
    ) {
        envelope.coverage = CoverageStateV1::Partial;
        envelope.sampling_probability = None;
    }
}

fn delayed_delivery_json(delivery: &str) -> Result<String, ApplicationContractError> {
    let mut envelope: ObservabilityEnvelopeV1 =
        serde_json::from_str(delivery).map_err(|error| {
            ApplicationContractError::Domain(format!(
                "observability pending delivery decode failed: {error}"
            ))
        })?;
    mark_delivery_delayed(&mut envelope);
    envelope
        .validate()
        .map_err(|error| ApplicationContractError::Domain(error.to_owned()))?;
    serde_json::to_string(&envelope).map_err(|error| {
        ApplicationContractError::Domain(format!(
            "observability delayed delivery serialization failed: {error}"
        ))
    })
}

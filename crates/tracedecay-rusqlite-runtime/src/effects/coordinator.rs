use std::{error::Error, fmt};

use tracedecay_store::{
    EffectIdentityV1, OutboxAcknowledgementReceiptV1, OutboxEffectStateV1,
    StorageRuntimeContractErrorV1, StoreEffectIdV1, StoreRuntimeBindingV1,
    TransactionalOutboxEntryV1,
};

use super::ports::{
    OriginDispatchPreparation, OriginEffectReplayTransactions, OriginEffectTransactions,
    TargetEffectTransactions,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginFailureStage {
    LoadReplayCandidates,
    PrepareDispatch,
    MarkEffectUnknown,
    Acknowledge,
}

#[derive(Debug)]
pub enum EffectCoordinatorError<O, T> {
    EffectNotFound {
        effect_id: StoreEffectIdV1,
    },
    Contract(StorageRuntimeContractErrorV1),
    Origin {
        stage: OriginFailureStage,
        source: O,
    },
    /// The target result is unknown and the origin fence also rejected the
    /// durable `EffectUnknown` transition. Operators must reconcile this entry.
    UnrecordedEffectUnknown {
        target: T,
        origin: O,
    },
}

impl<O: fmt::Display, T: fmt::Display> fmt::Display for EffectCoordinatorError<O, T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EffectNotFound { effect_id } => {
                write!(
                    formatter,
                    "durable effect {} was not found",
                    effect_id.as_str()
                )
            }
            Self::Contract(error) => write!(formatter, "durable effect contract failed: {error}"),
            Self::Origin { stage, source } => {
                write!(
                    formatter,
                    "durable effect origin failed at {stage:?}: {source}"
                )
            }
            Self::UnrecordedEffectUnknown { target, origin } => write!(
                formatter,
                "target result became unknown ({target}) and origin reconciliation failed ({origin})"
            ),
        }
    }
}

impl<O: Error + 'static, T: Error + 'static> Error for EffectCoordinatorError<O, T> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Contract(error) => Some(error),
            Self::Origin { source, .. } => Some(source),
            Self::UnrecordedEffectUnknown { target, .. } => Some(target),
            Self::EffectNotFound { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum EffectUnknownCause<T> {
    /// A restart observed `Dispatched` without a receipt. No target call is made
    /// on this pass; the durable unknown state makes a later retry explicit.
    RecoveredInFlight,
    Target(T),
}

#[derive(Debug)]
pub struct EffectUnknown<T> {
    pub entry: TransactionalOutboxEntryV1,
    pub cause: EffectUnknownCause<T>,
}

#[derive(Debug)]
pub enum EffectDispatchOutcome<T> {
    Acknowledged {
        receipt: Box<OutboxAcknowledgementReceiptV1>,
        replayed: bool,
    },
    EffectUnknown(Box<EffectUnknown<T>>),
}

pub type EffectDispatchResult<O, T> =
    Result<EffectDispatchOutcome<T>, EffectCoordinatorError<O, T>>;

#[derive(Debug)]
pub struct EffectReplayAttempt<O, T> {
    pub effect_id: StoreEffectIdV1,
    pub result: EffectDispatchResult<O, T>,
}

/// Observable result of one bounded recovery pass.
///
/// Per-effect failures stay in `attempts` so operators can inspect them while
/// independent ordering keys continue making progress.
#[derive(Debug)]
pub struct EffectReplayReport<O, T> {
    pub limit: usize,
    pub attempts: Vec<EffectReplayAttempt<O, T>>,
}

/// Coordinates one durable effect at a time. Ordering and all physical
/// transaction details remain in the origin and target ports.
#[derive(Clone, Copy, Debug, Default)]
pub struct EffectCoordinator;

impl EffectCoordinator {
    pub async fn replay_bounded<O, T>(
        &self,
        origin_binding: &StoreRuntimeBindingV1,
        target_binding: &StoreRuntimeBindingV1,
        origin: &mut O,
        target: &mut T,
        limit: usize,
    ) -> Result<EffectReplayReport<O::Error, T::Error>, EffectCoordinatorError<O::Error, T::Error>>
    where
        O: OriginEffectReplayTransactions,
        T: TargetEffectTransactions,
    {
        validate_route(origin_binding, target_binding)?;
        if limit == 0 {
            return Ok(EffectReplayReport {
                limit,
                attempts: Vec::new(),
            });
        }
        let mut candidates = origin
            .replay_candidates(origin_binding, target_binding, limit)
            .await
            .map_err(|source| EffectCoordinatorError::Origin {
                stage: OriginFailureStage::LoadReplayCandidates,
                source,
            })?;
        candidates.truncate(limit);
        let mut attempts = Vec::with_capacity(candidates.len());
        for effect_id in candidates {
            let result = self
                .dispatch(&effect_id, origin_binding, target_binding, origin, target)
                .await;
            attempts.push(EffectReplayAttempt { effect_id, result });
        }
        Ok(EffectReplayReport { limit, attempts })
    }

    pub async fn dispatch<O, T>(
        &self,
        effect_id: &StoreEffectIdV1,
        origin_binding: &StoreRuntimeBindingV1,
        target_binding: &StoreRuntimeBindingV1,
        origin: &mut O,
        target: &mut T,
    ) -> EffectDispatchResult<O::Error, T::Error>
    where
        O: OriginEffectTransactions,
        T: TargetEffectTransactions,
    {
        validate_route(origin_binding, target_binding)?;
        let prepared = origin
            .prepare_dispatch(origin_binding, effect_id)
            .await
            .map_err(|source| EffectCoordinatorError::Origin {
                stage: OriginFailureStage::PrepareDispatch,
                source,
            })?
            .ok_or_else(|| EffectCoordinatorError::EffectNotFound {
                effect_id: effect_id.clone(),
            })?;

        match prepared {
            OriginDispatchPreparation::Acknowledged(receipt) => {
                receipt
                    .validate()
                    .map_err(EffectCoordinatorError::Contract)?;
                validate_identity(&receipt.identity, effect_id, origin_binding, target_binding)?;
                Ok(EffectDispatchOutcome::Acknowledged {
                    receipt: Box::new(receipt),
                    replayed: true,
                })
            }
            OriginDispatchPreparation::InFlightWithoutReceipt(entry) => {
                validate_entry(&entry, effect_id, origin_binding, target_binding)?;
                let unknown = origin
                    .mark_effect_unknown(origin_binding, &entry)
                    .await
                    .map_err(|source| EffectCoordinatorError::Origin {
                        stage: OriginFailureStage::MarkEffectUnknown,
                        source,
                    })?;
                validate_unknown(&unknown, &entry)?;
                Ok(EffectDispatchOutcome::EffectUnknown(Box::new(
                    EffectUnknown {
                        entry: unknown,
                        cause: EffectUnknownCause::RecoveredInFlight,
                    },
                )))
            }
            OriginDispatchPreparation::Prepared(entry) => {
                validate_entry(&entry, effect_id, origin_binding, target_binding)?;
                match target.apply_once(target_binding, &entry).await {
                    Ok(inbox) => {
                        let replayed = inbox.disposition
                            == tracedecay_store::InboxEffectDispositionV1::Replayed;
                        inbox
                            .validate_for(&entry.identity)
                            .map_err(EffectCoordinatorError::Contract)?;
                        validate_binding(&inbox.target_commit_watermark, target_binding, "target")?;
                        let acknowledgement = origin
                            .acknowledge(origin_binding, &inbox)
                            .await
                            .map_err(|source| EffectCoordinatorError::Origin {
                                stage: OriginFailureStage::Acknowledge,
                                source,
                            })?;
                        acknowledgement
                            .validate()
                            .map_err(EffectCoordinatorError::Contract)?;
                        if acknowledgement.inbox_receipt != inbox {
                            return Err(EffectCoordinatorError::Contract(
                                StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                                    field: "coordinator inbox acknowledgement",
                                },
                            ));
                        }
                        validate_binding(
                            &acknowledgement.source_commit_watermark,
                            origin_binding,
                            "source",
                        )?;
                        Ok(EffectDispatchOutcome::Acknowledged {
                            receipt: Box::new(acknowledgement),
                            replayed,
                        })
                    }
                    Err(target_error) => {
                        let unknown = match origin.mark_effect_unknown(origin_binding, &entry).await
                        {
                            Ok(unknown) => unknown,
                            Err(origin) => {
                                return Err(EffectCoordinatorError::UnrecordedEffectUnknown {
                                    target: target_error,
                                    origin,
                                });
                            }
                        };
                        validate_unknown(&unknown, &entry)?;
                        Ok(EffectDispatchOutcome::EffectUnknown(Box::new(
                            EffectUnknown {
                                entry: unknown,
                                cause: EffectUnknownCause::Target(target_error),
                            },
                        )))
                    }
                }
            }
        }
    }
}

fn validate_route<O, T>(
    origin: &StoreRuntimeBindingV1,
    target: &StoreRuntimeBindingV1,
) -> Result<(), EffectCoordinatorError<O, T>> {
    if origin.shard_id.brain_id != target.shard_id.brain_id
        || origin.shard_id.profile_id != target.shard_id.profile_id
    {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::ShardMismatch {
                field: "effect authority root",
            },
        ));
    }
    if origin.shard_id.scope.project_id().is_none()
        || origin.shard_id.scope.project_id() != target.shard_id.scope.project_id()
    {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::ShardMismatch {
                field: "effect project identity",
            },
        ));
    }
    if origin.shard_id == target.shard_id {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::ShardMismatch {
                field: "cross-shard effect target",
            },
        ));
    }
    Ok(())
}

fn validate_entry<O, T>(
    entry: &TransactionalOutboxEntryV1,
    effect_id: &StoreEffectIdV1,
    origin: &StoreRuntimeBindingV1,
    target: &StoreRuntimeBindingV1,
) -> Result<(), EffectCoordinatorError<O, T>> {
    entry.validate().map_err(EffectCoordinatorError::Contract)?;
    validate_identity(&entry.identity, effect_id, origin, target)?;
    if entry.state != OutboxEffectStateV1::Dispatched || entry.acknowledgement.is_some() {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "prepared outbox state",
            },
        ));
    }
    Ok(())
}

fn validate_unknown<O, T>(
    unknown: &TransactionalOutboxEntryV1,
    dispatched: &TransactionalOutboxEntryV1,
) -> Result<(), EffectCoordinatorError<O, T>> {
    unknown
        .validate()
        .map_err(EffectCoordinatorError::Contract)?;
    if unknown.identity != dispatched.identity
        || unknown.effect != dispatched.effect
        || unknown.state != OutboxEffectStateV1::EffectUnknown
        || unknown.acknowledgement.is_some()
    {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "effect unknown transition",
            },
        ));
    }
    Ok(())
}

fn validate_identity<O, T>(
    identity: &EffectIdentityV1,
    effect_id: &StoreEffectIdV1,
    origin: &StoreRuntimeBindingV1,
    target: &StoreRuntimeBindingV1,
) -> Result<(), EffectCoordinatorError<O, T>> {
    identity
        .validate()
        .map_err(EffectCoordinatorError::Contract)?;
    if &identity.effect_id != effect_id {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "requested effect id",
            },
        ));
    }
    identity
        .enforce_epochs(origin.authority_epoch, target.authority_epoch)
        .map_err(EffectCoordinatorError::Contract)?;
    validate_binding(&identity.source_watermark, origin, "source")?;
    validate_binding(&identity.target_watermark, target, "target")
}

fn validate_binding<O, T>(
    watermark: &tracedecay_store::ShardWatermarkV1,
    binding: &StoreRuntimeBindingV1,
    side: &'static str,
) -> Result<(), EffectCoordinatorError<O, T>> {
    if watermark.shard_id != binding.shard_id {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::ShardMismatch { field: side },
        ));
    }
    if watermark.incarnation != binding.incarnation {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::EffectIncarnationMismatch { side },
        ));
    }
    if watermark.authority_epoch != binding.authority_epoch {
        return Err(EffectCoordinatorError::Contract(
            StorageRuntimeContractErrorV1::EffectEpochMismatch { side },
        ));
    }
    Ok(())
}

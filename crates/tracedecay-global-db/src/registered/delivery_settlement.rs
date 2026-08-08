use super::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
use tracedecay_domain::{
    CoverageStateV1, DeliveryDropReasonV1, DeliveryEventClassV1, DeliverySettlementAttemptV1,
    DeliverySettlementCensusV1, DeliverySettlementOutcomeV1, DeliverySettlementV1,
    DeliverySurfaceFamilyV1, UtcMicros, WorkAttemptIdentityV1, canonical_sha256,
};

/// One Work attempt can have at most this many separately addressed delivery
/// fan-outs. The recipient bound is enforced per fan-out by the domain; this
/// independent bound keeps the Work-attempt join enumerable without parsing
/// opaque event names.
pub const MAX_WORK_ATTEMPT_DELIVERY_FANOUTS_V1: usize = 64;
pub const MAX_PENDING_RECEIPTED_DELIVERIES_V1: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryAttemptClaimV1 {
    Claimed,
    ReplayedAttempt,
    AlreadySettled(DeliverySettlementV1),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableDeliverySettlementReceiptV1 {
    pub settlement: DeliverySettlementV1,
    pub census: DeliverySettlementCensusV1,
    pub replayed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliverySourceReceiptReadV1 {
    Pending(DeliverySettlementAttemptV1),
    Settled(DeliverySettlementV1),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingDeliverySourceReceiptV1 {
    pub receipt_ref: String,
    pub attempt: DeliverySettlementAttemptV1,
}

/// Exact, bounded delivery evidence for one typed Work attempt.
///
/// `Unbound` only says this delivery authority has no typed binding. Consumers
/// must retain `Unknown` coverage rather than treating it as no delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkAttemptDeliveryCensusReadV1 {
    Unbound,
    Bounded(Vec<DeliverySettlementCensusV1>),
    ExceededBound { observed_at_least: usize },
}

impl RegisteredGlobalDb {
    /// Reads the current census of every delivery fan-out that was explicitly
    /// bound to `work_attempt`. The query is authorization-scoped by project,
    /// keyed by a canonical identity digest, and capped before any data leaves
    /// the reader. It never derives a Work identity from `owner_event_id`.
    pub async fn work_attempt_delivery_censuses(
        &self,
        project_id: &str,
        work_attempt: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptDeliveryCensusReadV1, String> {
        if project_id.is_empty() {
            return Err("invalid delivery project".to_owned());
        }
        let digest = work_attempt_binding_digest(work_attempt)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin Work delivery census snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT f.owner_event_id, f.surface, f.event_class, f.eligible,
                        f.valid_at_micros, f.work_attempt_json, f.work_attempt_digest,
                        COUNT(s.channel_ref),
                        SUM(CASE WHEN s.outcome = 'delivered' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN s.outcome = 'deduplicated' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN s.outcome = 'dropped' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN s.outcome IS NULL THEN 1 ELSE 0 END),
                        MAX(COALESCE(s.settled_at_micros, s.attempted_at_micros))
                 FROM delivery_fanout_events f
                 JOIN delivery_settlements s
                   ON s.project_id = f.project_id
                  AND s.owner_event_id = f.owner_event_id
                  AND s.surface = f.surface
                 WHERE f.project_id = ?1 AND f.work_attempt_digest = ?2
                 GROUP BY f.project_id, f.owner_event_id, f.surface
                 ORDER BY f.owner_event_id, f.surface
                 LIMIT ?3",
                tracedecay_runtime_core::db::engine::params![
                    project_id,
                    digest.as_str(),
                    i64::try_from(MAX_WORK_ATTEMPT_DELIVERY_FANOUTS_V1 + 1)
                        .map_err(|_| "invalid Work delivery fan-out bound")?,
                ],
            )
            .await
            .map_err(|error| format!("failed to query Work delivery censuses: {error}"))?;
        let mut censuses = Vec::with_capacity(MAX_WORK_ATTEMPT_DELIVERY_FANOUTS_V1 + 1);
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read Work delivery census: {error}"))?
        {
            censuses.push(decode_work_attempt_delivery_census(
                &row,
                work_attempt,
                &digest,
            )?);
        }
        if censuses.len() > MAX_WORK_ATTEMPT_DELIVERY_FANOUTS_V1 {
            return Ok(WorkAttemptDeliveryCensusReadV1::ExceededBound {
                observed_at_least: censuses.len(),
            });
        }
        if censuses.is_empty() {
            Ok(WorkAttemptDeliveryCensusReadV1::Unbound)
        } else {
            Ok(WorkAttemptDeliveryCensusReadV1::Bounded(censuses))
        }
    }

    /// Durably records one concrete recipient at the completion boundary that
    /// observed it. Callers choose whether that boundary is pre-write or
    /// post-hoc and must describe their timing truthfully.
    pub async fn begin_delivery_attempt(
        &self,
        project_id: &str,
        attempt: &DeliverySettlementAttemptV1,
    ) -> Result<DeliveryAttemptClaimV1, String> {
        self.begin_delivery_attempt_inner(project_id, attempt, None)
            .await
    }

    /// Durably binds an opaque source acknowledgement token to the exact
    /// admitted recipient in the same transaction as attempt admission.
    pub async fn begin_receipted_delivery_attempt(
        &self,
        project_id: &str,
        attempt: &DeliverySettlementAttemptV1,
        source_receipt_ref: &str,
    ) -> Result<DeliveryAttemptClaimV1, String> {
        validate_source_receipt_ref(source_receipt_ref)?;
        self.begin_delivery_attempt_inner(project_id, attempt, Some(source_receipt_ref))
            .await
    }

    async fn begin_delivery_attempt_inner(
        &self,
        project_id: &str,
        attempt: &DeliverySettlementAttemptV1,
        source_receipt_ref: Option<&str>,
    ) -> Result<DeliveryAttemptClaimV1, String> {
        validate_project_and_attempt(project_id, attempt)?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin delivery attempt transaction: {error}"))?;
        ensure_fanout_identity(&transaction, project_id, attempt).await?;

        if let Some(stored) = read_delivery_attempt(&transaction, project_id, attempt).await? {
            if stored.attempt != *attempt {
                return Err("delivery attempt identity conflict".to_owned());
            }
            let outcome = if stored.outcome.is_some() {
                DeliveryAttemptClaimV1::AlreadySettled(stored.into_settlement()?)
            } else {
                DeliveryAttemptClaimV1::ReplayedAttempt
            };
            if let Some(source_receipt_ref) = source_receipt_ref {
                bind_source_receipt(&transaction, project_id, attempt, source_receipt_ref).await?;
            }
            transaction
                .commit()
                .await
                .map_err(|error| format!("failed to close delivery attempt replay: {error}"))?;
            return Ok(outcome);
        }

        let existing = count_attempts(&transaction, project_id, attempt).await?;
        if existing >= u64::from(attempt.eligible) {
            return Err("delivery attempt exceeds eligible recipients".to_owned());
        }
        transaction
            .execute(
                "INSERT INTO delivery_settlements
                    (project_id, owner_event_id, surface, channel_ref, attempted_at_micros,
                     outcome, settled_at_micros, drop_reason, census_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL, NULL)",
                tracedecay_runtime_core::db::engine::params![
                    project_id,
                    attempt.owner_event_id.as_str(),
                    surface_name(attempt.channel.surface),
                    attempt.channel.channel_ref.as_str(),
                    attempt.attempted_at.0,
                ],
            )
            .await
            .map_err(|error| format!("failed to record delivery attempt: {error}"))?;
        if let Some(source_receipt_ref) = source_receipt_ref {
            bind_source_receipt(&transaction, project_id, attempt, source_receipt_ref).await?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit delivery attempt: {error}"))?;
        Ok(DeliveryAttemptClaimV1::Claimed)
    }

    /// Resolves a project-scoped source acknowledgement token to the exact
    /// durable attempt admitted for it. Unknown tokens remain a typed absence.
    pub async fn delivery_attempt_for_source_receipt(
        &self,
        project_id: &str,
        source_receipt_ref: &str,
    ) -> Result<Option<DeliverySourceReceiptReadV1>, String> {
        if project_id.is_empty() {
            return Err("invalid delivery project".to_owned());
        }
        validate_source_receipt_ref(source_receipt_ref)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| format!("failed to begin delivery receipt snapshot: {error}"))?;
        let mut rows = snapshot
            .query(
                "SELECT r.owner_event_id, r.surface, r.channel_ref,
                        s.attempted_at_micros, f.event_class, f.eligible,
                        f.valid_at_micros, f.work_attempt_json, f.work_attempt_digest,
                        s.outcome, s.settled_at_micros, s.drop_reason
                 FROM delivery_source_receipts r
                 JOIN delivery_settlements s
                   ON s.project_id = r.project_id
                  AND s.owner_event_id = r.owner_event_id
                  AND s.surface = r.surface
                  AND s.channel_ref = r.channel_ref
                 JOIN delivery_fanout_events f
                   ON f.project_id = r.project_id
                  AND f.owner_event_id = r.owner_event_id
                  AND f.surface = r.surface
                 WHERE r.project_id = ?1 AND r.receipt_ref = ?2
                 LIMIT 1",
                tracedecay_runtime_core::db::engine::params![project_id, source_receipt_ref],
            )
            .await
            .map_err(|error| format!("failed to query delivery source receipt: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read delivery source receipt: {error}"))?
        else {
            return Ok(None);
        };
        let attempt = decode_source_receipt_attempt(&row, 0)?;
        let outcome: Option<String> = row
            .get(9)
            .map_err(|error| format!("failed to decode delivery receipt outcome: {error}"))?;
        let Some(outcome) = outcome else {
            return Ok(Some(DeliverySourceReceiptReadV1::Pending(attempt)));
        };
        let settlement = DeliverySettlementV1 {
            attempt,
            outcome: parse_outcome(&outcome)?,
            settled_at: UtcMicros(
                row.get::<Option<i64>>(10)
                    .map_err(|error| {
                        format!("failed to decode delivery receipt settlement time: {error}")
                    })?
                    .ok_or_else(|| "delivery receipt settlement time is missing".to_owned())?,
            ),
            drop_reason: row
                .get::<Option<String>>(11)
                .map_err(|error| format!("failed to decode delivery receipt drop reason: {error}"))?
                .as_deref()
                .map(parse_drop_reason)
                .transpose()?,
        };
        settlement
            .validate()
            .map_err(|error| format!("invalid stored delivery receipt settlement: {error}"))?;
        Ok(Some(DeliverySourceReceiptReadV1::Settled(settlement)))
    }

    /// Reads one bounded due page of pending opaque source receipts. This is
    /// an indexed deadline scan; callers advance durable state by settling the
    /// returned exact attempts and may repeat until the page is empty.
    pub async fn pending_receipted_delivery_attempts_due(
        &self,
        project_id: &str,
        surface: DeliverySurfaceFamilyV1,
        attempted_at_through: UtcMicros,
        limit: usize,
    ) -> Result<Vec<PendingDeliverySourceReceiptV1>, String> {
        if project_id.is_empty()
            || attempted_at_through.0 <= 0
            || limit == 0
            || limit > MAX_PENDING_RECEIPTED_DELIVERIES_V1
        {
            return Err("invalid pending delivery receipt query".to_owned());
        }
        let snapshot = self.read_snapshot().await.map_err(|error| {
            format!("failed to begin pending delivery receipt snapshot: {error}")
        })?;
        let mut rows = snapshot
            .query(
                "SELECT r.receipt_ref, r.owner_event_id, r.surface, r.channel_ref,
                        s.attempted_at_micros, f.event_class, f.eligible,
                        f.valid_at_micros, f.work_attempt_json, f.work_attempt_digest
                 FROM delivery_settlements s
                 JOIN delivery_source_receipts r
                   ON r.project_id = s.project_id
                  AND r.owner_event_id = s.owner_event_id
                  AND r.surface = s.surface
                  AND r.channel_ref = s.channel_ref
                 JOIN delivery_fanout_events f
                   ON f.project_id = s.project_id
                  AND f.owner_event_id = s.owner_event_id
                  AND f.surface = s.surface
                 WHERE s.project_id = ?1 AND s.surface = ?2
                   AND s.outcome IS NULL AND s.attempted_at_micros <= ?3
                 ORDER BY s.attempted_at_micros, r.receipt_ref
                 LIMIT ?4",
                tracedecay_runtime_core::db::engine::params![
                    project_id,
                    surface_name(surface),
                    attempted_at_through.0,
                    i64::try_from(limit).map_err(|_| "invalid pending delivery receipt limit")?,
                ],
            )
            .await
            .map_err(|error| format!("failed to query pending delivery receipts: {error}"))?;
        let mut pending = Vec::with_capacity(limit);
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("failed to read pending delivery receipt: {error}"))?
        {
            let receipt_ref: String = row
                .get(0)
                .map_err(|error| format!("failed to decode delivery receipt reference: {error}"))?;
            validate_source_receipt_ref(&receipt_ref)?;
            pending.push(PendingDeliverySourceReceiptV1 {
                receipt_ref,
                attempt: decode_source_receipt_attempt(&row, 1)?,
            });
        }
        Ok(pending)
    }

    /// CASes an admitted recipient to one immutable terminal outcome and
    /// returns the exact bounded surface census from the same transaction.
    pub async fn settle_delivery_attempt(
        &self,
        project_id: &str,
        settlement: &DeliverySettlementV1,
    ) -> Result<DurableDeliverySettlementReceiptV1, String> {
        if project_id.is_empty() {
            return Err("invalid delivery project".to_owned());
        }
        settlement
            .validate()
            .map_err(|error| format!("invalid delivery settlement: {error}"))?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin delivery settlement transaction: {error}"))?;
        ensure_fanout_identity(&transaction, project_id, &settlement.attempt).await?;
        let stored = read_delivery_attempt(&transaction, project_id, &settlement.attempt)
            .await?
            .ok_or_else(|| "delivery attempt is unavailable".to_owned())?;
        if stored.attempt != settlement.attempt {
            return Err("delivery attempt identity conflict".to_owned());
        }
        let (replayed, census) = if stored.outcome.is_some() {
            let census = stored
                .census
                .clone()
                .ok_or_else(|| "delivery settlement census is missing".to_owned())?;
            if stored.into_settlement()? != *settlement {
                return Err("delivery settlement conflict".to_owned());
            }
            (true, census)
        } else {
            let census = read_pending_delivery_census(
                &transaction,
                project_id,
                &settlement.attempt,
                settlement,
            )
            .await?;
            let census_json = serde_json::to_string(&census)
                .map_err(|error| format!("failed to serialize delivery census: {error}"))?;
            transaction
                .execute(
                    "UPDATE delivery_settlements
                     SET outcome = ?5, settled_at_micros = ?6,
                         drop_reason = ?7, census_json = ?8
                     WHERE project_id = ?1 AND owner_event_id = ?2 AND surface = ?3
                       AND channel_ref = ?4 AND outcome IS NULL",
                    tracedecay_runtime_core::db::engine::params![
                        project_id,
                        settlement.attempt.owner_event_id.as_str(),
                        surface_name(settlement.attempt.channel.surface),
                        settlement.attempt.channel.channel_ref.as_str(),
                        outcome_name(settlement.outcome),
                        settlement.settled_at.0,
                        settlement.drop_reason.map(drop_reason_name),
                        census_json,
                    ],
                )
                .await
                .map_err(|error| format!("failed to settle delivery attempt: {error}"))?;
            (false, census)
        };
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit delivery settlement: {error}"))?;
        Ok(DurableDeliverySettlementReceiptV1 {
            settlement: settlement.clone(),
            census,
            replayed,
        })
    }
}

fn decode_source_receipt_attempt(
    row: &tracedecay_runtime_core::db::engine::Row,
    offset: i32,
) -> Result<DeliverySettlementAttemptV1, String> {
    let surface: String = row
        .get(offset + 1)
        .map_err(|error| format!("failed to decode delivery receipt surface: {error}"))?;
    let work_attempt = decode_stored_work_attempt_binding(
        row.get(offset + 7)
            .map_err(|error| format!("failed to decode delivery receipt Work binding: {error}"))?,
        row.get(offset + 8).map_err(|error| {
            format!("failed to decode delivery receipt Work binding digest: {error}")
        })?,
    )?;
    let eligible: i64 = row
        .get(offset + 5)
        .map_err(|error| format!("failed to decode delivery receipt eligibility: {error}"))?;
    let attempt =
        DeliverySettlementAttemptV1 {
            owner_event_id: row
                .get(offset)
                .map_err(|error| format!("failed to decode delivery receipt owner: {error}"))?,
            event_class: parse_event_class(&row.get::<String>(offset + 4).map_err(|error| {
                format!("failed to decode delivery receipt event class: {error}")
            })?)?,
            channel: tracedecay_domain::DeliveryChannelIdentityV1 {
                surface: parse_surface(&surface)?,
                channel_ref: row.get(offset + 2).map_err(|error| {
                    format!("failed to decode delivery receipt channel: {error}")
                })?,
            },
            work_attempt,
            eligible: u16::try_from(eligible)
                .map_err(|_| "invalid delivery receipt eligibility".to_owned())?,
            valid_at: UtcMicros(
                row.get(offset + 6).map_err(|error| {
                    format!("failed to decode delivery receipt validity: {error}")
                })?,
            ),
            attempted_at: UtcMicros(row.get(offset + 3).map_err(|error| {
                format!("failed to decode delivery receipt attempt time: {error}")
            })?),
        };
    attempt
        .validate()
        .map_err(|error| format!("invalid stored delivery receipt attempt: {error}"))?;
    Ok(attempt)
}

fn validate_project_and_attempt(
    project_id: &str,
    attempt: &DeliverySettlementAttemptV1,
) -> Result<(), String> {
    if project_id.is_empty() {
        return Err("invalid delivery project".to_owned());
    }
    attempt
        .validate()
        .map_err(|error| format!("invalid delivery attempt: {error}"))
}

fn validate_source_receipt_ref(source_receipt_ref: &str) -> Result<(), String> {
    if source_receipt_ref.is_empty()
        || source_receipt_ref.len() > 256
        || !source_receipt_ref
            .bytes()
            .all(|byte| byte.is_ascii_graphic())
    {
        return Err("invalid delivery source receipt".to_owned());
    }
    Ok(())
}

async fn bind_source_receipt(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    project_id: &str,
    attempt: &DeliverySettlementAttemptV1,
    source_receipt_ref: &str,
) -> Result<(), String> {
    let surface = surface_name(attempt.channel.surface);
    let mut by_receipt = transaction
        .query(
            "SELECT owner_event_id, surface, channel_ref
             FROM delivery_source_receipts
             WHERE project_id = ?1 AND receipt_ref = ?2
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![project_id, source_receipt_ref],
        )
        .await
        .map_err(|error| format!("failed to read delivery source receipt binding: {error}"))?;
    if let Some(row) = by_receipt
        .next()
        .await
        .map_err(|error| format!("failed to decode delivery source receipt binding: {error}"))?
    {
        let owner_event_id: String = row
            .get(0)
            .map_err(|error| format!("failed to decode delivery receipt owner: {error}"))?;
        let stored_surface: String = row
            .get(1)
            .map_err(|error| format!("failed to decode delivery receipt surface: {error}"))?;
        let channel_ref: String = row
            .get(2)
            .map_err(|error| format!("failed to decode delivery receipt channel: {error}"))?;
        if owner_event_id == attempt.owner_event_id
            && stored_surface == surface
            && channel_ref == attempt.channel.channel_ref
        {
            return Ok(());
        }
        return Err("delivery source receipt identity conflict".to_owned());
    }
    drop(by_receipt);

    let mut by_attempt = transaction
        .query(
            "SELECT receipt_ref
             FROM delivery_source_receipts
             WHERE project_id = ?1 AND owner_event_id = ?2
               AND surface = ?3 AND channel_ref = ?4
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![
                project_id,
                attempt.owner_event_id.as_str(),
                surface,
                attempt.channel.channel_ref.as_str(),
            ],
        )
        .await
        .map_err(|error| format!("failed to read delivery attempt receipt binding: {error}"))?;
    if let Some(row) = by_attempt
        .next()
        .await
        .map_err(|error| format!("failed to decode delivery attempt receipt binding: {error}"))?
    {
        let stored_receipt_ref: String = row
            .get(0)
            .map_err(|error| format!("failed to decode delivery receipt reference: {error}"))?;
        if stored_receipt_ref == source_receipt_ref {
            return Ok(());
        }
        return Err("delivery attempt source receipt conflict".to_owned());
    }
    drop(by_attempt);

    transaction
        .execute(
            "INSERT INTO delivery_source_receipts
                (project_id, receipt_ref, owner_event_id, surface, channel_ref)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            tracedecay_runtime_core::db::engine::params![
                project_id,
                source_receipt_ref,
                attempt.owner_event_id.as_str(),
                surface,
                attempt.channel.channel_ref.as_str(),
            ],
        )
        .await
        .map_err(|error| format!("failed to bind delivery source receipt: {error}"))?;
    Ok(())
}

async fn ensure_fanout_identity(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    project_id: &str,
    attempt: &DeliverySettlementAttemptV1,
) -> Result<(), String> {
    let surface = surface_name(attempt.channel.surface);
    let mut rows = transaction
        .query(
            "SELECT event_class, eligible, valid_at_micros,
                    work_attempt_json, work_attempt_digest
             FROM delivery_fanout_events
             WHERE project_id = ?1 AND owner_event_id = ?2 AND surface = ?3
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![
                project_id,
                attempt.owner_event_id.as_str(),
                surface,
            ],
        )
        .await
        .map_err(|error| format!("failed to read delivery fanout identity: {error}"))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("failed to decode delivery fanout identity: {error}"))?
    {
        let event_class: String = row
            .get(0)
            .map_err(|error| format!("failed to decode delivery event class: {error}"))?;
        let eligible: i64 = row
            .get(1)
            .map_err(|error| format!("failed to decode delivery eligibility: {error}"))?;
        let valid_at: i64 = row
            .get(2)
            .map_err(|error| format!("failed to decode delivery validity: {error}"))?;
        let work_attempt = decode_stored_work_attempt_binding(
            row.get(3)
                .map_err(|error| format!("failed to decode Work delivery binding: {error}"))?,
            row.get(4).map_err(|error| {
                format!("failed to decode Work delivery binding digest: {error}")
            })?,
        )?;
        if event_class != event_class_name(attempt.event_class)
            || eligible != i64::from(attempt.eligible)
            || valid_at != attempt.valid_at.0
            || work_attempt != attempt.work_attempt
        {
            return Err("delivery fanout identity conflict".to_owned());
        }
        return Ok(());
    }
    transaction
        .execute(
            "INSERT INTO delivery_fanout_events
                (project_id, owner_event_id, surface, event_class, eligible, valid_at_micros,
                 work_attempt_json, work_attempt_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            tracedecay_runtime_core::db::engine::params![
                project_id,
                attempt.owner_event_id.as_str(),
                surface,
                event_class_name(attempt.event_class),
                i64::from(attempt.eligible),
                attempt.valid_at.0,
                attempt
                    .work_attempt
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|error| format!("failed to encode Work delivery binding: {error}"))?,
                attempt
                    .work_attempt
                    .as_ref()
                    .map(work_attempt_binding_digest)
                    .transpose()?
                    .map(|digest| digest.as_str().to_owned()),
            ],
        )
        .await
        .map_err(|error| format!("failed to record delivery fanout identity: {error}"))?;
    Ok(())
}

struct StoredDeliveryAttempt {
    attempt: DeliverySettlementAttemptV1,
    outcome: Option<DeliverySettlementOutcomeV1>,
    settled_at: Option<UtcMicros>,
    drop_reason: Option<DeliveryDropReasonV1>,
    census: Option<DeliverySettlementCensusV1>,
}

impl StoredDeliveryAttempt {
    fn into_settlement(self) -> Result<DeliverySettlementV1, String> {
        let settlement = DeliverySettlementV1 {
            attempt: self.attempt,
            outcome: self
                .outcome
                .ok_or_else(|| "delivery attempt is not settled".to_owned())?,
            settled_at: self
                .settled_at
                .ok_or_else(|| "delivery settlement timestamp is missing".to_owned())?,
            drop_reason: self.drop_reason,
        };
        settlement
            .validate()
            .map_err(|error| format!("invalid stored delivery settlement: {error}"))?;
        Ok(settlement)
    }
}

async fn read_delivery_attempt(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    project_id: &str,
    attempt: &DeliverySettlementAttemptV1,
) -> Result<Option<StoredDeliveryAttempt>, String> {
    let mut rows = transaction
        .query(
            "SELECT attempted_at_micros, outcome, settled_at_micros, drop_reason, census_json
             FROM delivery_settlements
             WHERE project_id = ?1 AND owner_event_id = ?2 AND surface = ?3
               AND channel_ref = ?4
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![
                project_id,
                attempt.owner_event_id.as_str(),
                surface_name(attempt.channel.surface),
                attempt.channel.channel_ref.as_str(),
            ],
        )
        .await
        .map_err(|error| format!("failed to read delivery attempt: {error}"))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("failed to decode delivery attempt: {error}"))?
    else {
        return Ok(None);
    };
    let attempted_at: i64 = row
        .get(0)
        .map_err(|error| format!("failed to decode delivery attempt time: {error}"))?;
    let outcome: Option<String> = row
        .get(1)
        .map_err(|error| format!("failed to decode delivery outcome: {error}"))?;
    let settled_at: Option<i64> = row
        .get(2)
        .map_err(|error| format!("failed to decode delivery settlement time: {error}"))?;
    let drop_reason: Option<String> = row
        .get(3)
        .map_err(|error| format!("failed to decode delivery drop reason: {error}"))?;
    let census_json: Option<String> = row
        .get(4)
        .map_err(|error| format!("failed to decode delivery census: {error}"))?;
    let mut stored_attempt = attempt.clone();
    stored_attempt.attempted_at = UtcMicros(attempted_at);
    Ok(Some(StoredDeliveryAttempt {
        attempt: stored_attempt,
        outcome: outcome.as_deref().map(parse_outcome).transpose()?,
        settled_at: settled_at.map(UtcMicros),
        drop_reason: drop_reason.as_deref().map(parse_drop_reason).transpose()?,
        census: census_json
            .map(|value| {
                serde_json::from_str::<DeliverySettlementCensusV1>(&value)
                    .map_err(|error| format!("failed to deserialize delivery census: {error}"))
            })
            .transpose()?,
    }))
}

async fn count_attempts(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    project_id: &str,
    attempt: &DeliverySettlementAttemptV1,
) -> Result<u64, String> {
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM delivery_settlements
             WHERE project_id = ?1 AND owner_event_id = ?2 AND surface = ?3",
            tracedecay_runtime_core::db::engine::params![
                project_id,
                attempt.owner_event_id.as_str(),
                surface_name(attempt.channel.surface),
            ],
        )
        .await
        .map_err(|error| format!("failed to count delivery attempts: {error}"))?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("failed to read delivery attempt count: {error}"))?
        .ok_or_else(|| "delivery attempt count is unavailable".to_owned())?;
    row.get::<i64>(0)
        .map_err(|error| format!("failed to decode delivery attempt count: {error}"))
        .and_then(|count| {
            u64::try_from(count).map_err(|_| "invalid delivery attempt count".to_owned())
        })
}

async fn read_pending_delivery_census(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    project_id: &str,
    attempt: &DeliverySettlementAttemptV1,
    settlement: &DeliverySettlementV1,
) -> Result<DeliverySettlementCensusV1, String> {
    let mut rows = transaction
        .query(
            "SELECT COUNT(*),
                    SUM(CASE WHEN outcome = 'delivered' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN outcome = 'deduplicated' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN outcome = 'dropped' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN outcome IS NULL THEN 1 ELSE 0 END)
             FROM delivery_settlements
             WHERE project_id = ?1 AND owner_event_id = ?2 AND surface = ?3",
            tracedecay_runtime_core::db::engine::params![
                project_id,
                attempt.owner_event_id.as_str(),
                surface_name(attempt.channel.surface),
            ],
        )
        .await
        .map_err(|error| format!("failed to read delivery census: {error}"))?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("failed to decode delivery census: {error}"))?
        .ok_or_else(|| "delivery census is unavailable".to_owned())?;
    let attempted = decode_count(&row, 0, "attempted")?;
    let mut delivered = decode_count(&row, 1, "delivered")?;
    let mut deduplicated = decode_count(&row, 2, "deduplicated")?;
    let mut dropped = decode_count(&row, 3, "dropped")?;
    let unknown = decode_count(&row, 4, "unknown")?
        .checked_sub(1)
        .ok_or_else(|| "delivery settlement has no pending attempt".to_owned())?;
    match settlement.outcome {
        DeliverySettlementOutcomeV1::Delivered => delivered = delivered.saturating_add(1),
        DeliverySettlementOutcomeV1::Deduplicated => deduplicated = deduplicated.saturating_add(1),
        DeliverySettlementOutcomeV1::Dropped => dropped = dropped.saturating_add(1),
    }
    let coverage = if attempted == attempt.eligible && unknown == 0 {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let census = DeliverySettlementCensusV1 {
        owner_event_id: attempt.owner_event_id.clone(),
        event_class: attempt.event_class,
        surface: attempt.channel.surface,
        work_attempt: attempt.work_attempt.clone(),
        eligible: attempt.eligible,
        attempted,
        delivered,
        deduplicated,
        dropped,
        unknown,
        valid_at: attempt.valid_at,
        settled_at: settlement.settled_at,
        coverage,
    };
    census
        .validate()
        .map_err(|error| format!("invalid delivery census: {error}"))?;
    Ok(census)
}

fn work_attempt_binding_digest(
    work_attempt: &WorkAttemptIdentityV1,
) -> Result<tracedecay_domain::ManifestDigest, String> {
    canonical_sha256(&(
        "tracedecay.global-db.delivery-work-attempt-binding.v1",
        work_attempt,
    ))
    .map_err(|error| format!("failed to digest Work delivery binding: {error}"))
}

fn decode_stored_work_attempt_binding(
    payload: Option<String>,
    digest: Option<String>,
) -> Result<Option<WorkAttemptIdentityV1>, String> {
    match (payload, digest) {
        (None, None) => Ok(None),
        (Some(payload), Some(digest)) => {
            let work_attempt = serde_json::from_str::<WorkAttemptIdentityV1>(&payload)
                .map_err(|error| format!("failed to decode Work delivery binding: {error}"))?;
            if work_attempt_binding_digest(&work_attempt)?.as_str() != digest {
                return Err("Work delivery binding digest mismatch".to_owned());
            }
            Ok(Some(work_attempt))
        }
        _ => Err("incomplete Work delivery binding".to_owned()),
    }
}

fn decode_work_attempt_delivery_census(
    row: &tracedecay_runtime_core::db::engine::Row,
    expected_attempt: &WorkAttemptIdentityV1,
    expected_digest: &tracedecay_domain::ManifestDigest,
) -> Result<DeliverySettlementCensusV1, String> {
    let owner_event_id: String = row
        .get(0)
        .map_err(|error| format!("failed to decode Work delivery owner event: {error}"))?;
    let surface = parse_surface(
        &row.get::<String>(1)
            .map_err(|error| format!("failed to decode Work delivery surface: {error}"))?,
    )?;
    let event_class = parse_event_class(
        &row.get::<String>(2)
            .map_err(|error| format!("failed to decode Work delivery event class: {error}"))?,
    )?;
    let eligible = decode_count(row, 3, "eligible")?;
    let valid_at = UtcMicros(
        row.get(4)
            .map_err(|error| format!("failed to decode Work delivery validity: {error}"))?,
    );
    let stored_digest: String = row
        .get(6)
        .map_err(|error| format!("failed to decode Work delivery binding digest: {error}"))?;
    if stored_digest != expected_digest.as_str() {
        return Err("Work delivery binding digest changed during scan".to_owned());
    }
    let work_attempt = decode_stored_work_attempt_binding(
        row.get(5)
            .map_err(|error| format!("failed to decode Work delivery binding: {error}"))?,
        Some(stored_digest),
    )?
    .ok_or_else(|| "Work delivery binding is missing".to_owned())?;
    if &work_attempt != expected_attempt {
        return Err("Work delivery binding changed during scan".to_owned());
    }
    let attempted = decode_count(row, 7, "attempted")?;
    let delivered = decode_count(row, 8, "delivered")?;
    let deduplicated = decode_count(row, 9, "deduplicated")?;
    let dropped = decode_count(row, 10, "dropped")?;
    let unknown = decode_count(row, 11, "unknown")?;
    let settled_at =
        UtcMicros(row.get(12).map_err(|error| {
            format!("failed to decode Work delivery observation time: {error}")
        })?);
    let coverage = if attempted == eligible && unknown == 0 {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let census = DeliverySettlementCensusV1 {
        owner_event_id,
        event_class,
        surface,
        work_attempt: Some(work_attempt),
        eligible,
        attempted,
        delivered,
        deduplicated,
        dropped,
        unknown,
        valid_at,
        settled_at,
        coverage,
    };
    census
        .validate()
        .map_err(|error| format!("invalid Work delivery census: {error}"))?;
    Ok(census)
}

fn decode_count(
    row: &tracedecay_runtime_core::db::engine::Row,
    index: i32,
    label: &str,
) -> Result<u16, String> {
    let count: i64 = row
        .get(index)
        .map_err(|error| format!("failed to decode {label} delivery count: {error}"))?;
    u16::try_from(count).map_err(|_| format!("invalid {label} delivery count"))
}

const fn surface_name(surface: DeliverySurfaceFamilyV1) -> &'static str {
    match surface {
        DeliverySurfaceFamilyV1::Hook => "hook",
        DeliverySurfaceFamilyV1::Mcp => "mcp",
        DeliverySurfaceFamilyV1::Lsp => "lsp",
        DeliverySurfaceFamilyV1::Dashboard => "dashboard",
        DeliverySurfaceFamilyV1::Cli => "cli",
        DeliverySurfaceFamilyV1::Other => "other",
    }
}

fn parse_surface(value: &str) -> Result<DeliverySurfaceFamilyV1, String> {
    match value {
        "hook" => Ok(DeliverySurfaceFamilyV1::Hook),
        "mcp" => Ok(DeliverySurfaceFamilyV1::Mcp),
        "lsp" => Ok(DeliverySurfaceFamilyV1::Lsp),
        "dashboard" => Ok(DeliverySurfaceFamilyV1::Dashboard),
        "cli" => Ok(DeliverySurfaceFamilyV1::Cli),
        "other" => Ok(DeliverySurfaceFamilyV1::Other),
        _ => Err("invalid stored delivery surface".to_owned()),
    }
}

const fn event_class_name(event_class: DeliveryEventClassV1) -> &'static str {
    match event_class {
        DeliveryEventClassV1::OperationAccepted => "operation_accepted",
        DeliveryEventClassV1::OperationProgress => "operation_progress",
        DeliveryEventClassV1::OperationTerminal => "operation_terminal",
        DeliveryEventClassV1::Diagnostic => "diagnostic",
        DeliveryEventClassV1::Activity => "activity",
        DeliveryEventClassV1::Other => "other",
    }
}

fn parse_event_class(value: &str) -> Result<DeliveryEventClassV1, String> {
    match value {
        "operation_accepted" => Ok(DeliveryEventClassV1::OperationAccepted),
        "operation_progress" => Ok(DeliveryEventClassV1::OperationProgress),
        "operation_terminal" => Ok(DeliveryEventClassV1::OperationTerminal),
        "diagnostic" => Ok(DeliveryEventClassV1::Diagnostic),
        "activity" => Ok(DeliveryEventClassV1::Activity),
        "other" => Ok(DeliveryEventClassV1::Other),
        _ => Err("invalid stored delivery event class".to_owned()),
    }
}

const fn outcome_name(outcome: DeliverySettlementOutcomeV1) -> &'static str {
    match outcome {
        DeliverySettlementOutcomeV1::Delivered => "delivered",
        DeliverySettlementOutcomeV1::Deduplicated => "deduplicated",
        DeliverySettlementOutcomeV1::Dropped => "dropped",
    }
}

fn parse_outcome(value: &str) -> Result<DeliverySettlementOutcomeV1, String> {
    match value {
        "delivered" => Ok(DeliverySettlementOutcomeV1::Delivered),
        "deduplicated" => Ok(DeliverySettlementOutcomeV1::Deduplicated),
        "dropped" => Ok(DeliverySettlementOutcomeV1::Dropped),
        _ => Err("invalid stored delivery outcome".to_owned()),
    }
}

const fn drop_reason_name(reason: DeliveryDropReasonV1) -> &'static str {
    match reason {
        DeliveryDropReasonV1::Backpressure => "backpressure",
        DeliveryDropReasonV1::Cancelled => "cancelled",
        DeliveryDropReasonV1::Deadline => "deadline",
        DeliveryDropReasonV1::Disconnected => "disconnected",
        DeliveryDropReasonV1::Invalid => "invalid",
        DeliveryDropReasonV1::Rejected => "rejected",
        DeliveryDropReasonV1::Unknown => "unknown",
    }
}

fn parse_drop_reason(value: &str) -> Result<DeliveryDropReasonV1, String> {
    match value {
        "backpressure" => Ok(DeliveryDropReasonV1::Backpressure),
        "cancelled" => Ok(DeliveryDropReasonV1::Cancelled),
        "deadline" => Ok(DeliveryDropReasonV1::Deadline),
        "disconnected" => Ok(DeliveryDropReasonV1::Disconnected),
        "invalid" => Ok(DeliveryDropReasonV1::Invalid),
        "rejected" => Ok(DeliveryDropReasonV1::Rejected),
        "unknown" => Ok(DeliveryDropReasonV1::Unknown),
        _ => Err("invalid stored delivery drop reason".to_owned()),
    }
}

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracedecay_application::remote::composition::{
    AuthenticityClaimV1, AuthorizationClaimV1, IntegrityClaimV1, PendingLocalObservationsV1,
    QueryManifestBindingV1, RemoteCompletenessV1, RemoteFreshnessV1, RemoteQueryCompositionV1,
    ShardCoverageStateV1, ShardQueryContributionV1,
};
use tracedecay_application::remote::query::{
    RemoteExactObservationQueryCommandV1, RemoteExactObservationQueryErrorV1,
    RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryReadPortV1,
    RemoteExactObservationResultV1, RemoteQueryCompleteValueV1, RemoteQueryResultV1,
    RemoteSanitizedObservationV1, remote_exact_observation_query_result_contract_v1,
};
use tracedecay_application::{
    ApplicationEnvelope, CancellationSignal, Deadline, EvidenceCoverage, EvidenceDomain,
    EvidencePacket, OperationBudgetUsage, OperationReceipt, PageState, RetrievalEvidence,
    TemporalState,
};
use tracedecay_domain::{CurrentRemoteAuthorityStateV1, UtcMicros};
use tracedecay_store::{
    ConsistencyModeV1, ObservationReadOperationV1, ObservationReadResultV1, OperationPriorityV1,
    ProjectReadOperationV1, ProjectReadResultV1, RepositoryReadOperationV1, RepositoryReadResultV1,
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1, StoreRuntimeBindingV1,
    StoreShardScopeV1, UnavailableReasonV1,
};
use tracedecay_tool_catalog::SortContractId;

use super::{RemoteQueryAuthoritySnapshotV1, RusqliteRemoteAuthorityStoreV1};
use crate::repository::RepositoryRuntimePhysicalAttachment;

pub struct RusqliteRemoteExactObservationQueryPortV1 {
    authority: Arc<RusqliteRemoteAuthorityStoreV1>,
    repository: Arc<RepositoryRuntimePhysicalAttachment>,
}

impl RusqliteRemoteExactObservationQueryPortV1 {
    pub fn new(
        authority: Arc<RusqliteRemoteAuthorityStoreV1>,
        repository: Arc<RepositoryRuntimePhysicalAttachment>,
    ) -> Self {
        Self {
            authority,
            repository,
        }
    }
}

impl RemoteExactObservationQueryReadPortV1 for RusqliteRemoteExactObservationQueryPortV1 {
    fn read_exact_observation(
        &self,
        command: &RemoteExactObservationQueryCommandV1,
    ) -> Result<RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryErrorV1> {
        if command.cancellation.is_cancelled() {
            return Err(RemoteExactObservationQueryErrorV1::Cancelled);
        }
        if command
            .effective_deadline
            .is_elapsed_at(command.observed_at)
        {
            return Err(RemoteExactObservationQueryErrorV1::DeadlineElapsed);
        }
        let snapshot = self
            .authority
            .query_authority_snapshot(
                &command.repository_scope.project_id,
                &command.repository_scope,
                &command.expected_authority,
                command.observed_at,
            )
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        validate_snapshot(command, &snapshot, &self.repository.binding())?;
        let authority = snapshot.authority.clone();
        let binding = snapshot.binding.clone();
        let frontier = snapshot
            .frontier
            .clone()
            .ok_or(RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        if frontier.shard_id != binding.shard_id
            || frontier.incarnation != binding.incarnation
            || frontier.authority_epoch != binding.authority_epoch
            || frontier.commit_sequence.0 == 0
        {
            return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
        }

        let operation = ObservationReadOperationV1::Observation {
            observation_id: command.observation_id.clone(),
        };
        let admission_bytes = serde_json::to_vec(&operation)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?
            .len();
        let control = runtime_control(command)?;
        let request = RuntimeReadRequestV1::new(
            binding.clone(),
            ConsistencyModeV1::LatestAvailable,
            RuntimeReadOperationV1::Repository {
                op: RepositoryReadOperationV1::Project(ProjectReadOperationV1::Observation(
                    operation,
                )),
            },
            OperationPriorityV1::Foreground,
            u64::try_from(admission_bytes)
                .map_err(|_| RemoteExactObservationQueryErrorV1::BudgetExceeded)?
                .max(1),
            control.clone(),
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        let started = Instant::now();
        let probe = QueryProbe {
            cancellation: control.cancellation,
            deadline: control.deadline,
            application_cancellation: command.cancellation.clone(),
            started,
            maximum_elapsed: Duration::from_micros(command.budget.maximum_elapsed_micros),
            decision: Mutex::new(None),
        };
        let outcome = self
            .repository
            .dispatch_read(request, &probe)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        if elapsed > command.budget.maximum_elapsed_micros {
            return Err(RemoteExactObservationQueryErrorV1::BudgetExceeded);
        }
        match outcome.coverage() {
            RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. } => {}
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::Cancelled,
                ..
            } => return Err(RemoteExactObservationQueryErrorV1::Cancelled),
            RuntimeReadCoverageV1::Unavailable {
                reason: UnavailableReasonV1::DeadlineExceeded,
                ..
            } => return Err(RemoteExactObservationQueryErrorV1::DeadlineElapsed),
            RuntimeReadCoverageV1::Partial { .. }
            | RuntimeReadCoverageV1::Stale { .. }
            | RuntimeReadCoverageV1::Unavailable { .. } => {
                return Err(RemoteExactObservationQueryErrorV1::AuthorityUnavailable);
            }
        }
        let current_snapshot = self
            .authority
            .query_authority_snapshot(
                &command.repository_scope.project_id,
                &command.repository_scope,
                &command.expected_authority,
                command.observed_at,
            )
            .map_err(|_| RemoteExactObservationQueryErrorV1::StaleFence)?;
        validate_snapshot(command, &current_snapshot, &self.repository.binding())?;
        if current_snapshot != snapshot {
            return Err(RemoteExactObservationQueryErrorV1::StaleFence);
        }
        let row = match outcome.value() {
            Some(RuntimeReadResultV1::Repository {
                result: RepositoryReadResultV1::Project(project),
            }) => match project.as_ref() {
                ProjectReadResultV1::Observation(ObservationReadResultV1::Observation(row)) => {
                    row.clone()
                }
                _ => return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch),
            },
            _ => return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch),
        };
        let observation = match *row {
            Some(row) => {
                RemoteExactObservationResultV1::Found(Box::new(RemoteSanitizedObservationV1 {
                    sequence: row.sequence,
                    observation: row.observation,
                    committed_cursor: row.committed_cursor,
                    retrieval_anchor: row.retrieval_anchor,
                    projection_generation: row.projection_generation,
                    repository_provenance: row.repository_provenance.availability().clone(),
                    repository_anchor: row.repository_provenance.anchor().cloned(),
                    projection_queued: row.projection_queued,
                }))
            }
            None => RemoteExactObservationResultV1::NotFound,
        };
        let schema_digest: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&remote_exact_observation_query_result_contract_v1())
                .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
        )
        .into();
        let contribution = ShardQueryContributionV1 {
            manifest: QueryManifestBindingV1 {
                brain_id: command.expected_shard.brain_id.clone(),
                shard_id: command.expected_shard.shard_id.clone(),
                generation_id: command.expected_shard.generation_id.clone(),
                schema_digest,
                watermark_sequence: frontier.commit_sequence.0,
                placement_revision: command.expected_authority.placement_revision.get(),
                authority_epoch: command.expected_authority.authority_epoch.0,
                cache_age_millis: 0,
                cache_lag_commits: 0,
            },
            integrity: IntegrityClaimV1::Verified,
            authenticity: AuthenticityClaimV1::Authenticated,
            freshness: RemoteFreshnessV1::Current,
            completeness: RemoteCompletenessV1::Complete,
            authorization: AuthorizationClaimV1::Authorized,
            coverage: ShardCoverageStateV1::Complete,
            authority_receipt: Some(command.caller_admission.admission.authority().clone()),
            value: Some(RemoteQueryCompleteValueV1 {
                complete_value_present: true,
            }),
            reason_code: None,
        };
        let composition = RemoteQueryCompositionV1::compose(
            BTreeSet::from([command.expected_shard.clone()]),
            vec![contribution],
            PendingLocalObservationsV1 {
                count: 0,
                oldest_age_millis: None,
                has_sequence_gap: false,
                has_quarantined: false,
            },
            1,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        let result = RemoteQueryResultV1 {
            composition,
            observation,
        };
        result
            .validate()
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;

        let ended_at = command
            .observed_at
            .0
            .checked_add(i64::try_from(elapsed).unwrap_or(i64::MAX))
            .map(UtcMicros)
            .ok_or(RemoteExactObservationQueryErrorV1::BudgetExceeded)?;
        if ended_at > command.effective_deadline.expires_at {
            return Err(RemoteExactObservationQueryErrorV1::DeadlineElapsed);
        }
        let result_bytes = serde_json::to_vec(&result)
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?
            .len();
        let measured_bytes = admission_bytes
            .checked_add(result_bytes)
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(RemoteExactObservationQueryErrorV1::BudgetExceeded)?;
        let budget = OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: measured_bytes,
            elapsed_micros: elapsed,
        };
        if budget.bytes_consumed > command.budget.maximum_bytes {
            return Err(RemoteExactObservationQueryErrorV1::BudgetExceeded);
        }
        let evidence = RetrievalEvidence {
            payload: Some(result),
            temporal: TemporalState::current(ended_at),
            evidence_authorities: Vec::new(),
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Anchor], 1, 1, 1)
                .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.remote.exact-observation.v1")
                    .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
                1,
                Some(1),
                1,
            )
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
            finished_at: ended_at,
            budget,
            cancellation: None,
        };
        let execution = OperationReceipt::completed(
            command.observed_at,
            ended_at,
            Deadline::new(command.effective_deadline.expires_at)
                .map_err(|_| RemoteExactObservationQueryErrorV1::DeadlineElapsed)?,
            budget,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        let packet = EvidencePacket::from_retrieval(
            evidence,
            command.caller_admission.admission.authority().clone(),
            execution,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        Ok(RemoteExactObservationQueryOutcomeV1 {
            authority,
            result: ApplicationEnvelope::evidence(
                remote_exact_observation_query_result_contract_v1(),
                command.request_id.clone(),
                command.scope.clone(),
                packet,
            ),
        })
    }
}

fn validate_snapshot(
    command: &RemoteExactObservationQueryCommandV1,
    snapshot: &RemoteQueryAuthoritySnapshotV1,
    repository_binding: &StoreRuntimeBindingV1,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    if snapshot.project_id != command.repository_scope.project_id
        || snapshot.scope != command.repository_scope
        || snapshot.observed_at != command.observed_at
    {
        return Err(RemoteExactObservationQueryErrorV1::ReceiptMismatch);
    }
    let CurrentRemoteAuthorityStateV1::Available(current) = &snapshot.authority else {
        return Err(RemoteExactObservationQueryErrorV1::AuthorityUnavailable);
    };
    if current.observed_at != command.observed_at
        || current.fence != command.expected_authority
        || snapshot.placement_revision != command.expected_authority.placement_revision.get()
        || snapshot.binding != *repository_binding
        || snapshot.binding.authority_epoch.get() != command.expected_authority.authority_epoch.0
        || snapshot.binding.shard_id.brain_id != command.expected_authority.brain_id
        || snapshot.binding.shard_id.scope.project_id()
            != Some(&command.repository_scope.project_id)
        || !matches!(
            &snapshot.binding.shard_id.scope,
            StoreShardScopeV1::ProjectSessions { .. }
        )
    {
        return Err(RemoteExactObservationQueryErrorV1::StaleFence);
    }
    Ok(())
}

fn runtime_control(
    command: &RemoteExactObservationQueryCommandV1,
) -> Result<RuntimeRequestControlV1, RemoteExactObservationQueryErrorV1> {
    let suffix = command
        .observation_id
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(RemoteExactObservationQueryErrorV1::InvalidRequest)?;
    Ok(RuntimeRequestControlV1 {
        requested_at: command.observed_at,
        deadline: RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!(
                "deadline.remote-exact-observation.{suffix}"
            ))
            .map_err(|_| RemoteExactObservationQueryErrorV1::InvalidRequest)?,
        },
        cancellation: RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "cancellation.remote-exact-observation.{suffix}"
            ))
            .map_err(|_| RemoteExactObservationQueryErrorV1::InvalidRequest)?,
            generation: 1,
        },
    })
}

struct QueryProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    application_cancellation: CancellationSignal,
    started: Instant,
    maximum_elapsed: Duration,
    decision: Mutex<Option<RuntimeInterruptionV1>>,
}

impl RuntimeRequestProbeV1 for QueryProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        let mut decision = self
            .decision
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if decision.is_none() {
            *decision = if self.application_cancellation.is_cancelled() {
                Some(RuntimeInterruptionV1::Cancelled)
            } else if self.started.elapsed() >= self.maximum_elapsed {
                Some(RuntimeInterruptionV1::DeadlineExceeded)
            } else {
                None
            };
        }
        *decision
    }
}

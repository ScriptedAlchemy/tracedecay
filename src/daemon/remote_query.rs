//! Exact remote reads over the daemon's already-published project runtime.
//!
//! The adapter receives neither a locator nor a database handle. It resolves
//! the same registered runtime used by replay and recovery, verifies the
//! durable `RemoteNode` authority before and after the read, and dispatches the
//! canonical repository read operation.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracedecay_application::remote::composition::{
    AuthenticityClaimV1, AuthorizationClaimV1, IntegrityClaimV1, PendingLocalEvidenceV1,
    PendingLocalUnavailableReasonV1, QueryManifestBindingV1, RemoteCompletenessV1,
    RemoteFreshnessV1, RemoteQueryCompositionV1, ShardCoverageStateV1, ShardQueryContributionV1,
};
use tracedecay_application::remote::query::{
    RemoteExactObservationQueryCommandV1, RemoteExactObservationQueryErrorV1,
    RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryReadPortV1,
    RemoteExactObservationResultV1, RemoteQueryCompleteValueV1, RemoteQueryResultV1,
    RemoteSanitizedObservationV1, remote_exact_observation_query_result_contract_v1,
};
use tracedecay_application::{
    ApplicationEnvelope, CoverageCompleteness, CoverageDomainState, Deadline, EvidenceCoverage,
    EvidenceDomain, EvidencePacket, OperationBudgetUsage, OperationReceipt, PageState,
    RetrievalEvidence, TemporalState,
};
use tracedecay_domain::{CurrentRemoteAuthorityStateV1, UtcMicros};
use tracedecay_rusqlite_runtime::remote::{RemoteQueryAuthoritySnapshotV1, RemoteSqliteStorageV1};
use tracedecay_store::{
    ConsistencyModeV1, ObservationReadOperationV1, ObservationReadResultV1, OperationPriorityV1,
    ProjectReadOperationV1, ProjectReadResultV1, RepositoryReadOperationV1, RepositoryReadResultV1,
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1, ShardWatermarkV1,
    StoreShardScopeV1, UnavailableReasonV1,
};
use tracedecay_tool_catalog::SortContractId;

use super::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1;

/// Query adapter over one authenticated `RemoteNode` store and the canonical
/// project runtime registry shared with replay/recovery.
pub(crate) struct DaemonRemoteExactObservationQueryPortV1 {
    authority: Arc<RemoteSqliteStorageV1>,
    targets: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
}

impl DaemonRemoteExactObservationQueryPortV1 {
    pub(crate) fn new(
        authority: Arc<RemoteSqliteStorageV1>,
        targets: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
    ) -> Self {
        Self { authority, targets }
    }
}

impl RemoteExactObservationQueryReadPortV1 for DaemonRemoteExactObservationQueryPortV1 {
    fn read_exact_observation(
        &self,
        command: &RemoteExactObservationQueryCommandV1,
    ) -> Result<RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryErrorV1> {
        if command
            .effective_deadline
            .is_elapsed_at(command.observed_at)
        {
            return Err(RemoteExactObservationQueryErrorV1::DeadlineElapsed);
        }
        let snapshot = self
            .authority
            .query_authority_snapshot(&command.repository_scope, command.observed_at)?;
        let target = self
            .targets
            .registered_query_target(&command.repository_scope.project_id)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        validate_snapshot(command, &snapshot, &target)?;
        let publication = target.publication().clone();

        let operation = ObservationReadOperationV1::Observation {
            observation_id: command.observation_id.clone(),
        };
        let admission_bytes = serde_json::to_vec(&operation)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?
            .len();
        let control = runtime_control(command)?;
        let request = RuntimeReadRequestV1::new(
            target.binding().clone(),
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
        let deadline_remaining = command
            .effective_deadline
            .expires_at
            .0
            .checked_sub(command.observed_at.0)
            .and_then(|micros| u64::try_from(micros).ok())
            .ok_or(RemoteExactObservationQueryErrorV1::DeadlineElapsed)?;
        let started = Instant::now();
        let probe = QueryProbe {
            cancellation: control.cancellation,
            deadline: control.deadline,
            started,
            maximum_elapsed: Duration::from_micros(
                command
                    .budget
                    .maximum_elapsed_micros
                    .min(deadline_remaining),
            ),
            decision: Mutex::new(None),
            commit_started: AtomicBool::new(false),
        };
        let outcome = target
            .dispatch_read(request.clone(), &probe)
            .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
        outcome
            .validate_for(&request)
            .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        let elapsed = u64::try_from(started.elapsed().as_micros())
            .map_err(|_| RemoteExactObservationQueryErrorV1::BudgetExceeded)?;
        if elapsed > command.budget.maximum_elapsed_micros {
            return Err(RemoteExactObservationQueryErrorV1::BudgetExceeded);
        }
        let frontier = current_frontier(outcome.coverage())?;

        let current_snapshot = self
            .authority
            .query_authority_snapshot(&command.repository_scope, command.observed_at)?;
        let current_target = self
            .targets
            .registered_query_target(&command.repository_scope.project_id)
            .map_err(|_| RemoteExactObservationQueryErrorV1::StaleFence)?;
        validate_snapshot(command, &current_snapshot, &current_target)?;
        if current_snapshot != snapshot || current_target.publication() != &publication {
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
        let result_count = match &observation {
            RemoteExactObservationResultV1::Found(_) => 1_u64,
            RemoteExactObservationResultV1::NotFound => 0_u64,
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
            authority_receipt: Some(command.query_authorization.authority.clone()),
            value: Some(RemoteQueryCompleteValueV1 {
                returned_observations: u8::try_from(result_count)
                    .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
            }),
            reason_code: None,
        };
        let composition = RemoteQueryCompositionV1::compose(
            BTreeSet::from([command.expected_shard.clone()]),
            vec![contribution],
            PendingLocalEvidenceV1::Unavailable {
                reason: PendingLocalUnavailableReasonV1::RequestingNodeSpoolNotSupplied,
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

        let elapsed_i64 = i64::try_from(elapsed)
            .map_err(|_| RemoteExactObservationQueryErrorV1::BudgetExceeded)?;
        let ended_at = command
            .observed_at
            .0
            .checked_add(elapsed_i64)
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
            coverage: EvidenceCoverage {
                requested_domains: vec![EvidenceDomain::Anchor],
                visited: Some(1),
                eligible: Some(result_count),
                returned: result_count,
                completeness: CoverageCompleteness::Unknown,
                domains: vec![CoverageDomainState {
                    domain: EvidenceDomain::Anchor,
                    completeness: CoverageCompleteness::Unknown,
                }],
            },
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.remote.exact-observation.v1")
                    .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?,
                1,
                Some(result_count),
                result_count,
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
            command.query_authorization.authority.clone(),
            execution,
        )
        .map_err(|_| RemoteExactObservationQueryErrorV1::ReceiptMismatch)?;
        Ok(RemoteExactObservationQueryOutcomeV1 {
            authority: snapshot.authority,
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
    target: &tracedecay_runtime_core::store_runtime::registry::StoreRuntimeClientLease,
) -> Result<(), RemoteExactObservationQueryErrorV1> {
    let CurrentRemoteAuthorityStateV1::Available(current) = &snapshot.authority else {
        return Err(RemoteExactObservationQueryErrorV1::AuthorityUnavailable);
    };
    if current.fence != command.expected_authority
        || &snapshot.writer.authority != current
        || snapshot.writer.project_id != command.repository_scope.project_id
        || snapshot.writer.scope != command.repository_scope
        || target.binding().shard_id.brain_id != command.expected_authority.brain_id
        || target.binding().authority_epoch.get() != command.expected_authority.authority_epoch.0
        || !matches!(
            &target.binding().shard_id.scope,
            StoreShardScopeV1::ProjectSessions { project_id }
                if project_id == &command.repository_scope.project_id
        )
    {
        return Err(RemoteExactObservationQueryErrorV1::StaleFence);
    }
    Ok(())
}

fn current_frontier(
    coverage: &RuntimeReadCoverageV1,
) -> Result<ShardWatermarkV1, RemoteExactObservationQueryErrorV1> {
    match coverage {
        RuntimeReadCoverageV1::Latest {
            observed: Some(frontier),
        } if frontier.commit_sequence.0 > 0 => Ok(frontier.clone()),
        RuntimeReadCoverageV1::Unavailable {
            reason: UnavailableReasonV1::DeadlineExceeded,
            ..
        } => Err(RemoteExactObservationQueryErrorV1::DeadlineElapsed),
        RuntimeReadCoverageV1::Latest { .. }
        | RuntimeReadCoverageV1::Complete { .. }
        | RuntimeReadCoverageV1::Partial { .. }
        | RuntimeReadCoverageV1::Stale { .. }
        | RuntimeReadCoverageV1::Unavailable { .. } => {
            Err(RemoteExactObservationQueryErrorV1::AuthorityUnavailable)
        }
    }
}

fn runtime_control(
    command: &RemoteExactObservationQueryCommandV1,
) -> Result<RuntimeRequestControlV1, RemoteExactObservationQueryErrorV1> {
    let suffix = hex::encode(Sha256::digest(format!(
        "{}:{}",
        command.request_id.as_str(),
        command.observation_id.as_str()
    )));
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
    started: Instant,
    maximum_elapsed: Duration,
    decision: Mutex<Option<RuntimeInterruptionV1>>,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for QueryProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        let Ok(mut decision) = self.decision.lock() else {
            return Some(RuntimeInterruptionV1::DeadlineExceeded);
        };
        if decision.is_none() && self.started.elapsed() >= self.maximum_elapsed {
            *decision = Some(RuntimeInterruptionV1::DeadlineExceeded);
        }
        *decision
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

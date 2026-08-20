use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

use tracedecay_store::ProjectionPersistOutcome;

use super::*;

/// One WARN per hour for a standing durable refusal, matching #538.
/// The host re-ticks this queue on a 60s scheduler; skip is already
/// cheaper (yield the rest of the batch). The storm is re-entry.
const DEFAULT_DETERMINISTIC_REFUSAL_WARN_SUPPRESSION_SECS: u64 = 3_600;

static DETERMINISTIC_REFUSAL_WARN_STATE: OnceLock<Mutex<Option<DeterministicRefusalWarnAnchor>>> =
    OnceLock::new();

/// Whether to emit `deterministic projection rejection committed`.
///
/// Skip + Deferred + yield still happen on every first durable refusal.
/// This gate only holds back the repeat WARN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeterministicRefusalWarnGate {
    Emit,
    Suppressed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeterministicRefusalWarnAnchor {
    observation_id: String,
    observed_at_secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeterministicRefusalWarnBackoff {
    suppression_secs: u64,
}

impl Default for DeterministicRefusalWarnBackoff {
    fn default() -> Self {
        Self::new(DEFAULT_DETERMINISTIC_REFUSAL_WARN_SUPPRESSION_SECS)
    }
}

impl DeterministicRefusalWarnBackoff {
    #[must_use]
    const fn new(suppression_secs: u64) -> Self {
        Self { suppression_secs }
    }

    #[cfg(test)]
    #[must_use]
    const fn suppression_secs(&self) -> u64 {
        self.suppression_secs
    }

    #[must_use]
    fn gate(
        &self,
        standing: Option<&DeterministicRefusalWarnAnchor>,
        observation_id: &str,
        now_secs: i64,
    ) -> DeterministicRefusalWarnGate {
        let Some(standing) = standing else {
            return DeterministicRefusalWarnGate::Emit;
        };
        if standing.observation_id != observation_id {
            return DeterministicRefusalWarnGate::Emit;
        }
        let until_secs = standing
            .observed_at_secs
            .saturating_add(i64::try_from(self.suppression_secs).unwrap_or(i64::MAX));
        if now_secs < until_secs {
            DeterministicRefusalWarnGate::Suppressed
        } else {
            DeterministicRefusalWarnGate::Emit
        }
    }
}

fn record_deterministic_refusal_warn(
    observation_id: &str,
    now_secs: i64,
) -> DeterministicRefusalWarnGate {
    let backoff = DeterministicRefusalWarnBackoff::default();
    let state = DETERMINISTIC_REFUSAL_WARN_STATE.get_or_init(|| Mutex::new(None));
    let mut standing = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let decision = backoff.gate(standing.as_ref(), observation_id, now_secs);
    if decision == DeterministicRefusalWarnGate::Emit {
        *standing = Some(DeterministicRefusalWarnAnchor {
            observation_id: observation_id.to_owned(),
            observed_at_secs: now_secs,
        });
    }
    decision
}

fn unix_now_secs() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(i64::MAX)
}

impl HostAdmissionFacade<'_> {
    pub async fn drain_projection_queue(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        max: usize,
    ) -> Result<HostProjectionDrainOutcome, HostAdmissionOutcome> {
        if cancellation.is_cancelled() {
            return Err(classify_error(&ObservationApplicationError::Cancelled));
        }
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        let external_source = crate::external_source_store::RuntimeExternalSourceStore::new(
            database.runtime_client(),
        );
        let external_replay = external_source
            .drain_host_projection_replay(max, cancellation)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "external-source projection replay failed during host drain");
                HostAdmissionOutcome::retained_unavailable("external_source_projection_unavailable")
            })?;
        if cancellation.is_cancelled() {
            return Err(classify_error(&ObservationApplicationError::Cancelled));
        }
        let store = self.store(provider, scope)?;
        let mut outcome = HostProjectionDrainOutcome {
            deferred: external_replay.deferred,
            ..HostProjectionDrainOutcome::default()
        };
        let mut session_ids = BTreeSet::new();
        let mut observation_deferred = false;
        let mut observation_queue_exhausted = false;
        for _ in 0..max {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            let Some(observation_id) = store.next_queued_observation().await.map_err(|error| {
                tracing::warn!(%error, "projection store operation failed during host drain");
                projection_store_unavailable()
            })?
            else {
                observation_queue_exhausted = true;
                break;
            };
            let projected = match store.project_observation(&observation_id).await {
                Ok(projected) => projected,
                Err(ProjectionStoreError::RetryDeferred { .. }) => {
                    observation_deferred = true;
                    break;
                }
                Err(
                    error @ (ProjectionStoreError::Contract(_)
                    | ProjectionStoreError::SanitizationRefused { .. }),
                ) => {
                    // Skip is already durable. Yield the rest of this drain so
                    // we do not keep paying sanitization/receipt construction
                    // on the same batch (Plan 23/26: typed skip + Deferred).
                    // Always break: after_deterministic_rejection is stop=true,
                    // and `if stop { break; }` types as () when stop is false.
                    if record_deterministic_refusal_warn(observation_id.as_str(), unix_now_secs())
                        == DeterministicRefusalWarnGate::Emit
                    {
                        tracing::warn!(
                            %error,
                            observation = observation_id.as_str(),
                            "deterministic projection rejection committed"
                        );
                    }
                    let (skipped, deferred, stop) = after_deterministic_rejection(outcome.skipped);
                    outcome.skipped = skipped;
                    observation_deferred = deferred;
                    debug_assert!(stop);
                    break;
                }
                Err(error) => {
                    // The head-of-queue failure aborts the drain (fail-closed
                    // sequence ordering); the full source chain must land in
                    // the log or the stall is undiagnosable from outside.
                    tracing::warn!(
                        error = %error.durable_detail(),
                        observation = observation_id.as_str(),
                        "projection store operation failed during host drain"
                    );
                    return Err(projection_error_outcome(&error));
                }
            };
            match projected {
                ProjectionPersistOutcome::Projected(projected) => {
                    outcome.projected = outcome.projected.saturating_add(1);
                    outcome.projected_outputs = outcome.projected_outputs.saturating_add(
                        u64::try_from(projected.output_count()).unwrap_or(u64::MAX),
                    );
                    if let Some(observation) = store
                        .get_observation(&observation_id)
                        .await
                        .map_err(|error| {
                            tracing::warn!(
                                %error,
                                "projection store operation failed during host drain"
                            );
                            projection_store_unavailable()
                        })?
                    {
                        session_ids.insert(
                            observation
                                .observation()
                                .source()
                                .session_id()
                                .as_str()
                                .to_owned(),
                        );
                    }
                }
                ProjectionPersistOutcome::Skipped { .. } => {
                    outcome.skipped = outcome.skipped.saturating_add(1);
                }
                ProjectionPersistOutcome::ExactDuplicate(_) => {
                    outcome.exact_duplicates = outcome.exact_duplicates.saturating_add(1);
                }
            }
        }
        if !observation_deferred && !observation_queue_exhausted {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            observation_deferred = store
                .next_queued_observation()
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "projection suffix probe failed during host drain");
                    projection_store_unavailable()
                })?
                .is_some();
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
        }
        outcome.deferred |= observation_deferred;
        outcome.session_ids = session_ids.into_iter().collect();
        Ok(outcome)
    }
}

fn after_deterministic_rejection(skipped: u64) -> (u64, bool, bool) {
    (skipped.saturating_add(1), true, true)
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum SimulatedProjectOutcome {
    Refusal,
    Projected,
}

#[cfg(test)]
fn simulate_drain_project_calls(batch: &[SimulatedProjectOutcome]) -> (u64, bool, usize) {
    let mut skipped = 0;
    let mut deferred = false;
    let mut project_calls = 0;
    for outcome in batch {
        project_calls = project_calls.saturating_add(1);
        match outcome {
            SimulatedProjectOutcome::Refusal => {
                let (next_skipped, next_deferred, stop) = after_deterministic_rejection(skipped);
                skipped = next_skipped;
                deferred = next_deferred;
                if stop {
                    break;
                }
            }
            SimulatedProjectOutcome::Projected => {}
        }
    }
    (skipped, deferred, project_calls)
}

#[cfg(test)]
fn warn_gates_for_refusals(
    backoff: DeterministicRefusalWarnBackoff,
    events: &[(&str, i64)],
) -> Vec<DeterministicRefusalWarnGate> {
    let mut standing = None;
    let mut gates = Vec::with_capacity(events.len());
    for &(observation_id, now_secs) in events {
        let gate = backoff.gate(standing.as_ref(), observation_id, now_secs);
        if gate == DeterministicRefusalWarnGate::Emit {
            standing = Some(DeterministicRefusalWarnAnchor {
                observation_id: observation_id.to_owned(),
                observed_at_secs: now_secs,
            });
        }
        gates.push(gate);
    }
    gates
}

#[cfg(test)]
mod tests {
    use super::{
        DeterministicRefusalWarnBackoff, DeterministicRefusalWarnGate, SimulatedProjectOutcome,
        after_deterministic_rejection, simulate_drain_project_calls, warn_gates_for_refusals,
    };

    #[test]
    fn first_deterministic_refusal_is_one_skip_and_yields_the_rest() {
        let max = 8_u32;
        let (skipped, deferred, stop) = after_deterministic_rejection(0);
        assert_eq!(skipped, 1);
        assert!(deferred);
        assert!(stop);
        let new_project_calls = 1_u32;
        assert!(
            new_project_calls < max,
            "yielding must do less project/sanitization work than continuing the batch"
        );
    }

    #[test]
    fn multi_item_batch_yields_after_first_refusal() {
        let batch = [
            SimulatedProjectOutcome::Refusal,
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Projected,
            SimulatedProjectOutcome::Projected,
        ];
        let (skipped, deferred, project_calls) = simulate_drain_project_calls(&batch);
        assert_eq!(skipped, 1);
        assert!(deferred);
        assert_eq!(
            project_calls, 1,
            "first durable refusal must yield; remaining max-1 items are unpaid"
        );
        assert!(project_calls < batch.len());
    }

    #[test]
    fn repeat_deterministic_refusal_warn_is_gated_by_observation_and_window() {
        let backoff = DeterministicRefusalWarnBackoff::new(3_600);
        let standing_id = "obs-standing";
        let later_id = "obs-later";
        let observed_at = 1_000_i64;
        let gates = warn_gates_for_refusals(
            backoff,
            &[
                (standing_id, observed_at),
                (standing_id, observed_at + 1),
                (standing_id, observed_at + 3_599),
                (later_id, observed_at + 60),
                (later_id, observed_at + 120),
                (later_id, observed_at + 60 + 3_600),
            ],
        );
        assert_eq!(
            gates,
            [
                DeterministicRefusalWarnGate::Emit,
                DeterministicRefusalWarnGate::Suppressed,
                DeterministicRefusalWarnGate::Suppressed,
                DeterministicRefusalWarnGate::Emit,
                DeterministicRefusalWarnGate::Suppressed,
                DeterministicRefusalWarnGate::Emit,
            ]
        );
        let emit_count = gates
            .iter()
            .filter(|gate| **gate == DeterministicRefusalWarnGate::Emit)
            .count();
        assert_eq!(
            emit_count, 3,
            "same standing id must not re-warn inside the window"
        );
        assert_eq!(
            DeterministicRefusalWarnBackoff::default().suppression_secs(),
            3_600
        );
    }
}

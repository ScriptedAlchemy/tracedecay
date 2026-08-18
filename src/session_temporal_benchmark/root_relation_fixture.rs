//! Production root-wide relation-hydration fixture for the session benchmark.

use std::collections::BTreeSet;

use tracedecay_application::RequestContext;
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_store::{
    SessionRefreshCompletionRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ports::ExecutionControl;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{
    SessionRefreshConfiguration, SessionRefreshOutcome, SessionRefreshService,
    SessionRefreshTarget, SessionRequestBinding, SessionRetrievalOutcome, SessionRetrievalScope,
    SessionTemporalQuery,
};

use super::{AllowAuthorizer, BenchResult, CONFIG_VERSION, NoopWake, PROJECTOR_VERSION};
use crate::store::GlobalDbSessionTemporalStore;

pub(super) const ROOT_RELATION_PARTICIPANT_COUNT: usize = 64;

pub(super) struct RefreshedRootRelationFixture {
    pub(super) sessions: Vec<SessionId>,
    pub(super) anchor_session: SessionId,
    pub(super) anchor_complete_request: SessionRefreshCompletionRequestV1,
    pub(super) anchor_durable_progress: SessionRefreshProgressV1,
    pub(super) record_count: usize,
}

pub(super) fn session_ids(repetition: usize) -> BenchResult<Vec<SessionId>> {
    (0..ROOT_RELATION_PARTICIPANT_COUNT)
        .map(|ordinal| {
            SessionId::new(format!(
                "benchmark-codex-root-relation-{repetition:02}-{ordinal:02}"
            ))
            .map_err(|error| format!("root relation session id: {error}"))
        })
        .collect()
}

pub(super) async fn refresh_sessions(
    db: &RegisteredGlobalDb,
    context: &RequestContext,
    binding: &SessionRequestBinding,
    sessions: Vec<SessionId>,
    observation_count: u64,
) -> BenchResult<RefreshedRootRelationFixture> {
    if sessions.len() != ROOT_RELATION_PARTICIPANT_COUNT {
        return Err(format!(
            "root relation fixture requires {ROOT_RELATION_PARTICIPANT_COUNT} sessions, got {}",
            sessions.len()
        ));
    }
    let refresh = SessionRefreshService::new(
        AllowAuthorizer,
        GlobalDbSessionTemporalStore::new(db),
        NoopWake,
        SessionRefreshConfiguration::new(PROJECTOR_VERSION, CONFIG_VERSION)
            .map_err(|error| format!("root refresh configuration: {error}"))?,
    );
    let mut anchor = None;
    let mut record_count = 0usize;

    for session_id in &sessions {
        let target = SessionRefreshTarget::new(
            session_id.clone(),
            Some("codex".to_owned()),
            TemporalModeV1::Current,
            RetrievalGrainV1::LogicalMessage,
            SessionRefreshFrontierV1::new(observation_count, 0)
                .map_err(|error| format!("root refresh frontier: {error}"))?,
        )
        .map_err(|error| format!("root refresh target: {error}"))?;
        let handle = match refresh.begin_or_join(context, binding, target).await {
            SessionRefreshOutcome::Started(handle) | SessionRefreshOutcome::Joined(handle) => {
                handle
            }
            other => return Err(format!("unexpected root refresh outcome: {other:?}")),
        };
        let session = handle.target().session_id().clone();
        if &session != session_id {
            return Err(format!(
                "root refresh returned session {} for requested {session_id}",
                session.as_str()
            ));
        }

        let mut projected = 0u64;
        loop {
            let recovery = db
                .session_refresh_recovery_result(&session)
                .await
                .map_err(|error| format!("root refresh recovery: {error:?}"))?
                .ok_or_else(|| "missing root refresh recovery after begin_or_join".to_owned())?;
            match db
                .materialize_session_temporal_refresh_batch_result(&recovery)
                .await
                .map_err(|error| {
                    format!("root CanonicalSessionTemporalProjector materialize: {error:?}")
                })? {
                Some((progress, batch)) => {
                    projected = projected.saturating_add(
                        u64::try_from(batch.item_count())
                            .map_err(|error| format!("root batch item count: {error}"))?,
                    );
                    GlobalDbSessionTemporalStore::new(db)
                        .persist_session_refresh_projection_batch(progress, batch)
                        .await
                        .map_err(|error| format!("persist root projection batch: {error:?}"))?;
                }
                None => break,
            }
        }
        if projected == 0 {
            return Err(format!(
                "root refresh projected zero temporal records for {session_id}"
            ));
        }
        let recovery = db
            .session_refresh_recovery_result(&session)
            .await
            .map_err(|error| format!("root refresh recovery before complete: {error:?}"))?
            .ok_or_else(|| "missing root refresh recovery before complete".to_owned())?;
        let progress = recovery
            .progress()
            .cloned()
            .ok_or_else(|| "root refresh progress missing before complete".to_owned())?;
        let complete_request = SessionRefreshCompletionRequestV1::new(
            handle.operation_id().clone(),
            session.clone(),
            progress.frontier(),
            *progress.coverage(),
        )
        .map_err(|error| format!("complete root refresh request: {error}"))?;
        db.complete_session_refresh_result(complete_request.clone(), ExecutionControl::new(None))
            .await
            .map_err(|error| format!("complete root refresh: {error:?}"))?;
        let committed = usize::try_from(progress.committed_records())
            .map_err(|error| format!("convert root committed records: {error}"))?;
        if committed == 0 {
            return Err(format!(
                "root refresh persisted zero temporal records for {session_id}"
            ));
        }
        record_count = record_count.saturating_add(committed);
        if anchor.is_none() {
            anchor = Some((session, complete_request, progress));
        }
    }

    if record_count < ROOT_RELATION_PARTICIPANT_COUNT {
        return Err(format!(
            "root refresh persisted {record_count} records for {ROOT_RELATION_PARTICIPANT_COUNT} sessions"
        ));
    }
    let (anchor_session, anchor_complete_request, anchor_durable_progress) =
        anchor.ok_or_else(|| "root refresh fixture had no anchor session".to_owned())?;
    Ok(RefreshedRootRelationFixture {
        sessions,
        anchor_session,
        anchor_complete_request,
        anchor_durable_progress,
        record_count,
    })
}

pub(super) fn root_relation_query(anchor_session: SessionId) -> BenchResult<SessionTemporalQuery> {
    SessionTemporalQuery::new(
        anchor_session,
        Some("codex".to_owned()),
        "pipeline",
        None,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        ROOT_RELATION_PARTICIPANT_COUNT,
        DiversityLimits::unbounded(),
        ContextBudget {
            max_bytes: 64_000,
            max_tokens: 16_000,
            estimator_version: "words-v1".to_owned(),
        },
    )
    .map(|query| query.with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot))
    .map_err(|error| format!("root relation query: {error}"))
}

pub(super) fn require_root_relation_hydration(
    outcome: SessionRetrievalOutcome<tracedecay_temporal_query::TemporalKernelResult>,
    expected_sessions: &[SessionId],
) -> BenchResult<()> {
    let expected_sessions = expected_sessions
        .iter()
        .map(|session| session.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    if expected_sessions.len() != ROOT_RELATION_PARTICIPANT_COUNT {
        return Err(format!(
            "root relation assertion expected {ROOT_RELATION_PARTICIPANT_COUNT} distinct sessions, got {}",
            expected_sessions.len()
        ));
    }
    let SessionRetrievalOutcome::Complete { mut items, .. } = outcome else {
        return Err(format!(
            "root relation hydration did not complete: {outcome:?}"
        ));
    };
    if items.len() != 1 {
        return Err(format!(
            "root relation hydration returned {} kernel results",
            items.len()
        ));
    }
    let result = items
        .pop()
        .ok_or_else(|| "root relation hydration result missing".to_owned())?;
    if !result.snapshot.has_authoritative_participant_manifest() {
        return Err(
            "root relation hydration lacks an authoritative participant manifest".to_owned(),
        );
    }
    let manifest_sessions = result
        .snapshot
        .participant_manifest()
        .entries()
        .iter()
        .filter(|participant| participant.source_id() == "codex")
        .map(|participant| participant.session_id().as_str().to_owned())
        .collect::<BTreeSet<_>>();
    if manifest_sessions != expected_sessions {
        return Err(format!(
            "root relation manifest sessions mismatch: expected {}, got {}",
            expected_sessions.len(),
            manifest_sessions.len()
        ));
    }
    if result.ranked.len() != ROOT_RELATION_PARTICIPANT_COUNT {
        return Err(format!(
            "root relation hydration ranked {} candidates, expected {ROOT_RELATION_PARTICIPANT_COUNT}",
            result.ranked.len()
        ));
    }
    let ranked_sessions = result
        .ranked
        .iter()
        .map(|candidate| {
            candidate
                .session
                .as_deref()
                .ok_or_else(|| "root relation candidate session is missing".to_owned())
                .map(str::to_owned)
        })
        .collect::<BenchResult<BTreeSet<_>>>()?;
    if ranked_sessions != expected_sessions {
        return Err(format!(
            "root relation hydration covered {} distinct candidate sessions, expected {ROOT_RELATION_PARTICIPANT_COUNT}",
            ranked_sessions.len()
        ));
    }
    let ranked_anchors = result
        .ranked
        .iter()
        .map(|candidate| candidate.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let hydrated_anchors = result
        .hydrated
        .iter()
        .map(|hydrated| hydrated.anchor_id().clone())
        .collect::<BTreeSet<_>>();
    let context_anchors = result
        .context
        .bundle
        .records
        .iter()
        .map(|record| record.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    if hydrated_anchors != ranked_anchors
        || context_anchors != ranked_anchors
        || result.context.bundle.records.len() != ROOT_RELATION_PARTICIPANT_COUNT
    {
        return Err(
            "root relation hydration did not carry every ranked record through the registered relation load"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_names_64_distinct_canonical_codex_sessions() {
        let sessions = session_ids(7).expect("fixture session ids");
        assert_eq!(sessions.len(), ROOT_RELATION_PARTICIPANT_COUNT);
        assert_eq!(
            sessions
                .iter()
                .map(tracedecay_domain::SessionId::as_str)
                .collect::<BTreeSet<_>>()
                .len(),
            ROOT_RELATION_PARTICIPANT_COUNT
        );
    }

    #[test]
    fn root_relation_hydration_error_names_locked_outcome() {
        let sessions = session_ids(7).expect("fixture session ids");

        let error = require_root_relation_hydration(SessionRetrievalOutcome::Locked, &sessions)
            .expect_err("locked root retrieval must not satisfy hydration");

        assert!(
            error.contains("Locked"),
            "non-complete root hydration error must name its typed outcome: {error}"
        );
    }
}

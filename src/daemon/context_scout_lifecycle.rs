//! Exact Context Scout lifecycle lookup from canonical durable observations.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tracedecay_domain::{
    AgentInstanceId, CanonicalObservationEnvelopeV1, DurableObservationV1, MessageId,
    ObservationScopeV1, ProjectId, SessionId, ThreadId, TurnId, UserProfileId, WorktreeId,
};
use tracedecay_store::StoreShardScopeV1;

use crate::agents::context_scout_ports::ContextScoutLifecycleAddressV1;
use crate::global_db::RegisteredGlobalDb;

const MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1: usize = 64;

struct ContextScoutLifecycleLookupAuthorityV1 {
    profile_id: UserProfileId,
    project_id: ProjectId,
    worktree_id: WorktreeId,
    sessions: Weak<RegisteredGlobalDb>,
}

fn registered_context_scout_lifecycle_authorities()
-> &'static Mutex<BTreeMap<([u8; 16], [u8; 16]), ContextScoutLifecycleLookupAuthorityV1>> {
    static AUTHORITIES: OnceLock<
        Mutex<BTreeMap<([u8; 16], [u8; 16]), ContextScoutLifecycleLookupAuthorityV1>>,
    > = OnceLock::new();
    AUTHORITIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(crate) fn register_context_scout_lifecycle_authority(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    project_id: ProjectId,
    worktree_id: WorktreeId,
    sessions: &Arc<RegisteredGlobalDb>,
) -> bool {
    if hook_project_id == [0; 16]
        || hook_worktree_id == [0; 16]
        || project_id.validate().is_err()
        || worktree_id.validate().is_err()
    {
        return false;
    }
    let profile_id = sessions.binding().shard_id.profile_id.clone();
    let Some(authority_project_id) = (match &sessions.binding().shard_id.scope {
        StoreShardScopeV1::ProjectSessions { project_id } => Some(project_id),
        _ => None,
    }) else {
        return false;
    };
    if authority_project_id != &project_id {
        return false;
    }
    let Ok(mut authorities) = registered_context_scout_lifecycle_authorities().lock() else {
        return false;
    };
    let key = (hook_project_id, hook_worktree_id);
    if let Some(existing) = authorities.get(&key)
        && let Some(existing_sessions) = existing.sessions.upgrade()
    {
        return existing.profile_id == profile_id
            && existing.project_id == project_id
            && existing.worktree_id == worktree_id
            && Arc::ptr_eq(&existing_sessions, sessions);
    }
    authorities.retain(|_, authority| authority.sessions.strong_count() > 0);
    authorities.insert(
        key,
        ContextScoutLifecycleLookupAuthorityV1 {
            profile_id,
            project_id,
            worktree_id,
            sessions: Arc::downgrade(sessions),
        },
    );
    true
}

pub(crate) async fn lookup_registered_context_scout_lifecycle(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    session_id: &SessionId,
) -> Option<ContextScoutLifecycleAddressV1> {
    let (profile_id, project_id, worktree_id, sessions) = {
        let authorities = registered_context_scout_lifecycle_authorities()
            .lock()
            .ok()?;
        let authority = authorities.get(&(hook_project_id, hook_worktree_id))?;
        (
            authority.profile_id.clone(),
            authority.project_id.clone(),
            authority.worktree_id.clone(),
            authority.sessions.upgrade()?,
        )
    };
    lookup_context_scout_lifecycle(
        &profile_id,
        &project_id,
        &worktree_id,
        session_id,
        sessions.as_ref(),
    )
    .await
}

/// Resolves one complete native lifecycle tuple from the registered
/// project-session store.
///
/// The lookup is exact and bounded. It accepts only receipt-checked durable
/// observations whose source identity, project scope, and canonical payload
/// all bind the requested session. Duplicate observations of the same tuple
/// are harmless; any distinct, incomplete, corrupt, released, or excessive
/// evidence fails closed.
pub(crate) async fn lookup_context_scout_lifecycle(
    profile_id: &UserProfileId,
    project_id: &ProjectId,
    worktree_id: &WorktreeId,
    session_id: &SessionId,
    sessions: &RegisteredGlobalDb,
) -> Option<ContextScoutLifecycleAddressV1> {
    profile_id.validate().ok()?;
    project_id.validate().ok()?;
    worktree_id.validate().ok()?;
    session_id.validate().ok()?;
    let binding = sessions.binding();
    if &binding.shard_id.profile_id != profile_id
        || !matches!(
            &binding.shard_id.scope,
            StoreShardScopeV1::ProjectSessions {
                project_id: bound_project_id
            } if bound_project_id == project_id
        )
    {
        return None;
    }

    let snapshot = sessions.read_snapshot().await.ok()?;
    let mut rows = snapshot
        .query(
            "SELECT observation_json
             FROM observations
             WHERE json_extract(observation_json, '$.__retention_released') IS NULL
               AND json_extract(
                    observation_json,
                    '$.identity.source.session_id'
               ) = ?1
             ORDER BY sequence
             LIMIT ?2",
            crate::db::engine::params![
                session_id.as_str(),
                i64::try_from(MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1 + 1).ok()?
            ],
        )
        .await
        .ok()?;

    let project_scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let mut resolved: Option<ContextScoutLifecycleAddressV1> = None;
    let mut count = 0usize;
    while let Some(row) = rows.next().await.ok()? {
        count = count.checked_add(1)?;
        if count > MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1 {
            return None;
        }
        let observation_json = row.get::<String>(0).ok()?;
        let durable = serde_json::from_str::<DurableObservationV1>(&observation_json).ok()?;
        if durable.scope() != &project_scope || durable.source().session_id() != session_id {
            return None;
        }
        let envelope =
            serde_json::from_value::<CanonicalObservationEnvelopeV1>(durable.payload().clone())
                .ok()?;
        if envelope.provider() != durable.source().provider()
            || envelope.relations().session_id() != session_id
        {
            return None;
        }
        let candidate =
            lifecycle_from_canonical(profile_id, project_id, worktree_id, session_id, &envelope)?;
        if resolved
            .as_ref()
            .is_some_and(|existing| existing != &candidate)
        {
            return None;
        }
        resolved = Some(candidate);
    }
    resolved
}

fn lifecycle_from_canonical(
    profile_id: &UserProfileId,
    project_id: &ProjectId,
    worktree_id: &WorktreeId,
    session_id: &SessionId,
    observation: &CanonicalObservationEnvelopeV1,
) -> Option<ContextScoutLifecycleAddressV1> {
    observation.validate().ok()?;
    let relations = observation.relations();
    if relations.session_id() != session_id {
        return None;
    }
    Some(ContextScoutLifecycleAddressV1 {
        profile_id: profile_id.clone(),
        provider_id: observation.provider().clone(),
        project_id: project_id.clone(),
        worktree_id: worktree_id.clone(),
        session_id: session_id.clone(),
        thread_id: ThreadId::new(relations.thread_id()?.as_str().to_owned()).ok()?,
        turn_id: TurnId::new(relations.turn_id()?.as_str().to_owned()).ok()?,
        agent_id: AgentInstanceId::new(relations.agent_id()?.as_str().to_owned()).ok()?,
        logical_message_id: MessageId::new(relations.message_id()?.as_str().to_owned()).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tracedecay_domain::{
        CanonicalMessageRoleV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
        CanonicalObservationRelationsV1, ObservationId, ObservationOrderingDomainV1,
        ObservationSourceRangeV1, ProviderId,
    };

    use super::*;

    fn id<T: TryFrom<String>>(value: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn complete_native_observation() -> CanonicalObservationEnvelopeV1 {
        let relations =
            CanonicalObservationRelationsV1::new(id::<SessionId>("session.native.codex"))
                .with_thread_id(id::<ObservationId>("thread.native.codex"))
                .with_turn_id(id::<ObservationId>("turn.native.codex"))
                .with_agent_id(id::<ObservationId>("agent.native.codex"))
                .with_message_id(id::<ObservationId>("message.native.codex"));
        CanonicalObservationEnvelopeV1::new(
            id::<ProviderId>("codex"),
            "event_msg",
            id::<ObservationId>("record.native.codex"),
            relations,
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: Value::String("saved".to_owned()),
                model: None,
                timestamp: None,
            }],
            CanonicalObservationEvidenceV1::new(
                ObservationOrderingDomainV1::FileBytes,
                ObservationSourceRangeV1::new(0, 1).unwrap(),
            ),
        )
        .unwrap()
    }

    #[test]
    fn complete_canonical_native_lifecycle_is_admitted_without_hash_identity() {
        let lifecycle = lifecycle_from_canonical(
            &id("profile.native"),
            &id("project.native"),
            &id("worktree.native"),
            &id("session.native.codex"),
            &complete_native_observation(),
        )
        .unwrap();
        assert_eq!(lifecycle.provider_id.as_str(), "codex");
        assert_eq!(lifecycle.session_id.as_str(), "session.native.codex");
        assert_eq!(lifecycle.thread_id.as_str(), "thread.native.codex");
        assert_eq!(lifecycle.turn_id.as_str(), "turn.native.codex");
        assert_eq!(lifecycle.agent_id.as_str(), "agent.native.codex");
        assert_eq!(
            lifecycle.logical_message_id.as_str(),
            "message.native.codex"
        );
    }

    #[test]
    fn wrong_session_fails_closed() {
        assert!(
            lifecycle_from_canonical(
                &id("profile.native"),
                &id("project.native"),
                &id("worktree.native"),
                &id("session.other"),
                &complete_native_observation(),
            )
            .is_none()
        );
    }

    #[test]
    fn checked_in_native_fixture_without_complete_lifecycle_fails_closed() {
        let fixture = include_str!(
            "../../tests/fixtures/provider_normalization/codex/agent_message.expected_envelope.json"
        )
        .replace("$STABLE_RECORD_ID", "message.native.fixture");
        let observation = serde_json::from_str::<CanonicalObservationEnvelopeV1>(&fixture).unwrap();
        assert!(
            lifecycle_from_canonical(
                &id("profile.native"),
                &id("project.native"),
                &id("worktree.native"),
                observation.relations().session_id(),
                &observation,
            )
            .is_none()
        );
    }
}

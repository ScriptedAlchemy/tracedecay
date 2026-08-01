//! Exact Context Scout lifecycle lookup from canonical durable observations.

use std::sync::{Arc, OnceLock};

use tracedecay_domain::{
    AgentInstanceId, CanonicalObservationEnvelopeV1, DurableObservationV1, MessageId,
    ObservationScopeV1, ProjectId, SessionId, ThreadId, TurnId, UserProfileId, WorktreeId,
};
use tracedecay_store::StoreShardScopeV1;

use crate::agents::context_scout_ports::ContextScoutLifecycleAddressV1;
use crate::global_db::RegisteredGlobalDb;
use crate::support::weak_registry::WeakRegistry;

const MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1: usize = 64;

type ContextScoutLifecycleKeyV1 = ([u8; 16], [u8; 16]);
type ContextScoutLifecycleAuthoritiesV1 = WeakRegistry<
    ContextScoutLifecycleKeyV1,
    RegisteredGlobalDb,
    ContextScoutLifecycleLookupAuthorityMetaV1,
>;

#[derive(Clone)]
struct ContextScoutLifecycleLookupAuthorityMetaV1 {
    profile_id: UserProfileId,
    project_id: ProjectId,
    worktree_id: WorktreeId,
}

fn registered_context_scout_lifecycle_authorities() -> &'static ContextScoutLifecycleAuthoritiesV1 {
    static AUTHORITIES: OnceLock<ContextScoutLifecycleAuthoritiesV1> = OnceLock::new();
    AUTHORITIES.get_or_init(WeakRegistry::new)
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
    let authorities = registered_context_scout_lifecycle_authorities();
    let key = (hook_project_id, hook_worktree_id);
    if let Some((existing_meta, existing_sessions)) = authorities.get_live_with_meta(&key) {
        return existing_meta.profile_id == profile_id
            && existing_meta.project_id == project_id
            && existing_meta.worktree_id == worktree_id
            && Arc::ptr_eq(&existing_sessions, sessions);
    }
    authorities.retain_live();
    authorities.insert_with_meta(
        key,
        ContextScoutLifecycleLookupAuthorityMetaV1 {
            profile_id,
            project_id,
            worktree_id,
        },
        sessions,
    );
    true
}

/// Test-only view of raw registry membership.
///
/// Deliberately distinct from [`resolve_authority`]: this reports whether the
/// key is still *present* without upgrading its `Weak`, so retain-driven
/// eviction is observable separately from a session store that merely died.
#[cfg(test)]
fn context_scout_lifecycle_authority_is_registered(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
) -> bool {
    registered_context_scout_lifecycle_authorities()
        .contains_key(&(hook_project_id, hook_worktree_id))
}

/// Looks up the registered lifecycle authority for a hook-scoped
/// (project, worktree) pair and upgrades its `Weak` session handle.
///
/// Shared prologue for both replay entry points below: resolve the
/// native profile/project/worktree identity and a live session handle,
/// or `None` if no authority is registered or its session store has
/// since been dropped.
fn resolve_authority(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
) -> Option<(
    UserProfileId,
    ProjectId,
    WorktreeId,
    Arc<RegisteredGlobalDb>,
)> {
    let (meta, sessions) = registered_context_scout_lifecycle_authorities()
        .get_live_with_meta(&(hook_project_id, hook_worktree_id))?;
    Some((meta.profile_id, meta.project_id, meta.worktree_id, sessions))
}

pub(crate) async fn lookup_registered_context_scout_lifecycle(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    session_id: &SessionId,
) -> Option<ContextScoutLifecycleAddressV1> {
    let (profile_id, project_id, worktree_id, sessions) =
        resolve_authority(hook_project_id, hook_worktree_id)?;
    lookup_context_scout_lifecycle(
        &profile_id,
        &project_id,
        &worktree_id,
        session_id,
        sessions.as_ref(),
    )
    .await
}

/// Re-resolves the native session behind a protected Hook V2 locator.
///
/// Replay has only the privacy-preserving locator carried by the validated
/// envelope. The authoritative project-session shard supplies the native
/// identity again; no hook payload or workspace path becomes durable replay
/// identity.
pub(crate) async fn lookup_registered_context_scout_native_session(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    protected_session_id: [u8; 32],
) -> Option<SessionId> {
    if protected_session_id == [0; 32] {
        return None;
    }
    let (profile_id, project_id, worktree_id, sessions) =
        resolve_authority(hook_project_id, hook_worktree_id)?;
    let session_id = {
        let snapshot = sessions.read_snapshot().await.ok()?;
        let mut rows = snapshot
            .query(
                "SELECT json_extract(
                        observation_json,
                        '$.identity.source.session_id'
                    ) AS session_id
                 FROM observations
                 WHERE json_extract(
                        observation_json,
                        '$.__retention_released'
                    ) IS NULL
                   AND json_type(
                        observation_json,
                        '$.identity.source.session_id'
                    ) = 'text'
                 GROUP BY session_id
                 ORDER BY MAX(sequence) DESC",
                (),
            )
            .await
            .ok()?;
        let mut resolved: Option<SessionId> = None;
        while let Some(row) = rows.next().await.ok()? {
            let raw_session_id = row.get::<String>(0).ok()?;
            if crate::hooks::hook_v2_protected_session_id_for_native(&raw_session_id)
                != protected_session_id
            {
                continue;
            }
            let candidate = SessionId::new(raw_session_id).ok()?;
            if resolved
                .as_ref()
                .is_some_and(|existing| existing != &candidate)
            {
                return None;
            }
            resolved = Some(candidate);
        }
        resolved?
    };
    lookup_context_scout_lifecycle(
        &profile_id,
        &project_id,
        &worktree_id,
        &session_id,
        sessions.as_ref(),
    )
    .await
    .map(|_| session_id)
}

/// Resolves one complete native lifecycle tuple from the registered
/// project-session store.
///
/// The lookup is exact and bounded. It accepts only receipt-checked durable
/// observations whose source identity, project scope, and canonical payload
/// all bind the requested session. The newest complete tuple wins, so a later
/// native tool call advances one session without making its prior turn
/// ambiguous. Corrupt, released, or excessive evidence still fails closed.
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
             ORDER BY sequence DESC
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
        if let Some(candidate) =
            lifecycle_from_canonical(profile_id, project_id, worktree_id, session_id, &envelope)
        {
            return Some(candidate);
        }
    }
    None
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
    use tempfile::TempDir;
    use tracedecay_domain::{
        CanonicalMessageRoleV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
        CanonicalObservationRelationsV1, ComponentVersion, ObservationId,
        ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationSourceCursorV1,
        ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
        PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass,
        SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, UtcMicros,
    };
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationStore, ObservationWrite,
        build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
    };

    use super::*;
    use crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

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

    fn durable_native_observation(project_id: &ProjectId) -> AnchoredObservationWrite {
        let envelope = complete_native_observation();
        let payload = serde_json::to_value(&envelope).unwrap();
        let range = ObservationSourceRangeV1::new(0, 1).unwrap();
        let source = ObservationSourceIdentityV1::for_provider(
            envelope.provider().clone(),
            envelope.relations().session_id().clone(),
        )
        .unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let generation = ObservationSourceGenerationV1::new(1).unwrap();
        let record_id = ObservationId::new("record.native.codex").unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.native.codex").unwrap(),
                ComponentVersion::new("sanitizer.native-test.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        let observation = DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                source.clone(),
                scope.clone(),
                generation,
                range,
                ObservationOrderingDomainV1::FileBytes,
                record_id,
            )
            .unwrap(),
            receipt,
            RetentionClass::new("retention.native-test").unwrap(),
            payload,
        )
        .unwrap();
        let cursor = ObservationSourceCursorV1::for_ordering(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::FileBytes,
            1,
        )
        .unwrap();
        let write = ObservationWrite::new(observation, None, cursor).unwrap();
        let projection_generation =
            ProjectionGenerationId::new("projection.native-test.v1").unwrap();
        let authorization =
            build_observation_resolution_authorization_v1(write.observation(), "native-test")
                .unwrap();
        let anchor = build_observation_retrieval_anchor_v2(
            write.observation(),
            projection_generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
    }

    /// `RegisteredGlobalDb` deliberately no longer builds this adapter
    /// itself; the composition root assembles it from the registered runtime
    /// and write authority (see the note on `RegisteredGlobalDb::db_path`).
    fn observation_store(
        sessions: &RegisteredGlobalDb,
    ) -> crate::store::GlobalDbObservationStore<'_> {
        crate::store::GlobalDbObservationStore::with_runtime(
            sessions.runtime(),
            sessions.authority(),
        )
    }

    /// Every test below registers under hook ids unique to that test: the
    /// authority registry is process-global, so overlapping keys would make
    /// parallel tests observe each other's entries.
    async fn project_runtime(
        temporary: &TempDir,
        project_id: &ProjectId,
    ) -> HostAdmissionTestRuntimeV1 {
        HostAdmissionTestRuntimeV1::project(
            temporary.path().join("profile"),
            temporary.path().join("project"),
            project_id.clone(),
        )
        .await
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

    #[tokio::test]
    async fn protected_replay_locator_re_resolves_the_authoritative_native_lifecycle() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.replay");
        let worktree_id = id::<WorktreeId>("worktree.native.replay");
        let runtime = HostAdmissionTestRuntimeV1::project(
            temporary.path().join("profile"),
            temporary.path().join("project"),
            project_id.clone(),
        )
        .await
        .unwrap();
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        observation_store(&sessions)
            .persist_observation(durable_native_observation(&project_id))
            .await
            .unwrap();
        let hook_project_id = [211; 16];
        let hook_worktree_id = [212; 16];
        assert!(register_context_scout_lifecycle_authority(
            hook_project_id,
            hook_worktree_id,
            project_id,
            worktree_id,
            &sessions,
        ));

        let session_id = lookup_registered_context_scout_native_session(
            hook_project_id,
            hook_worktree_id,
            crate::hooks::hook_v2_protected_session_id_for_native("session.native.codex"),
        )
        .await
        .unwrap();

        assert_eq!(session_id.as_str(), "session.native.codex");
    }

    /// The cap is a fail-closed bound, not a tuning knob: it must stay small
    /// enough to keep one hook lookup bounded, and `cap + 1` must remain a
    /// valid `i64` SQL `LIMIT` (the `try_from` in the lookup returns `None`
    /// otherwise, silently failing every lookup closed).
    #[test]
    fn session_observation_cap_stays_bounded_and_expressible_as_a_sql_limit() {
        assert_eq!(MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1, 64);
        assert!(i64::try_from(MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1 + 1).is_ok());
    }

    #[tokio::test]
    async fn zero_hook_identifiers_are_rejected_before_registration() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.zero-hook");
        let worktree_id = id::<WorktreeId>("worktree.native.zero-hook");
        let runtime = project_runtime(&temporary, &project_id).await;
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();

        assert!(!register_context_scout_lifecycle_authority(
            [0; 16],
            [71; 16],
            project_id.clone(),
            worktree_id.clone(),
            &sessions,
        ));
        assert!(!register_context_scout_lifecycle_authority(
            [71; 16],
            [0; 16],
            project_id,
            worktree_id,
            &sessions,
        ));
        assert!(!context_scout_lifecycle_authority_is_registered(
            [0; 16], [71; 16]
        ));
        assert!(!context_scout_lifecycle_authority_is_registered(
            [71; 16], [0; 16]
        ));
    }

    #[tokio::test]
    async fn profile_scoped_session_authority_is_rejected() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.profile-scope");
        let worktree_id = id::<WorktreeId>("worktree.native.profile-scope");
        let runtime = project_runtime(&temporary, &project_id).await;
        // A profile shard carries `StoreShardScopeV1::ProfileSessions`, which
        // can never authorize a project-scoped lifecycle lookup.
        let profile_sessions = runtime
            .registered_database_arc(HostAdmissionScope::Profile)
            .unwrap();

        assert!(!register_context_scout_lifecycle_authority(
            [41; 16],
            [42; 16],
            project_id,
            worktree_id,
            &profile_sessions,
        ));
        assert!(!context_scout_lifecycle_authority_is_registered(
            [41; 16], [42; 16]
        ));
    }

    #[tokio::test]
    async fn registration_rejects_a_project_that_the_authority_does_not_own() {
        let temporary = TempDir::new().unwrap();
        let bound_project_id = id::<ProjectId>("project.native.bound");
        let worktree_id = id::<WorktreeId>("worktree.native.bound");
        let runtime = project_runtime(&temporary, &bound_project_id).await;
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();

        assert!(!register_context_scout_lifecycle_authority(
            [43; 16],
            [44; 16],
            id::<ProjectId>("project.native.unowned"),
            worktree_id,
            &sessions,
        ));
        assert!(!context_scout_lifecycle_authority_is_registered(
            [43; 16], [44; 16]
        ));
    }

    #[tokio::test]
    async fn re_registration_is_idempotent_but_conflicting_identity_is_rejected() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.rereg");
        let worktree_id = id::<WorktreeId>("worktree.native.rereg");
        let runtime = project_runtime(&temporary, &project_id).await;
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();

        assert!(register_context_scout_lifecycle_authority(
            [31; 16],
            [32; 16],
            project_id.clone(),
            worktree_id.clone(),
            &sessions,
        ));
        assert!(register_context_scout_lifecycle_authority(
            [31; 16],
            [32; 16],
            project_id.clone(),
            worktree_id,
            &sessions,
        ));
        // Same hook key, different native worktree: the live entry wins and
        // the caller is told the registration did not take.
        assert!(!register_context_scout_lifecycle_authority(
            [31; 16],
            [32; 16],
            project_id,
            id::<WorktreeId>("worktree.native.rereg-conflict"),
            &sessions,
        ));
        assert!(context_scout_lifecycle_authority_is_registered(
            [31; 16], [32; 16]
        ));
    }

    #[tokio::test]
    async fn a_dropped_session_authority_is_evicted_by_the_next_registration() {
        let temporary = TempDir::new().unwrap();
        let dead_project_id = id::<ProjectId>("project.native.dead");
        {
            let runtime = project_runtime(&temporary, &dead_project_id).await;
            let sessions = runtime
                .registered_database_arc(HostAdmissionScope::Project)
                .unwrap();
            assert!(register_context_scout_lifecycle_authority(
                [11; 16],
                [12; 16],
                dead_project_id,
                id::<WorktreeId>("worktree.native.dead"),
                &sessions,
            ));
            // Asserted while the authority is still alive: `retain` never
            // evicts a live entry, so this cannot race a parallel test.
            assert!(context_scout_lifecycle_authority_is_registered(
                [11; 16], [12; 16]
            ));
            drop(sessions);
            drop(runtime);
        }

        let live_temporary = TempDir::new().unwrap();
        let live_project_id = id::<ProjectId>("project.native.live");
        let live_runtime = project_runtime(&live_temporary, &live_project_id).await;
        let live_sessions = live_runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        assert!(register_context_scout_lifecycle_authority(
            [13; 16],
            [14; 16],
            live_project_id,
            id::<WorktreeId>("worktree.native.live"),
            &live_sessions,
        ));

        assert!(
            !context_scout_lifecycle_authority_is_registered([11; 16], [12; 16]),
            "a registration must retain-evict authorities whose session store died"
        );
        assert!(context_scout_lifecycle_authority_is_registered(
            [13; 16], [14; 16]
        ));
    }

    #[tokio::test]
    async fn registered_lifecycle_lookup_resolves_the_authoritative_tuple() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.registered-lookup");
        let worktree_id = id::<WorktreeId>("worktree.native.registered-lookup");
        let runtime = project_runtime(&temporary, &project_id).await;
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        observation_store(&sessions)
            .persist_observation(durable_native_observation(&project_id))
            .await
            .unwrap();
        assert!(register_context_scout_lifecycle_authority(
            [21; 16],
            [22; 16],
            project_id,
            worktree_id,
            &sessions,
        ));
        let session_id = id::<SessionId>("session.native.codex");

        let lifecycle = lookup_registered_context_scout_lifecycle([21; 16], [22; 16], &session_id)
            .await
            .unwrap();

        assert_eq!(
            lifecycle.project_id.as_str(),
            "project.native.registered-lookup"
        );
        assert_eq!(
            lifecycle.worktree_id.as_str(),
            "worktree.native.registered-lookup"
        );
        assert_eq!(lifecycle.provider_id.as_str(), "codex");
        assert_eq!(lifecycle.session_id.as_str(), "session.native.codex");
        assert_eq!(lifecycle.thread_id.as_str(), "thread.native.codex");
        assert_eq!(lifecycle.turn_id.as_str(), "turn.native.codex");
        assert_eq!(lifecycle.agent_id.as_str(), "agent.native.codex");
        assert_eq!(
            lifecycle.logical_message_id.as_str(),
            "message.native.codex"
        );
        assert_eq!(
            lifecycle.profile_id.as_str(),
            sessions.binding().shard_id.profile_id.as_str()
        );

        // An unregistered hook pair resolves nothing, even though the same
        // observation is durable and the same session id is requested.
        assert!(
            lookup_registered_context_scout_lifecycle([51; 16], [52; 16], &session_id)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn lookup_rejects_a_binding_that_does_not_match_the_requested_identity() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.binding");
        let worktree_id = id::<WorktreeId>("worktree.native.binding");
        let runtime = project_runtime(&temporary, &project_id).await;
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        observation_store(&sessions)
            .persist_observation(durable_native_observation(&project_id))
            .await
            .unwrap();
        let bound_profile_id = sessions.binding().shard_id.profile_id.clone();
        let session_id = id::<SessionId>("session.native.codex");

        // Control: the matching identity does resolve, so the rejections
        // below are attributable to the binding checks and nothing else.
        assert!(
            lookup_context_scout_lifecycle(
                &bound_profile_id,
                &project_id,
                &worktree_id,
                &session_id,
                sessions.as_ref(),
            )
            .await
            .is_some()
        );
        assert!(
            lookup_context_scout_lifecycle(
                &UserProfileId::new("profile.native.not-bound").unwrap(),
                &project_id,
                &worktree_id,
                &session_id,
                sessions.as_ref(),
            )
            .await
            .is_none(),
            "a profile that does not own the shard must fail closed"
        );
        assert!(
            lookup_context_scout_lifecycle(
                &bound_profile_id,
                &id::<ProjectId>("project.native.not-bound"),
                &worktree_id,
                &session_id,
                sessions.as_ref(),
            )
            .await
            .is_none(),
            "a project the shard is not scoped to must fail closed"
        );
    }

    #[tokio::test]
    async fn durable_scope_and_session_mismatches_fail_closed() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.durable-scope");
        let worktree_id = id::<WorktreeId>("worktree.native.durable-scope");
        let runtime = project_runtime(&temporary, &project_id).await;
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        // Durable evidence carrying a foreign project scope, stored in the
        // shard that the lookup is otherwise authorized against.
        observation_store(&sessions)
            .persist_observation(durable_native_observation(&id::<ProjectId>(
                "project.native.foreign-scope",
            )))
            .await
            .unwrap();
        let bound_profile_id = sessions.binding().shard_id.profile_id.clone();

        assert!(
            lookup_context_scout_lifecycle(
                &bound_profile_id,
                &project_id,
                &worktree_id,
                &id::<SessionId>("session.native.codex"),
                sessions.as_ref(),
            )
            .await
            .is_none(),
            "an observation scoped to another project must never satisfy this lookup"
        );
        assert!(
            lookup_context_scout_lifecycle(
                &bound_profile_id,
                &project_id,
                &worktree_id,
                &id::<SessionId>("session.native.absent"),
                sessions.as_ref(),
            )
            .await
            .is_none(),
            "a session with no durable evidence must fail closed"
        );
    }

    #[tokio::test]
    async fn zero_protected_session_id_is_rejected_even_with_a_live_authority() {
        let temporary = TempDir::new().unwrap();
        let project_id = id::<ProjectId>("project.native.zero-protected");
        let runtime = project_runtime(&temporary, &project_id).await;
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        observation_store(&sessions)
            .persist_observation(durable_native_observation(&project_id))
            .await
            .unwrap();
        assert!(register_context_scout_lifecycle_authority(
            [61; 16],
            [62; 16],
            project_id,
            id::<WorktreeId>("worktree.native.zero-protected"),
            &sessions,
        ));

        assert!(
            lookup_registered_context_scout_native_session([61; 16], [62; 16], [0; 32])
                .await
                .is_none(),
            "an all-zero protected locator is never a resolvable session"
        );
    }
}

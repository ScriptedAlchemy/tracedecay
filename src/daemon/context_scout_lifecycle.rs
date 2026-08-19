//! Exact Context Scout lifecycle lookup from canonical durable observations.

use std::collections::BTreeMap;
use std::sync::{Mutex as StdMutex, OnceLock};

use tracedecay_domain::{
    AgentInstanceId, CanonicalObservationEnvelopeV1, DurableObservationV1, MessageId,
    ObservationScopeV1, ProjectId, SessionId, ThreadId, TurnId, UserProfileId, WorktreeId,
};
use tracedecay_store::StoreShardScopeV1;

use crate::agents::context_scout_ports::ContextScoutLifecycleAddressV1;
use crate::global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};

const MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1: usize = 64;

type ContextScoutLifecycleKeyV1 = ([u8; 16], [u8; 16]);

struct RegisteredContextScoutLifecycleAuthorityV1 {
    profile_id: UserProfileId,
    project_id: ProjectId,
    worktree_id: WorktreeId,
    sessions: RegisteredGlobalDbLeaseV1,
}

type ContextScoutLifecycleAuthoritiesV1 =
    StdMutex<BTreeMap<ContextScoutLifecycleKeyV1, RegisteredContextScoutLifecycleAuthorityV1>>;

fn registered_context_scout_lifecycle_authorities() -> &'static ContextScoutLifecycleAuthoritiesV1 {
    static AUTHORITIES: OnceLock<ContextScoutLifecycleAuthoritiesV1> = OnceLock::new();
    AUTHORITIES.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

/// Why a lifecycle authority registration was never admissible.
///
/// Distinct from [`AuthorityRegistrationV1::Conflict`]: nothing about the
/// existing registry contents could have made these requests succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityRejectionV1 {
    ZeroHookProjectId,
    ZeroHookWorktreeId,
    InvalidProjectId,
    InvalidWorktreeId,
    NonProjectSessionScope,
    ProjectNotOwnedByAuthority,
}

impl AuthorityRejectionV1 {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ZeroHookProjectId => "zero_hook_project_id",
            Self::ZeroHookWorktreeId => "zero_hook_worktree_id",
            Self::InvalidProjectId => "invalid_project_id",
            Self::InvalidWorktreeId => "invalid_worktree_id",
            Self::NonProjectSessionScope => "non_project_session_scope",
            Self::ProjectNotOwnedByAuthority => "project_not_owned_by_authority",
        }
    }
}

/// Outcome of registering a Context Scout lifecycle authority.
///
/// Only [`Self::Conflict`] means "a different live authority already owns
/// this hook key", which is an operator-visible misconfiguration rather than
/// a validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityRegistrationV1 {
    /// A fresh authority was installed for this hook key.
    Registered,
    /// The live authority already binds exactly this identity and store.
    AlreadyRegistered,
    /// A different live authority owns this hook key; the request was refused
    /// and the incumbent kept.
    Conflict,
    /// The request could never have been admitted.
    Rejected(AuthorityRejectionV1),
}

pub(crate) fn register_context_scout_lifecycle_authority(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    project_id: ProjectId,
    worktree_id: WorktreeId,
    sessions: &RegisteredGlobalDbLeaseV1,
) -> AuthorityRegistrationV1 {
    if hook_project_id == [0; 16] {
        return AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::ZeroHookProjectId);
    }
    if hook_worktree_id == [0; 16] {
        return AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::ZeroHookWorktreeId);
    }
    if project_id.validate().is_err() {
        return AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::InvalidProjectId);
    }
    if worktree_id.validate().is_err() {
        return AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::InvalidWorktreeId);
    }
    let profile_id = sessions.binding().shard_id.profile_id.clone();
    let Some(authority_project_id) = (match &sessions.binding().shard_id.scope {
        StoreShardScopeV1::ProjectSessions { project_id } => Some(project_id),
        _ => None,
    }) else {
        return AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::NonProjectSessionScope);
    };
    if authority_project_id != &project_id {
        return AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::ProjectNotOwnedByAuthority);
    }
    let key = (hook_project_id, hook_worktree_id);
    let mut authorities = registered_context_scout_lifecycle_authorities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = authorities.get(&key) {
        let identical = existing.profile_id == profile_id
            && existing.project_id == project_id
            && existing.worktree_id == worktree_id
            && existing.sessions.shares_client_with(sessions);
        return if identical {
            AuthorityRegistrationV1::AlreadyRegistered
        } else {
            AuthorityRegistrationV1::Conflict
        };
    }
    authorities.insert(
        key,
        RegisteredContextScoutLifecycleAuthorityV1 {
            profile_id,
            project_id,
            worktree_id,
            sessions: sessions.clone(),
        },
    );
    AuthorityRegistrationV1::Registered
}

/// Removes exactly the given session store's registration; a different live
/// authority under the same locator pair is left untouched so a rolled-back
/// advisory setup can never unregister its successor.
pub(crate) fn unregister_context_scout_lifecycle_authority(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    sessions: &RegisteredGlobalDbLeaseV1,
) -> bool {
    let key = (hook_project_id, hook_worktree_id);
    let mut authorities = registered_context_scout_lifecycle_authorities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if authorities
        .get(&key)
        .is_some_and(|existing| existing.sessions.shares_client_with(sessions))
    {
        authorities.remove(&key);
        return true;
    }
    false
}

/// Looks up the registered lifecycle authority for a hook-scoped
/// (project, worktree) pair.
///
/// Shared prologue for both replay entry points below: resolve the
/// native profile/project/worktree identity and a session lease, or
/// `None` if no authority is registered.
fn resolve_authority(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
) -> Option<(
    UserProfileId,
    ProjectId,
    WorktreeId,
    RegisteredGlobalDbLeaseV1,
)> {
    let key = (hook_project_id, hook_worktree_id);
    let authorities = registered_context_scout_lifecycle_authorities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(authority) = authorities.get(&key) else {
        tracing::debug!(
            target: "tracedecay::context_scout_lifecycle",
            reason = "no_registered_authority",
            "Context Scout lifecycle resolution failed closed"
        );
        return None;
    };
    Some((
        authority.profile_id.clone(),
        authority.project_id.clone(),
        authority.worktree_id.clone(),
        authority.sessions.clone(),
    ))
}

pub(crate) async fn lookup_registered_context_scout_lifecycle(
    hook_project_id: [u8; 16],
    hook_worktree_id: [u8; 16],
    session_id: &SessionId,
) -> Option<ContextScoutLifecycleAddressV1> {
    let (profile_id, project_id, worktree_id, sessions) =
        resolve_authority(hook_project_id, hook_worktree_id)?;
    lookup_context_scout_lifecycle(&profile_id, &project_id, &worktree_id, session_id, &sessions)
        .await
        .into_address()
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
            if crate::hooks::protected_native_session_id(&raw_session_id) != protected_session_id {
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
    lookup_context_scout_lifecycle(&profile_id, &project_id, &worktree_id, &session_id, &sessions)
        .await
        .is_resolved()
        .then_some(session_id)
}

/// Why an exact lifecycle lookup resolved nothing.
///
/// Every reason fails closed for the `Option`-shaped callers, but an
/// unauthorized shard binding, a corrupt durable row, a blown evidence
/// budget, and "this session simply has no complete tuple yet" stay
/// distinguishable in logs and in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextScoutLifecycleLookupFailureV1 {
    InvalidProfileId,
    InvalidProjectId,
    InvalidWorktreeId,
    InvalidSessionId,
    /// The session store is not the authority for the requested identity.
    UnauthorizedBinding,
    SnapshotUnavailable,
    ObservationQueryFailed,
    ObservationRowUnreadable,
    /// More session evidence than the bounded scan admits.
    ObservationBudgetExceeded,
    MalformedDurableObservation,
    /// Durable evidence bound another project scope or another session.
    DurableScopeMismatch,
    MalformedCanonicalEnvelope,
    /// The canonical payload disagreed with its own durable source identity.
    CanonicalEnvelopeMismatch,
    /// Well-formed evidence exists, but none carries a complete tuple.
    NoCompleteLifecycle,
}

impl ContextScoutLifecycleLookupFailureV1 {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidProfileId => "invalid_profile_id",
            Self::InvalidProjectId => "invalid_project_id",
            Self::InvalidWorktreeId => "invalid_worktree_id",
            Self::InvalidSessionId => "invalid_session_id",
            Self::UnauthorizedBinding => "unauthorized_binding",
            Self::SnapshotUnavailable => "snapshot_unavailable",
            Self::ObservationQueryFailed => "observation_query_failed",
            Self::ObservationRowUnreadable => "observation_row_unreadable",
            Self::ObservationBudgetExceeded => "observation_budget_exceeded",
            Self::MalformedDurableObservation => "malformed_durable_observation",
            Self::DurableScopeMismatch => "durable_scope_mismatch",
            Self::MalformedCanonicalEnvelope => "malformed_canonical_envelope",
            Self::CanonicalEnvelopeMismatch => "canonical_envelope_mismatch",
            Self::NoCompleteLifecycle => "no_complete_lifecycle",
        }
    }
}

/// Outcome of one exact Context Scout lifecycle lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ContextScoutLifecycleLookupV1 {
    /// Boxed so the resolved tuple's ~216 bytes do not travel with every
    /// one-byte failure reason.
    Resolved(Box<ContextScoutLifecycleAddressV1>),
    Unresolved(ContextScoutLifecycleLookupFailureV1),
}

impl ContextScoutLifecycleLookupV1 {
    /// Collapses to the fail-closed `Option`, discarding the reason (which
    /// the lookup already emitted to tracing).
    pub(crate) fn into_address(self) -> Option<ContextScoutLifecycleAddressV1> {
        match self {
            Self::Resolved(address) => Some(*address),
            Self::Unresolved(_) => None,
        }
    }

    pub(crate) const fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved(_))
    }
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
) -> ContextScoutLifecycleLookupV1 {
    match lookup_context_scout_lifecycle_inner(
        profile_id,
        project_id,
        worktree_id,
        session_id,
        sessions,
    )
    .await
    {
        Ok(address) => ContextScoutLifecycleLookupV1::Resolved(Box::new(address)),
        Err(reason) => {
            tracing::debug!(
                target: "tracedecay::context_scout_lifecycle",
                reason = reason.as_str(),
                session_id = session_id.as_str(),
                "Context Scout lifecycle lookup failed closed"
            );
            ContextScoutLifecycleLookupV1::Unresolved(reason)
        }
    }
}

/// Fail-closed body of [`lookup_context_scout_lifecycle`].
///
/// Every early return names the evidence that failed instead of collapsing
/// into a bare `None`; the caller-visible contract is still "anything but
/// `Ok` resolves nothing".
async fn lookup_context_scout_lifecycle_inner(
    profile_id: &UserProfileId,
    project_id: &ProjectId,
    worktree_id: &WorktreeId,
    session_id: &SessionId,
    sessions: &RegisteredGlobalDb,
) -> std::result::Result<ContextScoutLifecycleAddressV1, ContextScoutLifecycleLookupFailureV1> {
    use ContextScoutLifecycleLookupFailureV1 as Failure;

    profile_id
        .validate()
        .map_err(|_| Failure::InvalidProfileId)?;
    project_id
        .validate()
        .map_err(|_| Failure::InvalidProjectId)?;
    worktree_id
        .validate()
        .map_err(|_| Failure::InvalidWorktreeId)?;
    session_id
        .validate()
        .map_err(|_| Failure::InvalidSessionId)?;
    let binding = sessions.binding();
    if &binding.shard_id.profile_id != profile_id
        || !matches!(
            &binding.shard_id.scope,
            StoreShardScopeV1::ProjectSessions {
                project_id: bound_project_id
            } if bound_project_id == project_id
        )
    {
        return Err(Failure::UnauthorizedBinding);
    }

    let snapshot = sessions
        .read_snapshot()
        .await
        .map_err(|_| Failure::SnapshotUnavailable)?;
    let limit = i64::try_from(MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1 + 1)
        .map_err(|_| Failure::ObservationBudgetExceeded)?;
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
            crate::db::engine::params![session_id.as_str(), limit],
        )
        .await
        .map_err(|_| Failure::ObservationQueryFailed)?;

    let project_scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let mut count = 0usize;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| Failure::ObservationRowUnreadable)?
    {
        count = count
            .checked_add(1)
            .ok_or(Failure::ObservationBudgetExceeded)?;
        if count > MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1 {
            return Err(Failure::ObservationBudgetExceeded);
        }
        let observation_json = row
            .get::<String>(0)
            .map_err(|_| Failure::ObservationRowUnreadable)?;
        let durable = serde_json::from_str::<DurableObservationV1>(&observation_json)
            .map_err(|_| Failure::MalformedDurableObservation)?;
        if durable.scope() != &project_scope || durable.source().session_id() != session_id {
            return Err(Failure::DurableScopeMismatch);
        }
        let envelope =
            serde_json::from_value::<CanonicalObservationEnvelopeV1>(durable.payload().clone())
                .map_err(|_| Failure::MalformedCanonicalEnvelope)?;
        if envelope.provider() != durable.source().provider()
            || envelope.relations().session_id() != session_id
        {
            return Err(Failure::CanonicalEnvelopeMismatch);
        }
        if let Some(candidate) =
            lifecycle_from_canonical(profile_id, project_id, worktree_id, session_id, &envelope)
        {
            return Ok(candidate);
        }
    }
    Err(Failure::NoCompleteLifecycle)
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
mod tests;

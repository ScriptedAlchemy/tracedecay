use std::fmt;

use tracedecay_application::RequestContext;
use tracedecay_domain::{
    ActorId, CursorManifestLimitKindV1, RetrievalGrainV1, SessionId, TemporalModeV1,
};
pub use tracedecay_global_db::session_temporal::execution::SessionDataFreshness;
pub use tracedecay_sessions::{
    AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionRetrievalScope,
};

use crate::context::{
    CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, RequestBudgets,
    ResolvedSessionIdentity, SessionOwner, session_application_grant_digest,
};

/// Typed authority for the already-resolved session store/root.
///
/// Root-wide authorization binds this value, never the compatibility anchor
/// session used by legacy callers to construct a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedSessionRoot {
    identity: ResolvedSessionIdentity,
}

impl AuthorizedSessionRoot {
    pub fn from_identity(identity: &ResolvedSessionIdentity) -> Self {
        Self {
            identity: identity.clone(),
        }
    }

    pub fn identity(&self) -> &ResolvedSessionIdentity {
        &self.identity
    }
}

/// Session-specific bindings that supplement the transport-neutral application
/// context without creating a second request-context authority.
#[derive(Clone, Debug)]
pub struct SessionRequestBinding {
    identity: ResolvedSessionIdentity,
    capability_digest: CapabilityDigest,
    policy_digest: PolicyDigest,
    configuration_digest: ConfigurationDigest,
    cancellation: CancellationToken,
    budgets: RequestBudgets,
}

impl SessionRequestBinding {
    pub fn new(
        identity: ResolvedSessionIdentity,
        capability_digest: CapabilityDigest,
        policy_digest: PolicyDigest,
        configuration_digest: ConfigurationDigest,
        cancellation: CancellationToken,
        budgets: RequestBudgets,
    ) -> Self {
        Self {
            identity,
            capability_digest,
            policy_digest,
            configuration_digest,
            cancellation,
            budgets,
        }
    }

    pub fn validate_context(
        &self,
        context: &RequestContext,
    ) -> Result<(), SessionAuthorizationError> {
        let scope = self
            .identity
            .session_request_scope()
            .map_err(|_| SessionAuthorizationError::UnresolvedApplicationScope)?;
        if &scope != context.scope() {
            return Err(SessionAuthorizationError::WrongScope);
        }
        if self.cancellation.application_token_id()
            != Some(context.cancellation().token_id.as_str())
        {
            return Err(SessionAuthorizationError::WrongContext);
        }
        let digest = session_application_grant_digest(
            self.capability_digest,
            self.policy_digest,
            self.configuration_digest,
            &self.cancellation,
            self.budgets,
        )
        .map_err(|_| SessionAuthorizationError::WrongContext)?;
        if digest != context.grant().digest {
            return Err(SessionAuthorizationError::WrongContext);
        }
        Ok(())
    }

    pub fn identity(&self) -> &ResolvedSessionIdentity {
        &self.identity
    }

    pub const fn capability_digest(&self) -> CapabilityDigest {
        self.capability_digest
    }

    pub const fn policy_digest(&self) -> PolicyDigest {
        self.policy_digest
    }

    pub const fn configuration_digest(&self) -> ConfigurationDigest {
        self.configuration_digest
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub const fn budgets(&self) -> RequestBudgets {
        self.budgets
    }

    fn matches(&self, other: &Self) -> bool {
        self.identity == other.identity
            && self.capability_digest == other.capability_digest
            && self.policy_digest == other.policy_digest
            && self.configuration_digest == other.configuration_digest
            && self.cancellation.is_same_token(&other.cancellation)
            && self.budgets == other.budgets
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionScopeAuthorizationRequest {
    actor_id: ActorId,
    identity: ResolvedSessionIdentity,
    session_id: SessionId,
    authorized_root: AuthorizedSessionRoot,
    retrieval_scope: SessionRetrievalScope,
    provider_scope: Option<String>,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    access: SessionAccess,
}

impl SessionScopeAuthorizationRequest {
    pub fn new(
        actor_id: ActorId,
        identity: ResolvedSessionIdentity,
        session_id: SessionId,
        provider_scope: Option<String>,
        temporal_mode: TemporalModeV1,
        grain: RetrievalGrainV1,
        access: SessionAccess,
    ) -> Result<Self, SessionAuthorizationError> {
        if provider_scope.as_deref().is_some_and(|provider| {
            provider.is_empty()
                || provider.trim() != provider
                || provider.len() > 512
                || provider.chars().any(char::is_control)
        }) {
            return Err(SessionAuthorizationError::InvalidProviderScope);
        }
        let retrieval_scope = SessionRetrievalScope::Session(session_id.clone());
        let authorized_root = AuthorizedSessionRoot::from_identity(&identity);
        Ok(Self {
            actor_id,
            identity,
            session_id,
            authorized_root,
            retrieval_scope,
            provider_scope,
            temporal_mode,
            grain,
            access,
        })
    }

    #[must_use]
    pub fn with_retrieval_scope(mut self, retrieval_scope: SessionRetrievalScope) -> Self {
        if let SessionRetrievalScope::Session(session_id) = &retrieval_scope {
            self.session_id = session_id.clone();
        }
        self.retrieval_scope = retrieval_scope;
        self
    }

    pub fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub fn identity(&self) -> &ResolvedSessionIdentity {
        &self.identity
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn authorized_root(&self) -> &AuthorizedSessionRoot {
        &self.authorized_root
    }

    pub fn retrieval_scope(&self) -> &SessionRetrievalScope {
        &self.retrieval_scope
    }

    pub fn provider_scope(&self) -> Option<&str> {
        self.provider_scope.as_deref()
    }

    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.temporal_mode
    }

    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    pub const fn access(&self) -> SessionAccess {
        self.access
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedSessionScope {
    actor_id: ActorId,
    identity: ResolvedSessionIdentity,
    session_id: Option<SessionId>,
    authorized_root: AuthorizedSessionRoot,
    retrieval_scope: SessionRetrievalScope,
    provider_scope: Option<String>,
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    access: SessionAccess,
}

impl AuthorizedSessionScope {
    fn from_request(request: &SessionScopeAuthorizationRequest) -> Self {
        Self {
            actor_id: request.actor_id.clone(),
            identity: request.identity.clone(),
            session_id: request.retrieval_scope.session_id().cloned(),
            authorized_root: request.authorized_root.clone(),
            retrieval_scope: request.retrieval_scope.clone(),
            provider_scope: request.provider_scope.clone(),
            temporal_mode: request.temporal_mode,
            grain: request.grain,
            access: request.access,
        }
    }

    pub fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub fn identity(&self) -> &ResolvedSessionIdentity {
        &self.identity
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        self.session_id.as_ref()
    }

    pub fn authorized_root(&self) -> &AuthorizedSessionRoot {
        &self.authorized_root
    }

    pub fn retrieval_scope(&self) -> &SessionRetrievalScope {
        &self.retrieval_scope
    }

    pub fn provider_scope(&self) -> Option<&str> {
        self.provider_scope.as_deref()
    }

    pub const fn temporal_mode(&self) -> TemporalModeV1 {
        self.temporal_mode
    }

    pub const fn grain(&self) -> RetrievalGrainV1 {
        self.grain
    }

    pub const fn access(&self) -> SessionAccess {
        self.access
    }
}

#[derive(Clone, Debug)]
pub struct SessionAuthorizationGrant {
    id: AuthorizationGrantId,
    revision: u64,
    scope: AuthorizedSessionScope,
    context: RequestContext,
    binding: SessionRequestBinding,
}

impl SessionAuthorizationGrant {
    /// Issues a grant after an authorizer accepts the exact resolved scope.
    pub fn issue(
        id: AuthorizationGrantId,
        revision: u64,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<Self, SessionAuthorizationError> {
        if revision == 0 {
            return Err(SessionAuthorizationError::ZeroRevision);
        }
        binding.validate_context(context)?;
        if request.actor_id() != context.actor() {
            return Err(SessionAuthorizationError::WrongContext);
        }
        if request.identity() != binding.identity() {
            return Err(SessionAuthorizationError::WrongScope);
        }
        if request.authorized_root().identity() != binding.identity() {
            return Err(SessionAuthorizationError::WrongScope);
        }
        if matches!(request.identity().owner(), SessionOwner::Project { .. })
            && request.identity().git_route().is_none()
        {
            return Err(SessionAuthorizationError::UnresolvedGitRoute);
        }

        Ok(Self {
            id,
            revision,
            scope: AuthorizedSessionScope::from_request(request),
            context: context.clone(),
            binding: binding.clone(),
        })
    }

    pub fn validate(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<(), SessionAuthorizationError> {
        binding.validate_context(context)?;
        if self.scope.actor_id() != context.actor() || request.actor_id() != context.actor() {
            return Err(SessionAuthorizationError::WrongContext);
        }
        if self.scope.identity() != binding.identity()
            || request.identity() != binding.identity()
            || self.scope.authorized_root() != request.authorized_root()
            || request.authorized_root().identity() != binding.identity()
        {
            return Err(SessionAuthorizationError::WrongScope);
        }
        if &self.context != context || !self.binding.matches(binding) {
            return Err(SessionAuthorizationError::WrongContext);
        }
        if !self
            .scope
            .access()
            .matches_requested_access(request.access())
        {
            return Err(SessionAuthorizationError::WrongAccess);
        }
        if !self
            .scope
            .retrieval_scope()
            .matches_requested_scope(request.retrieval_scope())
            || self.scope.provider_scope() != request.provider_scope()
            || self.scope.temporal_mode() != request.temporal_mode()
            || self.scope.grain() != request.grain()
        {
            return Err(SessionAuthorizationError::WrongTarget);
        }
        Ok(())
    }

    pub fn id(&self) -> &AuthorizationGrantId {
        &self.id
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn scope(&self) -> &AuthorizedSessionScope {
        &self.scope
    }

    pub const fn capability_digest(&self) -> CapabilityDigest {
        self.binding.capability_digest()
    }

    pub const fn policy_digest(&self) -> PolicyDigest {
        self.binding.policy_digest()
    }

    pub const fn configuration_digest(&self) -> ConfigurationDigest {
        self.binding.configuration_digest()
    }

    pub fn deadline(&self) -> &tracedecay_application::Deadline {
        self.context.deadline()
    }

    pub fn cancellation(&self) -> &CancellationToken {
        self.binding.cancellation()
    }

    pub const fn budgets(&self) -> RequestBudgets {
        self.binding.budgets()
    }
}

pub trait SessionScopeAuthorizer {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFreshnessPolicy {
    AllowStored,
    RequireFresh,
}

impl SessionFreshnessPolicy {
    pub const fn accepts(self, freshness: SessionDataFreshness) -> bool {
        matches!(
            (self, freshness),
            (Self::AllowStored, _) | (_, SessionDataFreshness::Fresh)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRetrievalTarget {
    Scope,
    Session(SessionId),
}

#[derive(Clone, Debug)]
pub struct SessionRetrievalRequest {
    grant: SessionAuthorizationGrant,
    target: SessionRetrievalTarget,
    freshness_policy: SessionFreshnessPolicy,
    limit: u64,
}

impl SessionRetrievalRequest {
    pub fn new(
        grant: SessionAuthorizationGrant,
        target: SessionRetrievalTarget,
        freshness_policy: SessionFreshnessPolicy,
        limit: u64,
    ) -> Result<Self, SessionRetrievalError> {
        if limit == 0 {
            return Err(SessionRetrievalError::ZeroLimit);
        }
        if limit > grant.budgets().max_results() {
            return Err(SessionRetrievalError::LimitExceedsGrant);
        }
        let target_is_authorized = match &target {
            SessionRetrievalTarget::Scope => grant
                .scope()
                .retrieval_scope()
                .matches_authorized_root_target(),
            SessionRetrievalTarget::Session(session_id) => grant
                .scope()
                .retrieval_scope()
                .matches_session_target(session_id),
        };
        if !target_is_authorized {
            return Err(SessionRetrievalError::TargetOutsideGrant);
        }
        Ok(Self {
            grant,
            target,
            freshness_policy,
            limit,
        })
    }

    pub fn grant(&self) -> &SessionAuthorizationGrant {
        &self.grant
    }

    pub fn target(&self) -> &SessionRetrievalTarget {
        &self.target
    }

    pub const fn freshness_policy(&self) -> SessionFreshnessPolicy {
        self.freshness_policy
    }

    pub const fn limit(&self) -> u64 {
        self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRetrievalOutcome<T> {
    Complete {
        items: Vec<T>,
        freshness: SessionDataFreshness,
    },
    CompleteZero {
        freshness: SessionDataFreshness,
    },
    Stale {
        freshness: SessionDataFreshness,
    },
    Partial {
        items: Vec<T>,
        freshness: SessionDataFreshness,
        omitted: u64,
    },
    WrongScope,
    Locked,
    Redacted,
    Deleted,
    Denied,
    Unavailable,
    CursorManifestLimitExceeded {
        kind: CursorManifestLimitKindV1,
        observed: usize,
        maximum: usize,
    },
    BudgetExhausted,
    Cancelled,
}

impl<T> SessionRetrievalOutcome<T> {
    pub fn complete(
        items: Vec<T>,
        freshness: SessionDataFreshness,
    ) -> Result<Self, SessionRetrievalError> {
        if items.is_empty() {
            return Err(SessionRetrievalError::EmptyComplete);
        }
        Ok(Self::Complete { items, freshness })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRetrievalError {
    ZeroLimit,
    LimitExceedsGrant,
    TargetOutsideGrant,
    EmptyComplete,
}

impl fmt::Display for SessionRetrievalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroLimit => "session retrieval limit must be greater than zero",
            Self::LimitExceedsGrant => "session retrieval limit exceeds the authorized budget",
            Self::TargetOutsideGrant => {
                "session retrieval target is outside the authorized grant scope"
            }
            Self::EmptyComplete => "complete session retrieval must contain at least one item",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SessionRetrievalError {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId,
    };
    use tracedecay_domain::{
        ActorId, ProjectId, RepositoryId, RetrievalGrainV1,
        SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS, SessionId, SessionSourceCoverageReceiptV1,
        SessionSourceCoverageV1, SessionSourceFrontierV1, SessionSourceIdV1,
        SessionTemporalCoverageRequestV1, TemporalModeV1, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;
    use crate::context::{
        BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest,
        ProfileId, RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId,
        SessionStoreId,
    };

    const DIGEST: [u8; 32] = [0xa5; 32];

    struct AllowAuthorizer;

    impl SessionScopeAuthorizer for AllowAuthorizer {
        fn authorize(
            &self,
            context: &RequestContext,
            binding: &SessionRequestBinding,
            request: &SessionScopeAuthorizationRequest,
        ) -> Result<SessionAuthorizationGrant, SessionAuthorizationError> {
            SessionAuthorizationGrant::issue(
                AuthorizationGrantId::new("grant.session.read").unwrap(),
                7,
                context,
                binding,
                request,
            )
        }
    }

    struct TestRequestContext {
        request: RequestContext,
        binding: SessionRequestBinding,
    }

    impl TestRequestContext {
        fn binding(&self) -> &SessionRequestBinding {
            &self.binding
        }

        fn actor_id(&self) -> &ActorId {
            self.request.actor()
        }

        fn identity(&self) -> &ResolvedSessionIdentity {
            self.binding.identity()
        }

        fn capability_digest(&self) -> CapabilityDigest {
            self.binding.capability_digest()
        }

        fn policy_digest(&self) -> PolicyDigest {
            self.binding.policy_digest()
        }

        fn configuration_digest(&self) -> ConfigurationDigest {
            self.binding.configuration_digest()
        }
    }

    impl std::ops::Deref for TestRequestContext {
        type Target = RequestContext;

        fn deref(&self) -> &Self::Target {
            &self.request
        }
    }

    fn context() -> TestRequestContext {
        context_with_digests(DIGEST, DIGEST, DIGEST)
    }

    fn context_with_digests(
        capability_digest: [u8; 32],
        policy_digest: [u8; 32],
        configuration_digest: [u8; 32],
    ) -> TestRequestContext {
        context_for_actor(
            "actor.cursor",
            capability_digest,
            policy_digest,
            configuration_digest,
        )
    }

    fn context_for_actor(
        actor_id: &str,
        capability_digest: [u8; 32],
        policy_digest: [u8; 32],
        configuration_digest: [u8; 32],
    ) -> TestRequestContext {
        let actor = ActorId::new(actor_id).unwrap();
        let request_id = RequestId::new("request.session.read").unwrap();
        let identity = ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.project.tracedecay").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.application-slice-1").unwrap(),
            ),
        );
        let scope = identity.application_scope().unwrap();
        let capability = CapabilityDigest::new(capability_digest);
        let policy = PolicyDigest::new(policy_digest);
        let configuration = ConfigurationDigest::new(configuration_digest);
        let cancellation = CancellationToken::for_application_request(request_id.as_str());
        let budgets = RequestBudgets::new(64, 4096, 16).unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.session.application").unwrap(),
            1,
            session_application_grant_digest(
                capability,
                policy,
                configuration,
                &cancellation,
                budgets,
            )
            .unwrap(),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(i64::MAX - 1),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.session.temporal-retrieval").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.session.temporal-retrieval").unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        let request = RequestContext::new(
            actor,
            scope,
            grant,
            request_id.clone(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active(cancellation.application_token_id().unwrap()).unwrap(),
        )
        .unwrap();
        let binding = SessionRequestBinding::new(
            identity,
            capability,
            policy,
            configuration,
            cancellation,
            budgets,
        );
        TestRequestContext { request, binding }
    }

    fn exact_authorization_request(
        identity: ResolvedSessionIdentity,
    ) -> SessionScopeAuthorizationRequest {
        SessionScopeAuthorizationRequest::new(
            ActorId::new("actor.cursor").unwrap(),
            identity,
            SessionId::new("session.application-slice-1").unwrap(),
            Some("cursor".to_owned()),
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(1_234_567),
            },
            RetrievalGrainV1::LogicalMessage,
            SessionAccess::Hydrate,
        )
        .unwrap()
    }

    #[test]
    fn grant_binds_the_exact_typed_retrieval_target_without_serialization() {
        let context = context();
        let request = exact_authorization_request(context.identity().clone());
        let grant = AllowAuthorizer
            .authorize(&context, context.binding(), &request)
            .unwrap();

        assert_eq!(grant.id().as_str(), "grant.session.read");
        assert_eq!(grant.revision(), 7);
        assert_eq!(grant.scope().actor_id(), context.actor_id());
        assert_eq!(grant.scope().identity(), context.identity());
        assert_eq!(
            grant.scope().session_id().unwrap().as_str(),
            "session.application-slice-1"
        );
        assert_eq!(grant.scope().provider_scope(), Some("cursor"));
        assert_eq!(
            grant.scope().temporal_mode(),
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(1_234_567)
            }
        );
        assert_eq!(grant.scope().grain(), RetrievalGrainV1::LogicalMessage);
        assert_eq!(grant.scope().access(), SessionAccess::Hydrate);
        assert_eq!(grant.capability_digest(), context.capability_digest());
        assert_eq!(grant.policy_digest(), context.policy_digest());
        assert_eq!(grant.configuration_digest(), context.configuration_digest());
        assert_eq!(
            grant.validate(&context, context.binding(), &request),
            Ok(())
        );
    }

    #[test]
    fn root_wide_grant_binds_the_authorized_root_without_binding_anchor_session() {
        let context = context();
        let request = exact_authorization_request(context.identity().clone())
            .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot);
        let grant = AllowAuthorizer
            .authorize(&context, context.binding(), &request)
            .unwrap();

        assert_eq!(grant.scope().session_id(), None);
        assert_eq!(
            grant.scope().authorized_root().identity(),
            context.identity()
        );
        assert_eq!(
            grant.scope().retrieval_scope(),
            &SessionRetrievalScope::AllSessionsInAuthorizedRoot
        );
        assert_eq!(
            grant.validate(&context, context.binding(), &request),
            Ok(())
        );

        let other_anchor = SessionScopeAuthorizationRequest::new(
            context.actor_id().clone(),
            context.identity().clone(),
            SessionId::new("session.other-anchor").unwrap(),
            request.provider_scope().map(str::to_owned),
            request.temporal_mode(),
            request.grain(),
            request.access(),
        )
        .unwrap()
        .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot);
        assert_eq!(
            grant.validate(&context, context.binding(), &other_anchor),
            Ok(())
        );
        assert_eq!(
            grant.scope().authorized_root(),
            &AuthorizedSessionRoot::from_identity(other_anchor.identity())
        );

        let exact = exact_authorization_request(context.identity().clone());
        assert_eq!(
            grant.validate(&context, context.binding(), &exact),
            Err(SessionAuthorizationError::WrongTarget)
        );
    }

    #[test]
    fn grant_validation_rejects_every_target_access_and_root_mutation() {
        let context = context();
        let request = exact_authorization_request(context.identity().clone());
        let grant = AllowAuthorizer
            .authorize(&context, context.binding(), &request)
            .unwrap();
        let target_mutations = [
            SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                context.identity().clone(),
                SessionId::new("session.other").unwrap(),
                Some("cursor".to_owned()),
                request.temporal_mode(),
                request.grain(),
                request.access(),
            )
            .unwrap(),
            SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                context.identity().clone(),
                request.session_id().clone(),
                None,
                request.temporal_mode(),
                request.grain(),
                request.access(),
            )
            .unwrap(),
            SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                context.identity().clone(),
                request.session_id().clone(),
                Some("claude".to_owned()),
                request.temporal_mode(),
                request.grain(),
                request.access(),
            )
            .unwrap(),
            SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                context.identity().clone(),
                request.session_id().clone(),
                Some("cursor".to_owned()),
                TemporalModeV1::Current,
                request.grain(),
                request.access(),
            )
            .unwrap(),
            SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                context.identity().clone(),
                request.session_id().clone(),
                Some("cursor".to_owned()),
                TemporalModeV1::AsOf {
                    cutoff: UtcMicros(1_234_568),
                },
                request.grain(),
                request.access(),
            )
            .unwrap(),
            SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                context.identity().clone(),
                request.session_id().clone(),
                Some("cursor".to_owned()),
                request.temporal_mode(),
                RetrievalGrainV1::Turn,
                request.access(),
            )
            .unwrap(),
        ];
        for mutation in &target_mutations {
            assert_eq!(
                grant.validate(&context, context.binding(), mutation),
                Err(SessionAuthorizationError::WrongTarget)
            );
        }

        for access in [SessionAccess::Read, SessionAccess::Search] {
            let mutation = SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                context.identity().clone(),
                request.session_id().clone(),
                Some("cursor".to_owned()),
                request.temporal_mode(),
                request.grain(),
                access,
            )
            .unwrap();
            assert_eq!(
                grant.validate(&context, context.binding(), &mutation),
                Err(SessionAuthorizationError::WrongAccess)
            );
        }

        let identity_mutations = [
            ResolvedSessionIdentity::for_project(
                ProfileId::new("profile.primary").unwrap(),
                ProjectId::new("project.other").unwrap(),
                SessionStoreId::new("store.project.tracedecay").unwrap(),
                SessionRootId::new("root.project.tracedecay").unwrap(),
                context.identity().git_route().unwrap().clone(),
            ),
            ResolvedSessionIdentity::for_project(
                ProfileId::new("profile.primary").unwrap(),
                ProjectId::new("project.tracedecay").unwrap(),
                SessionStoreId::new("store.project.other").unwrap(),
                SessionRootId::new("root.project.tracedecay").unwrap(),
                context.identity().git_route().unwrap().clone(),
            ),
            ResolvedSessionIdentity::for_project(
                ProfileId::new("profile.primary").unwrap(),
                ProjectId::new("project.tracedecay").unwrap(),
                SessionStoreId::new("store.project.tracedecay").unwrap(),
                SessionRootId::new("root.project.other").unwrap(),
                context.identity().git_route().unwrap().clone(),
            ),
            ResolvedSessionIdentity::for_project(
                ProfileId::new("profile.primary").unwrap(),
                ProjectId::new("project.tracedecay").unwrap(),
                SessionStoreId::new("store.project.tracedecay").unwrap(),
                SessionRootId::new("root.project.tracedecay").unwrap(),
                ResolvedGitRoute::new(
                    RepositoryId::new("repository.other").unwrap(),
                    WorktreeId::new("worktree.main").unwrap(),
                    BranchId::new("branch.application-slice-1").unwrap(),
                ),
            ),
        ];
        for identity in identity_mutations {
            let mutation = SessionScopeAuthorizationRequest::new(
                context.actor_id().clone(),
                identity,
                request.session_id().clone(),
                Some("cursor".to_owned()),
                request.temporal_mode(),
                request.grain(),
                request.access(),
            )
            .unwrap();
            assert_eq!(
                grant.validate(&context, context.binding(), &mutation),
                Err(SessionAuthorizationError::WrongScope)
            );
        }
    }

    #[test]
    fn grant_validation_rejects_each_context_digest_mutation() {
        let context = context();
        let request = exact_authorization_request(context.identity().clone());
        let grant = AllowAuthorizer
            .authorize(&context, context.binding(), &request)
            .unwrap();

        for mutation in [
            context_with_digests([0x11; 32], DIGEST, DIGEST),
            context_with_digests(DIGEST, [0x22; 32], DIGEST),
            context_with_digests(DIGEST, DIGEST, [0x33; 32]),
        ] {
            assert_eq!(
                grant.validate(&mutation, mutation.binding(), &request),
                Err(SessionAuthorizationError::WrongContext)
            );
        }
    }

    #[test]
    fn grant_validation_rejects_actor_substitution_with_identical_context_digests() {
        let context = context();
        let request = exact_authorization_request(context.identity().clone());
        let grant = AllowAuthorizer
            .authorize(&context, context.binding(), &request)
            .unwrap();
        let other_actor = context_for_actor("actor.other", DIGEST, DIGEST, DIGEST);
        let other_actor_request = SessionScopeAuthorizationRequest::new(
            other_actor.actor_id().clone(),
            context.identity().clone(),
            request.session_id().clone(),
            request.provider_scope().map(str::to_owned),
            request.temporal_mode(),
            request.grain(),
            request.access(),
        )
        .unwrap();

        assert_eq!(
            grant.validate(&other_actor, other_actor.binding(), &request),
            Err(SessionAuthorizationError::WrongContext)
        );
        assert_eq!(
            grant.validate(&context, context.binding(), &other_actor_request),
            Err(SessionAuthorizationError::WrongContext)
        );
        assert!(matches!(
            AllowAuthorizer.authorize(&context, context.binding(), &other_actor_request),
            Err(SessionAuthorizationError::WrongContext)
        ));
    }

    #[test]
    fn authorization_request_rejects_noncanonical_provider_scope() {
        let context = context();

        for provider in ["", " cursor", "cursor ", "cursor\nagent"] {
            assert_eq!(
                SessionScopeAuthorizationRequest::new(
                    context.actor_id().clone(),
                    context.identity().clone(),
                    SessionId::new("session.application-slice-1").unwrap(),
                    Some(provider.to_owned()),
                    TemporalModeV1::Current,
                    RetrievalGrainV1::LogicalMessage,
                    SessionAccess::Hydrate,
                ),
                Err(SessionAuthorizationError::InvalidProviderScope)
            );
        }
    }

    #[test]
    fn authorizer_issues_an_opaque_grant_for_the_exact_resolved_route() {
        let context = context();
        let request = exact_authorization_request(context.identity().clone());
        let grant = AllowAuthorizer
            .authorize(&context, context.binding(), &request)
            .unwrap();

        assert_eq!(grant.id().as_str(), "grant.session.read");
        assert_eq!(grant.revision(), 7);
        assert_eq!(grant.scope().identity(), context.identity());
        assert_eq!(grant.scope().access(), SessionAccess::Hydrate);
        assert_eq!(
            grant
                .scope()
                .identity()
                .git_route()
                .unwrap()
                .branch_id()
                .as_str(),
            "branch.application-slice-1"
        );
    }

    #[test]
    fn grant_rejects_scope_or_route_substitution() {
        let context = context();
        let profile_request = SessionScopeAuthorizationRequest::new(
            context.actor_id().clone(),
            ResolvedSessionIdentity::for_profile(
                ProfileId::new("profile.primary").unwrap(),
                SessionStoreId::new("store.profile.primary").unwrap(),
                SessionRootId::new("root.profile.primary").unwrap(),
            ),
            SessionId::new("session.application-slice-1").unwrap(),
            Some("cursor".to_owned()),
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(1_234_567),
            },
            RetrievalGrainV1::LogicalMessage,
            SessionAccess::Read,
        )
        .unwrap();

        assert!(matches!(
            AllowAuthorizer.authorize(&context, context.binding(), &profile_request),
            Err(SessionAuthorizationError::WrongScope)
        ));
    }

    #[test]
    fn profile_binding_fails_closed_against_a_project_context() {
        let context = context();
        let binding = SessionRequestBinding::new(
            ResolvedSessionIdentity::for_profile(
                ProfileId::new("profile.primary").unwrap(),
                SessionStoreId::new("store.profile.primary").unwrap(),
                SessionRootId::new("root.profile.primary").unwrap(),
            ),
            context.capability_digest(),
            context.policy_digest(),
            context.configuration_digest(),
            context.binding.cancellation.clone(),
            context.binding.budgets,
        );

        // A profile-owned binding resolves the profile session store's own
        // scope, which is disjoint from every project scope, so pairing it with
        // a project request context still fails closed.
        assert_eq!(
            binding.validate_context(&context),
            Err(SessionAuthorizationError::WrongScope)
        );
    }

    #[test]
    fn binding_rejects_scope_and_grant_digest_substitution() {
        let context = context();
        let wrong_scope = SessionRequestBinding::new(
            ResolvedSessionIdentity::for_project(
                ProfileId::new("profile.primary").unwrap(),
                ProjectId::new("project.other").unwrap(),
                SessionStoreId::new("store.project.tracedecay").unwrap(),
                SessionRootId::new("root.project.tracedecay").unwrap(),
                context.identity().git_route().unwrap().clone(),
            ),
            context.capability_digest(),
            context.policy_digest(),
            context.configuration_digest(),
            context.binding.cancellation.clone(),
            context.binding.budgets,
        );
        assert_eq!(
            wrong_scope.validate_context(&context),
            Err(SessionAuthorizationError::WrongScope)
        );

        let wrong_grant = SessionRequestBinding::new(
            context.identity().clone(),
            CapabilityDigest::new([0x11; 32]),
            context.policy_digest(),
            context.configuration_digest(),
            context.binding.cancellation.clone(),
            context.binding.budgets,
        );
        assert_eq!(
            wrong_grant.validate_context(&context),
            Err(SessionAuthorizationError::WrongContext)
        );
    }

    #[test]
    fn binding_rejects_cancellation_or_budget_substitution() {
        let context = context();
        let fresh_cancellation =
            CancellationToken::for_application_request(context.request_id().as_str());
        let wrong_cancellation = SessionRequestBinding::new(
            context.identity().clone(),
            context.capability_digest(),
            context.policy_digest(),
            context.configuration_digest(),
            fresh_cancellation,
            context.binding.budgets,
        );
        assert_eq!(
            wrong_cancellation.validate_context(&context),
            Err(SessionAuthorizationError::WrongContext)
        );

        let widened_budgets = SessionRequestBinding::new(
            context.identity().clone(),
            context.capability_digest(),
            context.policy_digest(),
            context.configuration_digest(),
            context.binding.cancellation.clone(),
            RequestBudgets::new(65, 4097, 17).unwrap(),
        );
        assert_eq!(
            widened_budgets.validate_context(&context),
            Err(SessionAuthorizationError::WrongContext)
        );
        assert!(matches!(
            AllowAuthorizer.authorize(
                &context,
                &widened_budgets,
                &exact_authorization_request(context.identity().clone()),
            ),
            Err(SessionAuthorizationError::WrongContext)
        ));
    }

    #[test]
    fn retrieval_request_carries_grant_target_freshness_and_limit() {
        let context = context();
        let authorization = exact_authorization_request(context.identity().clone());
        let grant = AllowAuthorizer
            .authorize(&context, context.binding(), &authorization)
            .unwrap();
        let request = SessionRetrievalRequest::new(
            grant,
            SessionRetrievalTarget::Session(SessionId::new("session.application-slice-1").unwrap()),
            SessionFreshnessPolicy::RequireFresh,
            25,
        )
        .unwrap();

        assert_eq!(request.limit(), 25);
        assert_eq!(
            request.freshness_policy(),
            SessionFreshnessPolicy::RequireFresh
        );
        assert!(matches!(
            request.target(),
            SessionRetrievalTarget::Session(session_id)
                if session_id.as_str() == "session.application-slice-1"
        ));
    }

    #[test]
    fn retrieval_request_rejects_targets_outside_the_grant_scope() {
        let context = context();
        let exact_authorization = exact_authorization_request(context.identity().clone());
        let exact_grant = AllowAuthorizer
            .authorize(&context, context.binding(), &exact_authorization)
            .unwrap();
        for target in [
            SessionRetrievalTarget::Scope,
            SessionRetrievalTarget::Session(SessionId::new("session.other").unwrap()),
        ] {
            assert!(matches!(
                SessionRetrievalRequest::new(
                    exact_grant.clone(),
                    target,
                    SessionFreshnessPolicy::AllowStored,
                    1,
                ),
                Err(SessionRetrievalError::TargetOutsideGrant)
            ));
        }

        let root_authorization = exact_authorization
            .with_retrieval_scope(SessionRetrievalScope::AllSessionsInAuthorizedRoot);
        let root_grant = AllowAuthorizer
            .authorize(&context, context.binding(), &root_authorization)
            .unwrap();
        assert!(
            SessionRetrievalRequest::new(
                root_grant.clone(),
                SessionRetrievalTarget::Scope,
                SessionFreshnessPolicy::AllowStored,
                1,
            )
            .is_ok()
        );
        assert!(matches!(
            SessionRetrievalRequest::new(
                root_grant,
                SessionRetrievalTarget::Session(
                    SessionId::new("session.application-slice-1").unwrap()
                ),
                SessionFreshnessPolicy::AllowStored,
                1,
            ),
            Err(SessionRetrievalError::TargetOutsideGrant)
        ));
    }

    #[test]
    fn freshness_policy_distinguishes_stored_from_fresh_data() {
        let stored = SessionDataFreshness::Stored { generation_lag: 2 };
        let partial = SessionDataFreshness::Partial { generation_lag: 2 };

        assert!(SessionFreshnessPolicy::AllowStored.accepts(stored));
        assert!(!SessionFreshnessPolicy::RequireFresh.accepts(stored));
        assert!(SessionFreshnessPolicy::AllowStored.accepts(partial));
        assert!(!SessionFreshnessPolicy::RequireFresh.accepts(partial));
        assert!(SessionFreshnessPolicy::RequireFresh.accepts(SessionDataFreshness::Fresh));
    }

    #[test]
    fn aggregate_freshness_is_derived_from_typed_source_coverage() {
        let request = SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current);
        let source = |id: &str, observed, committed, target| {
            SessionSourceCoverageV1::from_frontiers(
                SessionSourceIdV1::new(id).unwrap(),
                SessionSourceFrontierV1::new(observed),
                SessionSourceFrontierV1::new(committed),
                SessionSourceFrontierV1::new(target),
                request.clone(),
            )
            .unwrap()
        };
        let stale =
            SessionSourceCoverageReceiptV1::new(request.clone(), vec![source("cursor", 10, 8, 10)])
                .unwrap();
        assert_eq!(
            SessionDataFreshness::from_source_coverage(&stale),
            SessionDataFreshness::Stored { generation_lag: 2 }
        );

        let partial = SessionSourceCoverageReceiptV1::new(
            request.clone(),
            vec![source("cursor", 10, 10, 10), source("claude", 10, 8, 10)],
        )
        .unwrap();
        assert_eq!(
            SessionDataFreshness::from_source_coverage(&partial),
            SessionDataFreshness::Partial { generation_lag: 2 }
        );
    }

    #[test]
    fn retrieval_terminal_states_never_collapse_to_complete_zero() {
        let states: [SessionRetrievalOutcome<()>; 12] = [
            SessionRetrievalOutcome::CompleteZero {
                freshness: SessionDataFreshness::Fresh,
            },
            SessionRetrievalOutcome::Stale {
                freshness: SessionDataFreshness::Stored { generation_lag: 1 },
            },
            SessionRetrievalOutcome::Partial {
                items: vec![],
                freshness: SessionDataFreshness::Fresh,
                omitted: 1,
            },
            SessionRetrievalOutcome::WrongScope,
            SessionRetrievalOutcome::Locked,
            SessionRetrievalOutcome::Redacted,
            SessionRetrievalOutcome::Deleted,
            SessionRetrievalOutcome::Denied,
            SessionRetrievalOutcome::Unavailable,
            SessionRetrievalOutcome::CursorManifestLimitExceeded {
                kind: CursorManifestLimitKindV1::Participants,
                observed: 257,
                maximum: SESSION_TEMPORAL_CURSOR_MAX_PARTICIPANTS,
            },
            SessionRetrievalOutcome::BudgetExhausted,
            SessionRetrievalOutcome::Cancelled,
        ];

        assert!(matches!(
            states[0],
            SessionRetrievalOutcome::CompleteZero { .. }
        ));
        for state in &states[1..] {
            assert!(!matches!(
                state,
                SessionRetrievalOutcome::CompleteZero { .. }
            ));
        }
    }

    #[test]
    fn complete_requires_at_least_one_item() {
        assert_eq!(
            SessionRetrievalOutcome::<()>::complete(Vec::new(), SessionDataFreshness::Fresh),
            Err(SessionRetrievalError::EmptyComplete)
        );
        assert!(matches!(
            SessionRetrievalOutcome::complete(vec![1], SessionDataFreshness::Fresh).unwrap(),
            SessionRetrievalOutcome::Complete { items, .. } if items == vec![1]
        ));
    }
}

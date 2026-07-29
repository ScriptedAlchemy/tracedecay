use std::fmt;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tracedecay_domain::{ActorId, ProjectId, RepositoryId, WorktreeId};

macro_rules! string_id {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct $name(String);

            impl $name {
                pub fn new(value: impl Into<String>) -> Result<Self, RequestContextError> {
                    let value = value.into();
                    validate_identifier(&value, stringify!($name))?;
                    Ok(Self(value))
                }

                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str(&self.0)
                }
            }
        )+
    };
}

string_id!(
    RequestId,
    ProfileId,
    SessionStoreId,
    SessionRootId,
    BranchId,
);

fn validate_identifier(value: &str, field: &'static str) -> Result<(), RequestContextError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(RequestContextError::NonCanonicalIdentifier(field));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionOwner {
    Profile {
        profile_id: ProfileId,
    },
    Project {
        profile_id: ProfileId,
        project_id: ProjectId,
    },
}

impl SessionOwner {
    pub fn profile_id(&self) -> &ProfileId {
        match self {
            Self::Profile { profile_id } | Self::Project { profile_id, .. } => profile_id,
        }
    }

    pub fn project_id(&self) -> Option<&ProjectId> {
        match self {
            Self::Profile { .. } => None,
            Self::Project { project_id, .. } => Some(project_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedGitRoute {
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    branch_id: BranchId,
}

impl ResolvedGitRoute {
    pub fn new(repository_id: RepositoryId, worktree_id: WorktreeId, branch_id: BranchId) -> Self {
        Self {
            repository_id,
            worktree_id,
            branch_id,
        }
    }

    pub fn repository_id(&self) -> &RepositoryId {
        &self.repository_id
    }

    pub fn worktree_id(&self) -> &WorktreeId {
        &self.worktree_id
    }

    pub fn branch_id(&self) -> &BranchId {
        &self.branch_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSessionIdentity {
    owner: SessionOwner,
    store_id: SessionStoreId,
    root_id: SessionRootId,
    git_route: Option<ResolvedGitRoute>,
}

impl ResolvedSessionIdentity {
    pub fn for_profile(
        profile_id: ProfileId,
        store_id: SessionStoreId,
        root_id: SessionRootId,
    ) -> Self {
        Self {
            owner: SessionOwner::Profile { profile_id },
            store_id,
            root_id,
            git_route: None,
        }
    }

    pub fn for_project(
        profile_id: ProfileId,
        project_id: ProjectId,
        store_id: SessionStoreId,
        root_id: SessionRootId,
        git_route: ResolvedGitRoute,
    ) -> Self {
        Self {
            owner: SessionOwner::Project {
                profile_id,
                project_id,
            },
            store_id,
            root_id,
            git_route: Some(git_route),
        }
    }

    pub fn owner(&self) -> &SessionOwner {
        &self.owner
    }

    pub fn profile_id(&self) -> &ProfileId {
        self.owner.profile_id()
    }

    pub fn project_id(&self) -> Option<&ProjectId> {
        self.owner.project_id()
    }

    pub fn store_id(&self) -> &SessionStoreId {
        &self.store_id
    }

    pub fn root_id(&self) -> &SessionRootId {
        &self.root_id
    }

    pub fn git_route(&self) -> Option<&ResolvedGitRoute> {
        self.git_route.as_ref()
    }

    /// Resolves this session identity to the exact transport-neutral
    /// application scope.
    ///
    /// Profile-owned identities deliberately fail closed because they do not
    /// name a project, repository, or worktree. Callers must never fabricate
    /// those fields from a path or the current working directory.
    pub fn application_scope(
        &self,
    ) -> Result<tracedecay_application::ResolvedScope, ApplicationScopeError> {
        let project_id = self
            .project_id()
            .ok_or(ApplicationScopeError::ProfileIdentityWithoutProject)?;
        let git_route = self
            .git_route()
            .ok_or_else(|| ApplicationScopeError::MissingGitRoute {
                project_id: project_id.as_str().to_owned(),
            })?;
        let reference =
            tracedecay_domain::RefId::new(format!("refs/heads/{}", git_route.branch_id().as_str()))
                .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
        tracedecay_application::ResolvedScope::new(
            project_id.clone(),
            git_route.repository_id().clone(),
            git_route.worktree_id().clone(),
            Some(reference),
        )
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))
    }
}

macro_rules! digest {
    ($($name:ident),+ $(,)?) => {
        $(
            #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct $name([u8; 32]);

            impl $name {
                pub const fn new(bytes: [u8; 32]) -> Self {
                    Self(bytes)
                }

                pub const fn as_bytes(&self) -> &[u8; 32] {
                    &self.0
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str(concat!(stringify!($name), "(sha256:"))?;
                    for byte in self.0 {
                        write!(formatter, "{byte:02x}")?;
                    }
                    formatter.write_str(")")
                }
            }
        )+
    };
}

digest!(CapabilityDigest, PolicyDigest, ConfigurationDigest);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonotonicDeadline(Instant);

impl MonotonicDeadline {
    pub const fn at(deadline: Instant) -> Self {
        Self(deadline)
    }

    pub const fn instant(self) -> Instant {
        self.0
    }

    pub fn is_elapsed_at(self, now: Instant) -> bool {
        now >= self.0
    }
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    token_id: Option<Arc<str>>,
    cancelled: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            token_id: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the live cancellation authority for an application request.
    pub fn for_application_request(request_id: &tracedecay_application::RequestId) -> Self {
        static NEXT_TOKEN_SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT_TOKEN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self {
            token_id: Some(Arc::from(format!(
                "cancellation.{}.{sequence}",
                request_id.as_str()
            ))),
            ..Self::default()
        }
    }

    pub fn application_token_id(&self) -> Option<&str> {
        self.token_id.as_deref()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn is_same_token(&self, other: &Self) -> bool {
        self.token_id == other.token_id
            && Arc::ptr_eq(&self.cancelled, &other.cancelled)
            && Arc::ptr_eq(&self.notify, &other.notify)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestBudgets {
    max_results: u64,
    max_bytes: u64,
    max_work_units: u64,
}

impl RequestBudgets {
    pub fn new(
        max_results: u64,
        max_bytes: u64,
        max_work_units: u64,
    ) -> Result<Self, RequestContextError> {
        for (field, value) in [
            ("max_results", max_results),
            ("max_bytes", max_bytes),
            ("max_work_units", max_work_units),
        ] {
            if value == 0 {
                return Err(RequestContextError::ZeroBudget(field));
            }
        }
        Ok(Self {
            max_results,
            max_bytes,
            max_work_units,
        })
    }

    pub const fn max_results(self) -> u64 {
        self.max_results
    }

    pub const fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    pub const fn max_work_units(self) -> u64 {
        self.max_work_units
    }
}

#[derive(Clone, Debug)]
pub struct RequestContext {
    actor_id: ActorId,
    request_id: RequestId,
    identity: ResolvedSessionIdentity,
    capability_digest: CapabilityDigest,
    policy_digest: PolicyDigest,
    configuration_digest: ConfigurationDigest,
    deadline: MonotonicDeadline,
    cancellation: CancellationToken,
    budgets: RequestBudgets,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestInterruption {
    Cancelled,
    DeadlineExceeded,
}

impl RequestContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        actor_id: ActorId,
        request_id: RequestId,
        identity: ResolvedSessionIdentity,
        capability_digest: CapabilityDigest,
        policy_digest: PolicyDigest,
        configuration_digest: ConfigurationDigest,
        deadline: MonotonicDeadline,
        cancellation: CancellationToken,
        budgets: RequestBudgets,
    ) -> Self {
        Self {
            actor_id,
            request_id,
            identity,
            capability_digest,
            policy_digest,
            configuration_digest,
            deadline,
            cancellation,
            budgets,
        }
    }

    pub fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
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

    pub const fn deadline(&self) -> MonotonicDeadline {
        self.deadline
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub const fn budgets(&self) -> RequestBudgets {
        self.budgets
    }

    pub async fn interrupted(&self) -> RequestInterruption {
        let cancelled = self.cancellation.cancelled();
        tokio::pin!(cancelled);
        let deadline =
            tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline.instant()));
        tokio::pin!(deadline);
        tokio::select! {
            biased;
            () = &mut cancelled => RequestInterruption::Cancelled,
            () = &mut deadline => RequestInterruption::DeadlineExceeded,
        }
    }

    pub async fn run_interruptible<T, F>(
        &self,
        future: impl std::future::Future<Output = T>,
        on_interruption: F,
    ) -> Result<T, RequestInterruption>
    where
        F: FnOnce(),
    {
        tokio::pin!(future);
        let interrupted = self.interrupted();
        tokio::pin!(interrupted);
        tokio::select! {
            biased;
            result = &mut future => Ok(result),
            interruption = &mut interrupted => {
                on_interruption();
                Err(interruption)
            },
        }
    }

    /// Crosses the root orchestration identity into the transport-neutral
    /// application scope: the project-owned [`ResolvedSessionIdentity`] plus
    /// its [`ResolvedGitRoute`] map onto one exact
    /// [`tracedecay_application::ResolvedScope`].
    ///
    /// Fails closed for a profile-owned identity (there is no path or CWD
    /// fallback that could fabricate a project scope) and for a project
    /// identity missing its git route.
    #[deprecated(
        note = "V2 RequestContext convergence compatibility facade: resolve tracedecay_application::ResolvedScope once at the transport boundary instead; deletion is gated on zero production callers"
    )]
    pub fn application_scope(
        &self,
    ) -> Result<tracedecay_application::ResolvedScope, ApplicationScopeError> {
        self.identity.application_scope()
    }

    /// Composes the capability, policy, and configuration digests into the
    /// single digest a [`tracedecay_application::CapabilityGrantSnapshot`]
    /// carries for this request. The composition is deterministic and embeds
    /// no paths or payloads.
    pub fn grant_snapshot_digest(
        &self,
    ) -> Result<tracedecay_domain::ManifestDigest, ApplicationScopeError> {
        application_grant_digest(
            self.capability_digest,
            self.policy_digest,
            self.configuration_digest,
        )
    }

    /// Crosses this orchestration context into the application boundary type.
    ///
    /// The grant is a pre-resolved input (the boundary narrows or rejects it;
    /// it never issues one): a grant minted for another scope fails closed.
    /// [`MonotonicDeadline`] anchors to wall time at this call — an elapsed
    /// deadline stays elapsed — and [`CancellationToken`] snapshots into a
    /// [`tracedecay_application::CancellationContext`] stamped with the
    /// observation instant, never zero.
    #[deprecated(
        note = "V2 RequestContext convergence compatibility facade: construct tracedecay_application::RequestContext at the transport boundary instead; deletion is gated on zero production callers"
    )]
    #[allow(deprecated)] // delegates to the sibling facade adapter above
    pub fn to_application(
        &self,
        grant: tracedecay_application::CapabilityGrantSnapshot,
    ) -> Result<tracedecay_application::RequestContext, ApplicationScopeError> {
        let scope = self.application_scope()?;
        let request_id = tracedecay_application::RequestId::new(self.request_id.as_str())
            .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
        let deadline = self.application_deadline();
        let cancellation = self.application_cancellation()?;
        tracedecay_application::RequestContext::new(
            self.actor_id.clone(),
            scope,
            grant,
            request_id,
            deadline,
            cancellation,
        )
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))
    }

    fn application_deadline(&self) -> tracedecay_application::Deadline {
        let remaining = self
            .deadline
            .instant()
            .saturating_duration_since(Instant::now());
        let remaining_micros = i64::try_from(remaining.as_micros()).unwrap_or(i64::MAX);
        tracedecay_application::Deadline {
            expires_at: tracedecay_domain::UtcMicros(
                wall_clock_micros().saturating_add(remaining_micros),
            ),
        }
    }

    fn application_cancellation(
        &self,
    ) -> Result<tracedecay_application::CancellationContext, ApplicationScopeError> {
        // The token id mirrors the request id: both name this exact request,
        // and the mirror keeps the id within the canonical length bound.
        let token_id = self.request_id.as_str();
        if self.cancellation.is_cancelled() {
            tracedecay_application::CancellationContext::cancelled(
                token_id,
                tracedecay_domain::UtcMicros(wall_clock_micros()),
            )
        } else {
            tracedecay_application::CancellationContext::active(token_id)
        }
        .map_err(|error| ApplicationScopeError::Contract(error.to_string()))
    }
}

fn wall_clock_micros() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_micros()),
    )
    .unwrap_or(i64::MAX)
}

/// Composes the legacy capability, policy, and configuration revisions into
/// the one immutable digest carried by an application capability grant.
pub fn application_grant_digest(
    capability: CapabilityDigest,
    policy: PolicyDigest,
    configuration: ConfigurationDigest,
) -> Result<tracedecay_domain::ManifestDigest, ApplicationScopeError> {
    tracedecay_domain::canonical_sha256(&(
        "tracedecay.root.grant-composition.v1",
        capability.as_bytes(),
        policy.as_bytes(),
        configuration.as_bytes(),
    ))
    .map_err(|error| ApplicationScopeError::Contract(error.to_string()))
}

/// Composes immutable session admission authority into one application grant
/// digest. Unlike the compatibility composition, this binds request budgets
/// and the live cancellation token identity so a supplemental session binding
/// cannot widen either after admission.
pub fn session_application_grant_digest(
    capability: CapabilityDigest,
    policy: PolicyDigest,
    configuration: ConfigurationDigest,
    cancellation: &CancellationToken,
    budgets: RequestBudgets,
) -> Result<tracedecay_domain::ManifestDigest, ApplicationScopeError> {
    let token_id = cancellation
        .application_token_id()
        .ok_or(ApplicationScopeError::MissingCancellationTokenId)?;
    tracedecay_domain::canonical_sha256(&(
        "tracedecay.session.grant-composition.v1",
        capability.as_bytes(),
        policy.as_bytes(),
        configuration.as_bytes(),
        token_id,
        budgets.max_results(),
        budgets.max_bytes(),
        budgets.max_work_units(),
    ))
    .map_err(|error| ApplicationScopeError::Contract(error.to_string()))
}

/// Returns the current wall-clock observation used by application deadlines.
pub fn application_observed_at() -> tracedecay_domain::UtcMicros {
    tracedecay_domain::UtcMicros(wall_clock_micros())
}

/// Rechecks immutable application admission together with the live transport
/// cancellation token retained by the root runtime.
pub fn application_request_interruption(
    context: &tracedecay_application::RequestContext,
    cancellation: &CancellationToken,
) -> Option<RequestInterruption> {
    if cancellation.is_cancelled()
        || matches!(
            context.admission_at(application_observed_at()),
            tracedecay_application::RequestAdmission::Cancelled
        )
    {
        Some(RequestInterruption::Cancelled)
    } else if !matches!(
        context.admission_at(application_observed_at()),
        tracedecay_application::RequestAdmission::Admitted
    ) {
        Some(RequestInterruption::DeadlineExceeded)
    } else {
        None
    }
}

/// Runs one awaitable application step against the exact immutable deadline
/// and the live cancellation token owned by the transport/runtime boundary.
pub async fn run_application_request_interruptible<T, F>(
    context: &tracedecay_application::RequestContext,
    cancellation: &CancellationToken,
    future: impl std::future::Future<Output = T>,
    on_interruption: F,
) -> Result<T, RequestInterruption>
where
    F: FnOnce(),
{
    if let Some(interruption) = application_request_interruption(context, cancellation) {
        on_interruption();
        return Err(interruption);
    }

    let terminal_at = context
        .deadline()
        .expires_at
        .0
        .min(context.grant().expires_at.0);
    let remaining_micros = terminal_at.saturating_sub(application_observed_at().0);
    let remaining_micros = u64::try_from(remaining_micros).unwrap_or(0);
    let cancelled = cancellation.cancelled();
    tokio::pin!(cancelled);
    let deadline = tokio::time::sleep(std::time::Duration::from_micros(remaining_micros));
    tokio::pin!(deadline);
    tokio::pin!(future);
    tokio::select! {
        biased;
        result = &mut future => Ok(result),
        () = &mut cancelled => {
            on_interruption();
            Err(RequestInterruption::Cancelled)
        }
        () = &mut deadline => {
            on_interruption();
            Err(RequestInterruption::DeadlineExceeded)
        }
    }
}

/// Resolves the exact transport-neutral scope for one registered project
/// root.
///
/// Binary-crate entry points (the CLI) cannot reach the daemon-owned identity
/// helpers; this facade is the single public path so no caller re-implements
/// repository/worktree/reference resolution ad hoc. It is the exact-root
/// special case of [`resolve_registered_root_scope`]: the registered root and
/// the served root are one and the same.
#[deprecated(
    note = "V2 RequestContext convergence compatibility facade: scope resolution moves behind the application boundary; deletion is gated on zero production callers"
)]
pub fn resolve_exact_root_scope(
    project_root: &Path,
    project_id: &ProjectId,
) -> Result<tracedecay_application::ResolvedScope, ApplicationScopeError> {
    #[allow(deprecated)] // exact-root special case of the consolidated facade
    let scope = resolve_registered_root_scope(project_root, project_root, project_id);
    scope
}

/// Resolves the exact transport-neutral scope for one already-authorized
/// registered project and the root the call will actually serve.
///
/// This is the single scope-resolution path behind the root facade: every
/// query-facing surface (CLI, MCP, dashboard) converges here instead of
/// re-implementing canonicalization guards, sibling-root authorization, or
/// digest revalidation. `registered_root` is the registry's canonical root
/// for the authorized project; `requested_root` is the worktree root the call
/// will serve — the registered root, a path inside it, or a linked worktree
/// of the same repository. Repository/worktree/reference identity itself
/// stays with the daemon-owned authority; this facade only guards the
/// crossing and delegates.
///
/// Every failure is explicit and fails closed: a relative requested root
/// (there is no CWD fallback), a root that cannot be canonicalized, an
/// unauthorized sibling root naming a different repository, an identity the
/// daemon-owned authority cannot resolve, or a resolved scope whose digest
/// does not match its fields.
#[deprecated(
    note = "V2 RequestContext convergence compatibility facade: scope resolution moves behind the application boundary; deletion is gated on zero production callers"
)]
pub fn resolve_registered_root_scope(
    registered_root: &Path,
    requested_root: &Path,
    project_id: &ProjectId,
) -> Result<tracedecay_application::ResolvedScope, ApplicationScopeError> {
    if !requested_root.is_absolute() {
        return Err(ApplicationScopeError::RelativeRoot {
            requested_root: requested_root.display().to_string(),
        });
    }
    if !registered_root.is_absolute() {
        return Err(ApplicationScopeError::Resolution(format!(
            "registered root '{}' is not absolute",
            registered_root.display()
        )));
    }
    let registered_root = registered_root.canonicalize().map_err(|error| {
        ApplicationScopeError::Resolution(format!(
            "registered root '{}' could not be canonicalized: {error}",
            registered_root.display()
        ))
    })?;
    let requested_root = requested_root.canonicalize().map_err(|error| {
        ApplicationScopeError::Resolution(format!(
            "requested root '{}' could not be canonicalized: {error}",
            requested_root.display()
        ))
    })?;
    // A requested root at or inside the registered canonical root names the
    // registered worktree itself, so the scope anchors to the canonical root.
    // A requested root outside it is authorized only as the same repository
    // (a linked worktree shares the git common dir); anything else is an
    // unauthorized sibling root and fails closed.
    let scope_root =
        if requested_root == registered_root || requested_root.starts_with(&registered_root) {
            registered_root
        } else {
            let registered_repository = repository_id_for_root(&registered_root)?;
            let requested_repository = repository_id_for_root(&requested_root)?;
            if registered_repository != requested_repository {
                return Err(ApplicationScopeError::UnauthorizedSiblingRoot {
                    registered_root: registered_root.display().to_string(),
                    requested_root: requested_root.display().to_string(),
                });
            }
            requested_root
        };
    let scope =
        crate::daemon::project_open_owners::resolved_scope_for_project(&scope_root, project_id)
            .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
    // A scope whose digest does not match its fields is stale or tampered and
    // must never cross the boundary.
    scope
        .validate()
        .map_err(|error| ApplicationScopeError::InconsistentScope(error.to_string()))?;
    Ok(scope)
}

fn repository_id_for_root(
    root: &Path,
) -> Result<tracedecay_domain::RepositoryId, ApplicationScopeError> {
    crate::daemon::code_index_scheduler::identity::repository_id_for(root)
        .map_err(|error| ApplicationScopeError::Resolution(error.to_string()))
}

/// The explicit failure states when the root orchestration context crosses
/// into the transport-neutral application surface. Every variant fails
/// closed: no path, CWD, or sibling-root fallback exists at this boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplicationScopeError {
    /// A profile-owned session identity has no exact project root; there is
    /// no fallback from profile scope to a project `ResolvedScope`.
    ProfileIdentityWithoutProject,
    /// A project-owned session identity without its resolved git route
    /// cannot name an exact repository/worktree scope.
    MissingGitRoute {
        /// The project whose identity lacks a git route.
        project_id: String,
    },
    /// Session admission requires a stable cancellation-token identity so the
    /// live token cannot be substituted after the application grant is issued.
    MissingCancellationTokenId,
    /// The application contract rejected the resolved scope, grant binding,
    /// or boundary adapter output.
    Contract(String),
    /// The requested root is not absolute; resolving it against the process
    /// CWD would be the CWD fallback the plan forbids.
    RelativeRoot {
        /// The offending requested root.
        requested_root: String,
    },
    /// The requested root lives outside the registered canonical root and
    /// belongs to a different repository (a linked worktree of the same
    /// repository remains authorized; an unrelated sibling root is not).
    UnauthorizedSiblingRoot {
        /// The registered canonical root.
        registered_root: String,
        /// The offending requested root.
        requested_root: String,
    },
    /// A root could not be canonicalized or an identity could not be derived
    /// for it.
    Resolution(String),
    /// The resolved scope failed its own validation; a stale or tampered
    /// digest must never cross the boundary.
    InconsistentScope(String),
}

impl fmt::Display for ApplicationScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProfileIdentityWithoutProject => write!(
                formatter,
                "profile-scoped session identity has no exact project root; application scope resolution fails closed without a path or CWD fallback"
            ),
            Self::MissingGitRoute { project_id } => write!(
                formatter,
                "project identity '{project_id}' has no resolved git route; application scope requires an exact repository and worktree"
            ),
            Self::MissingCancellationTokenId => write!(
                formatter,
                "session application admission requires a bound cancellation-token identity"
            ),
            Self::Contract(message) => {
                write!(
                    formatter,
                    "application contract rejected the boundary crossing: {message}"
                )
            }
            Self::RelativeRoot { requested_root } => write!(
                formatter,
                "requested root '{requested_root}' is not absolute; scope resolution fails closed without a CWD fallback"
            ),
            Self::UnauthorizedSiblingRoot {
                registered_root,
                requested_root,
            } => write!(
                formatter,
                "requested root '{requested_root}' resolves outside registered root '{registered_root}' and names a different repository; refusing to serve a sibling root implicitly"
            ),
            Self::Resolution(message) => {
                write!(formatter, "application scope resolution failed: {message}")
            }
            Self::InconsistentScope(message) => write!(
                formatter,
                "resolved scope failed validation and must not cross the boundary: {message}"
            ),
        }
    }
}

impl std::error::Error for ApplicationScopeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestContextError {
    NonCanonicalIdentifier(&'static str),
    ZeroBudget(&'static str),
}

impl fmt::Display for RequestContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonCanonicalIdentifier(field) => {
                write!(
                    formatter,
                    "{field} must be a canonical non-empty identifier"
                )
            }
            Self::ZeroBudget(field) => write!(formatter, "{field} must be greater than zero"),
        }
    }
}

impl std::error::Error for RequestContextError {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    const DIGEST: [u8; 32] = [0x5a; 32];

    fn project_identity() -> ResolvedSessionIdentity {
        ResolvedSessionIdentity::for_project(
            ProfileId::new("profile.primary").unwrap(),
            ProjectId::new("project.tracedecay").unwrap(),
            SessionStoreId::new("store.project.tracedecay").unwrap(),
            SessionRootId::new("root.project.tracedecay").unwrap(),
            ResolvedGitRoute::new(
                RepositoryId::new("repository.tracedecay").unwrap(),
                WorktreeId::new("worktree.main").unwrap(),
                BranchId::new("branch.application-slice-1").unwrap(),
            ),
        )
    }

    #[test]
    fn request_context_preserves_resolved_identity_and_controls() {
        let now = Instant::now();
        let cancellation = CancellationToken::new();
        let context = RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            RequestId::new("request.application-slice-1").unwrap(),
            project_identity(),
            CapabilityDigest::new(DIGEST),
            PolicyDigest::new(DIGEST),
            ConfigurationDigest::new(DIGEST),
            MonotonicDeadline::at(now + Duration::from_secs(5)),
            cancellation.clone(),
            RequestBudgets::new(128, 4096, 32).unwrap(),
        );

        assert_eq!(context.actor_id().as_str(), "actor.cursor");
        assert_eq!(
            context.identity().project_id().unwrap().as_str(),
            "project.tracedecay"
        );
        assert_eq!(context.identity().profile_id().as_str(), "profile.primary");
        assert_eq!(
            context.identity().store_id().as_str(),
            "store.project.tracedecay"
        );
        assert_eq!(
            context.identity().root_id().as_str(),
            "root.project.tracedecay"
        );
        assert!(!context.deadline().is_elapsed_at(now));
        assert!(!context.cancellation().is_cancelled());

        cancellation.cancel();
        assert!(context.cancellation().is_cancelled());
    }

    #[test]
    fn profile_and_project_owners_are_explicit_and_never_fallback() {
        let profile = ResolvedSessionIdentity::for_profile(
            ProfileId::new("profile.primary").unwrap(),
            SessionStoreId::new("store.profile.primary").unwrap(),
            SessionRootId::new("root.profile.primary").unwrap(),
        );
        let project = project_identity();

        assert!(matches!(profile.owner(), SessionOwner::Profile { .. }));
        assert!(profile.project_id().is_none());
        assert!(profile.git_route().is_none());
        assert!(matches!(project.owner(), SessionOwner::Project { .. }));
        assert!(project.project_id().is_some());
        assert!(project.git_route().is_some());
    }

    #[test]
    fn digest_bindings_cannot_embed_paths_or_payloads() {
        let capability = CapabilityDigest::new(DIGEST);
        let policy = PolicyDigest::new(DIGEST);
        let configuration = ConfigurationDigest::new(DIGEST);

        assert_eq!(capability.as_bytes(), &DIGEST);
        assert_eq!(policy.as_bytes(), &DIGEST);
        assert_eq!(configuration.as_bytes(), &DIGEST);
        assert!(!format!("{capability:?}").contains("/fast/projects"));
    }

    #[test]
    fn budgets_must_be_bounded() {
        assert_eq!(
            RequestBudgets::new(0, 1, 1),
            Err(RequestContextError::ZeroBudget("max_results"))
        );
        assert_eq!(
            RequestBudgets::new(1, 0, 1),
            Err(RequestContextError::ZeroBudget("max_bytes"))
        );
        assert_eq!(
            RequestBudgets::new(1, 1, 0),
            Err(RequestContextError::ZeroBudget("max_work_units"))
        );
    }

    #[test]
    fn identifiers_reject_empty_or_noncanonical_values() {
        assert!(RequestId::new("").is_err());
        assert!(ProfileId::new(" profile.primary").is_err());
        assert!(SessionStoreId::new("store\nprimary").is_err());
        assert!(SessionRootId::new("root.primary ").is_err());
        assert!(BranchId::new("branch\0main").is_err());
    }

    fn project_context() -> RequestContext {
        RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            RequestId::new("request.application-slice-1").unwrap(),
            project_identity(),
            CapabilityDigest::new(DIGEST),
            PolicyDigest::new(DIGEST),
            ConfigurationDigest::new(DIGEST),
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
            RequestBudgets::new(128, 4096, 32).unwrap(),
        )
    }

    fn grant_for(
        scope: &tracedecay_application::ResolvedScope,
    ) -> tracedecay_application::CapabilityGrantSnapshot {
        use std::collections::BTreeSet;

        use tracedecay_application::{CapabilityGrantId, CapabilityGrantSnapshot, DisclosureClass};
        use tracedecay_domain::{ManifestDigest, UtcMicros};
        use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

        CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.application-slice-1").unwrap(),
            1,
            ManifestDigest::new(format!("sha256:{}", "5a".repeat(32))).unwrap(),
            ActorId::new("actor.cursor").unwrap(),
            UtcMicros(1),
            UtcMicros(i64::MAX - 1),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.application-slice-1").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.application-slice-1").unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap()
    }

    #[test]
    #[allow(deprecated)]
    fn application_scope_maps_project_identity_and_git_route() {
        let context = project_context();

        let scope = context.application_scope().unwrap();

        assert_eq!(scope.project_id.as_str(), "project.tracedecay");
        assert_eq!(scope.repository_id.as_str(), "repository.tracedecay");
        assert_eq!(scope.worktree_id.as_str(), "worktree.main");
        assert_eq!(
            scope
                .reference
                .as_ref()
                .map(tracedecay_domain::RefId::as_str),
            Some("refs/heads/branch.application-slice-1")
        );
        scope.validate().unwrap();
        assert_eq!(
            scope.scope_digest,
            scope.compute_digest().unwrap(),
            "scope digest must be stable for the same identity"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn application_scope_fails_closed_for_profile_identity() {
        let context = RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            RequestId::new("request.application-slice-1").unwrap(),
            ResolvedSessionIdentity::for_profile(
                ProfileId::new("profile.primary").unwrap(),
                SessionStoreId::new("store.profile.primary").unwrap(),
                SessionRootId::new("root.profile.primary").unwrap(),
            ),
            CapabilityDigest::new(DIGEST),
            PolicyDigest::new(DIGEST),
            ConfigurationDigest::new(DIGEST),
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
            RequestBudgets::new(128, 4096, 32).unwrap(),
        );

        // A profile-scoped identity has no exact project root; the boundary
        // must fail closed rather than fabricate one from a path or the CWD.
        assert_eq!(
            context.application_scope().unwrap_err(),
            ApplicationScopeError::ProfileIdentityWithoutProject
        );
    }

    #[test]
    #[allow(deprecated)]
    fn application_scope_fails_closed_without_git_route() {
        let identity = ResolvedSessionIdentity {
            owner: SessionOwner::Project {
                profile_id: ProfileId::new("profile.primary").unwrap(),
                project_id: ProjectId::new("project.tracedecay").unwrap(),
            },
            store_id: SessionStoreId::new("store.project.tracedecay").unwrap(),
            root_id: SessionRootId::new("root.project.tracedecay").unwrap(),
            git_route: None,
        };
        let context = RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            RequestId::new("request.application-slice-1").unwrap(),
            identity,
            CapabilityDigest::new(DIGEST),
            PolicyDigest::new(DIGEST),
            ConfigurationDigest::new(DIGEST),
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
            RequestBudgets::new(128, 4096, 32).unwrap(),
        );

        assert_eq!(
            context.application_scope().unwrap_err(),
            ApplicationScopeError::MissingGitRoute {
                project_id: "project.tracedecay".to_string(),
            }
        );
    }

    #[test]
    #[allow(deprecated)]
    fn to_application_binds_scope_grant_deadline_and_cancellation() {
        use tracedecay_application::RequestAdmission;
        use tracedecay_domain::UtcMicros;

        let context = project_context();
        let scope = context.application_scope().unwrap();
        let grant = grant_for(&scope);

        let application = context.to_application(grant).unwrap();

        assert_eq!(application.scope(), &scope);
        assert_eq!(application.actor().as_str(), "actor.cursor");
        assert_eq!(
            application.request_id().as_str(),
            "request.application-slice-1"
        );
        assert!(!application.cancellation().is_cancelled());
        assert_eq!(
            application.admission_at(UtcMicros(1)),
            RequestAdmission::Admitted
        );
    }

    #[test]
    #[allow(deprecated)]
    fn to_application_rejects_grant_for_another_scope() {
        let context = project_context();
        let other_scope = tracedecay_application::ResolvedScope::new(
            ProjectId::new("project.other").unwrap(),
            RepositoryId::new("repository.other").unwrap(),
            WorktreeId::new("worktree.other").unwrap(),
            None,
        )
        .unwrap();
        let grant = grant_for(&other_scope);

        // A grant minted for a different scope must fail closed, never be
        // rebound onto this request's scope.
        let error = context.to_application(grant).unwrap_err();
        assert!(
            matches!(error, ApplicationScopeError::Contract(ref message) if message.contains("grant scope")),
            "unexpected error: {error}"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn to_application_marks_cancelled_token_and_elapsed_deadline() {
        use tracedecay_application::RequestAdmission;
        use tracedecay_domain::UtcMicros;

        let cancellation = CancellationToken::new();
        let context = RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            RequestId::new("request.application-slice-1").unwrap(),
            project_identity(),
            CapabilityDigest::new(DIGEST),
            PolicyDigest::new(DIGEST),
            ConfigurationDigest::new(DIGEST),
            MonotonicDeadline::at(Instant::now()),
            cancellation.clone(),
            RequestBudgets::new(128, 4096, 32).unwrap(),
        );
        let scope = context.application_scope().unwrap();
        cancellation.cancel();

        let application = context.to_application(grant_for(&scope)).unwrap();

        assert!(application.cancellation().is_cancelled());
        match application.cancellation().state {
            tracedecay_application::CancellationState::Cancelled { requested_at } => {
                assert!(requested_at.0 > 0, "cancellation stamps observation time");
            }
            tracedecay_application::CancellationState::Active => {
                panic!("cancelled token must cross as cancelled")
            }
        }
        assert_eq!(
            application.admission_at(UtcMicros(i64::MAX)),
            RequestAdmission::Cancelled
        );
        // Without cancellation the already-elapsed deadline stays elapsed; the
        // boundary never resets it to a fresh budget.
        let context = RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            RequestId::new("request.application-slice-1").unwrap(),
            project_identity(),
            CapabilityDigest::new(DIGEST),
            PolicyDigest::new(DIGEST),
            ConfigurationDigest::new(DIGEST),
            MonotonicDeadline::at(Instant::now()),
            CancellationToken::new(),
            RequestBudgets::new(128, 4096, 32).unwrap(),
        );
        let application = context
            .to_application(grant_for(&context.application_scope().unwrap()))
            .unwrap();
        assert_eq!(
            application.admission_at(UtcMicros(i64::MAX)),
            RequestAdmission::TimedOut
        );
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn application_interruptible_observes_live_transport_cancellation() {
        let legacy = project_context();
        let scope = legacy.application_scope().unwrap();
        let application = legacy.to_application(grant_for(&scope)).unwrap();
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            trigger.cancel();
        });

        let result = run_application_request_interruptible(
            &application,
            &cancellation,
            std::future::pending::<()>(),
            || {},
        )
        .await;

        assert_eq!(result, Err(RequestInterruption::Cancelled));
        assert!(
            !application.cancellation().is_cancelled(),
            "the immutable admission snapshot stays unchanged while the live token cancels"
        );
    }

    #[test]
    fn grant_snapshot_digest_composes_capability_policy_configuration() {
        let context = project_context();
        let digest = context.grant_snapshot_digest().unwrap();
        assert_eq!(digest, context.grant_snapshot_digest().unwrap());

        let mut changed = [0x5a; 32];
        changed[0] ^= 0xff;
        let other = RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            RequestId::new("request.application-slice-1").unwrap(),
            project_identity(),
            CapabilityDigest::new(changed),
            PolicyDigest::new(DIGEST),
            ConfigurationDigest::new(DIGEST),
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
            RequestBudgets::new(128, 4096, 32).unwrap(),
        );
        assert_ne!(digest, other.grant_snapshot_digest().unwrap());
    }

    #[test]
    #[allow(deprecated)]
    fn exact_root_scope_resolution_is_stable_and_valid() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project_id = ProjectId::new("project.cli-scope-test").unwrap();

        let first = resolve_exact_root_scope(&root, &project_id).unwrap();
        let second = resolve_exact_root_scope(&root, &project_id).unwrap();

        assert_eq!(first, second);
        first.validate().unwrap();
        assert_eq!(first.project_id, project_id);
    }
}

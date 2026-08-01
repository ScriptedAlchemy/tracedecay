//! Request-context value types, plus the source-read helpers moved down from
//! the root binary's `src/context/{read_modes,source_read}.rs` (their only
//! consumer is `primitives::concrete` in this crate). See SEAMS.md.

pub mod read_cache;
pub mod read_modes;
pub mod source_read;

use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use tracedecay_domain::{ProjectId, RefId, RepositoryId, WorktreeId};

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

string_id!(ProfileId, SessionStoreId, SessionRootId, BranchId,);

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

/// The monotonic deadline and cooperative cancellation token moved into
/// `tracedecay_runtime_core::cancellation`: the kernel's
/// `store_runtime::rusqlite_parity` bounds every parity probe with them.
/// Re-exported so every historical `application::context::<item>` path keeps
/// resolving.
pub use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestInterruption {
    Cancelled,
    DeadlineExceeded,
}

fn wall_clock_micros() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_micros()),
    )
    .unwrap_or(i64::MAX)
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
    let repository_id = repository_id_for_root(&scope_root)?;
    let worktree_id = WorktreeId::new(format!(
        "worktree.daemon.{}",
        hex::encode(Sha256::digest(scope_root.to_string_lossy().as_bytes()))
    ))
    .map_err(|error| ApplicationScopeError::Contract(error.to_string()))?;
    let reference = tracedecay_runtime_core::branch::current_branch(&scope_root)
        .and_then(|branch| RefId::new(format!("refs/heads/{branch}")).ok());
    let scope = tracedecay_application::ResolvedScope::new(
        project_id.clone(),
        repository_id,
        worktree_id,
        reference,
    )
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
    let common = tracedecay_runtime_core::worktree::git_common_dir(root)
        .unwrap_or_else(|| root.to_path_buf());
    RepositoryId::new(format!(
        "repository.daemon.{}",
        hex::encode(Sha256::digest(common.to_string_lossy().as_bytes()))
    ))
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
    use std::collections::BTreeSet;

    use super::*;
    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestAdmission,
    };
    use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros};
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

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
        assert!(ProfileId::new(" profile.primary").is_err());
        assert!(SessionStoreId::new("store\nprimary").is_err());
        assert!(SessionRootId::new("root.primary ").is_err());
        assert!(BranchId::new("branch\0main").is_err());
    }

    fn grant_for(
        scope: &tracedecay_application::ResolvedScope,
        expires_at: UtcMicros,
    ) -> CapabilityGrantSnapshot {
        CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.application-slice-1").unwrap(),
            1,
            ManifestDigest::new(format!("sha256:{}", "5a".repeat(32))).unwrap(),
            ActorId::new("actor.cursor").unwrap(),
            UtcMicros(1),
            expires_at,
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.application-slice-1").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.application-slice-1").unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap()
    }

    fn application_context(
        scope: tracedecay_application::ResolvedScope,
        grant: CapabilityGrantSnapshot,
        deadline: UtcMicros,
        cancellation: CancellationContext,
    ) -> tracedecay_application::RequestContext {
        tracedecay_application::RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            scope,
            grant,
            tracedecay_application::RequestId::new("request.application-slice-1").unwrap(),
            Deadline::new(deadline).unwrap(),
            cancellation,
        )
        .unwrap()
    }

    #[test]
    fn application_scope_maps_project_identity_and_git_route() {
        let scope = project_identity().application_scope().unwrap();

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
    fn application_scope_fails_closed_for_profile_identity() {
        let identity = ResolvedSessionIdentity::for_profile(
            ProfileId::new("profile.primary").unwrap(),
            SessionStoreId::new("store.profile.primary").unwrap(),
            SessionRootId::new("root.profile.primary").unwrap(),
        );

        // A profile-scoped identity has no exact project root; the boundary
        // must fail closed rather than fabricate one from a path or the CWD.
        assert_eq!(
            identity.application_scope().unwrap_err(),
            ApplicationScopeError::ProfileIdentityWithoutProject
        );
    }

    #[test]
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

        assert_eq!(
            identity.application_scope().unwrap_err(),
            ApplicationScopeError::MissingGitRoute {
                project_id: "project.tracedecay".to_string(),
            }
        );
    }

    #[test]
    fn canonical_application_context_binds_scope_grant_deadline_and_cancellation() {
        let scope = project_identity().application_scope().unwrap();
        let grant = grant_for(&scope, UtcMicros(100));
        let application = application_context(
            scope.clone(),
            grant,
            UtcMicros(50),
            CancellationContext::active("request.application-slice-1").unwrap(),
        );

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
    fn canonical_application_context_rejects_grant_for_another_scope() {
        let scope = project_identity().application_scope().unwrap();
        let other_scope = tracedecay_application::ResolvedScope::new(
            ProjectId::new("project.other").unwrap(),
            RepositoryId::new("repository.other").unwrap(),
            WorktreeId::new("worktree.other").unwrap(),
            None,
        )
        .unwrap();
        let grant = grant_for(&other_scope, UtcMicros(100));

        // A grant minted for a different scope must fail closed, never be
        // rebound onto this request's scope.
        let error = tracedecay_application::RequestContext::new(
            ActorId::new("actor.cursor").unwrap(),
            scope,
            grant,
            tracedecay_application::RequestId::new("request.application-slice-1").unwrap(),
            Deadline::new(UtcMicros(50)).unwrap(),
            CancellationContext::active("request.application-slice-1").unwrap(),
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                tracedecay_application::ApplicationContractError::Inconsistent {
                    field: "request context grant scope"
                }
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn canonical_application_context_marks_cancelled_token() {
        let scope = project_identity().application_scope().unwrap();
        let application = application_context(
            scope.clone(),
            grant_for(&scope, UtcMicros(100)),
            UtcMicros(50),
            CancellationContext::cancelled("request.application-slice-1", UtcMicros(10)).unwrap(),
        );

        assert!(application.cancellation().is_cancelled());
        match application.cancellation().state {
            tracedecay_application::CancellationState::Cancelled { requested_at } => {
                assert_eq!(requested_at, UtcMicros(10));
            }
            tracedecay_application::CancellationState::Active => {
                panic!("cancelled token must cross as cancelled")
            }
        }
        assert_eq!(
            application.admission_at(UtcMicros(i64::MAX)),
            RequestAdmission::Cancelled
        );
    }

    #[tokio::test]
    async fn application_interruptible_observes_live_transport_cancellation() {
        let scope = project_identity().application_scope().unwrap();
        let application = application_context(
            scope.clone(),
            grant_for(&scope, UtcMicros(i64::MAX - 1)),
            UtcMicros(i64::MAX - 1),
            CancellationContext::active("request.application-slice-1").unwrap(),
        );
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

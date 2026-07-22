use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

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

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn is_same_token(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.cancelled, &other.cancelled) && Arc::ptr_eq(&self.notify, &other.notify)
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
}

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
}

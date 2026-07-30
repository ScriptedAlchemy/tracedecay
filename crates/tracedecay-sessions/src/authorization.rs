use std::fmt;

use tracedecay_domain::SessionId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorizationGrantId(String);

impl AuthorizationGrantId {
    pub fn new(value: impl Into<String>) -> Result<Self, SessionAuthorizationError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 512
            || value.chars().any(char::is_control)
        {
            return Err(SessionAuthorizationError::InvalidGrantId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AuthorizationGrantId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAccess {
    Read,
    Search,
    Hydrate,
}

impl SessionAccess {
    pub const fn matches_requested_access(self, requested: Self) -> bool {
        matches!(
            (self, requested),
            (Self::Read, Self::Read)
                | (Self::Search, Self::Search)
                | (Self::Hydrate, Self::Hydrate)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionRetrievalScope {
    Session(SessionId),
    AllSessionsInAuthorizedRoot,
}

impl SessionRetrievalScope {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Session(_) => "session",
            Self::AllSessionsInAuthorizedRoot => "all_sessions_in_authorized_root",
        }
    }

    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::Session(session_id) => Some(session_id),
            Self::AllSessionsInAuthorizedRoot => None,
        }
    }

    pub fn matches_requested_scope(&self, requested: &Self) -> bool {
        match (self, requested) {
            (Self::Session(granted), Self::Session(requested)) => granted == requested,
            (Self::AllSessionsInAuthorizedRoot, Self::AllSessionsInAuthorizedRoot) => true,
            _ => false,
        }
    }

    pub fn matches_session_target(&self, requested: &SessionId) -> bool {
        matches!(self, Self::Session(granted) if granted == requested)
    }

    pub const fn matches_authorized_root_target(&self) -> bool {
        matches!(self, Self::AllSessionsInAuthorizedRoot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAuthorizationError {
    InvalidGrantId,
    InvalidProviderScope,
    ZeroRevision,
    WrongScope,
    WrongContext,
    WrongTarget,
    WrongAccess,
    UnresolvedGitRoute,
    UnresolvedApplicationScope,
    Denied,
    Unavailable,
}

impl fmt::Display for SessionAuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidGrantId => "authorization grant ID is not canonical",
            Self::InvalidProviderScope => "authorization provider scope is not canonical",
            Self::ZeroRevision => "authorization grant revision must be greater than zero",
            Self::WrongScope => "requested session scope differs from resolved request scope",
            Self::WrongContext => {
                "authorization grant context digests differ from the current request"
            }
            Self::WrongTarget => "authorization grant target differs from the requested target",
            Self::WrongAccess => "authorization grant access differs from the requested access",
            Self::UnresolvedGitRoute => {
                "project session scope requires resolved repository, worktree, and branch routing"
            }
            Self::UnresolvedApplicationScope => {
                "session identity cannot resolve the exact project application scope"
            }
            Self::Denied => "session scope authorization was denied",
            Self::Unavailable => "session scope authorization is unavailable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SessionAuthorizationError {}

#[cfg(test)]
mod tests {
    use tracedecay_domain::SessionId;

    use super::{
        AuthorizationGrantId, SessionAccess, SessionAuthorizationError, SessionRetrievalScope,
    };

    #[test]
    fn grant_id_rejects_non_canonical_values() {
        assert_eq!(
            AuthorizationGrantId::new(" grant"),
            Err(SessionAuthorizationError::InvalidGrantId)
        );
        assert_eq!(
            AuthorizationGrantId::new("grant\n"),
            Err(SessionAuthorizationError::InvalidGrantId)
        );
        assert_eq!(
            AuthorizationGrantId::new("grant")
                .expect("canonical grant")
                .as_str(),
            "grant"
        );
    }

    #[test]
    fn root_scope_never_synthesizes_a_session() {
        let scope = SessionRetrievalScope::AllSessionsInAuthorizedRoot;
        assert_eq!(scope.kind(), "all_sessions_in_authorized_root");
        assert_eq!(scope.session_id(), None);
    }

    #[test]
    fn access_matches_only_the_exact_requested_access() {
        for granted in [
            SessionAccess::Read,
            SessionAccess::Search,
            SessionAccess::Hydrate,
        ] {
            for requested in [
                SessionAccess::Read,
                SessionAccess::Search,
                SessionAccess::Hydrate,
            ] {
                assert_eq!(
                    granted.matches_requested_access(requested),
                    granted == requested
                );
            }
        }
    }

    #[test]
    fn retrieval_scope_matches_only_the_exact_requested_scope() {
        let session = SessionId::new("session.one").expect("canonical session");
        let other_session = SessionId::new("session.two").expect("canonical session");
        let scoped = SessionRetrievalScope::Session(session.clone());
        let root = SessionRetrievalScope::AllSessionsInAuthorizedRoot;

        assert!(scoped.matches_requested_scope(&SessionRetrievalScope::Session(session.clone())));
        assert!(!scoped.matches_requested_scope(&SessionRetrievalScope::Session(other_session)));
        assert!(!scoped.matches_requested_scope(&root));
        assert!(root.matches_requested_scope(&SessionRetrievalScope::AllSessionsInAuthorizedRoot));
        assert!(!root.matches_requested_scope(&SessionRetrievalScope::Session(session)));
    }

    #[test]
    fn retrieval_scope_matches_only_its_typed_target_kind() {
        let session = SessionId::new("session.one").expect("canonical session");
        let other_session = SessionId::new("session.two").expect("canonical session");
        let scoped = SessionRetrievalScope::Session(session.clone());
        let root = SessionRetrievalScope::AllSessionsInAuthorizedRoot;

        assert!(scoped.matches_session_target(&session));
        assert!(!scoped.matches_session_target(&other_session));
        assert!(!scoped.matches_authorized_root_target());
        assert!(root.matches_authorized_root_target());
        assert!(!root.matches_session_target(&session));
    }
}

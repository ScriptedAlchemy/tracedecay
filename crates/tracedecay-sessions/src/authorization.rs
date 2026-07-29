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
    use super::{AuthorizationGrantId, SessionAuthorizationError, SessionRetrievalScope};

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
}

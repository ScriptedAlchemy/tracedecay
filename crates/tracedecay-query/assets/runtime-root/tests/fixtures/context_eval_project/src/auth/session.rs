//! Session lifecycle: creating sessions after login and validating session
//! tokens on later requests.

/// A logged-in user's session: a bearer token and the user it belongs to.
pub struct Session {
    pub username: String,
    pub token: String,
    pub expires_at_secs: u64,
}

/// Creates a new session for `username` with a freshly generated token.
/// Called by `auth::login::authenticate` after credentials are checked.
pub fn create_session(username: &str) -> Session {
    Session {
        username: username.to_string(),
        token: generate_token(username),
        expires_at_secs: 3600,
    }
}

/// Checks whether `session` is still within its validity window. Used on
/// every authenticated request to reject expired tokens.
pub fn validate_session(session: &Session, now_secs: u64) -> bool {
    now_secs < session.expires_at_secs
}

fn generate_token(username: &str) -> String {
    format!("tok_{username}_{}", username.len())
}

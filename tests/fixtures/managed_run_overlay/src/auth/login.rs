//! Credential checking for interactive login.

use crate::auth::session::{Session, create_session};

/// Checks a username/password pair and, on success, creates a new
/// authenticated session. This is the entry point for "how does
/// authentication work" style questions.
pub fn authenticate(username: &str, password: &str) -> Result<Session, String> {
    if !credentials_are_valid(username, password) {
        return Err("invalid username or password".to_string());
    }
    Ok(create_session(username))
}

/// Very small stand-in for a real credential check: non-empty username and
/// a password of at least 8 characters.
fn credentials_are_valid(username: &str, password: &str) -> bool {
    !username.is_empty() && password.len() >= 8
}

#[cfg(test)]
mod tests {
    use super::authenticate;

    #[test]
    fn successful_login_creates_session() {
        let session = authenticate("alice", "password123").expect("valid credentials");
        assert_eq!(session.username, "alice");
    }
}

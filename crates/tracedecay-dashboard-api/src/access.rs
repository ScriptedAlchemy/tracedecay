//! Per-listener dashboard access capability.
//!
//! Loopback is reachable by every local account, so binding to 127.0.0.1 is
//! not an access boundary. Each dashboard listener mints a random token that
//! only its launch URL carries. A browser trades that URL once for an
//! `HttpOnly`, `SameSite=Strict` cookie; programmatic clients send the token as
//! the Basic-auth password.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, Method, Uri, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use tracedecay_domain::errors::{Result, TraceDecayError};

const LAUNCH_QUERY_KEY: &str = "token";
const BASIC_AUTH_USER: &str = "tracedecay";

#[derive(Clone)]
pub struct DashboardAccessToken {
    token: Arc<str>,
    basic_authorization: Arc<str>,
}

impl DashboardAccessToken {
    pub fn mint() -> Result<Self> {
        let mut bytes = [0_u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|error| TraceDecayError::Config {
            message: format!("failed to generate dashboard access token: {error}"),
        })?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let basic_authorization = format!(
            "Basic {}",
            BASE64.encode(format!("{BASIC_AUTH_USER}:{token}"))
        );
        Ok(Self {
            token: token.into(),
            basic_authorization: basic_authorization.into(),
        })
    }

    /// The URL a person opens; the listener trades it for a session cookie.
    pub fn launch_url(&self, addr: SocketAddr) -> String {
        format!("http://{addr}/?{LAUNCH_QUERY_KEY}={}", self.token)
    }

    #[cfg(test)]
    pub(crate) fn basic_authorization(&self) -> &str {
        &self.basic_authorization
    }
}

impl fmt::Debug for DashboardAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DashboardAccessToken(<redacted>)")
    }
}

pub(crate) enum DashboardCredential {
    Admitted,
    /// A valid launch URL: answer with a redirect that drops the token from
    /// the address bar and sets the session cookie.
    Launch {
        location: String,
        set_cookie: HeaderValue,
    },
    Missing,
}

pub(crate) fn classify_credential(
    access: &DashboardAccessToken,
    port: u16,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> DashboardCredential {
    let cookie_name = session_cookie_name(port);
    // A launch URL is exchanged even when a session already exists, so the
    // token never stays in the address bar or history.
    if let Some(launch) = launch_exchange(access, &cookie_name, method, uri) {
        return launch;
    }
    let basic = headers.get(header::AUTHORIZATION).is_some_and(|value| {
        constant_time_eq(value.as_bytes(), access.basic_authorization.as_bytes())
    });
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| {
            name == cookie_name && constant_time_eq(value.as_bytes(), access.token.as_bytes())
        });
    if basic || cookie {
        DashboardCredential::Admitted
    } else {
        DashboardCredential::Missing
    }
}

fn launch_exchange(
    access: &DashboardAccessToken,
    cookie_name: &str,
    method: &Method,
    uri: &Uri,
) -> Option<DashboardCredential> {
    if !matches!(*method, Method::GET | Method::HEAD) {
        return None;
    }
    let query = uri.query()?;
    let mut launch = false;
    let remaining: Vec<&str> = query
        .split('&')
        .filter(|pair| match pair.split_once('=') {
            Some((LAUNCH_QUERY_KEY, value)) => {
                launch |= constant_time_eq(value.as_bytes(), access.token.as_bytes());
                false
            }
            _ => true,
        })
        .collect();
    if !launch {
        return None;
    }
    let location = if remaining.is_empty() {
        uri.path().to_owned()
    } else {
        format!("{}?{}", uri.path(), remaining.join("&"))
    };
    let set_cookie = HeaderValue::from_str(&format!(
        "{cookie_name}={}; Path=/; HttpOnly; SameSite=Strict",
        access.token
    ))
    .ok()?;
    Some(DashboardCredential::Launch {
        location,
        set_cookie,
    })
}

/// Browsers scope cookies by host, not port, so the name carries the port to
/// keep concurrent dashboards on one loopback host from overwriting each
/// other's session.
// ponytail: any listener on another 127.0.0.1 port that this browser visits
// also receives the cookie; closing that needs per-request header tokens in
// the SPA (EventSource cannot send headers), so the cookie stays until then.
fn session_cookie_name(port: u16) -> String {
    format!("tracedecay_dashboard_{port}")
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (l, r)| difference | (l ^ r))
            == 0
}

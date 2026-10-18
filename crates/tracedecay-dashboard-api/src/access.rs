//! Per-listener dashboard access capability.
//!
//! Loopback is reachable by every local account, so binding to 127.0.0.1 is
//! not an access boundary. Each dashboard listener mints a random token that
//! only its launch URL carries.
//!
//! The token is never stored in a cookie: browsers scope cookies by host, not
//! port, so a cookie would ride along to every other service on the same
//! loopback host. Instead the launch URL is traded for a redirect that moves
//! the token into the URL fragment (never sent on the wire, never in a
//! `Referer`); the page script keeps it in origin-scoped storage and sends it
//! as the Basic-auth password on each request, as programmatic clients do.
//! The embedded page shell carries no secret, so it is served without a
//! credential; every `/api` route needs one.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::{HeaderMap, Method, Uri, header};
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
        getrandom::fill(&mut bytes).map_err(|error| TraceDecayError::Config {
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

    /// The URL a person opens; the listener trades it for a redirect that
    /// hands the token to the page through the URL fragment.
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
    /// A valid launch URL on the page shell: answer with a redirect that drops
    /// the token from the query and delivers it in the fragment.
    Launch {
        location: String,
    },
    Missing,
}

pub(crate) fn classify_credential(
    access: &DashboardAccessToken,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> DashboardCredential {
    if is_page_shell(method, uri) {
        // A launch URL is exchanged even when the page already holds the
        // token, so the token never stays in the query or history.
        return launch_exchange(access, uri).unwrap_or(DashboardCredential::Admitted);
    }
    let basic = headers.get(header::AUTHORIZATION).is_some_and(|value| {
        constant_time_eq(value.as_bytes(), access.basic_authorization.as_bytes())
    });
    if basic {
        DashboardCredential::Admitted
    } else {
        DashboardCredential::Missing
    }
}

/// The embedded single-page app (index, `/static/**`, client-side routes) is
/// public code the browser must fetch before it can hold a token. Everything
/// under `/api` is data and stays credentialed.
fn is_page_shell(method: &Method, uri: &Uri) -> bool {
    let path = uri.path();
    matches!(*method, Method::GET | Method::HEAD) && path != "/api" && !path.starts_with("/api/")
}

fn launch_exchange(access: &DashboardAccessToken, uri: &Uri) -> Option<DashboardCredential> {
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
    let path = uri.path();
    let location = if remaining.is_empty() {
        format!("{path}#{LAUNCH_QUERY_KEY}={}", access.token)
    } else {
        format!(
            "{path}?{}#{LAUNCH_QUERY_KEY}={}",
            remaining.join("&"),
            access.token
        )
    };
    Some(DashboardCredential::Launch { location })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (l, r)| difference | (l ^ r))
            == 0
}
